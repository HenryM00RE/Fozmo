use crate::app::state::AppState;
use crate::playback::control::{pause_for_zone, seek_for_zone};
use crate::playback::dispatcher::PlaybackDispatcher;
use crate::playback::error::PlaybackError;
use crate::playback::intent::PlaybackIntent;
use crate::playback::now_playing::current_playback_matches_expected;
use crate::playback::queue::apply_zone_queue_sources;
use crate::playback::request::{PlaybackGuard, PlaybackRequest};
use crate::playback::status::build_status_response_for_zone;
use crate::protocol::SourceRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TransferSourceAction {
    #[default]
    Pause,
    KeepPlaying,
}

#[derive(Debug, Deserialize)]
pub struct TransferRequest {
    pub destination_zone_id: String,
    #[serde(default)]
    pub source_action: TransferSourceAction,
    #[serde(default)]
    pub expected_current: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TransferResponse {
    pub source_zone_id: String,
    pub destination_zone_id: String,
    pub source_action: TransferSourceAction,
    pub source_key: String,
    pub queued_count: usize,
    pub destination_state: String,
    pub seek_error: Option<String>,
    pub source_queue_clear_error: Option<String>,
}

pub async fn transfer_zone(
    state: &AppState,
    source_zone_id: &str,
    req: TransferRequest,
) -> Result<TransferResponse, PlaybackError> {
    let destination_zone_id = req.destination_zone_id.trim();
    if destination_zone_id.is_empty() {
        return Err(PlaybackError::bad_request(
            "destination_zone_id is required",
        ));
    }
    if source_zone_id == destination_zone_id {
        return Err(PlaybackError::bad_request(
            "source and destination zones must differ",
        ));
    }
    if state.zones().zone_protocol(source_zone_id).is_none() {
        return Err(PlaybackError::ZoneNotAvailable);
    }
    if state.zones().zone_protocol(destination_zone_id).is_none() {
        return Err(PlaybackError::ZoneNotAvailable);
    }

    let source_status =
        build_status_response_for_zone(state, source_zone_id).map_err(PlaybackError::not_found)?;
    if source_status.state != "Playing" && source_status.state != "Paused" {
        return Err(PlaybackError::conflict(
            "Source zone is not playing or paused",
        ));
    }

    if !current_playback_matches_expected(state, source_zone_id, &req.expected_current) {
        return Err(PlaybackError::conflict("Current track changed"));
    }

    let source = source_status
        .current_source
        .clone()
        .or_else(|| state.listening().active_source(source_zone_id))
        .ok_or_else(|| PlaybackError::conflict("Source zone has no current source"))?;
    if let Some(expected) = req
        .expected_current
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && source.key() != expected
        && !current_playback_matches_expected(state, source_zone_id, &Some(expected.to_string()))
    {
        return Err(PlaybackError::conflict("Current track changed"));
    }

    let source_key = source.key();
    let queue = transfer_queue_sources(state, source_zone_id, &source)?;
    let queued_count = queue.len();
    let profile_id = state
        .listening()
        .profile_id(source_zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    let source_queue_state = state
        .library()
        .now_playing_queue(source_zone_id)
        .map_err(PlaybackError::library)?
        .map(|snapshot| snapshot.state);

    PlaybackDispatcher::new(state)
        .execute(
            destination_zone_id,
            PlaybackIntent::Play {
                request: PlaybackRequest {
                    profile_id,
                    source: source.clone(),
                    queue: queue.clone(),
                    radio_auto: source.is_radio(),
                    guard: PlaybackGuard::none(),
                    qobuz_request: None,
                },
            },
        )
        .await?;

    let seek_error =
        seek_destination(state, destination_zone_id, source_status.position_secs).await;

    if let Some(queue_state) = source_queue_state {
        let _ = state
            .library()
            .set_now_playing_queue(destination_zone_id, &queue_state);
    } else {
        let _ = state
            .library()
            .set_now_playing_queue(destination_zone_id, &rebuilt_queue_state(&source, &queue));
    }

    let moved_listening = state.listening().transfer_to_zone(
        source_zone_id,
        destination_zone_id.to_string(),
        state.zones().zone_name(destination_zone_id),
        source_status.position_secs,
        source_status.duration_secs,
        source_status.state == "Playing",
    );
    if !moved_listening {
        warn!(
            event = "zone_transfer",
            status = "warning",
            source_zone_id,
            destination_zone_id,
            error_kind = "state",
            "Zone transfer could not move active listen"
        );
    }

    let source_queue_clear_error = clear_source_queue(state, source_zone_id, None).await;

    if req.source_action == TransferSourceAction::Pause {
        pause_for_zone(state, source_zone_id).await?;
    }

    let destination_state = build_status_response_for_zone(state, destination_zone_id)
        .map(|status| status.state)
        .unwrap_or_else(|_| "Unknown".to_string());

    Ok(TransferResponse {
        source_zone_id: source_zone_id.to_string(),
        destination_zone_id: destination_zone_id.to_string(),
        source_action: req.source_action,
        source_key,
        queued_count,
        destination_state,
        seek_error,
        source_queue_clear_error,
    })
}

fn transfer_queue_sources(
    state: &AppState,
    source_zone_id: &str,
    source: &SourceRef,
) -> Result<Vec<SourceRef>, PlaybackError> {
    let mut queue = state.listening().queued_sources(source_zone_id);
    if queue.is_empty() {
        queue = state
            .library()
            .zone_queue(source_zone_id)
            .map_err(PlaybackError::library)?
            .into_iter()
            .map(|entry| entry.source)
            .collect();
    }
    let source_key = source.key();
    while queue
        .first()
        .is_some_and(|queued| queued.key() == source_key)
    {
        queue.remove(0);
    }
    Ok(queue)
}

async fn seek_destination(
    state: &AppState,
    destination_zone_id: &str,
    position_secs: f64,
) -> Option<String> {
    if !position_secs.is_finite() || position_secs <= 0.0 {
        return None;
    }
    seek_for_zone(state, destination_zone_id, position_secs)
        .await
        .err()
        .map(|error| error.message().to_string())
}

async fn clear_source_queue(
    state: &AppState,
    source_zone_id: &str,
    expected_current: Option<String>,
) -> Option<String> {
    let profile_id = state
        .listening()
        .profile_id(source_zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    apply_zone_queue_sources(
        state,
        source_zone_id,
        &profile_id,
        Vec::new(),
        expected_current,
    )
    .await
    .err()
    .map(|error| error.message().to_string())
}

fn rebuilt_queue_state(source: &SourceRef, queue: &[SourceRef]) -> Value {
    let items = std::iter::once(source)
        .chain(queue.iter())
        .map(source_ref_queue_item)
        .collect::<Vec<_>>();
    serde_json::json!({
        "kind": queue_kind_for_items(&items),
        "cursor": 0,
        "items": items,
        "loopMode": "off",
    })
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
        } => serde_json::json!({
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
        } => serde_json::json!({
            "title": title.clone().unwrap_or_else(|| format!("Track {track_id}")),
            "artist": artist.clone().unwrap_or_default(),
            "album": album.clone().unwrap_or_default(),
            "albumId": album_id,
            "imageUrl": image_url,
            "durationSecs": duration_secs.unwrap_or(0.0),
            "filename": artist
                .as_deref()
                .zip(title.as_deref())
                .map(|(artist, title)| format!("{artist} - {title}"))
                .unwrap_or_else(|| format!("qobuz:{track_id}")),
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
        }),
    }
}

fn queue_kind_for_items(items: &[Value]) -> Value {
    let mut has_local = false;
    let mut has_qobuz = false;
    for item in items {
        if item.get("qobuzTrack").is_some() {
            has_qobuz = true;
        } else if item.get("ref").is_some() {
            has_local = true;
        }
    }
    match (has_local, has_qobuz) {
        (true, true) => Value::String("mixed".to_string()),
        (true, false) => Value::String("local".to_string()),
        (false, true) => Value::String("qobuz".to_string()),
        (false, false) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{agent_capabilities, app_state, qobuz_source};
    use crate::protocol::{AgentPlaybackState, CoreToAgentCommand};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn transfer_rejects_same_zone() {
        let state = app_state("transfer-same-zone");
        let result = transfer_zone(
            &state,
            "local-core",
            TransferRequest {
                destination_zone_id: "local-core".to_string(),
                source_action: TransferSourceAction::Pause,
                expected_current: None,
            },
        )
        .await;
        assert!(matches!(result, Err(PlaybackError::BadRequest(_))));
    }

    #[tokio::test]
    async fn transfer_rejects_missing_current_source() {
        let state = app_state("transfer-missing-source");
        let (tx, _rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Kitchen".to_string(),
            agent_capabilities("Kitchen DAC"),
            tx,
        );
        let destination = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Kitchen"))
            .unwrap()
            .id;
        let result = transfer_zone(
            &state,
            "local-core",
            TransferRequest {
                destination_zone_id: destination,
                source_action: TransferSourceAction::Pause,
                expected_current: None,
            },
        )
        .await;
        assert!(matches!(result, Err(PlaybackError::Conflict(_))));
    }

    #[tokio::test]
    async fn transfer_remote_to_remote_moves_current_and_queue() {
        let state = app_state("transfer-remote-success");
        let (source_tx, mut source_rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Lounge".to_string(),
            agent_capabilities("Lounge DAC"),
            source_tx,
        );
        let (dest_tx, mut dest_rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-2".to_string(),
            "Kitchen".to_string(),
            agent_capabilities("Kitchen DAC"),
            dest_tx,
        );
        let zones = state.zones().list_zones();
        let source_zone_id = zones
            .iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Lounge"))
            .unwrap()
            .id
            .clone();
        let destination_zone_id = zones
            .iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Kitchen"))
            .unwrap()
            .id
            .clone();
        state
            .library()
            .upsert_zone_definition(&source_zone_id, "Lounge", "remote_agent", None, true)
            .unwrap();
        state
            .library()
            .upsert_zone_definition(&destination_zone_id, "Kitchen", "remote_agent", None, true)
            .unwrap();
        let current = qobuz_source(10, false);
        let next = qobuz_source(11, false);
        state.listening().start(
            state.library(),
            source_zone_id.clone(),
            "Lounge".to_string(),
            "profile-a".to_string(),
            current.clone(),
            vec![next.clone()],
        );
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                file_name: Some("Artist - Track 10".to_string()),
                track_title: Some("Track 10".to_string()),
                track_artist: Some("Artist".to_string()),
                track_album: Some("Album".to_string()),
                position_secs: 12.0,
                duration_secs: 180.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );
        state
            .library()
            .set_zone_queue(&source_zone_id, std::slice::from_ref(&next))
            .unwrap();
        while source_rx.try_recv().is_ok() {}
        while dest_rx.try_recv().is_ok() {}

        let response = transfer_zone(
            &state,
            &source_zone_id,
            TransferRequest {
                destination_zone_id: destination_zone_id.clone(),
                source_action: TransferSourceAction::Pause,
                expected_current: Some(current.key()),
            },
        )
        .await
        .unwrap();

        assert_eq!(response.source_key, current.key());
        assert_eq!(response.queued_count, 1);
        match dest_rx.try_recv().unwrap() {
            CoreToAgentCommand::PlaySource {
                source_ref, queue, ..
            } => {
                assert_eq!(source_ref.key(), current.key());
                assert_eq!(
                    queue.into_iter().map(|s| s.key()).collect::<Vec<_>>(),
                    vec![next.key()]
                );
            }
            other => panic!("expected destination play, got {other:?}"),
        }
        let source_commands = std::iter::from_fn(|| source_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            source_commands.iter().any(
                |cmd| matches!(cmd, CoreToAgentCommand::SetQueue { queue } if queue.is_empty())
            )
        );
        assert!(
            source_commands
                .iter()
                .any(|cmd| matches!(cmd, CoreToAgentCommand::Pause))
        );
        assert!(state.listening().active_source(&source_zone_id).is_none());
        assert_eq!(
            state
                .listening()
                .active_source(&destination_zone_id)
                .unwrap()
                .key(),
            current.key()
        );
    }

    #[tokio::test]
    async fn transfer_rejects_expected_current_conflict() {
        let state = app_state("transfer-expected-conflict");
        let (source_tx, _source_rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Lounge".to_string(),
            agent_capabilities("Lounge DAC"),
            source_tx,
        );
        let (dest_tx, _dest_rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-2".to_string(),
            "Kitchen".to_string(),
            agent_capabilities("Kitchen DAC"),
            dest_tx,
        );
        let zones = state.zones().list_zones();
        let source_zone_id = zones
            .iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Lounge"))
            .unwrap()
            .id
            .clone();
        let destination_zone_id = zones
            .iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Kitchen"))
            .unwrap()
            .id
            .clone();
        let current = qobuz_source(10, false);
        state.listening().start(
            state.library(),
            source_zone_id.clone(),
            "Lounge".to_string(),
            "profile-a".to_string(),
            current,
            Vec::new(),
        );
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                file_name: Some("Artist - Track 10".to_string()),
                track_title: Some("Track 10".to_string()),
                track_artist: Some("Artist".to_string()),
                track_album: Some("Album".to_string()),
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );

        let result = transfer_zone(
            &state,
            &source_zone_id,
            TransferRequest {
                destination_zone_id,
                source_action: TransferSourceAction::Pause,
                expected_current: Some("qobuz:999".to_string()),
            },
        )
        .await;

        assert!(matches!(result, Err(PlaybackError::Conflict(_))));
    }
}
