use crate::app::state::AppState;
use crate::playback::commands::{PlaybackRequestSequence, accept_playback_request_sequence};
use crate::playback::dispatcher::PlaybackDispatcher;
use crate::playback::error::PlaybackError;
use crate::playback::intent::PlaybackIntent;
use crate::playback::request::{PlaybackGuard, PlaybackRequest};
use crate::playback::resolver::{
    QueueRequestItem, source_ref_from_play_request, source_ref_from_queue_request,
};
use crate::playback::source::source_ref_with_playlist_context;
use crate::protocol::{PlaylistContext, SourceRef};

#[allow(dead_code)]
pub(crate) async fn play_file_request_for_zone(
    state: &AppState,
    zone_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    track_id: Option<i64>,
    file_name: Option<&str>,
    playlist_context: Option<PlaylistContext>,
    queue: &[QueueRequestItem],
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    play_file_request_for_zone_with_profile(
        state,
        zone_id,
        &profile_id,
        sequence,
        track_id,
        file_name,
        playlist_context,
        queue,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn play_file_request_for_zone_with_profile(
    state: &AppState,
    zone_id: &str,
    profile_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    track_id: Option<i64>,
    file_name: Option<&str>,
    playlist_context: Option<PlaylistContext>,
    queue: &[QueueRequestItem],
) -> Result<(), PlaybackError> {
    if !accept_playback_request_sequence(state, sequence.as_ref()) {
        return Err(PlaybackError::conflict("Playback changed"));
    }

    let source = source_ref_with_playlist_context(
        source_ref_from_play_request(state, track_id, file_name)?
            .ok_or_else(missing_play_source_error)?,
        playlist_context,
    );
    let queue_sources = queue_source_refs_from_request(state, queue)?;
    PlaybackDispatcher::new(state)
        .execute(
            zone_id,
            PlaybackIntent::Play {
                request: PlaybackRequest {
                    profile_id: profile_id.to_string(),
                    source,
                    queue: queue_sources,
                    radio_auto: false,
                    guard: PlaybackGuard::from_expected_sequence(sequence),
                    qobuz_request: None,
                },
            },
        )
        .await
        .map(|_| ())
}

#[allow(dead_code)]
pub(crate) async fn play_file_request_for_active_zone(
    state: &AppState,
    sequence: Option<PlaybackRequestSequence>,
    track_id: Option<i64>,
    file_name: Option<&str>,
    playlist_context: Option<PlaylistContext>,
    queue: &[QueueRequestItem],
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    play_file_request_for_active_zone_with_profile(
        state,
        &profile_id,
        sequence,
        track_id,
        file_name,
        playlist_context,
        queue,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn play_file_request_for_active_zone_with_profile(
    state: &AppState,
    profile_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    track_id: Option<i64>,
    file_name: Option<&str>,
    playlist_context: Option<PlaylistContext>,
    queue: &[QueueRequestItem],
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    play_file_request_for_zone_with_profile(
        state,
        &zone_id,
        profile_id,
        sequence,
        track_id,
        file_name,
        playlist_context,
        queue,
    )
    .await
}

fn queue_source_refs_from_request(
    state: &AppState,
    queue: &[QueueRequestItem],
) -> Result<Vec<SourceRef>, PlaybackError> {
    let mut queue_sources = Vec::with_capacity(queue.len());
    for item in queue {
        let source = source_ref_from_queue_request(state, item)?
            .ok_or_else(|| PlaybackError::bad_request("Queue item is missing a playable source"))?;
        queue_sources.push(source);
    }
    Ok(queue_sources)
}

fn missing_play_source_error() -> PlaybackError {
    PlaybackError::bad_request("Missing file_name or track_id")
}
