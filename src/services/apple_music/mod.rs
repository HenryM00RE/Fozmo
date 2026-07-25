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

/// Fozmo-owned identity and timeline for a catalog track playing through the
/// native Music.app -> Fozmo Capture -> local Player path.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppleMusicPlaybackSnapshot {
    pub(crate) zone_id: String,
    pub(crate) player_epoch: u64,
    pub(crate) generation: u64,
    pub(crate) source: SourceRef,
    pub(crate) playback_state: String,
    /// Source position represented by Player position zero. This is non-zero
    /// after a seek because reopening the live capture resets Player metrics.
    pub(crate) timeline_origin_secs: f64,
    pub(crate) position_secs: f64,
    pub(crate) duration_secs: f64,
}

struct CaptureRuntime {
    running: bool,
    player: Option<Arc<Player>>,
    stopped_unix_ms: Option<u64>,
    session: Option<capture_session::LiveSession>,
    session_params: Option<LiveSessionParams>,
    saved_default_output_uid: Option<String>,
    playback: Option<AppleMusicPlaybackSnapshot>,
    next_playback_generation: u64,
    next_prefetch_revision: u64,
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
            playback: None,
            next_playback_generation: 1,
            next_prefetch_revision: 1,
        }
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

    #[cfg(target_os = "macos")]
    fn start_macos(
        self: &Arc<Self>,
        player: Arc<Player>,
        settings: &AppleMusicPlaybackSettings,
    ) -> Result<(), String> {
        self.guard_against_feedback_loop(&player)?;
        let configured_output_device_name = player
            .selected_device_name()
            .or_else(|| normalize_optional(settings.output_device_name.as_deref()));
        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID).ok_or_else(|| {
            "Fozmo Capture HAL driver is not visible to CoreAudio. Install the driver first."
                .to_string()
        })?;

        let current_default = coreaudio::default_output_device_uid();
        let saved_default_output_uid = if current_default.as_deref() == Some(CAPTURE_DEVICE_UID) {
            configured_output_device_name
                .as_deref()
                .and_then(coreaudio::local_physical_device_uid_for_name)
        } else {
            current_default
        };
        coreaudio::set_default_output_device(device_id)
            .map_err(|error| format!("Could not route macOS output to Fozmo Capture: {error}"))?;

        let restore_on_error = |saved: &Option<String>| {
            if let Some(uid) = saved.as_deref()
                && let Some(previous) = coreaudio::device_id_for_uid(uid)
            {
                let _ = coreaudio::set_default_output_device(previous);
            }
        };
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
            source_bit_depth: None,
        };
        let session = capture_session::start_live_session(&player, &params, true)
            .inspect_err(|_| restore_on_error(&saved_default_output_uid))?;

        let previous_session = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = true;
            runtime.player = Some(Arc::clone(&player));
            runtime.stopped_unix_ms = None;
            runtime.session_params = Some(params);
            runtime.saved_default_output_uid = saved_default_output_uid;
            runtime.session.replace(session)
        };
        drop(previous_session);
        Ok(())
    }

    /// Start the sole supported Apple Music audio route.
    #[cfg(target_os = "macos")]
    pub(crate) fn start_playback_capture(
        self: &Arc<Self>,
        player: Arc<Player>,
        settings: &AppleMusicPlaybackSettings,
    ) -> Result<u64, String> {
        self.start_macos(player, settings)?;
        self.session_player_epoch()
            .ok_or_else(|| "Apple Music capture started without a Player session.".to_string())
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
        let (session, player, saved_default_output_uid) = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = false;
            runtime.stopped_unix_ms = Some(now_unix_ms());
            runtime.session_params = None;
            runtime.playback = None;
            (
                runtime.session.take(),
                runtime.player.take(),
                runtime.saved_default_output_uid.take(),
            )
        };
        drop(session);
        let player = player.unwrap_or_else(|| Arc::clone(&self.player));
        if stop_player {
            player.stop();
        }
        restore_default_output(saved_default_output_uid);
        Some(player.playback_epoch())
    }

    pub(crate) fn capture_running(&self) -> bool {
        self.runtime.lock().unwrap().running
    }

    /// Keep zone discovery from marking the physical DAC offline during the
    /// temporary system-output handoff.
    pub(crate) fn quiet_local_device_refresh(&self) -> bool {
        let runtime = self.runtime.lock().unwrap();
        runtime.running
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
        let mut runtime = self.runtime.lock().unwrap();
        let generation = runtime.next_playback_generation.max(1);
        runtime.next_playback_generation = generation.wrapping_add(1).max(1);
        let snapshot = AppleMusicPlaybackSnapshot {
            zone_id,
            player_epoch,
            generation,
            duration_secs: source.duration_secs().unwrap_or(0.0),
            source,
            playback_state: "preparing".to_string(),
            timeline_origin_secs: 0.0,
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
        if let Some(position) = position_secs.filter(|value| value.is_finite() && *value >= 0.0) {
            snapshot.position_secs = position;
        }
        if let Some(duration) = duration_secs.filter(|value| value.is_finite() && *value > 0.0) {
            snapshot.duration_secs = duration;
        }
        true
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
        true
    }

    #[cfg(target_os = "macos")]
    fn restart_session(
        self: &Arc<Self>,
        rate_hz: u32,
        source_bit_depth: Option<u32>,
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

        let old_session = self.runtime.lock().unwrap().session.take();
        drop(old_session);
        player.stop();
        if params.rate_hz != rate_hz {
            let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID)
                .ok_or_else(|| "Fozmo Capture disappeared during the format switch.".to_string())?;
            rate_control::set_nominal_rate(device_id, rate_hz)?;
        }
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
        self.restart_session(rate_hz, source_bit_depth)
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
        self.restart_session(params.rate_hz, params.source_bit_depth)
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

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
