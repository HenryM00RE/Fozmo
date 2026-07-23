use crate::app::state::AppState;
use crate::diagnostics::logging::{next_operation_id, sanitize_error};
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackIntent, PlaybackOutcome};
use crate::playback::queue_advance::{QueueAdvance, QueueAdvancePolicy};
use crate::playback::request::PlaybackRequest;
use crate::playback::service::remember_active_zone_playback_settings_applied;
use crate::playback::sinks::SinkResolver;
use crate::protocol::SinkProtocol;
use serde_json::{Value, json};
use std::time::Instant;
use tracing::{Instrument, info, info_span, warn};

pub(crate) struct PlaybackDispatcher<'a> {
    state: &'a AppState,
}

impl<'a> PlaybackDispatcher<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(crate) async fn execute(
        &self,
        zone_id: &str,
        intent: PlaybackIntent,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let operation_id = next_operation_id();
        let command = intent_command(&intent);
        let (source_kind, track_id, qobuz_track_id, queue_len) = intent_source_fields(&intent);
        let started = Instant::now();
        info!(
            event = "playback_command_start",
            operation_id,
            command,
            zone_id,
            source_kind,
            track_id = track_id.unwrap_or_default(),
            qobuz_track_id = qobuz_track_id.unwrap_or_default(),
            queue_len,
            "Playback command started"
        );
        let span = info_span!("playback_command", operation_id, command, zone_id);
        let result = async {
            match intent {
                PlaybackIntent::Play { request } => self.dispatch_play(zone_id, request).await,
                PlaybackIntent::Pause => self.pause(zone_id).await,
                PlaybackIntent::Resume => self.resume(zone_id).await,
                PlaybackIntent::Next => self.next(zone_id).await,
                PlaybackIntent::SetVolume { volume } => self.set_volume(zone_id, volume).await,
                PlaybackIntent::SetDeviceVolume { volume } => {
                    self.set_device_volume(zone_id, volume).await
                }
                PlaybackIntent::Seek { seconds } => self.seek(zone_id, seconds).await,
                PlaybackIntent::Stop => self.stop(zone_id).await,
                PlaybackIntent::SetLoopMode { mode } => self.set_loop_mode(zone_id, mode),
            }
        }
        .instrument(span)
        .await;
        log_playback_result(operation_id, command, zone_id, started, &result);
        result
    }

    pub(crate) fn execute_immediate(
        &self,
        zone_id: &str,
        intent: PlaybackIntent,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let operation_id = next_operation_id();
        let command = intent_command(&intent);
        let started = Instant::now();
        info!(
            event = "playback_command_start",
            operation_id, command, zone_id, "Playback command started"
        );
        let result = match intent {
            PlaybackIntent::Seek { .. } => Err(PlaybackError::bad_request(
                "Seek requires asynchronous execution",
            )),
            PlaybackIntent::SetLoopMode { mode } => self.set_loop_mode(zone_id, mode),
            _ => Err(PlaybackError::bad_request(
                "Playback intent requires asynchronous execution",
            )),
        };
        log_playback_result(operation_id, command, zone_id, started, &result);
        result
    }

    async fn dispatch_play(
        &self,
        zone_id: &str,
        request: PlaybackRequest,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let (source_kind, track_id, qobuz_track_id, queue_len) = request.source_fields();
        let radio_auto = request.radio_auto;
        let sink = SinkResolver::new(self.state).resolve(zone_id)?;
        info!(
            event = "zone_route",
            command = "play",
            zone_id,
            sink = sink.as_str(),
            source_kind,
            track_id = track_id.unwrap_or_default(),
            qobuz_track_id = qobuz_track_id.unwrap_or_default(),
            queue_len,
            radio_auto,
            "Resolved playback route"
        );
        sink.play(zone_id, request).await
    }

    async fn next(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        match QueueAdvancePolicy::new(self.state).advance(zone_id).await? {
            QueueAdvance::Completed => Ok(PlaybackOutcome::Completed),
            QueueAdvance::Play { request, outcome } => {
                self.dispatch_play(zone_id, *request).await?;
                Ok(outcome)
            }
            QueueAdvance::AdvanceSink { profile_id } => {
                SinkResolver::new(self.state)
                    .resolve(zone_id)?
                    .next(zone_id, profile_id)
                    .await
            }
        }
    }

    async fn pause(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .pause(zone_id)
            .await?;
        Ok(PlaybackOutcome::Completed)
    }

    async fn resume(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .resume(zone_id)
            .await?;
        Ok(PlaybackOutcome::Completed)
    }

    async fn stop(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .stop(zone_id)
            .await?;
        self.state.listening().stop(self.state.library(), zone_id);
        Ok(PlaybackOutcome::Completed)
    }

    async fn seek(&self, zone_id: &str, seconds: f64) -> Result<PlaybackOutcome, PlaybackError> {
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .seek(zone_id, seconds)
            .await?;
        Ok(PlaybackOutcome::Completed)
    }

    fn set_loop_mode(
        &self,
        zone_id: &str,
        mode: LoopMode,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .set_loop_mode(zone_id, &mode)?;
        persist_zone_loop_mode(self.state, zone_id, mode.as_str())?;
        Ok(PlaybackOutcome::Completed)
    }

    async fn set_volume(
        &self,
        zone_id: &str,
        volume: f32,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let volume = volume.clamp(0.0, 1.5);
        let player = self
            .state
            .zones()
            .player_for_zone(zone_id)
            .unwrap_or_else(|| self.state.zones().active_player());
        player.set_volume(volume);
        let settings_state = self.state.clone();
        let settings_zone_id = zone_id.to_string();
        tokio::task::spawn_blocking(move || {
            settings_state
                .settings()
                .try_update_playback_for_zone_debounced(&settings_zone_id, |settings| {
                    settings.volume = Some(volume);
                })
        })
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("settings persistence task stopped: {error}"))
        })?
        .map_err(PlaybackError::persistence)?;
        remember_active_zone_playback_settings_applied(self.state);
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .set_volume(zone_id, volume)
            .await?;
        Ok(PlaybackOutcome::Completed)
    }

    async fn set_device_volume(
        &self,
        zone_id: &str,
        volume: f32,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        if !volume.is_finite() {
            return Err(PlaybackError::bad_request("Device volume must be finite"));
        }
        SinkResolver::new(self.state)
            .resolve(zone_id)?
            .set_device_volume(zone_id, volume.clamp(0.0, 1.0))
            .await?;
        Ok(PlaybackOutcome::Completed)
    }
}

fn intent_command(intent: &PlaybackIntent) -> &'static str {
    match intent {
        PlaybackIntent::Play { .. } => "play",
        PlaybackIntent::Pause => "pause",
        PlaybackIntent::Resume => "resume",
        PlaybackIntent::Stop => "stop",
        PlaybackIntent::Next => "next",
        PlaybackIntent::Seek { .. } => "seek",
        PlaybackIntent::SetLoopMode { .. } => "loop",
        PlaybackIntent::SetVolume { .. } => "volume",
        PlaybackIntent::SetDeviceVolume { .. } => "device_volume",
    }
}

fn intent_source_fields(
    intent: &PlaybackIntent,
) -> (&'static str, Option<i64>, Option<u64>, usize) {
    match intent {
        PlaybackIntent::Play { request } => request.source_fields(),
        _ => ("none", None, None, 0),
    }
}

fn log_playback_result(
    operation_id: u64,
    command: &str,
    zone_id: &str,
    started: Instant,
    result: &Result<PlaybackOutcome, PlaybackError>,
) {
    let duration_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(outcome) => info!(
            event = "playback_command_finish",
            operation_id,
            command,
            zone_id,
            status = "ok",
            outcome = ?outcome,
            duration_ms,
            "Playback command finished"
        ),
        Err(error) => warn!(
            event = "playback_command_finish",
            operation_id,
            command,
            zone_id,
            status = "error",
            error_kind = error.kind(),
            error = %sanitize_error(error.message()),
            duration_ms,
            "Playback command failed"
        ),
    }
}

fn persist_zone_loop_mode(
    state: &AppState,
    zone_id: &str,
    mode: &str,
) -> Result<(), PlaybackError> {
    if let Some(zone) = state
        .zones()
        .list_zones()
        .into_iter()
        .find(|zone| zone.id == zone_id)
    {
        state
            .library()
            .upsert_zone_definition(
                &zone.id,
                &zone.name,
                zone_definition_kind(&zone.protocol),
                zone.device_name.as_deref(),
                zone.enabled,
            )
            .map_err(PlaybackError::library)?;
    }
    let mut queue_state = match state.library().now_playing_queue(zone_id) {
        Ok(Some(snapshot)) => snapshot.state,
        Ok(None) => json!({
            "kind": null,
            "cursor": -1,
            "items": [],
        }),
        Err(error) => return Err(PlaybackError::library(error)),
    };
    match &mut queue_state {
        Value::Object(object) => {
            object.insert("loopMode".to_string(), Value::String(mode.to_string()));
        }
        _ => {
            queue_state = json!({
                "kind": null,
                "cursor": -1,
                "items": [],
                "loopMode": mode,
            });
        }
    }
    state
        .library()
        .set_now_playing_queue(zone_id, &queue_state)
        .map_err(PlaybackError::library)
}

fn zone_definition_kind(protocol: &SinkProtocol) -> &'static str {
    match protocol {
        SinkProtocol::RemoteAgent => "remote_agent",
        SinkProtocol::AirPlayCoreAudio => "airplay_coreaudio",
        SinkProtocol::AirPlayRaop => "airplay_raop",
        SinkProtocol::AirPlay2 => "airplay2",
        SinkProtocol::SonosUpnp => "sonos_upnp",
        SinkProtocol::UpnpAvRenderer => "upnp_av_renderer",
        SinkProtocol::AsioOutput => "asio",
        SinkProtocol::LocalCoreAudio => "local_coreaudio",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "hegel")]
    use crate::playback::request::PlaybackGuard;
    #[cfg(feature = "hegel")]
    use crate::playback::test_support::{app_state, qobuz_source};
    #[cfg(feature = "hegel")]
    use crate::settings::HegelSettings;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "hegel")]
    #[tokio::test]
    async fn qobuz_play_does_not_reserve_epoch_when_hegel_prep_fails() {
        let state = app_state("hegel-prep-no-reserve");
        let zone_id = state.zones().active_zone_id();
        let _ = state.settings().update(|persisted| {
            persisted.hegel = HegelSettings {
                enabled: true,
                zone_id: Some(zone_id.clone()),
                linked_airplay_zone_id: None,
                host: Some("192.168.255.254".to_string()),
                port: 50001,
                input: 9,
                default_volume: 20,
                max_volume: 50,
                standby_usb_visible: false,
            };
        });
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active local zone should have a player");
        let epoch_before = player.playback_epoch();

        let result = PlaybackDispatcher::new(&state)
            .execute(
                &zone_id,
                PlaybackIntent::Play {
                    request: PlaybackRequest {
                        profile_id: state.settings().active_profile_id(),
                        source: qobuz_source(42, false),
                        queue: Vec::new(),
                        radio_auto: false,
                        guard: PlaybackGuard::none(),
                        qobuz_request: None,
                    },
                },
            )
            .await;

        assert!(matches!(result, Err(PlaybackError::RetryableNetwork(_))));
        assert_eq!(player.playback_epoch(), epoch_before);
    }

    #[test]
    fn playback_failure_log_is_structured_and_sanitized() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer_buffer = Arc::clone(&buffer);
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .without_time()
            .with_writer(move || CaptureWriter(Arc::clone(&writer_buffer)))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let result = Err(PlaybackError::not_found(
                "/Users/fixture/Music/private.flac",
            ));
            log_playback_result(77, "play", "local-core", Instant::now(), &result);
        });

        let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(output.contains("\"event\":\"playback_command_finish\""));
        assert!(output.contains("\"operation_id\":77"));
        assert!(output.contains("\"command\":\"play\""));
        assert!(output.contains("\"status\":\"error\""));
        assert!(output.contains("\"error_kind\":\"not_found\""));
        assert!(!output.contains("/Users/fixture"));
        assert!(!output.contains("private.flac"));
    }
}
