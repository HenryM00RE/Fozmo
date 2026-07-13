use crate::app::state::AppState;
use crate::audio::device_volume;
use crate::audio::player::{TrackCover, TrackTags};
use crate::diagnostics::logging::{next_operation_id, sanitize_error};
use crate::playback::artist_radio::local_artist_radio_next_source_from_source_for_zone;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackGuard, PlaybackIntent, PlaybackOutcome};
use crate::playback::lastfm::{lastfm_radio_has_future_queue, lastfm_radio_next_source_for_zone};
use crate::playback::now_playing::current_playback_matches_expected;
use crate::playback::qobuz::qobuz_radio_next_request_for_zone;
use crate::playback::queue::append_source_to_now_playing_queue;
use crate::playback::resolver::{
    local_player_queue_items_from_sources, lookup_fallback_cover, resolve_track_path,
};
use crate::playback::service::{
    apply_playback_settings_for_zone, hegel_settings_for_zone, playback_config_for_zone,
    prepare_airplay_volume_for_zone, prepare_hegel_for_zone,
    remember_active_zone_playback_settings_applied,
};
use crate::playback::sonos::{play_sonos_source_for_zone, sonos_target_for_zone};
use crate::playback::source::qobuz_play_request_from_source_ref;
use crate::playback::upnp::{
    play_upnp_source_for_zone, seek_upnp_with_dsp_fallback, upnp_target_for_zone,
};
use crate::protocol::{CoreToAgentCommand, SinkProtocol, SourceRef};
use crate::services::hegel;
use crate::services::qobuz::QobuzPlayRequest;
use serde_json::{Value, json};
use std::future::Future;
use std::time::{Duration, Instant};
use symphonia::core::io::MediaSource;
use tracing::{Instrument, debug, info, info_span, warn};

pub(crate) struct PlaybackRouter<'a> {
    state: &'a AppState,
}

enum ZoneSink {
    Local,
    RemoteAgent,
    Sonos,
    Upnp,
}

struct HegelDopMuteGuard {
    host: String,
    port: u16,
    play_id: u64,
    restore_unmuted: bool,
}

struct SelectedQobuzStream {
    source: Box<dyn MediaSource>,
    ext: String,
    display_name: String,
}

const HEGEL_DOP_SEEK_MEDIA_EVIDENCE_TIMEOUT: Duration = Duration::from_secs(4);

impl ZoneSink {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::RemoteAgent => "remote_agent",
            Self::Sonos => "sonos",
            Self::Upnp => "upnp",
        }
    }
}

impl<'a> PlaybackRouter<'a> {
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
                PlaybackIntent::Play {
                    profile_id,
                    source,
                    queue,
                    radio_auto,
                    guard,
                    qobuz_request,
                } => {
                    self.play_source(
                        zone_id,
                        profile_id,
                        source,
                        queue,
                        radio_auto,
                        guard,
                        qobuz_request,
                    )
                    .await
                }
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

    #[allow(clippy::too_many_arguments)]
    async fn play_source(
        &self,
        zone_id: &str,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio_auto: bool,
        guard: PlaybackGuard,
        qobuz_request: Option<Box<QobuzPlayRequest>>,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let source_kind = source.kind();
        let track_id = source.local_track_id();
        let qobuz_track_id = source.qobuz_track_id();
        let sink = self.sink_for_zone(zone_id)?;
        info!(
            event = "zone_route",
            command = "play",
            zone_id,
            sink = sink.as_str(),
            source_kind,
            track_id = track_id.unwrap_or_default(),
            qobuz_track_id = qobuz_track_id.unwrap_or_default(),
            queue_len = queue.len(),
            radio_auto,
            "Resolved playback route"
        );
        match sink {
            ZoneSink::RemoteAgent => {
                self.play_remote_agent(zone_id, profile_id, source, queue, radio_auto, guard)
            }
            ZoneSink::Sonos => {
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
            ZoneSink::Upnp => {
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
            ZoneSink::Local => match source {
                SourceRef::LocalTrack { .. } => {
                    self.play_local_track(zone_id, profile_id, source, queue, radio_auto, guard)
                        .await
                }
                SourceRef::QobuzTrack { .. } => {
                    self.play_qobuz_stream(
                        zone_id,
                        profile_id,
                        source,
                        queue,
                        radio_auto,
                        guard,
                        qobuz_request,
                    )
                    .await
                }
            },
        }
    }

    async fn next(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        let profile_id = self
            .state
            .listening()
            .profile_id(zone_id)
            .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
        let active_source = self.state.listening().active_source(zone_id);
        let active_source_key = active_source.as_ref().map(SourceRef::key);
        let queue_empty = active_source
            .as_ref()
            .map(|source| !lastfm_radio_has_future_queue(self.state, zone_id, source))
            .unwrap_or_else(|| {
                self.state
                    .library()
                    .zone_queue(zone_id)
                    .map(|queue| queue.is_empty())
                    .unwrap_or(false)
            });
        if queue_empty && self.state.settings().lastfm_radio_enabled() {
            match lastfm_radio_next_source_for_zone((*self.state).clone(), zone_id).await {
                Ok(Some(source)) => {
                    let current_active_source = self.state.listening().active_source(zone_id);
                    if current_active_source.as_ref().map(SourceRef::key) != active_source_key {
                        return Ok(PlaybackOutcome::Completed);
                    }
                    let still_queue_empty = current_active_source
                        .as_ref()
                        .map(|source| !lastfm_radio_has_future_queue(self.state, zone_id, source))
                        .unwrap_or_else(|| {
                            self.state
                                .library()
                                .zone_queue(zone_id)
                                .map(|queue| queue.is_empty())
                                .unwrap_or(false)
                        });
                    if still_queue_empty {
                        if let Err(e) =
                            append_source_to_now_playing_queue(self.state, zone_id, &source)
                        {
                            warn!(
                                event = "playback_queue_persist_failed",
                                service = "lastfm",
                                zone_id,
                                error_kind = "library",
                                error = %sanitize_error(&e),
                                "Failed to append Last.fm radio source"
                            );
                        }
                        let radio_auto = source.is_radio();
                        self.play_source(
                            zone_id,
                            profile_id.clone(),
                            source,
                            Vec::new(),
                            radio_auto,
                            PlaybackGuard::none(),
                            None,
                        )
                        .await?;
                        return Ok(PlaybackOutcome::Completed);
                    }
                }
                Ok(None) => {
                    let current_active_source = self.state.listening().active_source(zone_id);
                    if current_active_source.as_ref().map(SourceRef::key) != active_source_key {
                        return Ok(PlaybackOutcome::Completed);
                    }
                    let still_queue_empty = current_active_source
                        .as_ref()
                        .map(|source| !lastfm_radio_has_future_queue(self.state, zone_id, source))
                        .unwrap_or_else(|| {
                            self.state
                                .library()
                                .zone_queue(zone_id)
                                .map(|queue| queue.is_empty())
                                .unwrap_or(false)
                        });
                    if still_queue_empty {
                        warn!(
                            event = "external_service_failure",
                            service = "lastfm",
                            error_kind = "not_found",
                            zone_id,
                            "Last.fm returned no playable recommendation; falling back"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        event = "external_service_failure",
                        service = "lastfm",
                        error_kind = "error",
                        zone_id,
                        error = %sanitize_error(&e),
                        "Last.fm radio failed; falling back"
                    );
                }
            }
        }

        if queue_empty && let Some(active_source) = active_source.as_ref() {
            match local_artist_radio_next_source_from_source_for_zone(
                self.state,
                zone_id,
                active_source,
            ) {
                Ok(Some(source)) => {
                    let current_active_source = self.state.listening().active_source(zone_id);
                    if current_active_source.as_ref().map(SourceRef::key) != active_source_key {
                        return Ok(PlaybackOutcome::Completed);
                    }
                    if let Err(e) = append_source_to_now_playing_queue(self.state, zone_id, &source)
                    {
                        warn!(
                            event = "playback_queue_persist_failed",
                            service = "artist_radio",
                            zone_id,
                            error_kind = "library",
                            error = %sanitize_error(&e),
                            "Failed to append local radio source"
                        );
                    }
                    let radio_auto = source.is_radio();
                    self.play_source(
                        zone_id,
                        profile_id.clone(),
                        source,
                        Vec::new(),
                        radio_auto,
                        PlaybackGuard::none(),
                        None,
                    )
                    .await?;
                    return Ok(PlaybackOutcome::Completed);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        event = "external_service_failure",
                        service = "artist_radio",
                        error_kind = "error",
                        zone_id,
                        error = %sanitize_error(&e),
                        "Local artist radio fallback failed"
                    );
                }
            }
        }

        match qobuz_radio_next_request_for_zone((*self.state).clone(), zone_id).await {
            Ok(Some(req)) => {
                let queue = crate::playback::source::qobuz_queue_source_refs(&req);
                let source = crate::playback::source::qobuz_source_ref_from_play_request(&req);
                self.play_source(
                    zone_id,
                    profile_id.clone(),
                    source,
                    queue,
                    req.radio_auto,
                    PlaybackGuard::none(),
                    Some(Box::new(req)),
                )
                .await?;
                return Ok(PlaybackOutcome::QobuzRadioAdvanced);
            }
            Ok(None) => {}
            Err(e) => return Err(PlaybackError::integration(e)),
        }

        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => {
                self.state
                    .zones()
                    .send_to_zone(zone_id, CoreToAgentCommand::Next)
                    .map_err(PlaybackError::integration)?;
                self.state.listening().next(self.state.library(), zone_id);
            }
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .next(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration)?;
                self.state.listening().next(self.state.library(), zone_id);
            }
            ZoneSink::Upnp => {
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
                    self.state.listening().set_queue(
                        zone_id,
                        profile_id.clone(),
                        queued_sources.clone(),
                    );
                    return self
                        .play_source(
                            zone_id,
                            profile_id.clone(),
                            source,
                            queued_sources,
                            radio_auto,
                            PlaybackGuard::none(),
                            None,
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
            }
            ZoneSink::Local => {
                let mut queued_sources = self.state.listening().queued_sources(zone_id);
                if matches!(queued_sources.first(), Some(SourceRef::QobuzTrack { .. })) {
                    let source = queued_sources.remove(0);
                    return self
                        .play_source(
                            zone_id,
                            profile_id.clone(),
                            source,
                            queued_sources,
                            false,
                            PlaybackGuard::none(),
                            None,
                        )
                        .await;
                }

                let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                    return Err(PlaybackError::ZoneNotAvailable);
                };
                player.next();
                self.state.listening().next(self.state.library(), zone_id);
            }
        }
        Ok(PlaybackOutcome::Completed)
    }

    async fn pause(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => self
                .state
                .zones()
                .send_to_zone(zone_id, CoreToAgentCommand::Pause)
                .map_err(PlaybackError::integration)?,
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .pause(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Upnp => {
                let target = upnp_target_for_zone(self.state, zone_id)?;
                let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
                let result = self
                    .state
                    .upnp()
                    .pause(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration);
                self.finish_hegel_dop_mute_guard(zone_id, guard).await;
                result?;
            }
            ZoneSink::Local => {
                let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                    return Err(PlaybackError::ZoneNotAvailable);
                };
                player.pause();
            }
        }
        Ok(PlaybackOutcome::Completed)
    }

    async fn resume(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => self
                .state
                .zones()
                .send_to_zone(zone_id, CoreToAgentCommand::Resume)
                .map_err(PlaybackError::integration)?,
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .resume(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Upnp => {
                let target = upnp_target_for_zone(self.state, zone_id)?;
                let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
                let result = self
                    .state
                    .upnp()
                    .resume(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration);
                self.finish_hegel_dop_mute_guard(zone_id, guard).await;
                result?;
            }
            ZoneSink::Local => {
                let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                    return Err(PlaybackError::ZoneNotAvailable);
                };
                prepare_hegel_for_zone(self.state, zone_id).await?;
                player.resume();
            }
        }
        Ok(PlaybackOutcome::Completed)
    }

    async fn stop(&self, zone_id: &str) -> Result<PlaybackOutcome, PlaybackError> {
        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => self
                .state
                .zones()
                .send_to_zone(zone_id, CoreToAgentCommand::Stop)
                .map_err(PlaybackError::integration)?,
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .stop(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Upnp => {
                let target = upnp_target_for_zone(self.state, zone_id)?;
                let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
                let result = self
                    .state
                    .upnp()
                    .stop(zone_id, &target)
                    .await
                    .map_err(PlaybackError::integration);
                self.finish_hegel_dop_mute_guard(zone_id, guard).await;
                result?;
            }
            ZoneSink::Local => {
                let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                    return Err(PlaybackError::ZoneNotAvailable);
                };
                player.stop();
            }
        }
        self.state.listening().stop(self.state.library(), zone_id);
        Ok(PlaybackOutcome::Completed)
    }

    async fn seek(&self, zone_id: &str, seconds: f64) -> Result<PlaybackOutcome, PlaybackError> {
        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => self
                .state
                .zones()
                .send_to_zone(zone_id, CoreToAgentCommand::Seek { seconds })
                .map_err(PlaybackError::integration)?,
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .seek(zone_id, &target, seconds)
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Upnp => {
                let target = upnp_target_for_zone(self.state, zone_id)?;
                let guard = self.begin_hegel_dop_mute_guard(zone_id, &target).await;
                if guard.is_some() {
                    self.state
                        .upnp()
                        .mark_dop_seek_strategy(zone_id, "hegel_mute_guarded_upnp_seek");
                }
                let result =
                    seek_upnp_with_dsp_fallback(self.state, zone_id, &target, seconds).await;
                self.finish_hegel_dop_mute_guard(zone_id, guard).await;
                result?;
            }
            ZoneSink::Local => {
                let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                    return Err(PlaybackError::ZoneNotAvailable);
                };
                player.seek(seconds);
            }
        }
        Ok(PlaybackOutcome::Completed)
    }

    fn set_loop_mode(
        &self,
        zone_id: &str,
        mode: LoopMode,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => {
                self.state
                    .zones()
                    .send_to_zone(
                        zone_id,
                        CoreToAgentCommand::SetLoopMode {
                            repeat_one: mode.repeat_one(),
                        },
                    )
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Sonos => {
                if let Some(player) = self.state.zones().player_for_zone(zone_id) {
                    player.set_repeat_one(mode.repeat_one());
                }
            }
            ZoneSink::Upnp => {}
            ZoneSink::Local => {
                let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                    return Err(PlaybackError::ZoneNotAvailable);
                };
                player.set_repeat_one(mode.repeat_one());
            }
        }
        persist_zone_loop_mode(self.state, zone_id, mode.as_str())?;
        Ok(PlaybackOutcome::Completed)
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
            tracing::warn!(
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

        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => {
                self.state
                    .zones()
                    .send_to_zone(
                        zone_id,
                        CoreToAgentCommand::SetPlaybackConfig {
                            playback_config: playback_config_for_zone(self.state, zone_id, &player),
                        },
                    )
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .set_volume(zone_id, &target, volume.min(1.0))
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Upnp => {
                let target = upnp_target_for_zone(self.state, zone_id)?;
                self.state
                    .upnp()
                    .set_volume(zone_id, &target, volume.min(1.0))
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Local => {}
        }
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
        let volume = volume.clamp(0.0, 1.0);
        match self.sink_for_zone(zone_id)? {
            ZoneSink::RemoteAgent => {
                return Err(PlaybackError::bad_request(
                    "Device volume for remote agents is not available from this control",
                ));
            }
            ZoneSink::Sonos => {
                let target = sonos_target_for_zone(self.state, zone_id)?;
                self.state
                    .sonos()
                    .set_volume(zone_id, &target, volume)
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Upnp => {
                let target = upnp_target_for_zone(self.state, zone_id)?;
                self.state
                    .upnp()
                    .set_volume(zone_id, &target, volume)
                    .await
                    .map_err(PlaybackError::integration)?;
            }
            ZoneSink::Local => {
                if let Some(hegel_settings) = hegel_settings_for_zone(self.state, zone_id) {
                    let hegel_volume = normalized_hegel_volume(&hegel_settings, volume);
                    let status = hegel::set_volume(
                        hegel_settings.host.as_deref().unwrap_or_default(),
                        hegel_settings.port,
                        hegel_volume,
                    )
                    .await
                    .map_err(PlaybackError::integration)?;
                    self.state.hegel_status().remember(status);
                } else if matches!(
                    self.state.zones().zone_protocol(zone_id),
                    Some(SinkProtocol::AirPlayRaop | SinkProtocol::AirPlay2)
                ) {
                    let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                        return Err(PlaybackError::ZoneNotAvailable);
                    };
                    player.set_airplay_device_volume(volume);
                    let _ = self
                        .state
                        .library()
                        .remember_zone_airplay_volume(zone_id, volume);
                } else {
                    let Some(player) = self.state.zones().player_for_zone(zone_id) else {
                        return Err(PlaybackError::ZoneNotAvailable);
                    };
                    let device_name = player.selected_device_name();
                    device_volume::set_output_device_volume(device_name.as_deref(), volume)
                        .map_err(PlaybackError::bad_request)?;
                }
            }
        }
        Ok(PlaybackOutcome::Completed)
    }

    fn play_remote_agent(
        &self,
        zone_id: &str,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio_auto: bool,
        guard: PlaybackGuard,
    ) -> Result<PlaybackOutcome, PlaybackError> {
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
        if let Err(e) = self.state.library().set_zone_queue(zone_id, &queue) {
            warn!(
                event = "playback_queue_persist_failed",
                zone_id,
                error_kind = "library",
                error = %sanitize_error(&e),
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

    async fn play_local_track(
        &self,
        zone_id: &str,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio_auto: bool,
        guard: PlaybackGuard,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let radio_auto = radio_auto || source.is_radio();
        let SourceRef::LocalTrack {
            track_id,
            file_name,
            ..
        } = &source
        else {
            return Err(PlaybackError::bad_request("Expected local source"));
        };
        let full_path = resolve_track_path(
            self.state,
            (*track_id > 0).then_some(*track_id),
            file_name.as_deref(),
        )?
        .ok_or_else(|| PlaybackError::bad_request("Missing file_name or track_id"))?;
        let path_str = full_path.to_string_lossy().to_string();
        let fallback_cover = lookup_fallback_cover(self.state, &path_str);
        let queue_items = local_player_queue_items_from_sources(self.state, &queue);
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        apply_playback_settings_for_zone(self.state, zone_id);
        prepare_airplay_volume_for_zone(self.state, zone_id, &player);
        prepare_hegel_for_zone(self.state, zone_id).await?;
        if !guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        let play_epoch = player.reserve_playback_change();
        if !player.play_if_epoch(play_epoch, path_str, fallback_cover, None, queue_items) {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        if let Err(e) = self.state.library().set_zone_queue(zone_id, &queue) {
            warn!(
                event = "playback_queue_persist_failed",
                zone_id,
                error_kind = "library",
                error = %sanitize_error(&e),
                "Failed to persist zone queue"
            );
        }
        if radio_auto {
            self.state.listening().start_with_radio(
                self.state.library(),
                zone_id.to_string(),
                self.state.zones().zone_name(zone_id),
                profile_id,
                source,
                queue,
                true,
            );
        } else {
            self.state.listening().start(
                self.state.library(),
                zone_id.to_string(),
                self.state.zones().zone_name(zone_id),
                profile_id,
                source,
                queue,
            );
        }
        Ok(PlaybackOutcome::Completed)
    }

    #[allow(clippy::too_many_arguments)]
    async fn play_qobuz_stream(
        &self,
        zone_id: &str,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio_auto: bool,
        guard: PlaybackGuard,
        qobuz_request: Option<Box<QobuzPlayRequest>>,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        let req = qobuz_request
            .map(|req| *req)
            .or_else(|| qobuz_play_request_from_source_ref(&source, &queue, radio_auto))
            .ok_or_else(|| PlaybackError::bad_request("Qobuz source was not playable"))?;
        let Some(player) = self.state.zones().player_for_zone(zone_id) else {
            return Err(PlaybackError::ZoneNotAvailable);
        };
        apply_playback_settings_for_zone(self.state, zone_id);
        prepare_airplay_volume_for_zone(self.state, zone_id, &player);
        prepare_hegel_for_zone(self.state, zone_id).await?;
        if !current_playback_matches_expected(self.state, zone_id, &req.expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        if !guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }

        let cover_fut = async {
            let url = req.image_url.as_deref()?;
            let (mime, data) = self.state.qobuz().fetch_cover_public(url).await.ok()?;
            match (mime, data) {
                (Some(mime), Some(data)) => Some(TrackCover { mime, data }),
                _ => None,
            }
        };
        let stream_fut = async {
            self.state
                .qobuz()
                .open_stream(&req)
                .await
                .map(|handle| SelectedQobuzStream {
                    source: Box::new(handle.source),
                    ext: handle.ext,
                    display_name: handle.display_name,
                })
        };

        self.complete_selected_qobuz_start(
            zone_id, profile_id, source, queue, radio_auto, &guard, &req, &player, stream_fut,
            cover_fut,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn complete_selected_qobuz_start<StreamFuture, CoverFuture>(
        &self,
        zone_id: &str,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio_auto: bool,
        guard: &PlaybackGuard,
        req: &QobuzPlayRequest,
        player: &std::sync::Arc<crate::audio::player::Player>,
        stream_fut: StreamFuture,
        cover_fut: CoverFuture,
    ) -> Result<PlaybackOutcome, PlaybackError>
    where
        StreamFuture: Future<Output = Result<SelectedQobuzStream, String>>,
        CoverFuture: Future<Output = Option<TrackCover>>,
    {
        // Only assets for the selected track belong on the initial-playback
        // critical path. The listening monitor observes the committed source
        // and fills the empty stream queue through the epoch-guarded Qobuz
        // prefetch path. A slow or failing next track must never delay this
        // handoff.
        let (stream_result, fallback_cover) = tokio::join!(stream_fut, cover_fut);
        let handle = stream_result.map_err(PlaybackError::retryable_network)?;
        if !current_playback_matches_expected(self.state, zone_id, &req.expected_current) {
            return Err(PlaybackError::conflict("Current track changed"));
        }
        if !guard.is_current(self.state) {
            return Err(PlaybackError::conflict("Playback changed"));
        }

        let fallback_tags = TrackTags {
            title: req.title.clone(),
            artist: req.artist.clone(),
            album: req.album.clone(),
            album_artist: req.artist.clone(),
            duration_secs: req.duration_secs,
            ..TrackTags::default()
        };

        let play_epoch = player.reserve_playback_change();
        let started = player.play_stream_if_epoch(
            play_epoch,
            handle.source,
            Some(handle.ext),
            handle.display_name,
            fallback_cover,
            Some(fallback_tags),
            Vec::new(),
        );
        if !started {
            return Err(PlaybackError::conflict("Playback changed"));
        }

        if let Err(e) = self.state.library().set_zone_queue(zone_id, &queue) {
            warn!(
                event = "playback_queue_persist_failed",
                zone_id,
                error_kind = "library",
                error = %sanitize_error(&e),
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

    fn sink_for_zone(&self, zone_id: &str) -> Result<ZoneSink, PlaybackError> {
        let sink = match self.state.zones().zone_protocol(zone_id) {
            Some(SinkProtocol::RemoteAgent) => Ok(ZoneSink::RemoteAgent),
            Some(SinkProtocol::SonosUpnp) if cfg!(feature = "sonos") => Ok(ZoneSink::Sonos),
            Some(SinkProtocol::SonosUpnp) => Err(PlaybackError::bad_request(
                "Sonos support was not compiled into this build",
            )),
            Some(SinkProtocol::UpnpAvRenderer) if cfg!(feature = "upnp") => Ok(ZoneSink::Upnp),
            Some(SinkProtocol::UpnpAvRenderer) => Err(PlaybackError::bad_request(
                "UPnP support was not compiled into this build",
            )),
            Some(_) => Ok(ZoneSink::Local),
            None => Err(PlaybackError::ZoneNotAvailable),
        }?;
        debug!(
            event = "zone_route",
            zone_id,
            sink = sink.as_str(),
            "Resolved zone sink"
        );
        Ok(sink)
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
        PlaybackIntent::Play { source, queue, .. } => (
            source.kind(),
            source.local_track_id(),
            source.qobuz_track_id(),
            queue.len(),
        ),
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
        Err(e) => return Err(PlaybackError::library(e)),
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

fn normalized_hegel_volume(settings: &crate::settings::HegelSettings, volume: f32) -> u8 {
    let requested = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    requested.min(settings.max_volume)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::qobuz::prefetch_qobuz_queue_track_into_player;
    use crate::playback::test_support::{app_state, qobuz_source};
    use crate::services::qobuz::QobuzQueueTrack;
    #[cfg(feature = "hegel")]
    use crate::settings::HegelSettings;
    use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
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

    struct TestMediaSource {
        cursor: Cursor<Vec<u8>>,
    }

    impl TestMediaSource {
        fn empty() -> Self {
            Self {
                cursor: Cursor::new(Vec::new()),
            }
        }
    }

    impl Read for TestMediaSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.cursor.read(buf)
        }
    }

    impl Seek for TestMediaSource {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(pos)
        }
    }

    impl MediaSource for TestMediaSource {
        fn is_seekable(&self) -> bool {
            true
        }

        fn byte_len(&self) -> Option<u64> {
            Some(self.cursor.get_ref().len() as u64)
        }
    }

    fn qobuz_request(track_id: u64) -> QobuzPlayRequest {
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

    fn qobuz_queue_track(track_id: u64) -> QobuzQueueTrack {
        QobuzQueueTrack {
            track_id,
            title: Some(format!("Track {track_id}")),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_id: Some("album".to_string()),
            image_url: None,
            duration_secs: Some(180.0),
            format_id: None,
            radio: false,
            playlist_context: None,
        }
    }

    #[tokio::test]
    async fn qobuz_selected_track_starts_while_next_prefetch_is_blocked_and_failing() {
        let state = app_state("qobuz-selected-before-next-prefetch");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let source = qobuz_source(41, false);
        let next = qobuz_source(42, false);
        let queue = vec![next.clone()];
        let req = qobuz_request(41);
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active test zone should have a player");
        apply_playback_settings_for_zone(&state, &zone_id);
        let config_before = player.snapshot().config;
        let epoch_before = player.playback_epoch();

        let (next_started_tx, next_started_rx) = tokio::sync::oneshot::channel();
        let (release_next_tx, release_next_rx) = tokio::sync::oneshot::channel();
        let (next_handle_tx, next_handle_rx) = tokio::sync::oneshot::channel();
        let selected_stream_fut = async move {
            let next_handle = tokio::spawn(async move {
                let _ = next_started_tx.send(());
                let _ = release_next_rx.await;
                Err::<(), &'static str>("simulated next-track failure")
            });
            let _ = next_handle_tx.send(next_handle);
            Ok(SelectedQobuzStream {
                source: Box::new(TestMediaSource::empty()),
                ext: "flac".to_string(),
                display_name: "Artist - Track 41".to_string(),
            })
        };
        let guard = PlaybackGuard::none();

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            PlaybackRouter::new(&state).complete_selected_qobuz_start(
                &zone_id,
                state.settings().active_profile_id(),
                source.clone(),
                queue.clone(),
                false,
                &guard,
                &req,
                &player,
                selected_stream_fut,
                std::future::ready(None),
            ),
        )
        .await
        .expect("selected-track handoff must not wait for next-track prefetch")
        .expect("selected track should start");

        assert!(matches!(outcome, PlaybackOutcome::Completed));
        assert!(player.playback_epoch() > epoch_before);
        assert_eq!(player.stream_queue_len(), 0);
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|active| active.key()),
            Some(source.key())
        );
        assert_eq!(
            state
                .listening()
                .queued_sources(&zone_id)
                .into_iter()
                .map(|queued| queued.key())
                .collect::<Vec<_>>(),
            vec![next.key()]
        );
        assert_eq!(
            state
                .library()
                .zone_queue(&zone_id)
                .unwrap()
                .into_iter()
                .map(|entry| entry.source.key())
                .collect::<Vec<_>>(),
            vec!["qobuz:42".to_string()]
        );
        let config_after = player.snapshot().config;
        assert_eq!(
            config_after.upsampling_enabled,
            config_before.upsampling_enabled
        );
        assert_eq!(
            config_after.configured_target_rate,
            config_before.configured_target_rate
        );

        let next_handle = next_handle_rx.await.unwrap();
        next_started_rx.await.unwrap();
        assert!(!next_handle.is_finished());
        release_next_tx.send(()).unwrap();
        assert_eq!(
            next_handle.await.unwrap(),
            Err("simulated next-track failure")
        );
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|active| active.key()),
            Some(source.key())
        );
    }

    #[tokio::test]
    async fn stale_qobuz_next_prefetch_epoch_cannot_replace_selected_track() {
        let state = app_state("qobuz-stale-next-prefetch-epoch");
        let zone_id = state.zones().active_zone_id();
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();
        let source = qobuz_source(51, false);
        let next = qobuz_source(52, false);
        state.listening().start(
            state.library(),
            zone_id.clone(),
            state.zones().zone_name(&zone_id),
            state.settings().active_profile_id(),
            source.clone(),
            vec![next.clone()],
        );
        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&next))
            .unwrap();
        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("active test zone should have a player");
        let stale_epoch = player.playback_epoch();
        player.reserve_playback_change();

        let result = prefetch_qobuz_queue_track_into_player(
            state.clone(),
            zone_id.clone(),
            qobuz_queue_track(52),
            source.key(),
            stale_epoch,
            false,
        )
        .await;

        assert_eq!(result.unwrap_err(), "Playback changed");
        assert_eq!(player.stream_queue_len(), 0);
        assert_eq!(
            state
                .listening()
                .active_source(&zone_id)
                .map(|active| active.key()),
            Some(source.key())
        );
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

        let result = PlaybackRouter::new(&state)
            .execute(
                &zone_id,
                PlaybackIntent::Play {
                    profile_id: state.settings().active_profile_id(),
                    source: qobuz_source(42, false),
                    queue: Vec::new(),
                    radio_auto: false,
                    guard: PlaybackGuard::none(),
                    qobuz_request: None,
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
