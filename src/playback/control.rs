use crate::app::state::AppState;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackIntent};
use crate::playback::router::PlaybackRouter;

pub(crate) async fn next_for_active_zone(state: &AppState) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    next_for_zone(state, &zone_id).await
}

pub(crate) async fn next_for_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::Next)
        .await
        .map(|_| ())
}

pub(crate) async fn pause_for_active_zone(state: &AppState) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    pause_for_zone(state, &zone_id).await
}

pub(crate) async fn pause_for_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::Pause)
        .await
        .map(|_| ())
}

pub(crate) async fn resume_for_active_zone(state: &AppState) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    resume_for_zone(state, &zone_id).await
}

pub(crate) async fn resume_for_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::Resume)
        .await
        .map(|_| ())
}

pub(crate) async fn stop_active_zone(state: &AppState) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    stop_for_zone(state, &zone_id).await
}

pub(crate) async fn stop_for_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::Stop)
        .await
        .map(|_| ())
}

pub(crate) async fn seek_active_zone(state: &AppState, seconds: f64) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    seek_for_zone(state, &zone_id, seconds).await
}

pub(crate) async fn seek_for_zone(
    state: &AppState,
    zone_id: &str,
    seconds: f64,
) -> Result<(), PlaybackError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(PlaybackError::bad_request(
            "Seek position must be a finite non-negative value",
        ));
    }
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::Seek { seconds })
        .await
        .map(|_| ())
}

pub(crate) fn set_loop_mode_for_active_zone(
    state: &AppState,
    mode: &str,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    set_loop_mode_for_zone(state, &zone_id, mode)
}

pub(crate) fn set_loop_mode_for_zone(
    state: &AppState,
    zone_id: &str,
    mode: &str,
) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute_immediate(
            zone_id,
            PlaybackIntent::SetLoopMode {
                mode: LoopMode::parse(mode)?,
            },
        )
        .map(|_| ())
}

pub(crate) async fn set_active_volume(state: &AppState, volume: f32) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    set_volume_for_zone(state, &zone_id, volume).await
}

pub(crate) async fn set_volume_for_zone(
    state: &AppState,
    zone_id: &str,
    volume: f32,
) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::SetVolume { volume })
        .await
        .map(|_| ())
}

pub(crate) async fn set_active_device_volume(
    state: &AppState,
    volume: f32,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    set_device_volume_for_zone(state, &zone_id, volume).await
}

pub(crate) async fn set_device_volume_for_zone(
    state: &AppState,
    zone_id: &str,
    volume: f32,
) -> Result<(), PlaybackError> {
    PlaybackRouter::new(state)
        .execute(zone_id, PlaybackIntent::SetDeviceVolume { volume })
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{agent_capabilities, app_state};
    use crate::protocol::CoreToAgentCommand;
    use serde_json::json;
    use tokio::sync::mpsc;

    #[test]
    fn loop_mode_persists_loop_mode_in_zone_queue_state() {
        let state = app_state("loop-mode-persist");
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
                    "cursor": 0,
                    "items": [{ "title": "Current", "artist": "", "album": "", "durationSecs": 1 }],
                    "loopMode": "off"
                }),
            )
            .unwrap();

        set_loop_mode_for_zone(&state, &zone_id, "loop").unwrap();

        let saved = state
            .library()
            .now_playing_queue(&zone_id)
            .unwrap()
            .unwrap()
            .state;
        assert_eq!(saved["kind"], "local");
        assert_eq!(saved["loopMode"], "loop");
    }

    #[test]
    fn remote_agent_loop_mode_arms_repeat_current_for_loop() {
        let state = app_state("remote-agent-loop-mode");
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

        set_loop_mode_for_zone(&state, &zone_id, "loop").unwrap();

        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::SetLoopMode { repeat_one: true })
        ));
        let saved = state
            .library()
            .now_playing_queue(&zone_id)
            .unwrap()
            .unwrap()
            .state;
        assert_eq!(saved["loopMode"], "loop");
    }
}
