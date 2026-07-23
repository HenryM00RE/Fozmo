use crate::app::state::AppState;
use crate::diagnostics::logging::sanitize_error;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackOutcome};
use crate::playback::request::{PlaybackGuard, PlaybackRequest};
use crate::playback::service::{hegel_settings_for_zone, playback_config_for_zone};
use crate::playback::upnp::{
    play_upnp_source_for_zone, seek_upnp_with_dsp_fallback, upnp_target_for_zone,
};
use crate::services::hegel;
use std::time::Duration;
use tracing::warn;

const HEGEL_DOP_SEEK_MEDIA_EVIDENCE_TIMEOUT: Duration = Duration::from_secs(4);

struct HegelDopMuteGuard {
    host: String,
    port: u16,
    play_id: u64,
    restore_unmuted: bool,
}

pub(crate) struct UpnpSink<'a> {
    state: &'a AppState,
}

impl<'a> UpnpSink<'a> {
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
        let player = self
            .state
            .zones()
            .player_for_zone(zone_id)
            .unwrap_or_else(|| self.state.zones().active_player());
        let playback_config = playback_config_for_zone(self.state, zone_id, &player);
        play_upnp_source_for_zone(
            (*self.state).clone(),
            zone_id,
            profile_id,
            guard.expected_sequence().cloned(),
            source,
            queue,
            radio_auto,
            playback_config,
        )
        .await?;
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) async fn next(
        &self,
        zone_id: &str,
        profile_id: String,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let mut queued_sources = self
            .state
            .library()
            .zone_queue(zone_id)
            .map_err(PlaybackError::library)?
            .into_iter()
            .map(|entry| entry.source)
            .collect::<Vec<_>>();
        if !queued_sources.is_empty() {
            let source = queued_sources[0].clone();
            let target = upnp_target_for_zone(self.state, zone_id)?;
            match self
                .state
                .upnp()
                .renderer_next_if_armed_for_source(zone_id, &target, &source)
                .await
            {
                Ok(true) => {
                    self.state.listening().next(self.state.library(), zone_id);
                    return Ok(PlaybackOutcome::Completed);
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(
                        event = "upnp_renderer_next_failed",
                        zone_id,
                        error = %sanitize_error(&error),
                        "UPnP renderer next handoff failed; falling back to app-side play"
                    );
                }
            }
            let source = queued_sources.remove(0);
            let radio_auto = source.is_radio();
            self.state
                .library()
                .set_zone_queue(zone_id, &queued_sources)
                .map_err(PlaybackError::library)?;
            self.state
                .listening()
                .set_queue(zone_id, profile_id.clone(), queued_sources.clone());
            return self
                .play(
                    zone_id,
                    PlaybackRequest {
                        profile_id,
                        source,
                        queue: queued_sources,
                        radio_auto,
                        guard: PlaybackGuard::none(),
                        qobuz_request: None,
                    },
                )
                .await;
        }

        if self.state.listening().active_source(zone_id).is_some() {
            return Ok(PlaybackOutcome::Completed);
        }
        let target = upnp_target_for_zone(self.state, zone_id)?;
        self.state
            .upnp()
            .next(&target)
            .await
            .map_err(PlaybackError::integration)?;
        self.state.listening().next(self.state.library(), zone_id);
        Ok(PlaybackOutcome::Completed)
    }

    pub(super) async fn pause(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let target = upnp_target_for_zone(self.state, zone_id)?;
        let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
        let result = self
            .state
            .upnp()
            .pause(zone_id, &target)
            .await
            .map_err(PlaybackError::integration);
        self.finish_hegel_dop_mute_guard(zone_id, guard).await;
        result
    }

    pub(super) async fn resume(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let target = upnp_target_for_zone(self.state, zone_id)?;
        let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
        let result = self
            .state
            .upnp()
            .resume(zone_id, &target)
            .await
            .map_err(PlaybackError::integration);
        self.finish_hegel_dop_mute_guard(zone_id, guard).await;
        result
    }

    pub(super) async fn stop(&self, zone_id: &str) -> Result<(), PlaybackError> {
        let target = upnp_target_for_zone(self.state, zone_id)?;
        let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
        let result = self
            .state
            .upnp()
            .stop(zone_id, &target)
            .await
            .map_err(PlaybackError::integration);
        self.finish_hegel_dop_mute_guard(zone_id, guard).await;
        result
    }

    pub(super) async fn seek(&self, zone_id: &str, seconds: f64) -> Result<(), PlaybackError> {
        let target = upnp_target_for_zone(self.state, zone_id)?;
        let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
        if guard.is_some() {
            self.state
                .upnp()
                .mark_dop_seek_strategy(zone_id, "hegel_mute_guarded_upnp_seek");
        }
        let result = seek_upnp_with_dsp_fallback(self.state, zone_id, &target, seconds).await;
        self.finish_hegel_dop_mute_guard(zone_id, guard).await;
        result
    }

    pub(super) fn set_loop_mode(
        &self,
        _zone_id: &str,
        _mode: &LoopMode,
    ) -> Result<(), PlaybackError> {
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
        let target = upnp_target_for_zone(self.state, zone_id)?;
        self.state
            .upnp()
            .set_volume(zone_id, &target, volume)
            .await
            .map_err(PlaybackError::integration)
    }

    async fn begin_hegel_dop_mute_guard(
        &self,
        zone_id: &str,
        target: &crate::audio::upnp::UpnpRendererTarget,
    ) -> Option<HegelDopMuteGuard> {
        if !cfg!(feature = "hegel")
            || !self
                .state
                .upnp()
                .current_playback_uses_hegel_dop_wav(zone_id, target)
        {
            return None;
        }
        let Some(settings) = hegel_settings_for_zone(self.state, zone_id) else {
            self.state
                .upnp()
                .mark_hegel_mute_guard(zone_id, "settings_missing");
            return None;
        };
        let host = settings.host.as_deref().unwrap_or_default().to_string();
        let play_id = self.state.upnp().current_play_id(zone_id);
        let status = match hegel::query_status(&host, settings.port).await {
            Ok(status) => self.state.hegel_status().remember(status),
            Err(error) => {
                self.state
                    .upnp()
                    .mark_hegel_mute_guard(zone_id, "query_failed");
                warn!(
                    event = "external_service_failure",
                    service = "hegel",
                    zone_id,
                    error = %sanitize_error(&error),
                    "Hegel mute guard status query failed"
                );
                return None;
            }
        };
        match status.muted {
            Some(true) => {
                self.state
                    .upnp()
                    .mark_hegel_mute_guard(zone_id, "already_muted");
                Some(HegelDopMuteGuard {
                    host,
                    port: settings.port,
                    play_id,
                    restore_unmuted: false,
                })
            }
            Some(false) => match hegel::set_mute(&host, settings.port, true).await {
                Ok(status) => {
                    self.state.hegel_status().remember(status);
                    self.state.upnp().mark_hegel_mute_guard(zone_id, "muted");
                    tokio::time::sleep(Duration::from_millis(90)).await;
                    Some(HegelDopMuteGuard {
                        host,
                        port: settings.port,
                        play_id,
                        restore_unmuted: true,
                    })
                }
                Err(error) => {
                    self.state
                        .upnp()
                        .mark_hegel_mute_guard(zone_id, "mute_failed");
                    warn!(
                        event = "external_service_failure",
                        service = "hegel",
                        zone_id,
                        error = %sanitize_error(&error),
                        "Hegel mute guard command failed"
                    );
                    None
                }
            },
            None => {
                self.state
                    .upnp()
                    .mark_hegel_mute_guard(zone_id, "mute_state_unknown");
                None
            }
        }
    }

    async fn finish_hegel_dop_mute_guard(&self, zone_id: &str, guard: Option<HegelDopMuteGuard>) {
        let Some(guard) = guard else {
            return;
        };
        if guard.play_id != 0 {
            let media_ready = self
                .state
                .upnp()
                .wait_for_seek_media_evidence(
                    zone_id,
                    guard.play_id,
                    HEGEL_DOP_SEEK_MEDIA_EVIDENCE_TIMEOUT,
                )
                .await;
            if media_ready {
                self.state
                    .upnp()
                    .mark_hegel_mute_guard(zone_id, "media_ready");
            } else {
                self.state
                    .upnp()
                    .mark_hegel_mute_guard(zone_id, "media_evidence_timeout");
            }
            warn!(
                event = "hegel_dop_recovery_skipped_format_unknown",
                zone_id,
                play_id = guard.play_id,
                "Hegel DoP lock recovery skipped because audio-format status is not available"
            );
        }
        if !guard.restore_unmuted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(160)).await;
        match hegel::set_mute(&guard.host, guard.port, false).await {
            Ok(status) => {
                self.state.hegel_status().remember(status);
                self.state.upnp().mark_hegel_mute_guard(zone_id, "restored");
            }
            Err(error) => {
                self.state
                    .upnp()
                    .mark_hegel_mute_guard(zone_id, "restore_failed");
                warn!(
                    event = "external_service_failure",
                    service = "hegel",
                    zone_id,
                    error = %sanitize_error(&error),
                    "Hegel mute guard restore failed"
                );
            }
        }
    }
}
