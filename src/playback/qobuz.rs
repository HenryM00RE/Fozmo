use crate::app::state::AppState;
use crate::audio::player::{StreamQueueItem, TrackCover, TrackTags};
use crate::playback::commands::{
    PlaybackRequestSequence, accept_playback_request_sequence,
    is_current_playback_request_sequence, is_current_playback_sequence,
    playback_request_sequence_is_stale,
};
use crate::playback::error::PlaybackError;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent};
use crate::playback::now_playing::{
    current_playback_matches_expected, sonos_current_file_name, sonos_current_matches,
};
use crate::playback::queue::queue_loop_enabled_for_zone;
use crate::playback::router::PlaybackRouter;
use crate::playback::service::playback_config_for_zone;
use crate::playback::sonos::{prepare_sonos_asset, sonos_target_for_zone};
use crate::playback::source::{
    qobuz_play_request_from_source_ref, qobuz_queue_source_refs,
    qobuz_source_ref_from_play_request, qobuz_source_ref_from_track, qobuz_track_id_from_source,
};
use crate::playback::upnp::prewarm_upnp_source_for_zone;
use crate::protocol::{CoreToAgentCommand, SinkProtocol, SourceRef};
use crate::services::qobuz::{QobuzPlayRequest, QobuzQueueTrack};

#[allow(dead_code)]
pub(crate) async fn play_qobuz_request_for_active_zone(
    state: AppState,
    sequence: Option<PlaybackRequestSequence>,
    req: QobuzPlayRequest,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    play_qobuz_request_for_active_zone_with_profile(state, &profile_id, sequence, req).await
}

pub(crate) async fn play_qobuz_request_for_active_zone_with_profile(
    state: AppState,
    profile_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    req: QobuzPlayRequest,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    if unmarked_qobuz_play_would_replace_local_source(&state, &zone_id, &req) {
        eprintln!(
            "qobuz: rejected unmarked play over active local source -- {} - {}",
            req.artist.as_deref().unwrap_or(""),
            req.title.as_deref().unwrap_or("")
        );
        return Err(PlaybackError::conflict("Playback changed"));
    }
    play_qobuz_request_for_zone_with_profile(state, &zone_id, profile_id, sequence, req).await
}

pub(crate) async fn play_qobuz_request_for_zone(
    state: AppState,
    zone_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    req: QobuzPlayRequest,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    play_qobuz_request_for_zone_with_profile(state, zone_id, &profile_id, sequence, req).await
}

pub(crate) async fn play_qobuz_request_for_zone_with_profile(
    state: AppState,
    zone_id: &str,
    profile_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    req: QobuzPlayRequest,
) -> Result<(), PlaybackError> {
    if !accept_playback_request_sequence(&state, sequence.as_ref()) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    if req.expected_current.as_ref().is_some()
        && !current_playback_matches_expected(&state, zone_id, &req.expected_current)
    {
        return Err(PlaybackError::conflict("Current track changed"));
    }
    let queue_sources = qobuz_queue_source_refs(&req);
    let source_ref = qobuz_source_ref_from_play_request(&req);
    let radio_auto = req.radio_auto;
    PlaybackRouter::new(&state)
        .execute(
            zone_id,
            PlaybackIntent::Play {
                profile_id: profile_id.to_string(),
                source: source_ref,
                queue: queue_sources,
                radio_auto,
                guard: PlaybackGuard::from_expected_sequence(sequence),
                qobuz_request: Some(Box::new(req)),
            },
        )
        .await
        .map(|_| ())
}

fn unmarked_qobuz_play_would_replace_local_source(
    state: &AppState,
    zone_id: &str,
    req: &QobuzPlayRequest,
) -> bool {
    if req.replace_current {
        return false;
    }
    matches!(
        state.listening().active_source(zone_id),
        Some(SourceRef::LocalTrack { .. })
    )
}

pub(crate) async fn prefetch_qobuz_request_for_active_zone(
    state: AppState,
    sequence: Option<PlaybackRequestSequence>,
    req: QobuzPlayRequest,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    prefetch_qobuz_request_for_zone(state, &zone_id, sequence, req).await
}

pub(crate) async fn prefetch_qobuz_request_for_zone(
    state: AppState,
    zone_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    req: QobuzPlayRequest,
) -> Result<(), PlaybackError> {
    if playback_request_sequence_is_stale(&state, sequence.as_ref()) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let expected_playback_sequence = sequence.clone();

    let prefetched_source = qobuz_source_ref_from_play_request(&req);
    let queued_sources = state
        .library()
        .zone_queue(zone_id)
        .map_err(internal_error)?;
    let repeat_current_prefetch =
        qobuz_repeat_current_prefetch_allowed(&state, zone_id, req.track_id);
    if queued_sources.is_empty() {
        if !req.radio_auto && !repeat_current_prefetch {
            return Err(PlaybackError::conflict("Track is no longer queued"));
        }
    } else if !qobuz_queue_contains_track(&queued_sources, req.track_id) && !repeat_current_prefetch
    {
        return Err(PlaybackError::conflict("Track is no longer queued"));
    }

    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::RemoteAgent) {
        state
            .zones()
            .send_to_zone(
                zone_id,
                CoreToAgentCommand::PreFetch {
                    source_ref: prefetched_source.clone(),
                    stream_base_url: state.public_base_url().clone(),
                },
            )
            .map_err(internal_error)?;
        if queued_sources.is_empty() {
            set_radio_prefetched_queue(&state, zone_id, prefetched_source, req.radio_auto);
        }
        return Ok(());
    }
    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::UpnpAvRenderer) {
        prewarm_upnp_source_for_zone(
            state.clone(),
            zone_id,
            expected_playback_sequence,
            state.listening().active_source(zone_id),
            prefetched_source.clone(),
            false,
        )
        .await?;
        return Ok(());
    }
    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::SonosUpnp) {
        let expected_current = req
            .expected_current
            .clone()
            .or_else(|| sonos_current_file_name(&state, zone_id));
        if let Some(expected) = expected_current.as_deref()
            && sonos_current_file_name(&state, zone_id).as_deref() != Some(expected)
        {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        let target = sonos_target_for_zone(&state, zone_id)?;
        let player = state
            .zones()
            .player_for_zone(zone_id)
            .unwrap_or_else(|| state.zones().active_player());
        let playback_config = playback_config_for_zone(&state, zone_id, player.as_ref());
        let asset = prepare_sonos_asset(&state, &prefetched_source, &playback_config).await?;
        if expected_playback_sequence
            .as_ref()
            .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
            || !sonos_current_matches(&state, zone_id, &expected_current)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        state
            .sonos()
            .set_next(zone_id, &target, asset)
            .await
            .map_err(PlaybackError::integration)?;
        if queued_sources.is_empty() {
            set_radio_prefetched_queue(&state, zone_id, prefetched_source, req.radio_auto);
        }
        return Ok(());
    }

    let expected_current = req.expected_current.clone();
    let Some(player) = state.zones().player_for_zone(zone_id) else {
        return Err(PlaybackError::ZoneNotAvailable);
    };
    let playback_epoch = player.playback_epoch();
    qobuz_prefetch_command_expected_current(&state, zone_id, &expected_current, &player)?;
    let cover_fut = async {
        let url = req.image_url.as_deref()?;
        let (mime, data) = state.qobuz().fetch_cover_public(url).await.ok()?;
        match (mime, data) {
            (Some(mime), Some(data)) => Some(TrackCover { mime, data }),
            _ => None,
        }
    };
    let stream_fut = state.qobuz().open_stream(&req);
    let (stream_result, fallback_cover) = tokio::join!(stream_fut, cover_fut);
    let handle = stream_result.map_err(internal_error)?;
    if !is_current_playback_request_sequence(&state, sequence.as_ref()) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    if player.playback_epoch() != playback_epoch {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let command_expected_current =
        qobuz_prefetch_command_expected_current(&state, zone_id, &expected_current, &player)?;

    let fallback_tags = TrackTags {
        title: req.title.clone(),
        artist: req.artist.clone(),
        album: req.album.clone(),
        album_artist: req.artist.clone(),
        duration_secs: req.duration_secs,
        ..TrackTags::default()
    };

    player.set_stream_queue_if_epoch(
        vec![StreamQueueItem {
            source: Box::new(handle.source),
            ext_hint: Some(handle.ext),
            display_name: handle.display_name,
            fallback_cover,
            fallback_tags: Some(fallback_tags),
        }],
        command_expected_current,
        Some(playback_epoch),
    );
    if queued_sources.is_empty() {
        set_radio_prefetched_queue(&state, zone_id, prefetched_source, req.radio_auto);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn qobuz_radio_next_for_zone(
    state: AppState,
    zone_id: &str,
) -> Result<bool, String> {
    let Some(req) = qobuz_radio_next_request_for_zone(state.clone(), zone_id).await? else {
        return Ok(false);
    };
    play_qobuz_request_for_zone(state, zone_id, None, req)
        .await
        .map_err(|error| error.message().to_string())?;
    Ok(true)
}

pub(crate) async fn qobuz_radio_next_request_for_zone(
    state: AppState,
    zone_id: &str,
) -> Result<Option<QobuzPlayRequest>, String> {
    let Some(active_source) = state.listening().active_source(zone_id) else {
        return Ok(None);
    };
    qobuz_radio_next_request_from_source_for_zone(state, zone_id, active_source).await
}

pub(crate) async fn qobuz_radio_next_from_source_for_zone(
    state: AppState,
    zone_id: &str,
    active_source: SourceRef,
) -> Result<bool, String> {
    let Some(req) =
        qobuz_radio_next_request_from_source_for_zone(state.clone(), zone_id, active_source)
            .await?
    else {
        return Ok(false);
    };
    play_qobuz_request_for_zone(state, zone_id, None, req)
        .await
        .map_err(|error| error.message().to_string())?;
    Ok(true)
}

pub(crate) async fn qobuz_radio_next_request_from_source_for_zone(
    state: AppState,
    zone_id: &str,
    active_source: SourceRef,
) -> Result<Option<QobuzPlayRequest>, String> {
    if !state.settings().qobuz_radio_enabled() {
        return Ok(None);
    }
    if let Ok(queue) = state.library().zone_queue(zone_id)
        && !queue.is_empty()
    {
        return Ok(None);
    }

    let recommendation = match active_source {
        SourceRef::QobuzTrack { track_id, .. } => {
            let exclude = radio_exclude_track_ids(&state, zone_id, Some(track_id));
            state.qobuz().radio_next(track_id, &exclude, 50).await?
        }
        SourceRef::LocalTrack { artist, .. } => {
            let Some(seed_artist_name) = artist
                .as_deref()
                .map(str::trim)
                .filter(|artist| !artist.is_empty())
            else {
                return Ok(None);
            };
            let exclude = radio_exclude_track_ids(&state, zone_id, None);
            state
                .qobuz()
                .radio_next_for_artist_name(seed_artist_name, &exclude, 50)
                .await?
        }
    };
    let Some(recommendation) = recommendation else {
        return Err("Qobuz radio returned no playable recommendation".to_string());
    };
    let source = qobuz_source_ref_from_track(&recommendation.track, true);
    let Some(req) = qobuz_play_request_from_source_ref(&source, &[], true) else {
        return Err("Qobuz radio recommendation was not playable".to_string());
    };
    Ok(Some(req))
}

pub(crate) async fn prefetch_qobuz_queue_track_into_player(
    state: AppState,
    zone_id: String,
    track: QobuzQueueTrack,
    expected_current: String,
    expected_epoch: u64,
    fallback_radio: bool,
) -> Result<(), String> {
    let Some(player) = state.zones().player_for_zone(&zone_id) else {
        return Err("Zone not available".to_string());
    };
    let expected_current_option = Some(expected_current.clone());
    if player.playback_epoch() != expected_epoch
        || !current_playback_matches_expected(&state, &zone_id, &expected_current_option)
    {
        return Err("Playback changed".to_string());
    }
    qobuz_prefetch_command_expected_current(&state, &zone_id, &expected_current_option, &player)
        .map_err(|error| error.message().to_string())?;

    let req = QobuzPlayRequest {
        track_id: track.track_id,
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        album_id: track.album_id.clone(),
        image_url: track.image_url.clone(),
        duration_secs: track.duration_secs,
        format_id: track.format_id,
        expected_current: Some(expected_current.clone()),
        radio_auto: track.radio || fallback_radio,
        replace_current: false,
        playlist_context: track.playlist_context.clone(),
        queue: Vec::new(),
    };

    let item = qobuz_stream_queue_item_for_request(&state, &req).await?;

    if player.playback_epoch() != expected_epoch
        || !current_playback_matches_expected(&state, &zone_id, &expected_current_option)
    {
        return Err("Playback changed".to_string());
    }
    let command_expected_current = qobuz_prefetch_command_expected_current(
        &state,
        &zone_id,
        &expected_current_option,
        &player,
    )
    .map_err(|error| error.message().to_string())?;

    player.set_stream_queue_if_epoch(vec![item], command_expected_current, Some(expected_epoch));
    Ok(())
}

async fn qobuz_stream_queue_item_for_request(
    state: &AppState,
    req: &QobuzPlayRequest,
) -> Result<StreamQueueItem, String> {
    let cover_fut = async {
        let url = req.image_url.as_deref()?;
        let (mime, data) = state.qobuz().fetch_cover_public(url).await.ok()?;
        match (mime, data) {
            (Some(mime), Some(data)) => Some(TrackCover { mime, data }),
            _ => None,
        }
    };
    let stream_fut = state.qobuz().open_stream(req);
    let (stream_result, fallback_cover) = tokio::join!(stream_fut, cover_fut);
    let handle = stream_result?;
    let fallback_tags = TrackTags {
        title: req.title.clone(),
        artist: req.artist.clone(),
        album: req.album.clone(),
        album_artist: req.artist.clone(),
        duration_secs: req.duration_secs,
        ..TrackTags::default()
    };

    Ok(StreamQueueItem {
        source: Box::new(handle.source),
        ext_hint: Some(handle.ext),
        display_name: handle.display_name,
        fallback_cover,
        fallback_tags: Some(fallback_tags),
    })
}

fn qobuz_queue_contains_track(queue: &[crate::library::ZoneQueueEntry], track_id: u64) -> bool {
    queue.iter().any(|entry| {
        matches!(
            &entry.source,
            SourceRef::QobuzTrack {
                track_id: queued_id,
                ..
            } if *queued_id == track_id
        )
    })
}

fn qobuz_repeat_current_prefetch_allowed(state: &AppState, zone_id: &str, track_id: u64) -> bool {
    if !queue_loop_enabled_for_zone(state, zone_id) {
        return false;
    }
    matches!(
        state.listening().active_source(zone_id),
        Some(SourceRef::QobuzTrack {
            track_id: active_track_id,
            ..
        }) if active_track_id == track_id
    )
}

fn qobuz_prefetch_command_expected_current(
    state: &AppState,
    zone_id: &str,
    expected_current: &Option<String>,
    player: &std::sync::Arc<crate::audio::player::Player>,
) -> Result<Option<String>, PlaybackError> {
    if !current_playback_matches_expected(state, zone_id, expected_current) {
        return Err(PlaybackError::conflict("Current track changed"));
    }
    Ok(expected_current.as_ref().map(|expected| {
        player
            .current_file_name()
            .unwrap_or_else(|| expected.clone())
    }))
}

fn set_radio_prefetched_queue(
    state: &AppState,
    zone_id: &str,
    source: SourceRef,
    radio_auto: bool,
) {
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    let queue = vec![source];
    let _ = state.library().set_zone_queue(zone_id, &queue);
    state
        .listening()
        .set_queue_with_radio(zone_id, profile_id, queue, radio_auto);
}

fn radio_exclude_track_ids(
    state: &AppState,
    zone_id: &str,
    seed_track_id: Option<u64>,
) -> Vec<u64> {
    let mut exclude = Vec::<u64>::new();
    let mut push = |track_id: u64| {
        if track_id > 0 && !exclude.contains(&track_id) {
            exclude.push(track_id);
        }
    };

    if let Some(seed_track_id) = seed_track_id {
        push(seed_track_id);
    }
    if let Some(active) = state.listening().active_source(zone_id)
        && let Some(track_id) = qobuz_track_id_from_source(&active)
    {
        push(track_id);
    }
    if let Ok(queue) = state.library().zone_queue(zone_id) {
        for entry in queue {
            if let Some(track_id) = qobuz_track_id_from_source(&entry.source) {
                push(track_id);
            }
        }
    }
    let live = state.listening().active_history_inputs();
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    if let Ok(recent) = state
        .library()
        .recent_playback_history_with_live_for_profile(&profile_id, 100, &live, true)
    {
        for entry in recent {
            if let Some(track_id) = qobuz_track_id_from_source(&entry.source) {
                push(track_id);
            }
        }
    }
    exclude.truncate(100);
    exclude
}

fn internal_error(e: String) -> PlaybackError {
    PlaybackError::library(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{agent_capabilities, app_state, qobuz_source};
    use crate::protocol::AgentPlaybackState;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn zone_qobuz_play_rejects_stale_sequence_before_playback() {
        let state = app_state("zone-qobuz-stale-sequence");
        let latest = PlaybackRequestSequence::new("client-a", 2);
        let stale = PlaybackRequestSequence::new("client-a", 1);
        assert!(accept_playback_request_sequence(&state, Some(&latest)));

        let result =
            play_qobuz_request_for_zone(state, "missing-zone", Some(stale), qobuz_play_request(11))
                .await;

        assert!(
            matches!(result, Err(PlaybackError::Conflict(message)) if message == "Playback changed")
        );
    }

    #[tokio::test]
    async fn qobuz_radio_next_yields_to_prefetched_queue() {
        let state = app_state("qobuz-radio-prefetch");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let active = qobuz_source(1, true);
        let prefetched = qobuz_source(2, true);
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&prefetched))
            .unwrap();
        state.listening().start_with_radio(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            active,
            vec![prefetched],
            true,
        );

        let advanced = qobuz_radio_next_for_zone(state, &zone_id).await.unwrap();

        assert!(!advanced);
    }

    #[tokio::test]
    async fn qobuz_radio_next_respects_disabled_setting() {
        let state = app_state("qobuz-radio-disabled");
        let zone_id = state.zones().active_zone_id();
        let _ = state.settings().update(|settings| {
            settings.qobuz_radio_enabled = Some(false);
        });
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            qobuz_source(1, false),
            Vec::new(),
        );

        let advanced = qobuz_radio_next_for_zone(state, &zone_id).await.unwrap();

        assert!(!advanced);
    }

    #[tokio::test]
    async fn qobuz_radio_next_from_captured_source_survives_missing_active_listen() {
        let state = app_state("qobuz-radio-captured-source");
        let zone_id = state.zones().active_zone_id();
        let _ = state.settings().update(|settings| {
            settings.qobuz_radio_enabled = Some(false);
        });

        let advanced =
            qobuz_radio_next_from_source_for_zone(state, &zone_id, qobuz_source(1, false))
                .await
                .unwrap();

        assert!(!advanced);
    }

    #[tokio::test]
    async fn remote_agent_qobuz_play_accepts_agent_current_file_name() {
        let state = app_state("remote-agent-qobuz-current");
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

        let current = qobuz_source(10, false);
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "Studio PC".to_string(),
            state.settings().active_profile_id(),
            current,
            Vec::new(),
        );
        state.zones().update_playback(
            "agent-1",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: None,
                file_name: Some("Artist - Track 10".to_string()),
                track_title: Some("Track 10".to_string()),
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

        let result = play_qobuz_request_for_zone(
            state.clone(),
            &zone_id,
            None,
            QobuzPlayRequest {
                track_id: 11,
                title: Some("Track 11".to_string()),
                artist: Some("Artist".to_string()),
                album: Some("Album".to_string()),
                album_id: Some("album".to_string()),
                image_url: None,
                duration_secs: Some(180.0),
                format_id: None,
                expected_current: Some("Artist - Track 10".to_string()),
                radio_auto: false,
                replace_current: true,
                playlist_context: None,
                queue: Vec::new(),
            },
        )
        .await;

        assert!(result.is_ok());
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::PlaySource { source_ref, .. })
                if source_ref.key() == "qobuz:11"
        ));
    }

    #[test]
    fn qobuz_prefetch_expected_current_accepts_source_key() {
        let state = app_state("qobuz-prefetch-source-key");
        let zone_id = state.zones().active_zone_id();
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active local zone should have a player");
        player.set_current_file_name_for_test(Some("Artist - Track 10".to_string()));
        let current = qobuz_source(10, false);
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "Core".to_string(),
            state.settings().active_profile_id(),
            current,
            Vec::new(),
        );

        let expected = qobuz_prefetch_command_expected_current(
            &state,
            &zone_id,
            &Some("qobuz:10".to_string()),
            &player,
        )
        .expect("source-key expected_current should match active Qobuz source");

        assert_eq!(expected.as_deref(), Some("Artist - Track 10"));
    }

    fn qobuz_play_request(track_id: u64) -> QobuzPlayRequest {
        QobuzPlayRequest {
            track_id,
            title: Some(format!("Track {track_id}")),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_id: Some("album".to_string()),
            image_url: None,
            duration_secs: Some(180.0),
            format_id: None,
            expected_current: None,
            radio_auto: false,
            replace_current: true,
            playlist_context: None,
            queue: Vec::new(),
        }
    }
}
