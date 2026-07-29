//! Product Apple Music audio path.
//!
//! Music.app decodes the catalog track into the Fozmo Capture HAL device. This
//! service owns that capture session and feeds its PCM into the normal local
//! Player/DSP/output path.

mod capture_session;
mod coreaudio;
mod live_source;
mod rate_control;

use crate::audio::player::Player;
use crate::protocol::SourceRef;
use crate::settings::AppleMusicPlaybackSettings;
use capture_session::LiveSessionParams;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CAPTURE_DEVICE_NAME: &str = "Fozmo Capture";
pub(crate) const APPLE_MUSIC_LIVE_DISPLAY_NAME: &str = capture_session::LIVE_DISPLAY_NAME;
const CAPTURE_DEVICE_UID: &str = "com.fozmo.audio.capture";
// CoreAudio can block the AudioWorker for several seconds while a high-rate
// DoP device changes physical format and opens. Keep Music.app's live PCM
// queued across that window instead of overflowing the capture ring.
const PLAYBACK_CAPTURE_BUFFER_MS: u32 = 20_000;
const MAX_PLAYBACK_CAPTURE_BUFFER_MS: u32 = 30_000;
const LOCAL_DEVICE_REFRESH_SETTLE_MS: u64 = 15_000;

/// Where a catalog track's captured PCM is going.
///
/// There is only ever one of these active at a time. Music.app has a single
/// output, and Fozmo owns it by rerouting the Mac's default output into Fozmo
/// Capture, so the choice is which single zone hears it — not how many.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum AppleMusicDelivery {
    /// Capture feeds the local Player, its DSP, and a local physical output.
    LocalPlayer,
    /// Capture is relayed over HTTP to a remote agent or browser zone, which
    /// applies its own DSP and renders on its own hardware.
    Relay,
}

/// Fozmo-owned identity and timeline for a catalog track playing through the
/// native Music.app -> Fozmo Capture -> Player-or-relay path.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppleMusicPlaybackSnapshot {
    pub(crate) zone_id: String,
    pub(crate) delivery: AppleMusicDelivery,
    /// Player epoch that owns this session on the local path. A relayed
    /// session has no local Player, and leaves this zero.
    pub(crate) player_epoch: u64,
    pub(crate) generation: u64,
    pub(crate) source: SourceRef,
    pub(crate) playback_state: String,
    /// Source position represented by Player position zero. This is non-zero
    /// after a seek because reopening the live capture resets Player metrics.
    pub(crate) timeline_origin_secs: f64,
    /// Player timeline position at which the current Apple Music track begins.
    /// This advances across native Music.app album transitions while the live
    /// capture and physical output remain open.
    pub(crate) player_position_origin_secs: f64,
    /// Listener-facing position captured when Fozmo pauses the local Player.
    ///
    /// Music.app is deliberately ahead of this point because its decoded PCM
    /// is buffered through Fozmo. Keeping the audible position separately
    /// prevents a paused status from falling back to 0:00 when Player metrics
    /// are transiently reset while the output settles.
    pub(crate) paused_position_secs: Option<f64>,
    pub(crate) position_secs: f64,
    pub(crate) duration_secs: f64,
}

impl AppleMusicPlaybackSnapshot {
    pub(crate) fn audible_position_secs(&self, player_position_secs: f64) -> f64 {
        let position = self.timeline_origin_secs
            + (player_position_secs - self.player_position_origin_secs).max(0.0);
        if self.duration_secs > 0.0 {
            position.min(self.duration_secs)
        } else {
            position
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedAppleMusicControl {
    pub(crate) source_key: String,
    pub(crate) rate_hz: u32,
    pub(crate) source_bit_depth: Option<u32>,
    pub(crate) duration_secs: Option<f64>,
}

struct CaptureRuntime {
    running: bool,
    player: Option<Arc<Player>>,
    stopped_unix_ms: Option<u64>,
    session: Option<capture_session::LiveSession>,
    session_params: Option<LiveSessionParams>,
    saved_default_output_uid: Option<String>,
    /// A finished track deliberately left macOS routed to Fozmo Capture for an
    /// Apple Music successor that has not opened its session yet. Every path
    /// that does not reach `start_playback_capture` must release it.
    route_retained: bool,
    playback: Option<AppleMusicPlaybackSnapshot>,
    next_playback_generation: u64,
    next_prefetch_revision: u64,
    prepared_control: Option<PreparedAppleMusicControl>,
    prepared_session: Option<capture_session::PreparedLiveSession>,
    /// Set instead of `session` when the zone that asked for Apple Music
    /// renders somewhere other than this Mac.
    relay: Option<capture_session::RelaySession>,
    /// The zone a retained capture route belongs to, so the gap between two of
    /// its tracks still reads as that zone owning the stream.
    route_retained_zone: Option<String>,
}

impl Default for CaptureRuntime {
    fn default() -> Self {
        Self {
            running: false,
            player: None,
            stopped_unix_ms: None,
            session: None,
            session_params: None,
            saved_default_output_uid: None,
            route_retained: false,
            playback: None,
            next_playback_generation: 1,
            next_prefetch_revision: 1,
            prepared_control: None,
            prepared_session: None,
            relay: None,
            route_retained_zone: None,
        }
    }
}

/// The live Apple Music stream, as bytes an HTTP response can write.
///
/// Reads block until Music.app produces more PCM, and return zero only once
/// the session is torn down, so this must be drained on a blocking thread.
pub struct AppleMusicRelayStream {
    rate_hz: u32,
    bits: u32,
    reader: capture_session::RelayReader,
}

impl AppleMusicRelayStream {
    pub fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    pub fn bits(&self) -> u32 {
        self.bits
    }
}

impl std::io::Read for AppleMusicRelayStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.reader, buf)
    }
}

pub struct AppleMusicPlaybackService {
    player: Arc<Player>,
    runtime: Mutex<CaptureRuntime>,
}

impl AppleMusicPlaybackService {
    pub fn new(player: Arc<Player>) -> Self {
        Self {
            player,
            runtime: Mutex::new(CaptureRuntime::default()),
        }
    }

    /// Recover from an interrupted process that left macOS routed to the
    /// virtual capture device.
    #[cfg(target_os = "macos")]
    pub(crate) fn restore_configured_output_if_idle(
        &self,
        settings: &AppleMusicPlaybackSettings,
    ) -> Result<bool, String> {
        if self.runtime.lock().unwrap().running
            || coreaudio::default_output_device_uid().as_deref() != Some(CAPTURE_DEVICE_UID)
        {
            return Ok(false);
        }
        let output_device_name = normalize_optional(settings.output_device_name.as_deref())
            .ok_or_else(|| {
                "macOS is still routed to Fozmo Capture, but no physical Apple Music output is configured."
                    .to_string()
            })?;
        let device_id = coreaudio::local_physical_device_id_for_name(&output_device_name)
            .ok_or_else(|| {
                format!(
                    "macOS is still routed to Fozmo Capture, and configured output {output_device_name} is not visible to CoreAudio."
                )
            })?;
        coreaudio::set_default_output_device(device_id).map_err(|error| {
            format!("Could not restore configured Apple Music output {output_device_name}: {error}")
        })?;
        self.runtime.lock().unwrap().stopped_unix_ms = Some(now_unix_ms());
        Ok(true)
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn restore_configured_output_if_idle(
        &self,
        _settings: &AppleMusicPlaybackSettings,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Take over the Mac's default output so Music.app decodes into Fozmo
    /// Capture, remembering the device to hand back afterwards.
    ///
    /// Shared by the Player and relay paths: which zone hears the result does
    /// not change how the route is claimed.
    #[cfg(target_os = "macos")]
    fn acquire_capture_route(
        &self,
        configured_output_device_name: Option<&str>,
    ) -> Result<(coreaudio_sys::AudioDeviceID, Option<String>), String> {
        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID).ok_or_else(|| {
            "Fozmo Capture HAL driver is not visible to CoreAudio. Install the driver first."
                .to_string()
        })?;

        // A retained route already knows which physical device to hand back,
        // and re-deriving it by name can fail on exactly the CoreAudio scan
        // that the capture handoff perturbed.
        let retained_output_uid = {
            let runtime = self.runtime.lock().unwrap();
            runtime
                .route_retained
                .then(|| runtime.saved_default_output_uid.clone())
                .flatten()
        };
        let current_default = coreaudio::default_output_device_uid();
        let already_routed_to_capture = current_default.as_deref() == Some(CAPTURE_DEVICE_UID);
        let saved_default_output_uid = if already_routed_to_capture {
            retained_output_uid.or_else(|| {
                configured_output_device_name
                    .and_then(coreaudio::local_physical_device_uid_for_name)
            })
        } else {
            current_default
        };
        if !already_routed_to_capture {
            coreaudio::set_default_output_device(device_id).map_err(|error| {
                format!("Could not route macOS output to Fozmo Capture: {error}")
            })?;
        }
        Ok((device_id, saved_default_output_uid))
    }

    #[cfg(target_os = "macos")]
    fn start_macos(
        self: &Arc<Self>,
        player: Arc<Player>,
        settings: &AppleMusicPlaybackSettings,
        verified_format: Option<(u32, Option<u32>)>,
    ) -> Result<(), String> {
        self.guard_against_feedback_loop(&player)?;
        let configured_output_device_name = player
            .selected_device_name()
            .or_else(|| normalize_optional(settings.output_device_name.as_deref()));
        let (device_id, saved_default_output_uid) =
            self.acquire_capture_route(configured_output_device_name.as_deref())?;

        let restore_on_error = |saved: &Option<String>| {
            if let Some(uid) = saved.as_deref()
                && let Some(previous) = coreaudio::device_id_for_uid(uid)
            {
                let _ = coreaudio::set_default_output_device(previous);
            }
        };
        if let Some((rate_hz, _)) = verified_format {
            if !rate_control::is_supported_capture_rate(rate_hz) {
                restore_on_error(&saved_default_output_uid);
                return Err(format!(
                    "Cached Apple Music format uses unsupported native rate {rate_hz} Hz."
                ));
            }
            rate_control::set_nominal_rate(device_id, rate_hz).inspect_err(|_| {
                restore_on_error(&saved_default_output_uid);
            })?;
        }
        let rate_hz = coreaudio::read_f64(
            device_id,
            coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
        )
        .map(|rate| rate.round().max(0.0) as u32)
        .filter(|rate| *rate > 0)
        .ok_or_else(|| {
            restore_on_error(&saved_default_output_uid);
            "Could not read the Fozmo Capture nominal sample rate.".to_string()
        })?;
        let params = LiveSessionParams {
            device_name: CAPTURE_DEVICE_NAME.to_string(),
            rate_hz,
            buffer_ms: normalized_buffer_ms(settings.buffer_ms.max(PLAYBACK_CAPTURE_BUFFER_MS)),
            source_bit_depth: verified_format.and_then(|(_, bits)| bits),
        };
        let session = capture_session::start_live_session(&player, &params, true)
            .inspect_err(|_| restore_on_error(&saved_default_output_uid))?;

        let previous_session = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = true;
            // This session now owns the capture route.
            runtime.route_retained = false;
            runtime.player = Some(Arc::clone(&player));
            runtime.stopped_unix_ms = None;
            runtime.session_params = Some(params);
            runtime.saved_default_output_uid = saved_default_output_uid;
            runtime.session.replace(session)
        };
        drop(previous_session);
        Ok(())
    }

    /// Route only macOS system audio to Fozmo Capture while the explicit local
    /// Player continues the outgoing Qobuz/local track on its physical DAC.
    /// The later live-session start claims this retained route.
    #[cfg(target_os = "macos")]
    pub(crate) fn prepare_capture_route(&self, player: &Player) -> Result<(), String> {
        self.guard_against_feedback_loop(player)?;
        {
            let runtime = self.runtime.lock().unwrap();
            if runtime.running {
                return Err(
                    "Apple Music capture is already active during route preparation.".to_string(),
                );
            }
        }
        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID).ok_or_else(|| {
            "Fozmo Capture HAL driver is not visible to CoreAudio. Install the driver first."
                .to_string()
        })?;
        let current_default = coreaudio::default_output_device_uid();
        if current_default.as_deref() == Some(CAPTURE_DEVICE_UID) {
            let fallback_output_uid = player
                .selected_device_name()
                .as_deref()
                .and_then(coreaudio::local_physical_device_uid_for_name);
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.saved_default_output_uid.is_none() {
                runtime.saved_default_output_uid = fallback_output_uid;
            }
            runtime.route_retained = true;
            return Ok(());
        }
        coreaudio::set_default_output_device(device_id)
            .map_err(|error| format!("Could not pre-route macOS to Fozmo Capture: {error}"))?;
        let mut runtime = self.runtime.lock().unwrap();
        runtime.saved_default_output_uid = current_default;
        runtime.route_retained = true;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn set_prepared_capture_rate(&self, rate_hz: u32) -> Result<(), String> {
        if !rate_control::is_supported_capture_rate(rate_hz) {
            return Err(format!(
                "Apple Music selected unsupported native rate {rate_hz} Hz."
            ));
        }
        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID)
            .ok_or_else(|| "Fozmo Capture disappeared during route preparation.".to_string())?;
        rate_control::set_nominal_rate(device_id, rate_hz)
    }

    /// Open and probe the Apple live source without replacing the outgoing
    /// Player session. Music.app can fill this ring during the old track's
    /// tail, and the Player installs it only at the exact handoff boundary.
    #[cfg(target_os = "macos")]
    pub(crate) fn prepare_boundary_capture(
        &self,
        player: Arc<Player>,
        settings: &AppleMusicPlaybackSettings,
        prepared: PreparedAppleMusicControl,
    ) -> Result<(), String> {
        self.set_prepared_capture_rate(prepared.rate_hz)?;
        let params = LiveSessionParams {
            device_name: CAPTURE_DEVICE_NAME.to_string(),
            rate_hz: prepared.rate_hz,
            buffer_ms: normalized_buffer_ms(settings.buffer_ms.max(PLAYBACK_CAPTURE_BUFFER_MS)),
            source_bit_depth: prepared.source_bit_depth,
        };
        let session = capture_session::prepare_live_session(&player, &params)?;
        let previous = {
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.running {
                return Err(
                    "Apple Music capture became active during boundary preparation.".to_string(),
                );
            }
            runtime.player = Some(player);
            runtime.session_params = Some(params);
            runtime.prepared_control = Some(prepared);
            runtime.prepared_session.replace(session)
        };
        drop(previous);
        Ok(())
    }

    pub(crate) fn set_prepared_capture_gate_open(&self, open: bool) -> bool {
        let runtime = self.runtime.lock().unwrap();
        let Some(session) = runtime.prepared_session.as_ref() else {
            return false;
        };
        session.set_capture_gate_open(open);
        true
    }

    pub(crate) fn prepared_boundary(
        &self,
        source_key: &str,
    ) -> Option<(PreparedAppleMusicControl, f64)> {
        let runtime = self.runtime.lock().unwrap();
        let control = runtime
            .prepared_control
            .as_ref()
            .filter(|prepared| prepared.source_key == source_key)?
            .clone();
        let buffered = runtime.prepared_session.as_ref()?.buffered_audio_secs();
        Some((control, buffered))
    }

    /// Install a fully prepared live source at the outgoing Player epoch. With
    /// `preserve_output`, the existing CoreAudio stream and DSP carrier remain
    /// open; after natural EOF the same method provides a prefilled fallback.
    pub(crate) fn promote_prepared_boundary(
        &self,
        zone_id: String,
        source: SourceRef,
        expected_epoch: u64,
        preserve_output: bool,
    ) -> Result<AppleMusicPlaybackSnapshot, String> {
        let source_key = source.key();
        let (session, control, player) = {
            let mut runtime = self.runtime.lock().unwrap();
            let control = runtime
                .prepared_control
                .take()
                .filter(|prepared| prepared.source_key == source_key)
                .ok_or_else(|| "The prepared Apple Music boundary changed.".to_string())?;
            let session = runtime
                .prepared_session
                .take()
                .ok_or_else(|| "The prepared Apple Music capture disappeared.".to_string())?;
            let player = runtime
                .player
                .clone()
                .unwrap_or_else(|| Arc::clone(&self.player));
            (session, control, player)
        };
        let live = session.activate(
            &player,
            expected_epoch,
            control.source_bit_depth,
            preserve_output,
        )?;
        let player_epoch = live.player_epoch();
        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = true;
            runtime.route_retained = false;
            runtime.stopped_unix_ms = None;
            runtime.player = Some(player);
            runtime.session = Some(live);
        }
        let snapshot = self.activate_playback(zone_id, player_epoch, source);
        self.update_playback(
            snapshot.generation,
            "playing",
            Some(0.0),
            control.duration_secs,
        );
        Ok(self.playback_snapshot().unwrap_or(snapshot))
    }

    /// Start the sole supported Apple Music audio route.
    #[cfg(target_os = "macos")]
    pub(crate) fn start_playback_capture(
        self: &Arc<Self>,
        player: Arc<Player>,
        settings: &AppleMusicPlaybackSettings,
    ) -> Result<u64, String> {
        self.start_macos(player, settings, None)?;
        self.session_player_epoch()
            .ok_or_else(|| "Apple Music capture started without a Player session.".to_string())
    }

    /// Start capture at a previously verified per-track format so the first
    /// live session is already rate-correct and needs no format-probe restart.
    #[cfg(target_os = "macos")]
    pub(crate) fn start_playback_capture_at_format(
        self: &Arc<Self>,
        player: Arc<Player>,
        settings: &AppleMusicPlaybackSettings,
        rate_hz: u32,
        source_bit_depth: Option<u32>,
    ) -> Result<u64, String> {
        self.start_macos(player, settings, Some((rate_hz, source_bit_depth)))?;
        self.session_player_epoch()
            .ok_or_else(|| "Apple Music capture started without a Player session.".to_string())
    }

    /// Open capture for a zone that renders elsewhere: a Windows or macOS
    /// agent, or a browser. Music.app still decodes into Fozmo Capture on this
    /// Mac; only the consumer of that PCM changes.
    ///
    /// Returns the rate the session actually opened at, which is the capture
    /// device's nominal rate and therefore the rate the caller must have set
    /// before Music.app started decoding.
    #[cfg(target_os = "macos")]
    pub(crate) fn start_relay_capture(
        &self,
        settings: &AppleMusicPlaybackSettings,
        verified_format: Option<(u32, Option<u32>)>,
    ) -> Result<u32, String> {
        {
            let runtime = self.runtime.lock().unwrap();
            if runtime.running {
                return Err(
                    "Apple Music capture is already running for another output.".to_string()
                );
            }
        }
        let (device_id, saved_default_output_uid) = self.acquire_capture_route(
            normalize_optional(settings.output_device_name.as_deref()).as_deref(),
        )?;
        let restore_on_error = |saved: &Option<String>| {
            if let Some(uid) = saved.as_deref()
                && let Some(previous) = coreaudio::device_id_for_uid(uid)
            {
                let _ = coreaudio::set_default_output_device(previous);
            }
        };
        if let Some((rate_hz, _)) = verified_format {
            if !rate_control::is_supported_capture_rate(rate_hz) {
                restore_on_error(&saved_default_output_uid);
                return Err(format!(
                    "Cached Apple Music format uses unsupported native rate {rate_hz} Hz."
                ));
            }
            rate_control::set_nominal_rate(device_id, rate_hz).inspect_err(|_| {
                restore_on_error(&saved_default_output_uid);
            })?;
        }
        let rate_hz = coreaudio::read_f64(
            device_id,
            coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
        )
        .map(|rate| rate.round().max(0.0) as u32)
        .filter(|rate| *rate > 0)
        .ok_or_else(|| {
            restore_on_error(&saved_default_output_uid);
            "Could not read the Fozmo Capture nominal sample rate.".to_string()
        })?;
        let params = LiveSessionParams {
            device_name: CAPTURE_DEVICE_NAME.to_string(),
            rate_hz,
            buffer_ms: normalized_buffer_ms(settings.buffer_ms.max(PLAYBACK_CAPTURE_BUFFER_MS)),
            source_bit_depth: verified_format.and_then(|(_, bits)| bits),
        };
        let session = capture_session::start_relay_session(&params)
            .inspect_err(|_| restore_on_error(&saved_default_output_uid))?;

        let previous = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = true;
            runtime.route_retained = false;
            runtime.stopped_unix_ms = None;
            runtime.session_params = Some(params);
            runtime.saved_default_output_uid = saved_default_output_uid;
            runtime.relay.replace(session)
        };
        drop(previous);
        Ok(rate_hz)
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn start_relay_capture(
        &self,
        _settings: &AppleMusicPlaybackSettings,
        _verified_format: Option<(u32, Option<u32>)>,
    ) -> Result<u32, String> {
        Err("Apple Music capture is only available on macOS.".to_string())
    }

    /// Reopen the relayed capture session — at a corrected rate, or simply to
    /// discard everything a seek left behind — while keeping the capture route
    /// and the session's identity.
    ///
    /// The route matters: handing the Mac's default output back to the user's
    /// device and taking it away again a moment later makes Music.app rebuild
    /// its output chain twice. The identity matters because the listener is
    /// still on the same track; only the bytes start again.
    #[cfg(target_os = "macos")]
    pub(crate) fn restart_relay_capture(
        &self,
        generation: u64,
        settings: &AppleMusicPlaybackSettings,
        verified_format: Option<(u32, Option<u32>)>,
    ) -> Result<u32, String> {
        let owns_generation = |runtime: &CaptureRuntime| {
            runtime
                .playback
                .as_ref()
                .is_some_and(|playback| playback.generation == generation)
        };
        let previous = {
            let mut runtime = self.runtime.lock().unwrap();
            if !owns_generation(&runtime) {
                return Err("Playback changed during the Apple Music relay restart.".to_string());
            }
            runtime.relay.take()
        };
        drop(previous);

        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID)
            .ok_or_else(|| "Fozmo Capture disappeared during the format switch.".to_string())?;
        if let Some((rate_hz, _)) = verified_format {
            if !rate_control::is_supported_capture_rate(rate_hz) {
                return Err(format!(
                    "Apple Music selected unsupported native rate {rate_hz} Hz."
                ));
            }
            rate_control::set_nominal_rate(device_id, rate_hz)?;
        }
        let rate_hz = coreaudio::read_f64(
            device_id,
            coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
        )
        .map(|rate| rate.round().max(0.0) as u32)
        .filter(|rate| *rate > 0)
        .ok_or_else(|| "Could not read the Fozmo Capture nominal sample rate.".to_string())?;
        let params = LiveSessionParams {
            device_name: CAPTURE_DEVICE_NAME.to_string(),
            rate_hz,
            buffer_ms: normalized_buffer_ms(settings.buffer_ms.max(PLAYBACK_CAPTURE_BUFFER_MS)),
            source_bit_depth: verified_format.and_then(|(_, bits)| bits),
        };
        let session = capture_session::start_relay_session(&params)?;

        let mut runtime = self.runtime.lock().unwrap();
        if !owns_generation(&runtime) {
            return Err("Playback changed during the Apple Music relay restart.".to_string());
        }
        runtime.running = true;
        runtime.session_params = Some(params);
        runtime.relay = Some(session);
        Ok(rate_hz)
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn restart_relay_capture(
        &self,
        _generation: u64,
        _settings: &AppleMusicPlaybackSettings,
        _verified_format: Option<(u32, Option<u32>)>,
    ) -> Result<u32, String> {
        Err("Apple Music capture is only available on macOS.".to_string())
    }

    pub(crate) fn relay_running(&self) -> bool {
        let runtime = self.runtime.lock().unwrap();
        runtime.running && runtime.relay.is_some()
    }

    pub(crate) fn set_relay_gate_open(&self, open: bool) -> bool {
        let runtime = self.runtime.lock().unwrap();
        let Some(session) = runtime.relay.as_ref() else {
            return false;
        };
        session.set_capture_gate_open(open);
        true
    }

    pub(crate) fn relay_buffered_audio_secs(&self) -> Option<f64> {
        self.runtime
            .lock()
            .unwrap()
            .relay
            .as_ref()
            .map(capture_session::RelaySession::buffered_audio_secs)
    }

    /// Whether the relayed consumer has already collected its stream. A zone
    /// that never connects leaves this false, which is how the caller notices
    /// an agent that failed to fetch.
    pub(crate) fn relay_stream_claimed(&self) -> bool {
        self.runtime
            .lock()
            .unwrap()
            .relay
            .as_ref()
            .is_some_and(capture_session::RelaySession::reader_taken)
    }

    /// Take the live stream for the HTTP response that will drain it. The
    /// second caller gets `None`: there is one Apple stream, and splitting its
    /// frames between two readers would corrupt both.
    pub(crate) fn open_relay_stream(&self) -> Option<AppleMusicRelayStream> {
        let runtime = self.runtime.lock().unwrap();
        let session = runtime.relay.as_ref()?;
        let rate_hz = session.rate_hz();
        let bits = session.wire_bits();
        let reader = session.take_reader()?;
        Some(AppleMusicRelayStream {
            rate_hz,
            bits,
            reader,
        })
    }

    /// The zone that currently holds the single Apple Music stream, if any.
    ///
    /// Rerouting the Mac's output is machine-wide, so a second zone cannot
    /// start Apple Music while this one is live; callers turn this into the
    /// listener-facing explanation of why.
    ///
    /// A zone between two tracks of its own owns the stream just as firmly as
    /// one mid-track: it has deliberately kept the capture route for the
    /// successor it is about to start, and handing that to another zone in the
    /// gap would break the boundary the retention exists to protect.
    pub(crate) fn streaming_zone_id(&self) -> Option<String> {
        let runtime = self.runtime.lock().unwrap();
        runtime
            .playback
            .as_ref()
            .map(|playback| playback.zone_id.clone())
            .or_else(|| {
                runtime
                    .route_retained
                    .then(|| runtime.route_retained_zone.clone())
                    .flatten()
            })
    }

    /// The local zone must use a local physical output so capture cannot feed
    /// back into itself or escape to a network renderer.
    fn guard_against_feedback_loop(&self, player: &Player) -> Result<(), String> {
        match player.selected_device_name() {
            None => Err(
                "The local Fozmo zone is set to the system-default output. Select an explicit physical output device before playing Apple Music."
                    .to_string(),
            ),
            Some(name) if name.trim() == CAPTURE_DEVICE_NAME => Err(
                "The local Fozmo zone is set to Fozmo Capture, which would create a feedback loop. Select a physical output device first."
                    .to_string(),
            ),
            Some(name) if is_remote_or_virtual_output_name(&name) => Err(
                "Apple Music can only play through a local physical CoreAudio output."
                    .to_string(),
            ),
            Some(name) if !selected_output_is_local_physical(&name) => Err(
                "Apple Music could not verify that the selected output is a local physical CoreAudio device."
                    .to_string(),
            ),
            Some(_) => Ok(()),
        }
    }

    /// Tear down capture and restore the user's previous macOS default output.
    pub(crate) fn stop_runtime(&self, stop_player: bool) -> Option<u64> {
        let (session, prepared_session, relay, player, saved_default_output_uid) = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = false;
            runtime.route_retained = false;
            runtime.route_retained_zone = None;
            runtime.stopped_unix_ms = Some(now_unix_ms());
            runtime.session_params = None;
            runtime.playback = None;
            runtime.prepared_control = None;
            (
                runtime.session.take(),
                runtime.prepared_session.take(),
                runtime.relay.take(),
                runtime.player.take(),
                runtime.saved_default_output_uid.take(),
            )
        };
        drop(session);
        drop(prepared_session);
        // Dropping the relay signals its reader to EOF, which ends the HTTP
        // response the remote zone is playing.
        drop(relay);
        let player = player.unwrap_or_else(|| Arc::clone(&self.player));
        if stop_player {
            player.stop();
        }
        restore_default_output(saved_default_output_uid);
        Some(player.playback_epoch())
    }

    /// End the finished track's capture session but keep macOS routed to Fozmo
    /// Capture for an Apple Music successor that is about to open its own.
    /// Handing the system default output back to the DAC and taking it away
    /// again a moment later makes Music.app rebuild its output chain twice and
    /// is what drops the DAC out of a CoreAudio scan at the boundary.
    ///
    /// Closing the session still releases the producer, so Player drains the
    /// captured tail and reaches EOF exactly as it does for any other boundary.
    /// The caller owns the retained route until `start_playback_capture` claims
    /// it, and must call [`Self::release_retained_route`] on every path that
    /// does not get there.
    pub(crate) fn stop_playback_retaining_route(&self) -> Option<u64> {
        let (session, player) = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = false;
            runtime.route_retained = true;
            runtime.route_retained_zone = runtime
                .playback
                .as_ref()
                .map(|playback| playback.zone_id.clone());
            runtime.stopped_unix_ms = Some(now_unix_ms());
            runtime.session_params = None;
            runtime.playback = None;
            (runtime.session.take(), runtime.player.take())
        };
        drop(session);
        let player = player.unwrap_or_else(|| Arc::clone(&self.player));
        Some(player.playback_epoch())
    }

    /// Hand the macOS default output back when a retained capture route was
    /// never claimed. Safe to call unconditionally: it does nothing unless a
    /// route is still outstanding.
    pub(crate) fn release_retained_route(&self) {
        let (saved, prepared_session) = {
            let mut runtime = self.runtime.lock().unwrap();
            if !runtime.route_retained || runtime.running {
                return;
            }
            runtime.route_retained = false;
            runtime.route_retained_zone = None;
            runtime.prepared_control = None;
            runtime.session_params = None;
            (
                runtime.saved_default_output_uid.take(),
                runtime.prepared_session.take(),
            )
        };
        drop(prepared_session);
        restore_default_output(saved);
    }

    pub(crate) fn capture_running(&self) -> bool {
        self.runtime.lock().unwrap().running
    }

    #[cfg(test)]
    fn route_is_retained(&self) -> bool {
        self.runtime.lock().unwrap().route_retained
    }

    /// Keep zone discovery from marking the physical DAC offline during the
    /// temporary system-output handoff.
    pub(crate) fn quiet_local_device_refresh(&self) -> bool {
        let runtime = self.runtime.lock().unwrap();
        runtime.running
            || runtime.route_retained
            || runtime.stopped_unix_ms.is_some_and(|stopped| {
                now_unix_ms().saturating_sub(stopped) <= LOCAL_DEVICE_REFRESH_SETTLE_MS
            })
    }

    pub(crate) fn session_player_epoch(&self) -> Option<u64> {
        self.runtime
            .lock()
            .unwrap()
            .session
            .as_ref()
            .map(capture_session::LiveSession::player_epoch)
    }

    pub(crate) fn buffered_audio_secs(&self) -> Result<f64, String> {
        self.runtime
            .lock()
            .unwrap()
            .session
            .as_ref()
            .map(capture_session::LiveSession::buffered_audio_secs)
            .ok_or_else(|| "Apple Music capture is not running.".to_string())
    }

    pub(crate) fn activate_playback(
        &self,
        zone_id: String,
        player_epoch: u64,
        source: SourceRef,
    ) -> AppleMusicPlaybackSnapshot {
        self.activate_playback_with_delivery(
            zone_id,
            AppleMusicDelivery::LocalPlayer,
            player_epoch,
            source,
        )
    }

    pub(crate) fn activate_playback_with_delivery(
        &self,
        zone_id: String,
        delivery: AppleMusicDelivery,
        player_epoch: u64,
        source: SourceRef,
    ) -> AppleMusicPlaybackSnapshot {
        let mut runtime = self.runtime.lock().unwrap();
        let generation = runtime.next_playback_generation.max(1);
        runtime.next_playback_generation = generation.wrapping_add(1).max(1);
        let snapshot = AppleMusicPlaybackSnapshot {
            zone_id,
            delivery,
            player_epoch,
            generation,
            duration_secs: source.duration_secs().unwrap_or(0.0),
            source,
            playback_state: "preparing".to_string(),
            timeline_origin_secs: 0.0,
            player_position_origin_secs: 0.0,
            paused_position_secs: None,
            position_secs: 0.0,
        };
        runtime.playback = Some(snapshot.clone());
        snapshot
    }

    pub(crate) fn playback_snapshot(&self) -> Option<AppleMusicPlaybackSnapshot> {
        self.runtime.lock().unwrap().playback.clone()
    }

    pub(crate) fn reserve_prefetch(&self, generation: u64) -> Option<u64> {
        let mut runtime = self.runtime.lock().unwrap();
        if runtime
            .playback
            .as_ref()
            .is_none_or(|playback| playback.generation != generation)
        {
            return None;
        }
        let revision = runtime.next_prefetch_revision.max(1);
        runtime.next_prefetch_revision = revision.wrapping_add(1).max(1);
        Some(revision)
    }

    pub(crate) fn prefetch_is_current(&self, generation: u64, revision: u64) -> bool {
        let runtime = self.runtime.lock().unwrap();
        runtime
            .playback
            .as_ref()
            .is_some_and(|playback| playback.generation == generation)
            && runtime.next_prefetch_revision == revision.wrapping_add(1).max(1)
    }

    pub(crate) fn playback_snapshot_for_zone(
        &self,
        zone_id: &str,
    ) -> Option<AppleMusicPlaybackSnapshot> {
        self.playback_snapshot()
            .filter(|snapshot| snapshot.zone_id == zone_id)
    }

    pub(crate) fn replace_player_epoch(&self, generation: u64, player_epoch: u64) -> bool {
        let mut runtime = self.runtime.lock().unwrap();
        let Some(snapshot) = runtime
            .playback
            .as_mut()
            .filter(|snapshot| snapshot.generation == generation)
        else {
            return false;
        };
        snapshot.player_epoch = player_epoch;
        true
    }

    pub(crate) fn update_playback(
        &self,
        generation: u64,
        playback_state: &str,
        position_secs: Option<f64>,
        duration_secs: Option<f64>,
    ) -> bool {
        let mut runtime = self.runtime.lock().unwrap();
        let Some(snapshot) = runtime
            .playback
            .as_mut()
            .filter(|snapshot| snapshot.generation == generation)
        else {
            return false;
        };
        snapshot.playback_state = playback_state.to_string();
        if playback_state != "paused" {
            snapshot.paused_position_secs = None;
        }
        if let Some(position) = position_secs.filter(|value| value.is_finite() && *value >= 0.0) {
            snapshot.position_secs = position;
        }
        if let Some(duration) = duration_secs.filter(|value| value.is_finite() && *value > 0.0) {
            snapshot.duration_secs = duration;
        }
        true
    }

    pub(crate) fn pause_playback_at(&self, generation: u64, position_secs: f64) -> bool {
        if !position_secs.is_finite() || position_secs < 0.0 {
            return false;
        }
        let mut runtime = self.runtime.lock().unwrap();
        let Some(snapshot) = runtime
            .playback
            .as_mut()
            .filter(|snapshot| snapshot.generation == generation)
        else {
            return false;
        };
        snapshot.playback_state = "paused".to_string();
        snapshot.paused_position_secs = Some(if snapshot.duration_secs > 0.0 {
            position_secs.min(snapshot.duration_secs)
        } else {
            position_secs
        });
        true
    }

    /// Promote Music.app's native next-album-track transition without
    /// restarting the capture session or local Player output.
    pub(crate) fn promote_continuous_playback(
        &self,
        generation: u64,
        source: SourceRef,
        fallback_player_position_secs: f64,
        position_secs: Option<f64>,
        duration_secs: Option<f64>,
    ) -> bool {
        let mut runtime = self.runtime.lock().unwrap();
        let Some(snapshot) = runtime
            .playback
            .as_mut()
            .filter(|snapshot| snapshot.generation == generation)
        else {
            return false;
        };
        snapshot.player_position_origin_secs = next_player_position_origin(
            snapshot.player_position_origin_secs,
            snapshot.duration_secs,
            fallback_player_position_secs,
        );
        snapshot.source = source;
        snapshot.playback_state = "playing".to_string();
        snapshot.timeline_origin_secs = 0.0;
        snapshot.paused_position_secs = None;
        snapshot.position_secs = position_secs
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0);
        snapshot.duration_secs = duration_secs
            .filter(|value| value.is_finite() && *value > 0.0)
            .or_else(|| snapshot.source.duration_secs())
            .unwrap_or(0.0);
        true
    }

    /// Drop producer PCM while Apple changes a decoder so transport noise and
    /// inter-track silence never enter the already-buffered live timeline.
    pub(crate) fn set_capture_gate_open(&self, open: bool) -> bool {
        let runtime = self.runtime.lock().unwrap();
        let Some(session) = runtime.session.as_ref() else {
            return false;
        };
        session.set_capture_gate_open(open);
        true
    }

    pub(crate) fn capture_underrun_count(&self) -> Option<u64> {
        self.runtime
            .lock()
            .unwrap()
            .session
            .as_ref()
            .map(capture_session::LiveSession::capture_underrun_count)
    }

    pub(crate) fn session_format(&self) -> Option<(u32, Option<u32>)> {
        self.runtime
            .lock()
            .unwrap()
            .session_params
            .as_ref()
            .map(|params| (params.rate_hz, params.source_bit_depth))
    }

    pub(crate) fn take_prepared_control(
        &self,
        source_key: &str,
    ) -> Option<PreparedAppleMusicControl> {
        let mut runtime = self.runtime.lock().unwrap();
        let control = runtime
            .prepared_control
            .take()
            .filter(|prepared| prepared.source_key == source_key);
        let prepared_session = runtime.prepared_session.take();
        drop(runtime);
        drop(prepared_session);
        control
    }

    pub(crate) fn set_timeline_origin(&self, generation: u64, position_secs: f64) -> bool {
        if !position_secs.is_finite() || position_secs < 0.0 {
            return false;
        }
        let mut runtime = self.runtime.lock().unwrap();
        let Some(snapshot) = runtime
            .playback
            .as_mut()
            .filter(|snapshot| snapshot.generation == generation)
        else {
            return false;
        };
        snapshot.timeline_origin_secs = position_secs;
        snapshot.player_position_origin_secs = 0.0;
        true
    }

    #[cfg(target_os = "macos")]
    fn restart_session(
        self: &Arc<Self>,
        rate_hz: u32,
        source_bit_depth: Option<u32>,
        force_rebuild: bool,
    ) -> Result<u64, String> {
        let source_bit_depth = source_bit_depth.filter(|bits| matches!(bits, 16 | 24 | 32));
        let (params, player, hold_paused) = {
            let runtime = self.runtime.lock().unwrap();
            if !runtime.running {
                return Err("Apple Music capture is not running.".to_string());
            }
            let params = runtime
                .session_params
                .clone()
                .ok_or_else(|| "Apple Music capture has no active session.".to_string())?;
            (
                params,
                runtime
                    .player
                    .clone()
                    .unwrap_or_else(|| Arc::clone(&self.player)),
                runtime
                    .playback
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.playback_state != "playing"),
            )
        };

        if params.rate_hz == rate_hz && !force_rebuild {
            let mut runtime = self.runtime.lock().unwrap();
            if !runtime.running {
                return Err("Apple Music capture stopped during format confirmation.".to_string());
            }
            if let Some(active) = runtime.session_params.as_mut() {
                active.source_bit_depth = source_bit_depth;
            }
            return runtime
                .session
                .as_ref()
                .map(capture_session::LiveSession::player_epoch)
                .ok_or_else(|| "Apple Music capture has no active session.".to_string());
        }

        let old_session = self.runtime.lock().unwrap().session.take();
        drop(old_session);
        player.stop();
        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID)
            .ok_or_else(|| "Fozmo Capture disappeared during the format switch.".to_string())?;
        rate_control::set_nominal_rate(device_id, rate_hz)?;
        let params = LiveSessionParams {
            rate_hz,
            source_bit_depth,
            ..params
        };
        let session = capture_session::start_live_session(&player, &params, hold_paused)?;
        let player_epoch = session.player_epoch();
        let mut runtime = self.runtime.lock().unwrap();
        if !runtime.running {
            return Err("Apple Music capture stopped during the format switch.".to_string());
        }
        runtime.session = Some(session);
        runtime.session_params = Some(params);
        if let Some(snapshot) = runtime.playback.as_mut() {
            snapshot.player_epoch = player_epoch;
        }
        Ok(player_epoch)
    }

    /// Apply Music.app's verified decoder format to a fresh capture session.
    #[cfg(target_os = "macos")]
    pub(crate) fn restart_at_verified_source_format(
        self: &Arc<Self>,
        rate_hz: u32,
        source_bit_depth: Option<u32>,
    ) -> Result<u64, String> {
        if !rate_control::is_supported_capture_rate(rate_hz) {
            return Err(format!(
                "Apple Music selected unsupported native rate {rate_hz} Hz."
            ));
        }
        self.restart_session(rate_hz, source_bit_depth, false)
    }

    /// Reopen the capture session at its verified format so no pre-seek PCM
    /// survives into the new timeline.
    #[cfg(target_os = "macos")]
    pub(crate) fn restart_current_managed_session(self: &Arc<Self>) -> Result<u64, String> {
        let params = self
            .runtime
            .lock()
            .unwrap()
            .session_params
            .clone()
            .ok_or_else(|| "Apple Music capture has no active session.".to_string())?;
        self.restart_session(params.rate_hz, params.source_bit_depth, true)
    }
}

impl Drop for AppleMusicPlaybackService {
    fn drop(&mut self) {
        let saved = self
            .runtime
            .lock()
            .map(|mut runtime| runtime.saved_default_output_uid.take())
            .unwrap_or_default();
        restore_default_output(saved);
    }
}

#[cfg(target_os = "macos")]
fn restore_default_output(saved_uid: Option<String>) {
    if let Some(uid) = saved_uid.as_deref()
        && let Some(device_id) = coreaudio::device_id_for_uid(uid)
    {
        let _ = coreaudio::set_default_output_device(device_id);
    }
}

#[cfg(not(target_os = "macos"))]
fn restore_default_output(_saved_uid: Option<String>) {}

fn is_remote_or_virtual_output_name(name: &str) -> bool {
    let trimmed = name.trim();
    crate::audio::sinks::airplay::is_airplay_device_name(trimmed)
        || crate::audio::sinks::sonos::is_sonos_device_name(trimmed)
        || crate::audio::sinks::upnp::is_upnp_device_name(trimmed)
        || trimmed == CAPTURE_DEVICE_NAME
}

#[cfg(target_os = "macos")]
fn selected_output_is_local_physical(name: &str) -> bool {
    coreaudio::output_device_is_local_physical_by_name(name)
}

#[cfg(not(target_os = "macos"))]
fn selected_output_is_local_physical(_name: &str) -> bool {
    true
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalized_buffer_ms(buffer_ms: u32) -> u32 {
    if buffer_ms == 0 {
        250
    } else {
        buffer_ms.clamp(50, MAX_PLAYBACK_CAPTURE_BUFFER_MS)
    }
}

fn next_player_position_origin(
    current_origin_secs: f64,
    completed_duration_secs: f64,
    fallback_player_position_secs: f64,
) -> f64 {
    if current_origin_secs.is_finite()
        && current_origin_secs >= 0.0
        && completed_duration_secs.is_finite()
        && completed_duration_secs > 0.0
    {
        return current_origin_secs + completed_duration_secs;
    }
    fallback_player_position_secs.max(0.0)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple_source(song_id: &str, title: &str, track_number: u32) -> SourceRef {
        SourceRef::AppleMusicTrack {
            song_id: song_id.to_string(),
            storefront: Some("nz".to_string()),
            title: Some(title.to_string()),
            artist: Some("Radiohead".to_string()),
            album: Some("In Rainbows".to_string()),
            album_artist: Some("Radiohead".to_string()),
            album_id: Some("1109714933".to_string()),
            artwork_url: None,
            duration_secs: Some(240.0),
            track_number: Some(track_number),
            disc_number: Some(1),
            isrc: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    #[test]
    fn playback_capture_buffer_can_cover_slow_coreaudio_startup() {
        assert_eq!(
            normalized_buffer_ms(PLAYBACK_CAPTURE_BUFFER_MS),
            PLAYBACK_CAPTURE_BUFFER_MS
        );
        assert_eq!(
            normalized_buffer_ms(u32::MAX),
            MAX_PLAYBACK_CAPTURE_BUFFER_MS
        );
    }

    #[test]
    fn continuous_track_origin_advances_by_the_completed_track_duration() {
        assert_eq!(next_player_position_origin(0.0, 237.5, 236.9), 237.5);
        assert_eq!(next_player_position_origin(237.5, 242.0, 478.8), 479.5);
        assert_eq!(next_player_position_origin(0.0, 0.0, 17.25), 17.25);
    }

    #[test]
    fn retaining_stop_ends_the_track_and_holds_the_route_until_it_is_released() {
        let player = Arc::new(Player::new());
        let service = AppleMusicPlaybackService::new(Arc::clone(&player));
        let snapshot = service.activate_playback(
            "local-core".to_string(),
            player.playback_epoch(),
            apple_source("1109715066", "15 Step", 1),
        );

        let detached = service.stop_playback_retaining_route();

        // The track ends exactly as it does for any other boundary...
        assert_eq!(detached, Some(snapshot.player_epoch));
        assert!(service.playback_snapshot().is_none());
        assert!(!service.capture_running());
        // ...but the successor still owns the capture route.
        assert!(service.route_is_retained());

        service.release_retained_route();
        assert!(!service.route_is_retained());
        // Releasing twice must not hand back a route a later session owns.
        service.release_retained_route();
        assert!(!service.route_is_retained());
    }

    #[test]
    fn a_plain_stop_never_leaves_a_route_outstanding() {
        let service = AppleMusicPlaybackService::new(Arc::new(Player::new()));
        service.activate_playback(
            "local-core".to_string(),
            0,
            apple_source("1109715066", "15 Step", 1),
        );

        service.stop_playback_retaining_route();
        assert!(service.route_is_retained());
        service.stop_runtime(false);

        assert!(!service.route_is_retained());
    }

    #[test]
    fn continuous_playback_promotion_reuses_generation_and_resets_track_timeline() {
        let service = AppleMusicPlaybackService::new(Arc::new(Player::new()));
        let first = apple_source("1109715066", "15 Step", 1);
        let next = apple_source("1109715161", "Bodysnatchers", 2);
        let snapshot = service.activate_playback("local-core".to_string(), 7, first);
        service.update_playback(snapshot.generation, "playing", Some(237.0), Some(237.5));

        assert!(service.promote_continuous_playback(
            snapshot.generation,
            next.clone(),
            237.0,
            Some(0.08),
            Some(242.0),
        ));

        let promoted = service.playback_snapshot().unwrap();
        assert_eq!(promoted.generation, snapshot.generation);
        assert_eq!(promoted.player_epoch, 7);
        assert_eq!(promoted.source, next);
        assert_eq!(promoted.player_position_origin_secs, 237.5);
        assert_eq!(promoted.timeline_origin_secs, 0.0);
        assert_eq!(promoted.position_secs, 0.08);
        assert_eq!(promoted.duration_secs, 242.0);
    }

    #[test]
    fn paused_position_is_frozen_until_playback_resumes() {
        let service = AppleMusicPlaybackService::new(Arc::new(Player::new()));
        let snapshot = service.activate_playback(
            "local-core".to_string(),
            7,
            apple_source("1109715066", "15 Step", 1),
        );
        service.update_playback(snapshot.generation, "playing", Some(44.0), Some(240.0));

        assert!(service.pause_playback_at(snapshot.generation, 41.75));
        let paused = service.playback_snapshot().unwrap();
        assert_eq!(paused.playback_state, "paused");
        assert_eq!(paused.paused_position_secs, Some(41.75));
        assert_eq!(
            paused.position_secs, 44.0,
            "freezing the listener position must not replace Music.app's decoder head"
        );

        service.update_playback(snapshot.generation, "playing", Some(44.1), None);
        let resumed = service.playback_snapshot().unwrap();
        assert_eq!(resumed.playback_state, "playing");
        assert_eq!(resumed.paused_position_secs, None);
    }
}
