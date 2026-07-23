use crate::app::state::AppState;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackOutcome};
use crate::playback::request::PlaybackRequest;
use crate::playback::sonos::{play_sonos_source_for_zone, sonos_target_for_zone};

pub(crate) struct SonosSink<'a> {
    state: &'a AppState,
}

impl<'a> SonosSink<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(super) async fn play(
        &self,
        zone_id: &str,
        request: PlaybackRequest,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let PlaybackRequest {
            profile_id,
            source,
            queue,
            radio_auto,
            guard,
            qobuz_request: _,
        } = request;
        play_sonos_source_for_zone(
            (*self.state).clone(),
            zone_id,
            profile_id,
            guard.expected_sequence().cloned(),
            source,
            queue,
            radio_auto,
        )
        .await?;
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) async fn next(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        let target = sonos_target_for_zone(self.state, zone_id)?;
        self.state
            .sonos()
            .next(zone_id, &target)
            .await
            .map_err(PlaybackError::integration)?;
        self.state.listening().next(self.state.library(), zone_id);
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) async fn pause(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let target = sonos_target_for_zone(self.state, zone_id)?;
        self.state
            .sonos()
            .pause(zone_id, &target)
            .await
            .map_err(PlaybackError::integration)
    }

    pub(super) async fn resume(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let target = sonos_target_for_zone(self.state, zone_id)?;
        self.state
            .sonos()
            .resume(zone_id, &target)
            .await
            .map_err(PlaybackError::integration)
    }

    pub(super) async fn stop(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let target = sonos_target_for_zone(self.state, zone_id)?;
        self.state
            .sonos()
            .stop(zone_id, &target)
            .await
            .map_err(PlaybackError::integration)
    }

    pub(super) async fn seek(&self, zone_id: &str, seconds: f64) -> Result<(), PlaybackError> {
        let target = sonos_target_for_zone(self.state, zone_id)?;
        self.state
            .sonos()
            .seek(zone_id, &target, seconds)
            .await
            .map_err(PlaybackError::integration)
    }

    pub(super) fn set_loop_mode(
        &self,
        zone_id: &str,
        mode: &LoopMode,
    ) -> Result<(), PlaybackError> {
        if let Some(player) = self.state.zones().player_for_zone(zone_id) {
            player.set_repeat_one(mode.repeat_one());
        }
        Ok(())
    }

    pub(super) async fn set_volume(&self, zone_id: &str, volume: f32) -> Result<(), PlaybackError> {
        self.set_device_volume(zone_id, volume.min(1.0)).await
    }

    pub(super) async fn set_device_volume(
        &self,
        zone_id: &str,
        volume: f32,
    ) -> Result<(), PlaybackError> {
        let target = sonos_target_for_zone(self.state, zone_id)?;
        self.state
            .sonos()
            .set_volume(zone_id, &target, volume)
            .await
            .map_err(PlaybackError::integration)
    }
}
