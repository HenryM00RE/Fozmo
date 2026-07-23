use crate::app::state::AppState;
use crate::playback::artist_radio::local_artist_radio_next_source_from_source_for_zone;
use crate::playback::dispatcher::PlaybackDispatcher;
use crate::playback::intent::PlaybackIntent;
use crate::playback::lastfm::{
    lastfm_radio_has_future_queue, lastfm_radio_next_source_from_source_for_zone,
};
use crate::playback::qobuz::{
    prefetch_qobuz_queue_track_into_player, qobuz_radio_next_from_source_for_zone,
    qobuz_radio_next_request_from_source_for_zone,
};
use crate::playback::queue::{append_source_to_now_playing_queue, queue_loop_enabled_for_zone};
use crate::playback::request::{PlaybackGuard, PlaybackRequest};
use crate::playback::resolver::local_player_queue_items_from_sources;
use crate::playback::service::playback_config_for_zone;
use crate::playback::sonos::{prefetch_sonos_next, sonos_target_for_zone};
use crate::playback::source::{
    qobuz_queue_track_from_source_ref, qobuz_source_ref_from_play_request,
};
use crate::playback::status::{StatusResponse, build_status_response_for_zone};
use crate::playback::upnp::{prewarm_upnp_source_for_zone, upnp_target_for_zone};
use crate::protocol::{CoreToAgentCommand, SinkProtocol, SourceRef};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};

const QOBUZ_AUTO_ADVANCE_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);
const QOBUZ_PREFETCH_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(10);
const LASTFM_RADIO_PREFETCH_FAILURE_COOLDOWN: std::time::Duration =
    std::time::Duration::from_secs(30);
const UPNP_PREWARM_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(3);
const UPNP_AUTO_ADVANCE_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);
const UPNP_AUTO_ADVANCE_RETRY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(750),
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(5),
];
const UPNP_NEXT_HANDOFF_CONFIRM_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(900);
const UPNP_NEXT_HANDOFF_CONFIRM_POLL: std::time::Duration = std::time::Duration::from_millis(150);
const UPNP_COMPLETED_PREWARM_LIMIT: usize = 512;
const REMOTE_AUTO_ADVANCE_SKIP_LOG_COOLDOWN: std::time::Duration =
    std::time::Duration::from_secs(5);

#[derive(Clone, Default)]
pub(crate) struct AutoAdvanceMonitorState {
    remote_tail_observations: Arc<Mutex<HashMap<String, RemoteTailObservation>>>,
    remote_skip_logs: Arc<Mutex<HashMap<String, RemoteSkipLog>>>,
}

#[derive(Clone)]
struct RemoteTailObservation {
    source_key: String,
    position_secs: f64,
    duration_secs: f64,
}

struct RemoteSkipLog {
    reason: &'static str,
    logged_at: std::time::Instant,
}

pub(crate) fn maybe_spawn_qobuz_auto_advance(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
    pending: &Arc<Mutex<HashSet<String>>>,
    monitor_state: &AutoAdvanceMonitorState,
) {
    let protocol = state.zones().zone_protocol(zone_id);
    let Some(active_source) = state.listening().active_source(zone_id) else {
        monitor_state.clear_remote_tail(zone_id);
        return;
    };
    if matches!(protocol.as_ref(), Some(SinkProtocol::UpnpAvRenderer))
        && promote_observed_upnp_renderer_next(state, zone_id, status, &active_source)
    {
        monitor_state.clear_remote_tail(zone_id);
        return;
    }
    let Ok(queued) = state.library().zone_queue(zone_id) else {
        return;
    };
    let next_source = queued.first().map(|entry| entry.source.clone());
    let rest_sources = queued
        .iter()
        .skip(1)
        .map(|entry| entry.source.clone())
        .collect::<Vec<_>>();
    let status_matches_active_source = status_matches_active_source(status, &active_source);
    let upnp_stopped_on_queued_next =
        matches!(protocol.as_ref(), Some(SinkProtocol::UpnpAvRenderer))
            && status.state == "Stopped"
            && status.current_source.as_ref().is_some_and(|status_source| {
                next_source
                    .as_ref()
                    .is_some_and(|next| status_source.key() == next.key())
            });
    if matches!(active_source, SourceRef::QobuzTrack { .. })
        && status_identifies_different_qobuz_source(status, &active_source)
        && !upnp_stopped_on_queued_next
    {
        monitor_state.log_remote_skip(
            protocol.as_ref(),
            zone_id,
            status,
            Some(&active_source),
            next_source.as_ref(),
            "status_source_mismatch",
        );
        return;
    }
    monitor_state.observe_remote_tail(protocol.as_ref(), zone_id, status, &active_source);
    let queued_sources = queued
        .iter()
        .map(|entry| entry.source.clone())
        .collect::<Vec<_>>();
    let loop_auto_advance = queue_loop_enabled_for_zone(state, zone_id)
        && matches!(
            active_source,
            SourceRef::QobuzTrack { .. } | SourceRef::LocalTrack { .. }
        );
    let radio_auto_advance = next_source.is_none()
        && matches!(
            active_source,
            SourceRef::QobuzTrack { .. } | SourceRef::LocalTrack { .. }
        );
    let queued_auto_advance = next_source.is_some()
        && matches!(
            active_source,
            SourceRef::QobuzTrack { .. } | SourceRef::LocalTrack { .. }
        );
    let local_queue_auto_advance = queued_auto_advance
        && !matches!(
            protocol.as_ref(),
            Some(
                SinkProtocol::RemoteAgent | SinkProtocol::SonosUpnp | SinkProtocol::UpnpAvRenderer
            )
        );
    let sonos_queue_auto_advance =
        queued_auto_advance && matches!(protocol.as_ref(), Some(SinkProtocol::SonosUpnp));
    let upnp_queue_auto_advance =
        queued_auto_advance && matches!(protocol.as_ref(), Some(SinkProtocol::UpnpAvRenderer));
    let remote_queue_auto_advance = sonos_queue_auto_advance || upnp_queue_auto_advance;
    let upnp_next_was_armed = upnp_queue_auto_advance
        && next_source
            .as_ref()
            .is_some_and(|next| state.upnp().has_armed_next_for_source(zone_id, next));
    let completion = auto_advance_completion_reason(
        protocol.as_ref(),
        zone_id,
        status,
        &active_source,
        monitor_state,
    )
    .or_else(|| {
        upnp_failed_handoff_completion_reason(
            protocol.as_ref(),
            status,
            status_matches_active_source,
            upnp_next_was_armed,
            upnp_stopped_on_queued_next,
        )
    });
    if completion.is_none() {
        if remote_queue_auto_advance {
            let reason = if status.state != "Stopped" {
                "state_not_stopped"
            } else if !status_matches_active_source {
                "status_source_mismatch"
            } else {
                "completion_not_observed"
            };
            monitor_state.log_remote_skip(
                protocol.as_ref(),
                zone_id,
                status,
                Some(&active_source),
                next_source.as_ref(),
                reason,
            );
        }
        return;
    }
    if !loop_auto_advance
        && !radio_auto_advance
        && !local_queue_auto_advance
        && !sonos_queue_auto_advance
        && !upnp_queue_auto_advance
    {
        if matches!(
            protocol.as_ref(),
            Some(SinkProtocol::SonosUpnp | SinkProtocol::UpnpAvRenderer)
        ) {
            monitor_state.log_remote_skip(
                protocol.as_ref(),
                zone_id,
                status,
                Some(&active_source),
                next_source.as_ref(),
                "no_queue_or_radio_candidate",
            );
        }
        return;
    }
    let player = state.zones().player_for_zone(zone_id);
    if local_queue_auto_advance {
        let Some(player) = player.as_ref() else {
            return;
        };
        if player.has_stream_auto_advance_in_flight() {
            return;
        }
    }
    {
        let mut pending = pending.lock().unwrap();
        if !pending.insert(zone_id.to_string()) {
            return;
        }
    }

    let state_for_settle = state.clone();
    let loop_source = loop_auto_advance.then_some(active_source.clone());
    let radio_seed_source = radio_auto_advance.then_some(active_source.clone());
    let expected_active_source = active_source;
    let protocol_for_task = protocol.clone();
    let zone_id = zone_id.to_string();
    let pending = Arc::clone(pending);
    tokio::spawn(async move {
        debug!(
            event = "auto_advance_triggered",
            zone_id,
            protocol = ?protocol_for_task,
            completion_reason = completion.unwrap_or("unknown"),
            next_source_key = next_source.as_ref().map(SourceRef::key).as_deref(),
            queued_count = queued_sources.len(),
            "Queue auto-advance triggered"
        );
        let mut promoted_upnp_source_key = None;
        let result = if loop_auto_advance {
            let Some(source) = loop_source else {
                pending.lock().unwrap().remove(&zone_id);
                return;
            };
            queue_auto_advance(
                state_for_settle.clone(),
                zone_id.clone(),
                source,
                queued_sources,
            )
            .await
        } else if radio_auto_advance {
            let Some(seed_source) = radio_seed_source else {
                pending.lock().unwrap().remove(&zone_id);
                return;
            };
            radio_next_from_source_for_zone(state_for_settle.clone(), &zone_id, seed_source).await
        } else if sonos_queue_auto_advance || upnp_queue_auto_advance {
            let Some(next_source) = next_source else {
                pending.lock().unwrap().remove(&zone_id);
                return;
            };
            if upnp_queue_auto_advance {
                let next_was_armed = state_for_settle
                    .upnp()
                    .has_armed_next_for_source(&zone_id, &next_source);
                if next_was_armed
                    && wait_for_observed_upnp_renderer_next(
                        &state_for_settle,
                        &zone_id,
                        &next_source,
                    )
                    .await
                {
                    promoted_upnp_source_key = Some(next_source.key());
                    state_for_settle
                        .listening()
                        .next(state_for_settle.library(), &zone_id);
                    Ok(())
                } else {
                    let source_key = next_source.key();
                    let fallback_reason = if next_was_armed {
                        "renderer accepted SetNextAVTransportURI but did not confirm automatic next-track playback"
                    } else {
                        "next asset was not armed for this renderer"
                    };
                    state_for_settle.upnp().mark_next_handoff_fallback(
                        &zone_id,
                        &source_key,
                        fallback_reason,
                    );
                    state_for_settle.upnp().mark_notice(
                        &zone_id,
                        format!(
                            "UPnP gapless handoff unavailable ({fallback_reason}); using fallback auto-advance for {source_key}"
                        ),
                    );
                    debug!(
                        event = "upnp_next_handoff_fallback",
                        zone_id,
                        next_source_key = %source_key,
                        fallback_reason,
                        "UPnP queued auto-advance is using a non-gapless fresh-play fallback"
                    );
                    retry_upnp_queue_auto_advance(
                        state_for_settle.clone(),
                        zone_id.clone(),
                        expected_active_source.clone(),
                        next_source,
                        rest_sources,
                    )
                    .await
                }
            } else {
                queue_auto_advance(
                    state_for_settle.clone(),
                    zone_id.clone(),
                    next_source,
                    rest_sources,
                )
                .await
            }
        } else {
            let Some(next_source) = next_source else {
                pending.lock().unwrap().remove(&zone_id);
                return;
            };
            queue_auto_advance(
                state_for_settle.clone(),
                zone_id.clone(),
                next_source,
                rest_sources,
            )
            .await
        };
        match result {
            Ok(()) => {
                if let Some(source_key) = promoted_upnp_source_key.as_deref() {
                    state_for_settle
                        .upnp()
                        .mark_next_handoff_promoted(&zone_id, source_key);
                    debug!(
                        event = "upnp_next_handoff_promoted",
                        zone_id,
                        next_source_key = source_key,
                        "UPnP next-track handoff promoted to current playback"
                    );
                }
                wait_for_qobuz_auto_advance_status_settle(&state_for_settle, &zone_id).await;
                pending.lock().unwrap().remove(&zone_id);
            }
            Err(e) => {
                warn!(
                    event = "auto_advance_failed",
                    zone_id,
                    protocol = ?protocol_for_task,
                    error = %e,
                    "Auto-advance failed"
                );
                let cooldown = if matches!(
                    protocol_for_task.as_ref(),
                    Some(SinkProtocol::UpnpAvRenderer)
                ) {
                    UPNP_AUTO_ADVANCE_FAILURE_COOLDOWN
                } else {
                    QOBUZ_AUTO_ADVANCE_FAILURE_COOLDOWN
                };
                tokio::time::sleep(cooldown).await;
                pending.lock().unwrap().remove(&zone_id);
            }
        }
    });
}

pub(crate) fn maybe_spawn_lastfm_radio_prefetch(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
    pending: &Arc<Mutex<HashSet<String>>>,
) {
    if !matches!(status.state.as_str(), "Playing" | "Paused") {
        return;
    }
    let Some(active_source) = state.listening().active_source(zone_id) else {
        return;
    };
    if lastfm_radio_has_future_queue(state, zone_id, &active_source) {
        return;
    }
    if matches!(active_source, SourceRef::QobuzTrack { .. })
        && status_identifies_different_qobuz_source(status, &active_source)
    {
        return;
    }
    let expected_current = status
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|file_name| !file_name.is_empty())
        .map(str::to_string);
    let expected_epoch = state
        .zones()
        .player_for_zone(zone_id)
        .map(|player| player.playback_epoch());

    {
        let mut pending = pending.lock().unwrap();
        if !pending.insert(zone_id.to_string()) {
            return;
        }
    }

    let state = state.clone();
    let zone_id = zone_id.to_string();
    let pending = Arc::clone(pending);
    tokio::spawn(async move {
        let result = async {
            let mut next_source = None;
            if state.settings().lastfm_radio_enabled() {
                match lastfm_radio_next_source_from_source_for_zone(
                    state.clone(),
                    &zone_id,
                    active_source.clone(),
                )
                .await
                {
                    Ok(source) => next_source = source,
                    Err(e) => eprintln!(
                        "lastfm: radio prefetch failed; trying local artist radio before Qobuz: {e}"
                    ),
                }
            }
            let next_source = if let Some(next_source) = next_source {
                next_source
            } else if let Some(source) = local_artist_radio_next_source_from_source_for_zone(
                &state,
                &zone_id,
                &active_source,
            )? {
                source
            } else {
                let Some(req) = qobuz_radio_next_request_from_source_for_zone(
                    state.clone(),
                    &zone_id,
                    active_source.clone(),
                )
                .await?
                else {
                    return Err("Last.fm radio returned no playable recommendation".to_string());
                };
                qobuz_source_ref_from_play_request(&req)
            };
            if lastfm_radio_has_future_queue(&state, &zone_id, &active_source) {
                return Err("Queue changed".to_string());
            }
            if state
                .listening()
                .active_source(&zone_id)
                .is_none_or(|source| source.key() != active_source.key())
            {
                return Err("Playback changed".to_string());
            }
            arm_lastfm_radio_source(
                state.clone(),
                &zone_id,
                next_source,
                active_source.key(),
                expected_current,
                expected_epoch,
            )
            .await
        }
        .await;
        if let Err(e) = result
            && e != "Playback changed"
            && e != "Queue changed"
        {
            eprintln!("lastfm: radio prefetch skipped: {e}");
            tokio::time::sleep(LASTFM_RADIO_PREFETCH_FAILURE_COOLDOWN).await;
        }
        pending.lock().unwrap().remove(&zone_id);
    });
}

pub(crate) fn maybe_spawn_qobuz_next_prefetch(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
    pending: &Arc<Mutex<HashSet<String>>>,
) {
    if !matches!(status.state.as_str(), "Playing" | "Paused") {
        return;
    }
    let protocol = state.zones().zone_protocol(zone_id);
    if matches!(
        protocol,
        Some(SinkProtocol::RemoteAgent | SinkProtocol::SonosUpnp)
    ) {
        return;
    }
    if protocol == Some(SinkProtocol::UpnpAvRenderer) {
        return;
    }
    let Some(expected_current) = status
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|file_name| !file_name.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let Some(active_source) = state.listening().active_source(zone_id) else {
        return;
    };
    if !matches!(active_source, SourceRef::QobuzTrack { .. }) {
        return;
    }
    if status_identifies_different_qobuz_source(status, &active_source) {
        return;
    }
    let next_track = if queue_loop_enabled_for_zone(state, zone_id) {
        qobuz_queue_track_from_source_ref(&active_source)
    } else {
        let Ok(queued) = state.library().zone_queue(zone_id) else {
            return;
        };
        queued
            .first()
            .and_then(|entry| qobuz_queue_track_from_source_ref(&entry.source))
    };
    let Some(next_track) = next_track else {
        return;
    };
    let Some(player) = state.zones().player_for_zone(zone_id) else {
        return;
    };
    if player.stream_queue_len() > 0 || player.has_stream_auto_advance_in_flight() {
        return;
    }
    let expected_epoch = player.playback_epoch();

    {
        let mut pending = pending.lock().unwrap();
        if !pending.insert(zone_id.to_string()) {
            return;
        }
    }

    let state = state.clone();
    let zone_id = zone_id.to_string();
    let pending = Arc::clone(pending);
    tokio::spawn(async move {
        let result = prefetch_qobuz_queue_track_into_player(
            state,
            zone_id.clone(),
            next_track,
            expected_current,
            expected_epoch,
            false,
        )
        .await;
        if let Err(e) = result
            && e != "Playback changed"
        {
            eprintln!("qobuz: monitor queued-track prefetch skipped: {e}");
            tokio::time::sleep(QOBUZ_PREFETCH_FAILURE_COOLDOWN).await;
        }
        pending.lock().unwrap().remove(&zone_id);
    });
}

pub(crate) fn maybe_spawn_upnp_next_prewarm(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
    pending: &Arc<Mutex<HashSet<String>>>,
    completed: &Arc<Mutex<HashSet<String>>>,
) {
    let Some((active_source, next_source, prewarm_key)) =
        upnp_next_prewarm_candidate(state, zone_id, status)
    else {
        clear_completed_upnp_prewarms_for_zone(completed, zone_id);
        return;
    };
    retain_completed_upnp_prewarm_candidate(completed, zone_id, &prewarm_key);
    if state
        .upnp()
        .has_armed_next_for_source(zone_id, &next_source)
    {
        // A transient renderer snapshot may have cleared the monitor's cache,
        // but the session still knows this exact source is armed. Re-sending
        // SetNext can make KEF reopen or replace the queued stream.
        remember_completed_upnp_prewarm(completed, prewarm_key);
        return;
    }
    {
        let completed = completed.lock().unwrap();
        if completed.contains(&prewarm_key) {
            return;
        }
    }
    {
        let mut pending = pending.lock().unwrap();
        if !pending.insert(prewarm_key.clone()) {
            return;
        }
    }

    let state = state.clone();
    let zone_id = zone_id.to_string();
    let pending = Arc::clone(pending);
    let completed = Arc::clone(completed);
    tokio::spawn(async move {
        let result = prewarm_upnp_source_for_zone(
            state,
            &zone_id,
            None,
            Some(active_source),
            next_source,
            true,
        )
        .await;
        match result {
            Ok(()) => {
                remember_completed_upnp_prewarm(&completed, prewarm_key.clone());
            }
            Err(e) if e.message() == "Playback changed" => {}
            Err(e) => {
                eprintln!("upnp: queued-track prewarm skipped: {}", e.message());
                tokio::time::sleep(UPNP_PREWARM_FAILURE_COOLDOWN).await;
            }
        }
        pending.lock().unwrap().remove(&prewarm_key);
    });
}

fn upnp_next_prewarm_candidate(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
) -> Option<(SourceRef, SourceRef, String)> {
    if state.zones().zone_protocol(zone_id) != Some(SinkProtocol::UpnpAvRenderer) {
        return None;
    }
    if state.upnp().next_handoff_blocked_by_seek(zone_id) {
        return None;
    }
    if !matches!(status.state.as_str(), "Playing" | "Paused") || status.transport_pending != "none"
    {
        return None;
    }
    if upnp_progressive_dop_is_settling(state, zone_id) {
        return None;
    }
    let active_source = state.listening().active_source(zone_id)?;
    if let Some(status_source) = status.current_source.as_ref()
        && status_source.key() != active_source.key()
    {
        return None;
    }
    if matches!(active_source, SourceRef::QobuzTrack { .. })
        && status_identifies_different_qobuz_source(status, &active_source)
    {
        return None;
    }
    let queued = state.library().zone_queue(zone_id).ok()?;
    let next_source = if queue_loop_enabled_for_zone(state, zone_id) {
        active_source.clone()
    } else {
        queued.first()?.source.clone()
    };
    if next_source.key() == active_source.key() {
        return None;
    }
    let render_signature = status
        .upnp_configured_render_signature
        .as_deref()
        .unwrap_or("unknown");
    let prewarm_key = format!(
        "{}|{}|{}|{}",
        zone_id,
        active_source.key(),
        next_source.key(),
        render_signature
    );
    Some((active_source, next_source, prewarm_key))
}

fn promote_observed_upnp_renderer_next(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
    active_source: &SourceRef,
) -> bool {
    if status.state != "Playing" || status.transport_pending != "none" {
        return false;
    }
    let Some(status_source) = status.current_source.as_ref() else {
        return false;
    };
    if status_source.key() == active_source.key() {
        return false;
    }
    let Ok(queued) = state.library().zone_queue(zone_id) else {
        return false;
    };
    if queued
        .first()
        .is_none_or(|entry| entry.source.key() != status_source.key())
    {
        return false;
    }
    state.listening().next(state.library(), zone_id);
    debug!(
        event = "upnp_observed_renderer_next_promoted",
        zone_id,
        previous_source_key = %active_source.key(),
        next_source_key = %status_source.key(),
        "UPnP renderer advanced to the armed next item; app queue state promoted without replay"
    );
    true
}

fn upnp_progressive_dop_is_settling(state: &AppState, zone_id: &str) -> bool {
    let Some(snapshot) = state.upnp().snapshot(zone_id) else {
        return false;
    };
    if snapshot.current_render_or_stream_plan.as_deref() != Some("progressive_wav_stream")
        || !matches!(
            snapshot.active_output_mode.as_deref(),
            Some("Dsd64" | "Dsd128" | "Dsd256")
        )
    {
        return false;
    }
    snapshot.state != "Playing"
        || snapshot.transport_pending != "none"
        || snapshot.position_secs < 8.0
}

fn remember_completed_upnp_prewarm(completed: &Arc<Mutex<HashSet<String>>>, key: String) {
    let mut completed = completed.lock().unwrap();
    if completed.len() >= UPNP_COMPLETED_PREWARM_LIMIT {
        completed.clear();
    }
    completed.insert(key);
}

fn retain_completed_upnp_prewarm_candidate(
    completed: &Arc<Mutex<HashSet<String>>>,
    zone_id: &str,
    current_key: &str,
) {
    let zone_prefix = upnp_prewarm_zone_prefix(zone_id);
    completed
        .lock()
        .unwrap()
        .retain(|key| !key.starts_with(&zone_prefix) || key == current_key);
}

fn clear_completed_upnp_prewarms_for_zone(completed: &Arc<Mutex<HashSet<String>>>, zone_id: &str) {
    let zone_prefix = upnp_prewarm_zone_prefix(zone_id);
    completed
        .lock()
        .unwrap()
        .retain(|key| !key.starts_with(&zone_prefix));
}

fn upnp_prewarm_zone_prefix(zone_id: &str) -> String {
    format!("{zone_id}|")
}

impl AutoAdvanceMonitorState {
    fn observe_remote_tail(
        &self,
        protocol: Option<&SinkProtocol>,
        zone_id: &str,
        status: &StatusResponse,
        active_source: &SourceRef,
    ) {
        if !is_remote_queue_protocol(protocol) {
            self.clear_remote_tail(zone_id);
            return;
        }
        if !status_matches_active_source(status, active_source) {
            return;
        }
        if looks_like_completed_track(status) {
            self.remote_tail_observations.lock().unwrap().insert(
                zone_id.to_string(),
                RemoteTailObservation {
                    source_key: active_source.key(),
                    position_secs: status.position_secs,
                    duration_secs: status.duration_secs,
                },
            );
        }
    }

    fn clear_remote_tail(&self, zone_id: &str) {
        self.remote_tail_observations
            .lock()
            .unwrap()
            .remove(zone_id);
        self.remote_skip_logs.lock().unwrap().remove(zone_id);
    }

    fn remote_tail_for_source(
        &self,
        zone_id: &str,
        active_source: &SourceRef,
    ) -> Option<RemoteTailObservation> {
        let source_key = active_source.key();
        self.remote_tail_observations
            .lock()
            .unwrap()
            .get(zone_id)
            .filter(|tail| tail.source_key == source_key)
            .cloned()
    }

    fn log_remote_skip(
        &self,
        protocol: Option<&SinkProtocol>,
        zone_id: &str,
        status: &StatusResponse,
        active_source: Option<&SourceRef>,
        next_source: Option<&SourceRef>,
        reason: &'static str,
    ) {
        if !is_remote_queue_protocol(protocol) {
            return;
        }
        let now = std::time::Instant::now();
        {
            let mut logs = self.remote_skip_logs.lock().unwrap();
            if let Some(previous) = logs.get(zone_id)
                && previous.reason == reason
                && now.duration_since(previous.logged_at) < REMOTE_AUTO_ADVANCE_SKIP_LOG_COOLDOWN
            {
                return;
            }
            logs.insert(
                zone_id.to_string(),
                RemoteSkipLog {
                    reason,
                    logged_at: now,
                },
            );
        }
        debug!(
            event = "auto_advance_skipped",
            zone_id,
            protocol = ?protocol,
            reason,
            state = status.state.as_str(),
            position_secs = status.position_secs,
            duration_secs = status.duration_secs,
            transport_pending = status.transport_pending.as_str(),
            active_source_key = active_source.map(SourceRef::key).as_deref(),
            status_source_key = status.current_source.as_ref().map(SourceRef::key).as_deref(),
            next_source_key = next_source.map(SourceRef::key).as_deref(),
            "Remote queue auto-advance skipped"
        );
    }
}

fn auto_advance_completion_reason(
    protocol: Option<&SinkProtocol>,
    zone_id: &str,
    status: &StatusResponse,
    active_source: &SourceRef,
    monitor_state: &AutoAdvanceMonitorState,
) -> Option<&'static str> {
    if status.state != "Stopped" {
        return None;
    }
    if !status_matches_active_source(status, active_source) {
        return None;
    }
    if looks_like_completed_track(status) {
        return Some("tail_snapshot");
    }
    if !is_remote_queue_protocol(protocol) {
        return None;
    }
    let tail = monitor_state.remote_tail_for_source(zone_id, active_source)?;
    debug!(
        event = "remote_completion_tail_fallback",
        zone_id,
        protocol = ?protocol,
        source_key = tail.source_key.as_str(),
        last_tail_position_secs = tail.position_secs,
        last_tail_duration_secs = tail.duration_secs,
        final_position_secs = status.position_secs,
        final_duration_secs = status.duration_secs,
        "Using prior remote tail observation for completed-track auto-advance"
    );
    Some("remote_tail_observed")
}

fn upnp_failed_handoff_completion_reason(
    protocol: Option<&SinkProtocol>,
    status: &StatusResponse,
    status_matches_active_source: bool,
    next_was_armed: bool,
    stopped_on_queued_next: bool,
) -> Option<&'static str> {
    if !matches!(protocol, Some(SinkProtocol::UpnpAvRenderer)) || status.state != "Stopped" {
        return None;
    }
    if stopped_on_queued_next {
        return Some("queued_next_loaded_but_stopped");
    }
    (status_matches_active_source && next_was_armed).then_some("armed_next_stopped")
}

fn is_remote_queue_protocol(protocol: Option<&SinkProtocol>) -> bool {
    matches!(
        protocol,
        Some(SinkProtocol::RemoteAgent | SinkProtocol::SonosUpnp | SinkProtocol::UpnpAvRenderer)
    )
}

fn status_matches_active_source(status: &StatusResponse, active_source: &SourceRef) -> bool {
    if let Some(status_source) = status.current_source.as_ref()
        && status_source.key() != active_source.key()
    {
        return false;
    }
    !matches!(active_source, SourceRef::QobuzTrack { .. })
        || !status_identifies_different_qobuz_source(status, active_source)
}

fn looks_like_completed_track(status: &StatusResponse) -> bool {
    let duration = status.duration_secs;
    let position = status.position_secs;
    duration.is_finite()
        && position.is_finite()
        && duration > 0.0
        && position > 0.0
        && ((duration > 2.0 && position >= duration - 2.0) || position / duration >= 0.95)
}

fn status_identifies_different_qobuz_source(status: &StatusResponse, source: &SourceRef) -> bool {
    let SourceRef::QobuzTrack {
        track_id,
        title,
        artist,
        ..
    } = source
    else {
        return false;
    };

    if let (Some(actual_title), Some(expected_title)) =
        (status.track_title.as_deref(), title.as_deref())
    {
        if !text_eq(actual_title, expected_title) {
            return true;
        }
        if let (Some(actual_artist), Some(expected_artist)) =
            (status.track_artist.as_deref(), artist.as_deref())
        {
            return !text_eq(actual_artist, expected_artist);
        }
        return false;
    }

    let expected_file_name =
        qobuz_source_display_name(*track_id, title.as_deref(), artist.as_deref());
    status
        .file_name
        .as_deref()
        .is_some_and(|actual| actual != expected_file_name)
}

fn qobuz_source_display_name(track_id: u64, title: Option<&str>, artist: Option<&str>) -> String {
    match (artist, title) {
        (Some(artist), Some(title)) => format!("{artist} - {title}"),
        (_, Some(title)) => title.to_string(),
        _ => format!("qobuz:{track_id}"),
    }
}

async fn wait_for_qobuz_auto_advance_status_settle(state: &AppState, zone_id: &str) {
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let Ok(status) = build_status_response_for_zone(state, zone_id) else {
            return;
        };
        if status.state == "Playing" || status.state == "Paused" {
            return;
        }
        if status.state != "Stopped" || !looks_like_completed_track(&status) {
            return;
        }
    }
}

async fn radio_next_from_source_for_zone(
    state: AppState,
    zone_id: &str,
    seed_source: SourceRef,
) -> Result<(), String> {
    if state.settings().lastfm_radio_enabled() {
        match lastfm_radio_next_source_from_source_for_zone(
            state.clone(),
            zone_id,
            seed_source.clone(),
        )
        .await
        {
            Ok(Some(source)) => {
                if let Err(e) = append_source_to_now_playing_queue(&state, zone_id, &source) {
                    eprintln!(
                        "lastfm: failed to append fallback radio source to now-playing queue: {e}"
                    );
                }
                return queue_auto_advance(state, zone_id.to_string(), source, Vec::new()).await;
            }
            Ok(None) => eprintln!(
                "lastfm: radio returned no playable recommendation; falling back to local artist radio"
            ),
            Err(e) => eprintln!(
                "lastfm: radio failed; falling back to local artist radio before Qobuz: {e}"
            ),
        }
    }

    if let Some(source) =
        local_artist_radio_next_source_from_source_for_zone(&state, zone_id, &seed_source)?
    {
        if let Err(e) = append_source_to_now_playing_queue(&state, zone_id, &source) {
            eprintln!("artist-radio: failed to append fallback source to now-playing queue: {e}");
        }
        return queue_auto_advance(state, zone_id.to_string(), source, Vec::new()).await;
    }

    qobuz_radio_next_from_source_for_zone(state, zone_id, seed_source)
        .await
        .map(|_| ())
}

async fn arm_lastfm_radio_source(
    state: AppState,
    zone_id: &str,
    source: SourceRef,
    expected_source_key: String,
    expected_current: Option<String>,
    expected_epoch: Option<u64>,
) -> Result<(), String> {
    ensure_lastfm_prefetch_target_is_current(
        &state,
        zone_id,
        &expected_source_key,
        expected_current.as_deref(),
        expected_epoch,
    )?;

    let queue = vec![source.clone()];

    match state.zones().zone_protocol(zone_id) {
        Some(SinkProtocol::RemoteAgent) => {
            commit_lastfm_radio_queue(&state, zone_id, &queue, &source)?;
            state
                .zones()
                .send_to_zone(
                    zone_id,
                    CoreToAgentCommand::SetQueue {
                        queue: queue.clone(),
                    },
                )
                .map_err(|e| format!("set remote Last.fm radio queue: {e}"))?;
            state
                .zones()
                .send_to_zone(
                    zone_id,
                    CoreToAgentCommand::PreFetch {
                        source_ref: source,
                        stream_base_url: state.public_base_url().clone(),
                    },
                )
                .map_err(|e| format!("prefetch remote Last.fm radio queue: {e}"))?;
        }
        Some(SinkProtocol::SonosUpnp) => {
            let target =
                sonos_target_for_zone(&state, zone_id).map_err(|e| e.message().to_string())?;
            let playback_config =
                playback_config_for_zone(&state, zone_id, state.zones().active_player().as_ref());
            prefetch_sonos_next(
                state.clone(),
                zone_id.to_string(),
                target,
                expected_current.clone(),
                None,
                queue.clone(),
                playback_config,
            )
            .await;
            ensure_lastfm_prefetch_target_is_current(
                &state,
                zone_id,
                &expected_source_key,
                expected_current.as_deref(),
                expected_epoch,
            )?;
            commit_lastfm_radio_queue(&state, zone_id, &queue, &source)?;
        }
        _ => match &source {
            SourceRef::LocalTrack { .. } => {
                let Some(player) = state.zones().player_for_zone(zone_id) else {
                    return Err("Zone not available".to_string());
                };
                let queue_items = local_player_queue_items_from_sources(&state, &queue);
                if queue_items.is_empty() {
                    return Err("Last.fm local radio track was not playable".to_string());
                }
                ensure_lastfm_prefetch_target_is_current(
                    &state,
                    zone_id,
                    &expected_source_key,
                    expected_current.as_deref(),
                    expected_epoch,
                )?;
                player.set_queue_if_epoch(queue_items, expected_epoch);
                ensure_lastfm_prefetch_target_is_current(
                    &state,
                    zone_id,
                    &expected_source_key,
                    expected_current.as_deref(),
                    expected_epoch,
                )?;
                commit_lastfm_radio_queue(&state, zone_id, &queue, &source)?;
            }
            SourceRef::QobuzTrack { .. } => {
                let Some(expected_current) = expected_current else {
                    return Ok(());
                };
                let Some(expected_epoch) = expected_epoch else {
                    return Ok(());
                };
                let Some(track) = qobuz_queue_track_from_source_ref(&source) else {
                    return Err("Last.fm Qobuz radio track was not playable".to_string());
                };
                prefetch_qobuz_queue_track_into_player(
                    state.clone(),
                    zone_id.to_string(),
                    track,
                    expected_current.clone(),
                    expected_epoch,
                    true,
                )
                .await?;
                ensure_lastfm_prefetch_target_is_current(
                    &state,
                    zone_id,
                    &expected_source_key,
                    Some(expected_current.as_str()),
                    Some(expected_epoch),
                )?;
                commit_lastfm_radio_queue(&state, zone_id, &queue, &source)?;
            }
        },
    }

    Ok(())
}

fn ensure_lastfm_prefetch_target_is_current(
    state: &AppState,
    zone_id: &str,
    expected_source_key: &str,
    expected_current: Option<&str>,
    expected_epoch: Option<u64>,
) -> Result<(), String> {
    if state
        .listening()
        .active_source(zone_id)
        .is_none_or(|source| source.key() != expected_source_key)
    {
        return Err("Playback changed".to_string());
    }

    let Some(player) = state.zones().player_for_zone(zone_id) else {
        return Ok(());
    };
    if expected_epoch.is_some_and(|epoch| player.playback_epoch() != epoch) {
        return Err("Playback changed".to_string());
    }
    if expected_current
        .is_some_and(|expected| player.current_file_name().as_deref() != Some(expected))
    {
        return Err("Playback changed".to_string());
    }
    Ok(())
}

fn commit_lastfm_radio_queue(
    state: &AppState,
    zone_id: &str,
    queue: &[SourceRef],
    source: &SourceRef,
) -> Result<(), String> {
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    state
        .library()
        .set_zone_queue(zone_id, queue)
        .map_err(|e| format!("set Last.fm radio queue: {e}"))?;
    if let Err(e) = append_source_to_now_playing_queue(state, zone_id, source) {
        eprintln!("lastfm: failed to append radio source to now-playing queue: {e}");
    }
    state
        .listening()
        .append_queue_with_radio(zone_id, profile_id, queue.to_vec(), true);
    Ok(())
}

async fn queue_auto_advance(
    state: AppState,
    zone_id: String,
    source: SourceRef,
    rest_sources: Vec<SourceRef>,
) -> Result<(), String> {
    let radio_auto = source.is_radio();
    PlaybackDispatcher::new(&state)
        .execute(
            &zone_id,
            PlaybackIntent::Play {
                request: PlaybackRequest {
                    profile_id: state
                        .listening()
                        .profile_id(&zone_id)
                        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string()),
                    source,
                    queue: rest_sources,
                    radio_auto,
                    guard: PlaybackGuard::none(),
                    qobuz_request: None,
                },
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| error.message().to_string())
}

async fn wait_for_observed_upnp_renderer_next(
    state: &AppState,
    zone_id: &str,
    next_source: &SourceRef,
) -> bool {
    let Ok(target) = upnp_target_for_zone(state, zone_id) else {
        return false;
    };
    let started = std::time::Instant::now();
    loop {
        let _ = state
            .upnp()
            .refresh_playback_snapshot(zone_id, &target, std::time::Duration::ZERO)
            .await;
        if state.upnp().snapshot(zone_id).is_some_and(|snapshot| {
            snapshot.state == "Playing"
                && snapshot.transport_pending == "none"
                && snapshot
                    .current_source
                    .as_ref()
                    .is_some_and(|source| source.key() == next_source.key())
        }) {
            return true;
        }
        if started.elapsed() >= UPNP_NEXT_HANDOFF_CONFIRM_TIMEOUT {
            return false;
        }
        tokio::time::sleep(UPNP_NEXT_HANDOFF_CONFIRM_POLL).await;
    }
}

async fn retry_upnp_queue_auto_advance(
    state: AppState,
    zone_id: String,
    expected_active_source: SourceRef,
    source: SourceRef,
    rest_sources: Vec<SourceRef>,
) -> Result<(), String> {
    let mut last_error = None;
    let mut expected_command_generation = None;
    for attempt in 0..=UPNP_AUTO_ADVANCE_RETRY_DELAYS.len() {
        if attempt > 0 {
            tokio::time::sleep(UPNP_AUTO_ADVANCE_RETRY_DELAYS[attempt - 1]).await;
            ensure_upnp_auto_advance_target_is_current(
                &state,
                &zone_id,
                &expected_active_source,
                &source,
                expected_command_generation,
            )?;
            debug!(
                event = "upnp_auto_advance_retry",
                zone_id,
                attempt = attempt + 1,
                next_source_key = %source.key(),
                previous_error = last_error.as_deref().unwrap_or("unknown"),
                "Retrying UPnP next-track resolution and playback"
            );
        }
        match queue_auto_advance(
            state.clone(),
            zone_id.clone(),
            source.clone(),
            rest_sources.clone(),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) if error == "Playback changed" || error == "Queue changed" => {
                return Err(error);
            }
            Err(error) => {
                last_error = Some(error);
                expected_command_generation =
                    Some(state.upnp().current_command_generation(&zone_id));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "UPnP auto-advance failed".to_string()))
}

fn ensure_upnp_auto_advance_target_is_current(
    state: &AppState,
    zone_id: &str,
    expected_active_source: &SourceRef,
    next_source: &SourceRef,
    expected_command_generation: Option<u64>,
) -> Result<(), String> {
    if expected_command_generation
        .is_some_and(|expected| state.upnp().current_command_generation(zone_id) != expected)
    {
        return Err("Playback changed".to_string());
    }
    if state
        .listening()
        .active_source(zone_id)
        .as_ref()
        .map(SourceRef::key)
        != Some(expected_active_source.key())
    {
        return Err("Playback changed".to_string());
    }
    let queued = state
        .library()
        .zone_queue(zone_id)
        .map_err(|error| format!("read UPnP queue before retry: {error}"))?;
    if queued
        .first()
        .is_none_or(|entry| entry.source.key() != next_source.key())
    {
        return Err("Queue changed".to_string());
    }
    Ok(())
}

fn text_eq(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{agent_capabilities, app_state, qobuz_source};
    use crate::protocol::{CoreToAgentCommand, SinkProtocol};
    use tokio::sync::mpsc;

    #[test]
    fn completion_detection_does_not_treat_immediate_short_stop_as_finished() {
        let status = status_with_position("Stopped", 0.0, 1.5);
        assert!(!looks_like_completed_track(&status));

        let status = status_with_position("Stopped", 1.43, 1.5);
        assert!(looks_like_completed_track(&status));
    }

    #[test]
    fn completion_detection_allows_tail_for_normal_tracks() {
        let status = status_with_position("Stopped", 118.2, 120.0);
        assert!(looks_like_completed_track(&status));
    }

    #[test]
    fn upnp_stopped_with_armed_next_uses_handoff_failure_completion() {
        let status = status_with_position("Stopped", 90.0, 180.0);

        assert_eq!(
            upnp_failed_handoff_completion_reason(
                Some(&SinkProtocol::UpnpAvRenderer),
                &status,
                true,
                true,
                false,
            ),
            Some("armed_next_stopped")
        );
        assert_eq!(
            upnp_failed_handoff_completion_reason(
                Some(&SinkProtocol::UpnpAvRenderer),
                &status,
                true,
                false,
                false,
            ),
            None,
            "an ordinary explicit stop must not advance without an armed handoff"
        );
    }

    #[test]
    fn upnp_loaded_queued_next_but_stopped_uses_fresh_play_completion() {
        let status = status_with_position("Stopped", 0.0, 240.0);

        assert_eq!(
            upnp_failed_handoff_completion_reason(
                Some(&SinkProtocol::UpnpAvRenderer),
                &status,
                false,
                false,
                true,
            ),
            Some("queued_next_loaded_but_stopped")
        );
        assert_eq!(
            upnp_failed_handoff_completion_reason(
                Some(&SinkProtocol::SonosUpnp),
                &status,
                false,
                false,
                true,
            ),
            None,
            "the KEF/UPnP fallback must not change Sonos completion semantics"
        );
    }

    #[test]
    fn remote_completion_uses_observed_tail_after_reset_snapshot() {
        let monitor_state = AutoAdvanceMonitorState::default();
        let source = qobuz_source(42, false);
        let mut tail_status = status_with_position("Playing", 118.2, 120.0);
        tail_status.current_source = Some(source.clone());
        monitor_state.observe_remote_tail(
            Some(&SinkProtocol::UpnpAvRenderer),
            "upnp-zone",
            &tail_status,
            &source,
        );

        let mut stopped_status = status_with_position("Stopped", 0.0, 0.0);
        stopped_status.current_source = None;

        assert_eq!(
            auto_advance_completion_reason(
                Some(&SinkProtocol::UpnpAvRenderer),
                "upnp-zone",
                &stopped_status,
                &source,
                &monitor_state,
            ),
            Some("remote_tail_observed")
        );
    }

    #[test]
    fn remote_completion_tail_fallback_rejects_source_change() {
        let monitor_state = AutoAdvanceMonitorState::default();
        let original = qobuz_source(42, false);
        let replacement = qobuz_source(43, false);
        let mut tail_status = status_with_position("Playing", 118.2, 120.0);
        tail_status.current_source = Some(original.clone());
        monitor_state.observe_remote_tail(
            Some(&SinkProtocol::SonosUpnp),
            "sonos-zone",
            &tail_status,
            &original,
        );

        let mut stopped_status = status_with_position("Stopped", 0.0, 0.0);
        stopped_status.current_source = Some(replacement.clone());

        assert_eq!(
            auto_advance_completion_reason(
                Some(&SinkProtocol::SonosUpnp),
                "sonos-zone",
                &stopped_status,
                &replacement,
                &monitor_state,
            ),
            None
        );
    }

    #[test]
    fn remote_completion_tail_fallback_only_runs_for_stopped_state() {
        let monitor_state = AutoAdvanceMonitorState::default();
        let source = qobuz_source(42, false);
        let mut tail_status = status_with_position("Playing", 118.2, 120.0);
        tail_status.current_source = Some(source.clone());
        monitor_state.observe_remote_tail(
            Some(&SinkProtocol::UpnpAvRenderer),
            "upnp-zone",
            &tail_status,
            &source,
        );

        let paused_status = status_with_position("Paused", 0.0, 0.0);

        assert_eq!(
            auto_advance_completion_reason(
                Some(&SinkProtocol::UpnpAvRenderer),
                "upnp-zone",
                &paused_status,
                &source,
                &monitor_state,
            ),
            None
        );
    }

    #[test]
    fn local_completion_does_not_use_remote_tail_fallback() {
        let monitor_state = AutoAdvanceMonitorState::default();
        let source = qobuz_source(42, false);
        let mut tail_status = status_with_position("Playing", 118.2, 120.0);
        tail_status.current_source = Some(source.clone());
        monitor_state.observe_remote_tail(
            Some(&SinkProtocol::LocalCoreAudio),
            "local-zone",
            &tail_status,
            &source,
        );

        let stopped_status = status_with_position("Stopped", 0.0, 0.0);

        assert_eq!(
            auto_advance_completion_reason(
                Some(&SinkProtocol::LocalCoreAudio),
                "local-zone",
                &stopped_status,
                &source,
                &monitor_state,
            ),
            None
        );
    }

    #[tokio::test]
    async fn remote_completion_restarts_current_item_when_loop_is_enabled() {
        let state = app_state("remote-auto-advance-repeat-current");
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        let current = qobuz_source(1, false);
        let next = qobuz_source(2, false);
        state
            .library()
            .upsert_zone_definition(
                &zone_id,
                "Studio PC",
                "remote_agent",
                Some("Agent DAC"),
                true,
            )
            .unwrap();
        state
            .library()
            .set_now_playing_queue(
                &zone_id,
                &serde_json::json!({
                    "kind": "qobuz",
                    "cursor": 0,
                    "items": [
                        { "resolvedSource": current.clone() },
                        { "resolvedSource": next.clone() }
                    ],
                    "loopMode": "loop"
                }),
            )
            .unwrap();
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "Studio PC".to_string(),
            state.settings().active_profile_id(),
            current.clone(),
            vec![next.clone()],
        );
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&next))
            .unwrap();
        let mut status = status_with_position("Stopped", 180.0, 180.0);
        status.current_source = Some(current.clone());
        let pending = Arc::new(Mutex::new(HashSet::new()));

        maybe_spawn_qobuz_auto_advance(
            &state,
            &zone_id,
            &status,
            &pending,
            &AutoAdvanceMonitorState::default(),
        );

        let command = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("repeat-one fallback should send a playback command")
            .expect("remote agent should remain connected");
        assert!(matches!(
            command,
            CoreToAgentCommand::PlaySource { source_ref, queue, .. }
                if source_ref.key() == current.key()
                    && queue.len() == 1
                    && queue[0].key() == "qobuz:2"
        ));
    }

    #[test]
    fn qobuz_status_match_checks_artist_when_title_matches() {
        let mut status = status_with_position("Stopped", 120.0, 120.0);
        status.track_title = Some("Intro".to_string());
        status.track_artist = Some("Artist B".to_string());
        let source = SourceRef::QobuzTrack {
            track_id: 7,
            title: Some("Intro".to_string()),
            artist: Some("Artist A".to_string()),
            album: None,
            album_id: None,
            image_url: None,
            duration_secs: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        };

        assert!(status_identifies_different_qobuz_source(&status, &source));
    }

    #[test]
    fn upnp_completed_prewarm_cache_keeps_only_current_candidate_for_zone() {
        let completed = Arc::new(Mutex::new(HashSet::from([
            "upnp-zone|current|next-a|render-a".to_string(),
            "upnp-zone|current|next-b|render-a".to_string(),
            "other-zone|current|next-c|render-a".to_string(),
        ])));

        retain_completed_upnp_prewarm_candidate(
            &completed,
            "upnp-zone",
            "upnp-zone|current|next-b|render-a",
        );

        let completed = completed.lock().unwrap();
        assert!(!completed.contains("upnp-zone|current|next-a|render-a"));
        assert!(completed.contains("upnp-zone|current|next-b|render-a"));
        assert!(completed.contains("other-zone|current|next-c|render-a"));
    }

    #[test]
    fn upnp_completed_prewarm_cache_clears_zone_when_candidate_disappears() {
        let completed = Arc::new(Mutex::new(HashSet::from([
            "upnp-zone|current|next-a|render-a".to_string(),
            "other-zone|current|next-c|render-a".to_string(),
        ])));

        clear_completed_upnp_prewarms_for_zone(&completed, "upnp-zone");

        let completed = completed.lock().unwrap();
        assert!(!completed.contains("upnp-zone|current|next-a|render-a"));
        assert!(completed.contains("other-zone|current|next-c|render-a"));
    }

    #[test]
    fn upnp_observed_renderer_next_promotes_listening_queue_without_replay() {
        let state = app_state("upnp-observed-renderer-next");
        let zone_id = state.zones().active_zone_id();
        let active = local_source(1, "Current");
        let next = local_source(2, "Next");
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        state.listening().start_with_radio(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            active.clone(),
            vec![next.clone()],
            false,
        );
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&next))
            .unwrap();
        let mut status = status_with_position("Playing", 0.42, 181.0);
        status.current_source = Some(next.clone());

        assert!(promote_observed_upnp_renderer_next(
            &state, &zone_id, &status, &active
        ));
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|source| source.key()),
            Some(next.key())
        );
        assert!(state.library().zone_queue(&zone_id).unwrap().is_empty());
    }

    #[test]
    fn upnp_transitioning_next_source_does_not_consume_queue_without_audio() {
        let state = app_state("upnp-unconfirmed-renderer-next");
        let zone_id = state.zones().active_zone_id();
        let active = local_source(1, "Current");
        let next = local_source(2, "Next");
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        state.listening().start_with_radio(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            active.clone(),
            vec![next.clone()],
            false,
        );
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&next))
            .unwrap();
        let mut status = status_with_position("Transitioning", 0.0, 181.0);
        status.current_source = Some(next.clone());
        status.transport_pending = "loading".to_string();

        assert!(!promote_observed_upnp_renderer_next(
            &state, &zone_id, &status, &active
        ));
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|source| source.key()),
            Some(active.key())
        );
        assert_eq!(
            state
                .library()
                .zone_queue(&zone_id)
                .unwrap()
                .first()
                .map(|entry| entry.source.key()),
            Some(next.key())
        );
    }

    fn local_source(track_id: i64, title: &str) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: Some(format!("{title}.flac")),
            title: Some(title.to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            album_id: None,
            art_id: None,
            duration_secs: Some(180.0),
            ext_hint: Some("flac".to_string()),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    #[tokio::test]
    async fn stale_lastfm_prefetch_source_does_not_mutate_queue_state() {
        let state = app_state("stale-lastfm-prefetch-source");
        let zone_id = state.zones().active_zone_id();
        let original = qobuz_source(1, true);
        let replacement = qobuz_source(2, true);
        let prefetched = qobuz_source(3, true);
        state.listening().start_with_radio(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            replacement,
            Vec::new(),
            true,
        );

        let result = arm_lastfm_radio_source(
            state.clone(),
            &zone_id,
            prefetched,
            original.key(),
            None,
            None,
        )
        .await;

        assert_eq!(result.unwrap_err(), "Playback changed");
        assert!(state.library().zone_queue(&zone_id).unwrap().is_empty());
        assert!(state.listening().queued_sources(&zone_id).is_empty());
    }

    #[tokio::test]
    async fn stale_lastfm_prefetch_epoch_does_not_mutate_queue_state() {
        let state = app_state("stale-lastfm-prefetch-epoch");
        let zone_id = state.zones().active_zone_id();
        let original = qobuz_source(1, true);
        let prefetched = qobuz_source(3, true);
        state.listening().start_with_radio(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            original.clone(),
            Vec::new(),
            true,
        );
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active test zone should have a player");
        let expected_epoch = player.playback_epoch();
        player.reserve_playback_change();

        let result = arm_lastfm_radio_source(
            state.clone(),
            &zone_id,
            prefetched,
            original.key(),
            None,
            Some(expected_epoch),
        )
        .await;

        assert_eq!(result.unwrap_err(), "Playback changed");
        assert!(state.library().zone_queue(&zone_id).unwrap().is_empty());
        assert!(state.listening().queued_sources(&zone_id).is_empty());
    }

    fn status_with_position(state: &str, position_secs: f64, duration_secs: f64) -> StatusResponse {
        StatusResponse {
            surface: "local".to_string(),
            capabilities: crate::app::capabilities::BuildCapabilities::current(),
            airplay_helper_state: "missing".to_string(),
            state: state.to_string(),
            file_name: None,
            current_source: None,
            track_title: None,
            track_artist: None,
            track_album: None,
            cover_version: 0,
            source_rate: 0,
            target_rate: 0,
            source_bits: 0,
            target_bits: 0,
            configured_target_rate: 0,
            configured_target_bit_depth: 24,
            upsampling_enabled: false,
            filter_type: String::new(),
            active_filter_type: String::new(),
            src_path_kind: None,
            src_capped_fallback: false,
            src_phase_profile_preserved: true,
            src_ratio_num: 0,
            src_ratio_den: 0,
            dither_mode: String::new(),
            output_mode: String::new(),
            active_output_mode: String::new(),
            dsd_modulator: String::new(),
            dsd_isi_penalty: 0.0,
            output_transport: String::new(),
            output_notice_id: 0,
            output_notice: None,
            upnp_config_applied_to_current_playback: true,
            upnp_restart_pending: false,
            upnp_render_status: "idle".to_string(),
            upnp_active_render_signature: None,
            upnp_configured_render_signature: None,
            upnp_last_render_ms: None,
            upnp_last_prepare_ms: None,
            upnp_last_cache_hit: None,
            transport_pending: "none".to_string(),
            transport_pending_position_secs: None,
            dsd_stability_resets: 0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            volume: 0.0,
            device_volume: None,
            device_volume_supported: false,
            device_volume_max: None,
            device_volume_message: None,
            headroom_db: 0.0,
            dsp_buffer_ms: 0,
            exclusive: false,
            position_secs,
            duration_secs,
            playback_speed: None,
            resample_time_ns: 0,
            dsd_upsample_time_ns: 0,
            dsd_modulate_time_ns: 0,
            dsd_output_pending_samples: 0,
            dsd_buffer_health: None,
            dsd_overbudget_blocks: 0,
            dsd_last_load: 0.0,
            dsd_recent_load_p95: 0.0,
            dsd_recent_load_p99: 0.0,
            dop_ring_capacity_ms: 0.0,
            dop_ring_fill_ms: 0.0,
            dop_ring_low_watermark_ms: 0.0,
            dop_callback_frames: 0,
            dop_callback_ms: 0.0,
            dop_requested_hardware_buffer_frames: 0,
            dop_requested_hardware_buffer_ms: 0.0,
            dop_hardware_buffer_min_frames: 0,
            dop_hardware_buffer_max_frames: 0,
            dop_hardware_buffer_frames: 0,
            dop_hardware_buffer_ms: 0.0,
            dop_lock_miss_events: 0,
            dop_callback_deadline_miss_events: 0,
            dop_soft_callback_gap_125_events: 0,
            dop_soft_callback_gap_150_events: 0,
            dop_soft_callback_gap_175_events: 0,
            dop_last_soft_callback_gap_ms: 0.0,
            dop_last_soft_callback_gap_at_ms: 0,
            dop_ring_below_250ms_events: 0,
            dop_ring_below_100ms_events: 0,
            dop_ring_below_50ms_events: 0,
            dop_ring_below_callback_events: 0,
            dop_last_ring_pressure_at_ms: 0,
            dop_marker_error_events: 0,
            dop_program_idle_splice_events: 0,
            dop_program_to_idle_events: 0,
            dop_idle_to_program_events: 0,
            dop_mixed_output_events: 0,
            dop_last_output_transition_id: 0,
            dop_last_output_transition_at_ms: 0,
            dop_repeated_payload_events: 0,
            dop_callback_index: 0,
            dop_last_callback_at_ms: 0,
            dop_last_callback_gap_ms: 0.0,
            dop_last_callback_frames: 0,
            dop_last_output_kind_id: 0,
            dop_last_ring_fill_samples: 0,
            dop_last_program_read_samples: 0,
            dop_ring_read_cursor_samples: 0,
            dop_last_payload_fingerprint: 0,
            dop_last_payload_fingerprint_at_ms: 0,
            dop_marker_scan_count: 0,
            dop_every_callback_scan_enabled: false,
            dop_last_underrun_at_ms: 0,
            output_ring_fill_now_ms: 0.0,
            output_ring_fill_min_ms: 0.0,
            startup_ring_low_watermark_ms: 0.0,
            startup_ready_ms: 0,
            startup_first_render_block_ms: 0.0,
            startup_producer_over_budget_count: 0,
            startup_callback_gaps_ms: Vec::new(),
            underrun_count: 0,
            producer_over_budget_count: 0,
            max_render_block_ms: 0.0,
            max_audio_callback_gap_ms: 0.0,
            dsp_graph_rebuild_count: 0,
            sample_rate_change_count: 0,
            dop_alignment_reset_count: 0,
            coreaudio_dop_open_count: 0,
            coreaudio_dop_start_count: 0,
            coreaudio_dop_stop_count: 0,
            coreaudio_dop_drop_count: 0,
            coreaudio_dop_quiesce_count: 0,
            coreaudio_dop_last_lifecycle_event_id: 0,
            coreaudio_dop_last_lifecycle_at_ms: 0,
            reopen_reason_count: 0,
            last_reopen_reason_id: 0,
            last_reopen_reason_at_ms: 0,
            flush_reason_count: 0,
            last_flush_reason_id: 0,
            last_flush_reason_at_ms: 0,
            modulator_reset_count: 0,
            decoder_starved_count: 0,
            source_read_time_ms: 0.0,
            max_source_read_ms: 0.0,
            source_read_stall_count: 0,
            source_read_stall_last_at_ms: 0,
            decoder_decode_time_ms: 0.0,
            max_decoder_decode_ms: 0.0,
            decoder_decode_stall_count: 0,
            decoder_decode_stall_last_at_ms: 0,
            lock_wait_max_ms: 0.0,
            block_duration_ns: 0,
            cpu_percent: 0.0,
            meter_l: 0.0,
            meter_r: 0.0,
            signal_peak: 0.0,
            signal_peak_max: 0.0,
            signal_clipping: false,
            signal_clip_events: 0,
            signal_clip_samples: 0,
            dsd_limiter_peak_ratio: 0.0,
            dsd_limiter_peak_ratio_max: 0.0,
            dsd_limiter_active: false,
            dsd_limiter_events: 0,
            dsd_limiter_samples: 0,
            underrun_events: 0,
            underrun_samples: 0,
            selected_device: None,
            active_zone_id: "test-zone".to_string(),
            active_zone_name: "Test Zone".to_string(),
            zone_protocol: SinkProtocol::LocalCoreAudio,
            remote_connected: false,
            remote_signal_path: None,
            remote_buffer_state: None,
            browser_stream_signal: None,
        }
    }
}
