use crate::app::state::AppState;
use crate::listening::PlaybackObservation;
use crate::playback::auto_advance::{
    AutoAdvanceMonitorState, maybe_spawn_lastfm_radio_prefetch, maybe_spawn_qobuz_auto_advance,
    maybe_spawn_qobuz_next_prefetch, maybe_spawn_upnp_next_prewarm,
};
use crate::playback::now_playing::sonos_current_file_name;
use crate::playback::service::playback_config_for_zone;
use crate::playback::sonos::{prefetch_sonos_next, sonos_target_for_zone};
use crate::playback::status::{
    build_status_response_for_zone, refresh_sonos_playback, refresh_upnp_playback,
};
use crate::protocol::SinkProtocol;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub(crate) fn spawn_listening_monitor(state: AppState) {
    let pending_qobuz_advances = Arc::new(Mutex::new(HashSet::<String>::new()));
    let qobuz_advance_monitor_state = AutoAdvanceMonitorState::default();
    let pending_lastfm_radio_prefetches = Arc::new(Mutex::new(HashSet::<String>::new()));
    let pending_qobuz_prefetches = Arc::new(Mutex::new(HashSet::<String>::new()));
    let pending_upnp_prewarms = Arc::new(Mutex::new(HashSet::<String>::new()));
    let completed_upnp_prewarms = Arc::new(Mutex::new(HashSet::<String>::new()));
    let pending_sonos_prefetches = Arc::new(Mutex::new(HashSet::<String>::new()));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            ticker.tick().await;
            refresh_sonos_playback(&state).await;
            refresh_upnp_playback(&state).await;
            for zone in state.zones().list_zones() {
                if !zone.enabled {
                    continue;
                }
                let blocking_state = state.clone();
                let zone_id = zone.id.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let status = build_status_response_for_zone(&blocking_state, &zone_id)?;
                    let profile_id = blocking_state
                        .listening()
                        .profile_id(&zone_id)
                        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
                    blocking_state.listening().observe(
                        blocking_state.library(),
                        &zone_id,
                        profile_id,
                        PlaybackObservation {
                            state: status.state.clone(),
                            current_source: status.current_source.clone(),
                            file_name: status.file_name.clone(),
                            track_title: status.track_title.clone(),
                            track_artist: status.track_artist.clone(),
                            track_album: status.track_album.clone(),
                            zone_name: Some(status.active_zone_name.clone()),
                            position_secs: status.position_secs,
                            duration_secs: status.duration_secs,
                        },
                    );
                    Ok::<_, String>(status)
                })
                .await;
                if let Ok(Ok(status)) = status {
                    maybe_spawn_upnp_next_prewarm(
                        &state,
                        &zone.id,
                        &status,
                        &pending_upnp_prewarms,
                        &completed_upnp_prewarms,
                    );
                    maybe_spawn_qobuz_auto_advance(
                        &state,
                        &zone.id,
                        &status,
                        &pending_qobuz_advances,
                        &qobuz_advance_monitor_state,
                    );
                    maybe_spawn_lastfm_radio_prefetch(
                        &state,
                        &zone.id,
                        &status,
                        &pending_lastfm_radio_prefetches,
                    );
                    maybe_spawn_qobuz_next_prefetch(
                        &state,
                        &zone.id,
                        &status,
                        &pending_qobuz_prefetches,
                    );
                    maybe_spawn_sonos_next_prefetch(&state, &zone.id, &pending_sonos_prefetches);
                }
            }
        }
    });
}

fn maybe_spawn_sonos_next_prefetch(
    state: &AppState,
    zone_id: &str,
    pending: &Arc<Mutex<HashSet<String>>>,
) {
    if state.zones().zone_protocol(zone_id) != Some(SinkProtocol::SonosUpnp) {
        return;
    }
    if state.sonos().queued_next_count(zone_id) > 0 {
        return;
    }
    let Some(expected_current) = sonos_current_file_name(state, zone_id) else {
        return;
    };
    let Ok(queue) = state.library().zone_queue(zone_id) else {
        return;
    };
    let queue_sources = queue
        .into_iter()
        .map(|entry| entry.source)
        .collect::<Vec<_>>();
    if queue_sources.is_empty() {
        return;
    }
    let Ok(target) = sonos_target_for_zone(state, zone_id) else {
        return;
    };
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
        let playback_config =
            playback_config_for_zone(&state, &zone_id, state.zones().active_player().as_ref());
        prefetch_sonos_next(
            state,
            zone_id.clone(),
            target,
            Some(expected_current),
            None,
            queue_sources,
            playback_config,
        )
        .await;
        pending.lock().unwrap().remove(&zone_id);
    });
}
