use crate::app::state::AppState;
use crate::diagnostics::logging::sanitize_error;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackOutcome};
use crate::playback::request::PlaybackRequest;
use crate::playback::service::playback_config_for_zone;
use crate::protocol::CoreToAgentCommand;
use tracing::warn;

pub(crate) struct RemoteAgentSink<'a> {
    state: &'a AppState,
}

impl<'a> RemoteAgentSink<'a> {
    pub(super) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(super) fn play(
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
        if !guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        let player = self.state.zones().active_player();
        self.state
            .zones()
            .send_to_zone(
                zone_id,
                CoreToAgentCommand::PlaySource {
                    source_ref: source.clone(),
                    queue: queue.clone(),
                    playback_config: playback_config_for_zone(self.state, zone_id, &player),
                    stream_base_url: self.state.public_base_url().clone(),
                },
            )
            .map_err(PlaybackError::integration)?;
        if let Err(error) = self.state.library().set_zone_queue(zone_id, &queue) {
            warn!(
                event = "playback_queue_persist_failed",
                zone_id,
                error_kind = "library",
                error = %sanitize_error(&error),
                "Failed to persist zone queue"
            );
        }
        self.state.listening().start_with_radio(
            self.state.library(),
            zone_id.to_string(),
            self.state.zones().zone_name(zone_id),
            profile_id,
            source,
            queue,
            radio_auto,
        );
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) fn next(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        self.send(zone_id, CoreToAgentCommand::Next)?;
        self.state.listening().next(self.state.library(), zone_id);
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) fn pause(&self, zone_id: &str) -> Result<(), PlaybackError> {
        self.send(zone_id, CoreToAgentCommand::Pause)
    }

    pub(super) fn resume(&self, zone_id: &str) -> Result<(), PlaybackError> {
        self.send(zone_id, CoreToAgentCommand::Resume)
    }

    pub(super) fn stop(&self, zone_id: &str) -> Result<(), PlaybackError> {
        self.send(zone_id, CoreToAgentCommand::Stop)
    }

    pub(super) fn seek(&self, zone_id: &str, seconds: f64) -> Result<(), PlaybackError> {
        self.send(zone_id, CoreToAgentCommand::Seek { seconds })
    }

    pub(super) fn set_loop_mode(
        &self,
        zone_id: &str,
        mode: &LoopMode,
    ) -> Result<(), PlaybackError> {
        self.send(
            zone_id,
            CoreToAgentCommand::SetLoopMode {
                repeat_one: mode.repeat_one(),
            },
        )
    }

    pub(super) fn set_volume(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let player = self
            .state
            .zones()
            .player_for_zone(zone_id)
            .unwrap_or_else(|| self.state.zones().active_player());
        self.send(
            zone_id,
            CoreToAgentCommand::SetPlaybackConfig {
                playback_config: playback_config_for_zone(self.state, zone_id, &player),
            },
        )
    }

    pub(super) fn set_device_volume(&self) -> Result<(), PlaybackError> {
        Err(PlaybackError::bad_request(
            "Device volume for remote agents is not available from this control",
        ))
    }

    fn send(&self, zone_id: &str, command: CoreToAgentCommand) -> Result<(), PlaybackError> {
        self.state
            .zones()
            .send_to_zone(zone_id, command)
            .map_err(PlaybackError::integration)
    }
}
