use crate::app::state::AppState;
use crate::library::ZoneQueueEntry;
use crate::playback::error::PlaybackError;
use crate::playback::now_playing::{
    current_playback_matches_expected, sonos_current_file_name, sonos_current_matches,
};
use crate::playback::qobuz::prefetch_qobuz_queue_track_into_player;
use crate::playback::resolver::{
    QueueRequestItem, local_player_queue_items_from_sources, source_ref_from_queue_request,
};
use crate::playback::service::playback_config_for_zone;
use crate::playback::sonos::{sonos_target_for_zone, spawn_sonos_next_prefetch};
use crate::playback::source::{
    qobuz_queue_track_from_source_ref, source_ref_with_radio, source_ref_with_radio_context,
};
use crate::playback::status::build_status_response_for_zone;
use crate::protocol::{CoreToAgentCommand, SinkProtocol, SourceRef};
use crate::services::qobuz::QobuzQueueTrack;
use rand::seq::SliceRandom;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
pub(crate) struct SetQueueRequest {
    #[serde(default)]
    pub queue: Vec<QueueRequestItem>,
    pub expected_current: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct QueueMutationRequest {
    pub expected_current: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SetNowPlayingQueueRequest {
    pub state: Value,
}

#[derive(Serialize, JsonSchema)]
pub struct NowPlayingQueueResponse {
    pub state: Value,
    pub updated_at: Option<i64>,
    pub current_source: Option<SourceRef>,
    pub queued_sources: Vec<SourceRef>,
}

pub(crate) fn zone_queue_for_zone(
    state: &AppState,
    zone_id: &str,
) -> Result<Vec<ZoneQueueEntry>, PlaybackError> {
    state
        .library()
        .zone_queue(zone_id)
        .map_err(PlaybackError::library)
}

pub(crate) fn now_playing_queue_for_zone(
    state: &AppState,
    zone_id: &str,
) -> Result<NowPlayingQueueResponse, PlaybackError> {
    let queued_sources: Vec<SourceRef> = zone_queue_for_zone(state, zone_id)?
        .into_iter()
        .map(|entry| refresh_source_ref_metadata(state, entry.source))
        .collect();
    let current_source = live_current_source_for_zone(state, zone_id)
        .map(|source| refresh_source_ref_metadata(state, source));
    state
        .library()
        .now_playing_queue(zone_id)
        .map(|snapshot| {
            snapshot
                .map(|snapshot| NowPlayingQueueResponse {
                    state: snapshot.state,
                    updated_at: Some(snapshot.updated_at),
                    current_source: current_source.clone(),
                    queued_sources: queued_sources.clone(),
                })
                .unwrap_or_else(|| NowPlayingQueueResponse {
                    state: json!(null),
                    updated_at: None,
                    current_source,
                    queued_sources,
                })
        })
        .map_err(PlaybackError::library)
}

pub(crate) fn now_playing_queue_for_active_zone(
    state: &AppState,
) -> Result<NowPlayingQueueResponse, PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    now_playing_queue_for_zone(state, &zone_id)
}

pub(crate) fn set_now_playing_queue_for_zone(
    state: &AppState,
    zone_id: &str,
    req: SetNowPlayingQueueRequest,
) -> Result<(), PlaybackError> {
    state
        .library()
        .set_now_playing_queue(zone_id, &req.state)
        .map_err(PlaybackError::library)
}

pub(crate) fn set_now_playing_queue_for_active_zone(
    state: &AppState,
    req: SetNowPlayingQueueRequest,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    set_now_playing_queue_for_zone(state, &zone_id, req)
}

#[allow(dead_code)]
pub(crate) async fn shuffle_active_zone_queue(
    state: &AppState,
    req: QueueMutationRequest,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    shuffle_active_zone_queue_for_profile(state, &profile_id, req).await
}

pub(crate) async fn shuffle_active_zone_queue_for_profile(
    state: &AppState,
    profile_id: &str,
    req: QueueMutationRequest,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    shuffle_zone_queue_by_id_for_profile(state, &zone_id, profile_id, req).await
}

#[allow(dead_code)]
pub(crate) async fn shuffle_zone_queue_by_id(
    state: &AppState,
    zone_id: &str,
    req: QueueMutationRequest,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    shuffle_zone_queue_by_id_for_profile(state, zone_id, &profile_id, req).await
}

pub(crate) async fn shuffle_zone_queue_by_id_for_profile(
    state: &AppState,
    zone_id: &str,
    profile_id: &str,
    req: QueueMutationRequest,
) -> Result<(), PlaybackError> {
    let mut queue_sources: Vec<SourceRef> = zone_queue_for_zone(state, zone_id)?
        .into_iter()
        .map(|entry| refresh_source_ref_metadata(state, entry.source))
        .collect();
    queue_sources = normalize_upcoming_queue_sources(state, zone_id, queue_sources);
    queue_sources.shuffle(&mut rand::thread_rng());
    apply_zone_queue_sources(
        state,
        zone_id,
        profile_id,
        queue_sources,
        req.expected_current,
    )
    .await
}

#[allow(dead_code)]
pub(crate) async fn set_active_zone_queue(
    state: &AppState,
    req: SetQueueRequest,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    set_active_zone_queue_for_profile(state, &profile_id, req).await
}

pub(crate) async fn set_active_zone_queue_for_profile(
    state: &AppState,
    profile_id: &str,
    req: SetQueueRequest,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    set_zone_queue_by_id_for_profile(state, &zone_id, profile_id, req).await
}

#[allow(dead_code)]
pub(crate) async fn set_zone_queue_by_id(
    state: &AppState,
    zone_id: &str,
    req: SetQueueRequest,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    set_zone_queue_by_id_for_profile(state, zone_id, &profile_id, req).await
}

pub(crate) async fn set_zone_queue_by_id_for_profile(
    state: &AppState,
    zone_id: &str,
    profile_id: &str,
    req: SetQueueRequest,
) -> Result<(), PlaybackError> {
    let mut queue_sources: Vec<SourceRef> = Vec::with_capacity(req.queue.len());
    for item in req.queue.iter() {
        let source = source_ref_from_queue_request(state, item)?
            .ok_or_else(|| PlaybackError::bad_request("Queue item is missing a playable source"))?;
        queue_sources.push(source);
    }
    apply_zone_queue_sources(
        state,
        zone_id,
        profile_id,
        queue_sources,
        req.expected_current,
    )
    .await
}

fn refresh_source_ref_metadata(state: &AppState, source: SourceRef) -> SourceRef {
    let radio_context = match &source {
        SourceRef::LocalTrack { radio_context, .. }
        | SourceRef::QobuzTrack { radio_context, .. } => radio_context.clone(),
    };
    match source.clone() {
        SourceRef::LocalTrack { track_id, .. } => state
            .library()
            .source_ref_for_track_id(track_id)
            .ok()
            .flatten()
            .map(|refreshed| {
                source_ref_with_radio_context(
                    source_ref_with_radio(refreshed, source.is_radio()),
                    radio_context,
                )
            })
            .unwrap_or(source),
        SourceRef::QobuzTrack { .. } => source,
    }
}

pub(crate) fn append_source_to_now_playing_queue(
    state: &AppState,
    zone_id: &str,
    source: &SourceRef,
) -> Result<(), String> {
    let mut queue_state = state
        .library()
        .now_playing_queue(zone_id)?
        .map(|snapshot| snapshot.state)
        .unwrap_or_else(default_now_playing_queue_state);
    append_source_to_now_playing_queue_state(&mut queue_state, source);
    state.library().set_now_playing_queue(zone_id, &queue_state)
}

pub(crate) fn queue_loop_enabled_for_zone(state: &AppState, zone_id: &str) -> bool {
    state
        .library()
        .now_playing_queue(zone_id)
        .ok()
        .flatten()
        .and_then(|snapshot| {
            snapshot
                .state
                .get("loopMode")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|mode| mode == "loop")
}

fn default_now_playing_queue_state() -> Value {
    json!({
        "kind": null,
        "cursor": -1,
        "items": [],
        "loopMode": "off",
    })
}

fn append_source_to_now_playing_queue_state(queue_state: &mut Value, source: &SourceRef) {
    if !queue_state.is_object() {
        *queue_state = default_now_playing_queue_state();
    }
    let Some(object) = queue_state.as_object_mut() else {
        return;
    };
    if !object.get("items").is_some_and(Value::is_array) {
        object.insert("items".to_string(), Value::Array(Vec::new()));
    }
    let (kind, item_count) = {
        let Some(items) = object.get_mut("items").and_then(Value::as_array_mut) else {
            return;
        };
        if items
            .last()
            .and_then(queue_item_source_key)
            .is_none_or(|key| key != source.key())
        {
            items.push(source_ref_queue_item(source));
        }
        (queue_kind_for_items(items), items.len())
    };
    object.insert("kind".to_string(), kind);
    if !object
        .get("cursor")
        .and_then(Value::as_i64)
        .is_some_and(|cursor| cursor >= -1 && cursor < item_count as i64)
    {
        object.insert("cursor".to_string(), json!(-1));
    }
    if !object
        .get("loopMode")
        .is_some_and(|mode| matches!(mode.as_str(), Some("off" | "loop" | "one")))
    {
        object.insert("loopMode".to_string(), json!("off"));
    }
}

fn source_ref_queue_item(source: &SourceRef) -> Value {
    match source {
        SourceRef::LocalTrack {
            track_id,
            file_name,
            title,
            artist,
            album,
            album_artist,
            album_id,
            art_id,
            duration_secs,
            radio,
            ..
        } => json!({
            "title": title.clone().unwrap_or_else(|| format!("Track {track_id}")),
            "artist": artist.clone().unwrap_or_default(),
            "album": album.clone().unwrap_or_default(),
            "albumArtist": album_artist.clone().or_else(|| artist.clone()).unwrap_or_default(),
            "albumId": album_id,
            "artId": art_id,
            "imageUrl": null,
            "durationSecs": duration_secs.unwrap_or(0.0),
            "filename": file_name,
            "ref": {
                "track_id": track_id,
                "file_name": file_name,
            },
            "resolvedSource": source,
            "radio": radio,
        }),
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            album,
            album_id,
            image_url,
            duration_secs,
            radio,
            ..
        } => {
            let display_name = match (
                artist
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
                title
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
            ) {
                (Some(artist), Some(title)) => format!("{artist} - {title}"),
                (_, Some(title)) => title.to_string(),
                _ => format!("qobuz:{track_id}"),
            };
            json!({
                "title": title.clone().unwrap_or_else(|| format!("Track {track_id}")),
                "artist": artist.clone().unwrap_or_default(),
                "album": album.clone().unwrap_or_default(),
                "albumId": album_id,
                "imageUrl": image_url,
                "durationSecs": duration_secs.unwrap_or(0.0),
                "filename": display_name,
                "qobuzTrack": {
                    "id": track_id,
                    "track_id": track_id,
                    "title": title,
                    "artist": artist,
                    "album": album,
                    "album_id": album_id,
                    "image_url": image_url,
                    "duration_secs": duration_secs,
                    "radio": radio,
                },
                "resolvedSource": source,
                "radio": radio,
            })
        }
    }
}

fn queue_item_source_key(item: &Value) -> Option<String> {
    item.get("resolvedSource")
        .and_then(|source| serde_json::from_value::<SourceRef>(source.clone()).ok())
        .map(|source| source.key())
        .or_else(|| {
            item.get("qobuzTrack")
                .and_then(|track| {
                    track
                        .get("track_id")
                        .or_else(|| track.get("id"))
                        .and_then(Value::as_u64)
                })
                .map(|track_id| format!("qobuz:{track_id}"))
        })
        .or_else(|| {
            item.get("ref")
                .and_then(|ref_value| ref_value.get("track_id"))
                .and_then(Value::as_i64)
                .map(|track_id| format!("local:{track_id}"))
        })
}

fn queue_kind_for_items(items: &[Value]) -> Value {
    let mut has_local = false;
    let mut has_qobuz = false;
    for item in items {
        if let Some(source) = item
            .get("resolvedSource")
            .and_then(|source| serde_json::from_value::<SourceRef>(source.clone()).ok())
        {
            match source {
                SourceRef::LocalTrack { .. } => has_local = true,
                SourceRef::QobuzTrack { .. } => has_qobuz = true,
            }
            continue;
        }
        if item.get("qobuzTrack").is_some() {
            has_qobuz = true;
        } else if item.get("ref").is_some() {
            has_local = true;
        }
    }
    match (has_local, has_qobuz) {
        (true, true) => json!("mixed"),
        (true, false) => json!("local"),
        (false, true) => json!("qobuz"),
        (false, false) => Value::Null,
    }
}

fn live_current_source_for_zone(state: &AppState, zone_id: &str) -> Option<SourceRef> {
    let status = build_status_response_for_zone(state, zone_id).ok()?;
    if status.state != "Playing" && status.state != "Paused" {
        return None;
    }
    state.listening().active_source(zone_id)
}

pub(crate) async fn apply_zone_queue_sources(
    state: &AppState,
    zone_id: &str,
    profile_id: &str,
    queue_sources: Vec<SourceRef>,
    expected_current: Option<String>,
) -> Result<(), PlaybackError> {
    let queue_sources = normalize_upcoming_queue_sources(state, zone_id, queue_sources);
    let protocol = state.zones().zone_protocol(zone_id);
    if protocol == Some(SinkProtocol::SonosUpnp) {
        if !sonos_current_matches(state, zone_id, &expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        if let Err(e) = state.library().set_zone_queue(zone_id, &queue_sources) {
            return Err(PlaybackError::library(e));
        }
        if let Ok(target) = sonos_target_for_zone(state, zone_id) {
            let playback_config =
                playback_config_for_zone(state, zone_id, state.zones().active_player().as_ref());
            spawn_sonos_next_prefetch(
                (*state).clone(),
                zone_id.to_string(),
                target,
                sonos_current_file_name(state, zone_id),
                None,
                queue_sources,
                playback_config,
            );
        }
        return Ok(());
    }

    let is_remote = protocol == Some(SinkProtocol::RemoteAgent);
    let (player, expected_epoch) = if is_remote {
        if !current_playback_matches_expected(state, zone_id, &expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        (None, None)
    } else {
        let Some(player) = state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        if !current_playback_matches_expected(state, zone_id, &expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        let expected_epoch = Some(player.playback_epoch());
        (Some(player), expected_epoch)
    };

    let queue_items = local_player_queue_items_from_sources(state, &queue_sources);
    state
        .library()
        .set_zone_queue(zone_id, &queue_sources)
        .map_err(PlaybackError::library)?;
    state
        .listening()
        .set_queue(zone_id, profile_id.to_string(), queue_sources.clone());
    if is_remote {
        state
            .zones()
            .send_to_zone(
                zone_id,
                CoreToAgentCommand::SetQueue {
                    queue: queue_sources,
                },
            )
            .map_err(PlaybackError::integration)?;
        return Ok(());
    }
    if let Some(player) = player {
        let active_source = state.listening().active_source(zone_id);
        if matches!(active_source, Some(SourceRef::QobuzTrack { .. })) {
            if let Some(first_track) = qobuz_prefetch_track_for_queue_update(
                queue_loop_enabled_for_zone(state, zone_id),
                active_source.as_ref(),
                &queue_sources,
            ) {
                let expected = expected_current.or_else(|| player.current_file_name());
                player.set_stream_queue_if_epoch(Vec::new(), expected.clone(), expected_epoch);
                let state_for_prefetch = state.clone();
                let zone_for_prefetch = zone_id.to_string();
                let expected_epoch = expected_epoch.unwrap_or_else(|| player.playback_epoch());
                tokio::spawn(async move {
                    let Some(expected_current) = expected else {
                        return;
                    };
                    if let Err(e) = prefetch_qobuz_queue_track_into_player(
                        state_for_prefetch,
                        zone_for_prefetch,
                        first_track,
                        expected_current,
                        expected_epoch,
                        false,
                    )
                    .await
                    {
                        eprintln!("qobuz: queued-track prefetch skipped: {e}");
                    }
                });
            } else {
                player.set_queue_if_epoch(queue_items, expected_epoch);
            }
        } else {
            player.set_queue_if_epoch(queue_items, expected_epoch);
        }
    }
    Ok(())
}

fn normalize_upcoming_queue_sources(
    state: &AppState,
    zone_id: &str,
    mut queue_sources: Vec<SourceRef>,
) -> Vec<SourceRef> {
    let Some(active_key) = state
        .listening()
        .active_source(zone_id)
        .as_ref()
        .map(SourceRef::key)
    else {
        return queue_sources;
    };
    queue_sources.retain(|source| source.key() != active_key);
    queue_sources
}

fn qobuz_prefetch_track_for_queue_update(
    loop_enabled: bool,
    active_source: Option<&SourceRef>,
    queue_sources: &[SourceRef],
) -> Option<QobuzQueueTrack> {
    if loop_enabled {
        active_source.and_then(qobuz_queue_track_from_source_ref)
    } else {
        queue_sources
            .first()
            .and_then(qobuz_queue_track_from_source_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{agent_capabilities, app_state, qobuz_source};
    use crate::protocol::AgentPlaybackState;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn shuffle_zone_queue_excludes_active_source_from_upcoming_queue() {
        let state = app_state("shuffle-keeps-current");
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
        state
            .library()
            .upsert_zone_definition(&zone_id, "Studio PC", "remote_agent", None, true)
            .unwrap();
        let current = qobuz_source(10, false);
        let next_a = qobuz_source(11, false);
        let next_b = qobuz_source(12, false);
        state
            .library()
            .set_zone_queue(&zone_id, &[current.clone(), next_a.clone(), next_b.clone()])
            .unwrap();
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "Studio PC".to_string(),
            state.settings().active_profile_id(),
            current.clone(),
            vec![next_a.clone(), next_b.clone()],
        );
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: None,
                file_name: Some("qobuz:10".to_string()),
                track_title: Some("Current".to_string()),
                track_artist: Some("Artist".to_string()),
                track_album: Some("Album".to_string()),
                source_rate: 44_100,
                target_rate: 44_100,
                source_bits: 16,
                target_bits: 24,
                duration_secs: 180.0,
                position_secs: 1.0,
                volume: 1.0,
            },
            "http://core.test",
        );

        let result = shuffle_zone_queue_by_id(
            &state,
            &zone_id,
            QueueMutationRequest {
                expected_current: None,
            },
        )
        .await;

        assert!(result.is_ok());
        let saved = state.library().zone_queue(&zone_id).unwrap();
        let saved_keys = saved
            .iter()
            .map(|entry| entry.source.key())
            .collect::<Vec<_>>();
        assert_eq!(saved_keys.len(), 2);
        assert!(!saved_keys.contains(&current.key()));
        assert!(saved_keys.contains(&next_a.key()));
        assert!(saved_keys.contains(&next_b.key()));
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::SetQueue { queue }) if queue.len() == 2
                && !queue.iter().any(|source| source.key() == current.key())
                && queue.iter().any(|source| source.key() == next_a.key())
                && queue.iter().any(|source| source.key() == next_b.key())
        ));
    }

    #[tokio::test]
    async fn shuffle_zone_queue_rejects_stale_expected_current() {
        let state = app_state("shuffle-stale-current");
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
        state
            .library()
            .upsert_zone_definition(&zone_id, "Studio PC", "remote_agent", None, true)
            .unwrap();
        let current = qobuz_source(10, false);
        let next = qobuz_source(11, false);
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&next))
            .unwrap();
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "Studio PC".to_string(),
            state.settings().active_profile_id(),
            current,
            vec![next.clone()],
        );
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Paused".to_string(),
                current_source: None,
                file_name: Some("qobuz:10".to_string()),
                track_title: Some("Current".to_string()),
                track_artist: Some("Artist".to_string()),
                track_album: Some("Album".to_string()),
                source_rate: 44_100,
                target_rate: 44_100,
                source_bits: 16,
                target_bits: 24,
                duration_secs: 180.0,
                position_secs: 1.0,
                volume: 1.0,
            },
            "http://core.test",
        );

        let result = shuffle_zone_queue_by_id(
            &state,
            &zone_id,
            QueueMutationRequest {
                expected_current: Some("qobuz:99".to_string()),
            },
        )
        .await;

        assert!(
            matches!(result, Err(PlaybackError::Conflict(message)) if message == "Current track changed")
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(
            state
                .library()
                .zone_queue(&zone_id)
                .unwrap()
                .into_iter()
                .map(|entry| entry.source.key())
                .collect::<Vec<_>>(),
            vec![next.key()]
        );
    }

    #[tokio::test]
    async fn set_zone_queue_rejects_unresolved_items() {
        let state = app_state("queue-rejects-unresolved");
        let zone_id = state.zones().active_zone_id();

        let result = set_zone_queue_by_id(
            &state,
            &zone_id,
            SetQueueRequest {
                queue: vec![QueueRequestItem::Local {
                    file_name: None,
                    track_id: Some(987_654),
                }],
                expected_current: None,
            },
        )
        .await;

        assert!(matches!(result, Err(PlaybackError::NotFound(_))));
        assert!(state.library().zone_queue(&zone_id).unwrap().is_empty());
    }

    #[test]
    fn append_source_to_now_playing_queue_preserves_existing_back_stack() {
        let state = app_state("append-now-playing-radio");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        state
            .library()
            .set_now_playing_queue(
                &zone_id,
                &json!({
                    "kind": "local",
                    "cursor": 1,
                    "items": [
                        { "title": "Previous", "artist": "", "album": "", "durationSecs": 1, "ref": { "track_id": 1 } },
                        { "title": "Current", "artist": "", "album": "", "durationSecs": 1, "ref": { "track_id": 2 } }
                    ],
                    "loopMode": "off"
                }),
            )
            .unwrap();
        let radio = qobuz_source(3, true);

        append_source_to_now_playing_queue(&state, &zone_id, &radio).unwrap();
        append_source_to_now_playing_queue(&state, &zone_id, &radio).unwrap();

        let saved = state
            .library()
            .now_playing_queue(&zone_id)
            .unwrap()
            .unwrap()
            .state;
        let items = saved["items"].as_array().unwrap();
        assert_eq!(saved["cursor"], 1);
        assert_eq!(saved["kind"], "mixed");
        assert_eq!(items.len(), 3);
        assert_eq!(items[2]["resolvedSource"]["kind"], "qobuz_track");
        assert_eq!(items[2]["resolvedSource"]["radio"], true);
    }

    #[test]
    fn queue_loop_enabled_reads_persisted_loop_mode() {
        let state = app_state("queue-loop-enabled");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let first = qobuz_source(1, false);
        state
            .library()
            .set_now_playing_queue(
                &zone_id,
                &json!({
                    "kind": "qobuz",
                    "cursor": 0,
                    "items": [source_ref_queue_item(&first)],
                    "loopMode": "loop"
                }),
            )
            .unwrap();

        assert!(queue_loop_enabled_for_zone(&state, &zone_id));
    }

    #[test]
    fn qobuz_queue_update_prefers_active_track_when_loop_is_enabled() {
        let active = qobuz_source(20, false);
        let next = qobuz_source(21, false);

        let loop_track =
            qobuz_prefetch_track_for_queue_update(true, Some(&active), std::slice::from_ref(&next))
                .expect("loop should prefetch active qobuz track");
        let normal_track = qobuz_prefetch_track_for_queue_update(
            false,
            Some(&active),
            std::slice::from_ref(&next),
        )
        .expect("normal mode should prefetch upcoming qobuz track");

        assert_eq!(loop_track.track_id, 20);
        assert_eq!(normal_track.track_id, 21);
    }
}
