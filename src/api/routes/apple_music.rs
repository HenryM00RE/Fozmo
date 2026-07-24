use crate::app::state::AppState;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent};
use crate::playback::queue::now_playing_queue_for_zone;
use crate::playback::router::PlaybackRouter;
use crate::playback::status::build_status_response_for_zone;
use crate::protocol::SinkProtocol;
use crate::services::apple_music_musickit::{
    AppleMusicAuthorizeRequest, AppleMusicComparisonReferenceState,
    AppleMusicComparisonSwitchRequest, AppleMusicDevPlaySongRequest, AppleMusicMvpError,
    AppleMusicMvpStatus, AppleMusicProcessTapStartRequest, AppleMusicTransportRequest,
    MusicAppSnapshot, music_app_status, pause_music_app, play_music_app, set_music_app_position,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};

type AppleMusicApiResult =
    Result<Json<AppleMusicMvpStatus>, (StatusCode, Json<AppleMusicMvpError>)>;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/apple-music/status", get(status))
        .route("/api/apple-music/launch", post(launch))
        .route("/api/apple-music/authorize", post(authorize))
        .route("/api/apple-music/dev/play-song", post(play_song))
        .route("/api/apple-music/transport", post(transport))
        .route("/api/apple-music/stop", post(stop))
        .route("/api/apple-music/shutdown", post(shutdown))
        .route(
            "/api/apple-music/process-tap/start",
            post(start_process_tap),
        )
        .route("/api/apple-music/process-tap/stop", post(stop_process_tap))
        .route(
            "/api/apple-music/comparison/switch",
            post(switch_comparison),
        )
}

async fn launch(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .launch()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn status(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .refresh_status()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn authorize(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicAuthorizeRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .authorize(request.present_ui)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn play_song(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicDevPlaySongRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .play_song(request.song_id, request.storefront)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn transport(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicTransportRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport(&request.command)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn stop(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport("stop")
        .await
        .map(Json)
        .map_err(api_error)
}

async fn shutdown(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport("shutdown")
        .await
        .map(Json)
        .map_err(api_error)
}

async fn start_process_tap(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicProcessTapStartRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .start_process_tap(
            state.zones().active_player(),
            request.confirm_system_audio_capture,
            request.mute_original_audio,
        )
        .map(Json)
        .map_err(api_error)
}

async fn stop_process_tap(State(state): State<AppState>) -> AppleMusicApiResult {
    Ok(Json(state.apple_music().stop_process_tap()))
}

async fn switch_comparison(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicComparisonSwitchRequest>,
) -> AppleMusicApiResult {
    let apple_music = state.apple_music().clone();
    let _switch_guard = apple_music.lock_comparison_switch().await;
    match request.target.trim().to_ascii_lowercase().as_str() {
        "apple_music" => {
            switch_to_apple_music(
                &state,
                request.confirm_system_audio_capture,
                request.match_position,
            )
            .await
        }
        "fozmo" => switch_to_fozmo(&state, request.match_position).await,
        _ => Err(comparison_error(
            "comparison_target_invalid",
            "Choose either Apple Music or Fozmo playback.",
            false,
            "validating_comparison",
            true,
        )),
    }
    .map(Json)
    .map_err(api_error)
}

async fn switch_to_apple_music(
    state: &AppState,
    confirm_system_audio_capture: bool,
    match_position: bool,
) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
    if state.apple_music().status().process_tap.state == "running" {
        return Ok(state.apple_music().status());
    }

    let zone_id = state.zones().active_zone_id();
    if state.zones().zone_protocol(&zone_id) != Some(SinkProtocol::LocalCoreAudio) {
        return Err(comparison_error(
            "comparison_local_zone_required",
            "Choose a local Core Audio output before starting the Apple Music comparison.",
            false,
            "capturing_fozmo_reference",
            true,
        ));
    }
    let player = state.zones().player_for_zone(&zone_id).ok_or_else(|| {
        comparison_error(
            "comparison_output_unavailable",
            "The selected local output is not currently available.",
            true,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let queue = now_playing_queue_for_zone(state, &zone_id).map_err(|error| {
        comparison_error(
            "comparison_reference_unavailable",
            error.message(),
            false,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let source = queue.current_source.ok_or_else(|| {
        comparison_error(
            "comparison_reference_missing",
            "Play the Qobuz or local version in Fozmo first, then switch to Apple Music.",
            false,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let playback = build_status_response_for_zone(state, &zone_id).map_err(|message| {
        comparison_error(
            "comparison_reference_unavailable",
            message,
            true,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let reference = AppleMusicComparisonReferenceState {
        zone_id: zone_id.clone(),
        zone_name: state.zones().zone_name(&zone_id),
        profile_id: state
            .listening()
            .profile_id(&zone_id)
            .unwrap_or_else(|| state.settings().active_profile_id()),
        source,
        queue: queue.queued_sources,
        position_secs: valid_position(playback.position_secs),
    };

    let music = music_app_snapshot().await?;
    if !music.running {
        return Err(comparison_error(
            "music_app_not_running",
            "Open the Music app and select the Apple Music version before switching.",
            false,
            "checking_music_app",
            true,
        ));
    }
    if !music.has_current_track() {
        return Err(comparison_error(
            "music_app_track_missing",
            "Select the matching track in the Music app before switching.",
            false,
            "checking_music_app",
            true,
        ));
    }

    let apple_position = if match_position {
        matched_position(reference.position_secs, music.track.duration_secs)
    } else {
        music.track.position_secs.unwrap_or(0.0)
    };
    state
        .apple_music()
        .start_process_tap(player, confirm_system_audio_capture, true)?;

    if let Err(mut error) = set_music_position(apple_position).await {
        error.cleanup_complete = restore_reference_after_failed_switch(state, &reference).await;
        return Err(error);
    }
    if let Err(mut error) = run_music_action(play_music_app, "starting_music_app").await {
        error.cleanup_complete = restore_reference_after_failed_switch(state, &reference).await;
        return Err(error);
    }

    let mut apple_track = music.track;
    apple_track.position_secs = Some(apple_position);
    state
        .apple_music()
        .comparison_switched_to_apple(reference, apple_track, match_position);
    Ok(state.apple_music().status())
}

async fn switch_to_fozmo(
    state: &AppState,
    match_position: bool,
) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
    let reference = state
        .apple_music()
        .comparison_reference()
        .ok_or_else(|| {
            comparison_error(
                "comparison_reference_missing",
                "Play the Qobuz or local version in Fozmo, then switch to Apple Music once so it can be remembered.",
                false,
                "loading_fozmo_reference",
                true,
            )
        })?;
    let music = music_app_snapshot().await?;
    let fozmo_position = if match_position {
        music.track.position_secs.unwrap_or(reference.position_secs)
    } else {
        reference.position_secs
    };

    if music.running {
        run_music_action(pause_music_app, "pausing_music_app").await?;
    }
    state.apple_music().stop_process_tap();
    if let Err(error) = play_reference(state, &reference, fozmo_position).await {
        let rollback_complete = restore_apple_after_failed_switch(state, &reference, &music).await;
        return Err(comparison_error(
            "comparison_playback_failed",
            format!("Could not restore the Fozmo reference: {}", error.message()),
            true,
            "restoring_fozmo_reference",
            rollback_complete,
        ));
    }

    state.apple_music().comparison_switched_to_fozmo(
        Some(music.track),
        match_position,
        fozmo_position,
    );
    Ok(state.apple_music().status())
}

async fn play_reference(
    state: &AppState,
    reference: &AppleMusicComparisonReferenceState,
    position_secs: f64,
) -> Result<(), crate::playback::error::PlaybackError> {
    PlaybackRouter::new(state)
        .execute(
            &reference.zone_id,
            PlaybackIntent::Play {
                profile_id: reference.profile_id.clone(),
                source: reference.source.clone(),
                queue: reference.queue.clone(),
                radio_auto: reference.source.is_radio(),
                guard: PlaybackGuard::none(),
                qobuz_request: None,
            },
        )
        .await?;
    if position_secs > 0.0 {
        PlaybackRouter::new(state)
            .execute(
                &reference.zone_id,
                PlaybackIntent::Seek {
                    seconds: position_secs,
                },
            )
            .await?;
    }
    Ok(())
}

async fn restore_reference_after_failed_switch(
    state: &AppState,
    reference: &AppleMusicComparisonReferenceState,
) -> bool {
    let _ = run_music_action(pause_music_app, "pausing_music_app").await;
    state.apple_music().stop_process_tap();
    play_reference(state, reference, reference.position_secs)
        .await
        .is_ok()
}

async fn restore_apple_after_failed_switch(
    state: &AppState,
    reference: &AppleMusicComparisonReferenceState,
    music: &MusicAppSnapshot,
) -> bool {
    if !music.running || !music.has_current_track() {
        return false;
    }
    let Some(player) = state.zones().player_for_zone(&reference.zone_id) else {
        return false;
    };
    if state
        .apple_music()
        .start_process_tap(player, true, true)
        .is_err()
    {
        return false;
    }
    if let Some(position) = music.track.position_secs {
        let _ = set_music_position(position).await;
    }
    run_music_action(play_music_app, "restoring_music_app")
        .await
        .is_ok()
}

async fn music_app_snapshot() -> Result<MusicAppSnapshot, AppleMusicMvpError> {
    tokio::task::spawn_blocking(music_app_status)
        .await
        .map_err(|error| {
            comparison_error(
                "music_app_control_failed",
                format!("Music app status task failed: {error}"),
                true,
                "checking_music_app",
                true,
            )
        })?
        .map_err(|message| {
            comparison_error(
                "music_app_control_failed",
                message,
                true,
                "checking_music_app",
                true,
            )
        })
}

async fn set_music_position(seconds: f64) -> Result<(), AppleMusicMvpError> {
    run_music_action(
        move || set_music_app_position(seconds),
        "matching_music_position",
    )
    .await
}

async fn run_music_action<F>(action: F, stage: &'static str) -> Result<(), AppleMusicMvpError>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|error| {
            comparison_error(
                "music_app_control_failed",
                format!("Music app control task failed: {error}"),
                true,
                stage,
                true,
            )
        })?
        .map_err(|message| comparison_error("music_app_control_failed", message, true, stage, true))
}

fn valid_position(position_secs: f64) -> f64 {
    if position_secs.is_finite() && position_secs >= 0.0 {
        position_secs
    } else {
        0.0
    }
}

fn matched_position(position_secs: f64, duration_secs: Option<f64>) -> f64 {
    let position = valid_position(position_secs);
    duration_secs
        .filter(|duration| duration.is_finite() && *duration > 0.5)
        .map(|duration| position.min(duration - 0.25))
        .unwrap_or(position)
}

fn comparison_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
    stage: impl Into<String>,
    cleanup_complete: bool,
) -> AppleMusicMvpError {
    AppleMusicMvpError {
        code: code.into(),
        message: message.into(),
        retryable,
        stage: stage.into(),
        cleanup_complete,
    }
}

fn api_error(error: AppleMusicMvpError) -> (StatusCode, Json<AppleMusicMvpError>) {
    let status = match error.code.as_str() {
        "helper_missing" | "song_not_found" => StatusCode::NOT_FOUND,
        "music_authorization_not_determined"
        | "music_authorization_denied"
        | "process_tap_confirmation_required" => StatusCode::FORBIDDEN,
        "session_limit_reached"
        | "process_tap_playback_changed"
        | "comparison_reference_missing"
        | "music_app_track_missing" => StatusCode::CONFLICT,
        "music_app_not_running" | "comparison_output_unavailable" => StatusCode::NOT_FOUND,
        "helper_launch_failed" | "helper_exited" => StatusCode::SERVICE_UNAVAILABLE,
        "comparison_playback_failed" | "music_app_control_failed" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_position_clamps_to_the_apple_track_duration() {
        assert_eq!(matched_position(90.0, Some(80.0)), 79.75);
        assert_eq!(matched_position(45.0, Some(80.0)), 45.0);
    }

    #[test]
    fn invalid_timeline_values_never_reach_a_transport() {
        assert_eq!(valid_position(f64::NAN), 0.0);
        assert_eq!(valid_position(f64::INFINITY), 0.0);
        assert_eq!(valid_position(-1.0), 0.0);
    }

    #[test]
    fn missing_reference_is_reported_as_a_conflict() {
        let failure = comparison_error(
            "comparison_reference_missing",
            "missing",
            false,
            "test",
            true,
        );

        assert_eq!(api_error(failure).0, StatusCode::CONFLICT);
    }
}
