//! Apple Music live capture: routes Apple Music playback through the Fozmo
//! Capture HAL device and feeds the captured PCM into the existing player/DSP
//! engine as a live stream.
//!
//! Module layout:
//! - `coreaudio` — CoreAudio FFI helpers (device lookup, scalars, default output).
//! - `rate_control` — nominal-rate switching and Music track-rate detection.
//! - `live_source` — the live WAV `MediaSource` served to Symphonia.
//! - `capture_session` — CPAL streams and the live player session.

mod capture_session;
mod coreaudio;
mod live_source;
mod rate_control;

use crate::audio::player::Player;
use crate::settings::AppleMusicCaptureSettings;
use capture_session::{
    CaptureWorker, DiagnosticMetrics, LiveSessionParams, SessionControl, now_unix_ms,
};
use serde::{Deserialize, Serialize};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

pub const CAPTURE_DEVICE_NAME: &str = "Fozmo Capture";
const CAPTURE_DEVICE_UID: &str = "com.fozmo.audio.capture";
const CAPTURE_DEVICE_ONLY_MESSAGE: &str =
    "Apple Music capture can only use the Fozmo Capture virtual device.";
const PROP_RING_FILL_FRAMES: u32 = 0x7472_6666; // trff
const PROP_RING_FILL_MS: u32 = 0x7472_666d; // trfm
const PROP_BUFFER_FRAMES: u32 = 0x7472_6266; // trbf
const PROP_UNDERRUNS: u32 = 0x7472_756e; // trun
const PROP_OVERRUNS: u32 = 0x7472_6f76; // trov
const PROP_SNAPS: u32 = 0x7472_736e; // trsn
const PROP_LAST_RATE_CHANGE_MS: u32 = 0x7472_7263; // trrc
const PROP_LAST_START_MS: u32 = 0x7472_7374; // trst
const PROP_LAST_STOP_MS: u32 = 0x7472_7370; // trsp
const PROP_VERSION: u32 = 0x7472_7672; // trvr

#[derive(Debug, Clone, Serialize)]
pub struct AppleMusicCaptureStatus {
    pub supported: bool,
    pub feature_enabled: bool,
    pub platform: &'static str,
    pub driver_installed: bool,
    pub driver_loaded: bool,
    pub driver_version: Option<String>,
    pub capture_device_input_visible: bool,
    pub capture_device_output_visible: bool,
    pub capture_running: bool,
    pub capture_device_name: String,
    pub output_device_name: Option<String>,
    pub capture_rate_hz: Option<u32>,
    pub capture_format: &'static str,
    pub channels: u16,
    pub buffer_ms: u32,
    pub buffer_frames: Option<u32>,
    pub ring_fill_frames: u64,
    pub ring_fill_ms: f32,
    pub underruns: u64,
    pub overruns: u64,
    /// Bounded-latency input snaps performed by the driver (see HAL docs).
    pub snaps: u64,
    /// Times the live capture ring dropped a callback because the DSP side
    /// fell behind.
    pub capture_ring_overruns: u64,
    pub last_sample_rate_change_unix_ms: Option<u64>,
    pub last_io_start_unix_ms: Option<u64>,
    pub last_io_stop_unix_ms: Option<u64>,
    pub frames_received: u64,
    pub callbacks_received: u64,
    pub diagnostic_dropouts: u64,
    pub rms_l: f32,
    pub rms_r: f32,
    pub diagnostic_observed_rate_hz: Option<u32>,
    pub last_callback_unix_ms: Option<u64>,
    /// Sample rate Apple Music reported for the current track, when known.
    pub detected_track_rate_hz: Option<u32>,
    /// True while a debounced rate switch (stop → set rate → restart) runs.
    pub rate_switch_pending: bool,
    pub auto_route_system_output: bool,
    pub music_app_running: bool,
    pub music_app_player_state: Option<String>,
    pub music_app_track_title: Option<String>,
    pub music_app_track_artist: Option<String>,
    pub music_app_track_album: Option<String>,
    pub music_app_sound_volume: Option<u32>,
    pub music_app_message: Option<String>,
    pub warnings: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppleMusicCaptureSettingsPayload {
    pub enabled: bool,
    pub capture_device_name: Option<String>,
    pub output_device_name: Option<String>,
    pub buffer_ms: u32,
    pub auto_route_system_output: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleMusicCaptureSettingsUpdate {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub capture_device_name: Option<Option<String>>,
    #[serde(default)]
    pub output_device_name: Option<Option<String>>,
    #[serde(default)]
    pub buffer_ms: Option<u32>,
    #[serde(default)]
    pub auto_route_system_output: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleMusicCaptureRateRequest {
    pub rate_hz: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppleMusicCaptureDevices {
    pub capture_devices: Vec<AudioDeviceSummary>,
    pub output_devices: Vec<AudioDeviceSummary>,
    pub preferred_capture_device_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioDeviceSummary {
    pub name: String,
    pub is_default: bool,
    pub is_fozmo_capture: bool,
    pub sample_rates: Vec<u32>,
    pub channels: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartAppleMusicCaptureRequest {
    #[serde(default)]
    pub capture_device_name: Option<String>,
    #[serde(default)]
    pub output_device_name: Option<String>,
    #[serde(default)]
    pub confirm_system_audio_capture: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleMusicAppControlRequest {
    pub command: AppleMusicAppCommand,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppleMusicAppCommand {
    Open,
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppleMusicAppStatus {
    pub running: bool,
    pub player_state: Option<String>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct DriverTelemetry {
    version: Option<String>,
    nominal_rate_hz: Option<u32>,
    buffer_frames: Option<u32>,
    ring_fill_frames: Option<u64>,
    ring_fill_ms: Option<f32>,
    underruns: Option<u64>,
    overruns: Option<u64>,
    snaps: Option<u64>,
    last_rate_change_unix_ms: Option<u64>,
    last_start_unix_ms: Option<u64>,
    last_stop_unix_ms: Option<u64>,
}

/// Background Music-app poller: track/rate/volume detection while capture runs.
struct MusicPoller {
    stop_tx: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for MusicPoller {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct CaptureRuntime {
    running: bool,
    capture_device_name: Option<String>,
    output_device_name: Option<String>,
    started_unix_ms: Option<u64>,
    stopped_unix_ms: Option<u64>,
    diagnostic: Option<CaptureWorker>,
    session: Option<capture_session::LiveSession>,
    session_params: Option<LiveSessionParams>,
    poller: Option<MusicPoller>,
    metrics: Arc<DiagnosticMetrics>,
    control: Arc<SessionControl>,
    saved_default_output_uid: Option<String>,
}

impl Default for CaptureRuntime {
    fn default() -> Self {
        Self {
            running: false,
            capture_device_name: None,
            output_device_name: None,
            started_unix_ms: None,
            stopped_unix_ms: None,
            diagnostic: None,
            session: None,
            session_params: None,
            poller: None,
            metrics: Arc::new(DiagnosticMetrics::default()),
            control: Arc::new(SessionControl::new_unknown()),
            saved_default_output_uid: None,
        }
    }
}

pub struct AppleMusicCaptureService {
    player: Arc<Player>,
    runtime: Mutex<CaptureRuntime>,
}

impl AppleMusicCaptureService {
    pub fn new(player: Arc<Player>) -> Self {
        Self {
            player,
            runtime: Mutex::new(CaptureRuntime::default()),
        }
    }

    pub fn status(&self, settings: &AppleMusicCaptureSettings) -> AppleMusicCaptureStatus {
        let devices = device_snapshot();
        let (
            running,
            runtime_capture_device_name,
            runtime_output_device_name,
            started_unix_ms,
            stopped_unix_ms,
            session_rate_hz,
            metrics,
            control,
        ) = {
            let runtime = self.runtime.lock().unwrap();
            (
                runtime.running,
                runtime.capture_device_name.clone(),
                runtime.output_device_name.clone(),
                runtime.started_unix_ms,
                runtime.stopped_unix_ms,
                runtime.session.as_ref().map(|session| session.rate_hz()),
                runtime.metrics.snapshot(),
                Arc::clone(&runtime.control),
            )
        };
        let capture_device_name = runtime_capture_device_name
            .or_else(|| normalize_capture_device_name(settings.capture_device_name.as_deref()))
            .unwrap_or_else(|| CAPTURE_DEVICE_NAME.to_string());
        let capture_device = devices
            .capture_devices
            .iter()
            .find(|device| device.name == capture_device_name)
            .or_else(|| {
                devices
                    .capture_devices
                    .iter()
                    .find(|device| device.is_fozmo_capture)
            });
        let output_device = devices
            .output_devices
            .iter()
            .find(|device| device.name == capture_device_name)
            .or_else(|| {
                devices
                    .output_devices
                    .iter()
                    .find(|device| device.is_fozmo_capture)
            });
        let capture_device_input_visible = capture_device.is_some();
        let capture_device_output_visible = output_device.is_some();
        let driver_installed = capture_device_input_visible || capture_device_output_visible;
        let capture_rate_hz = capture_device.and_then(|device| {
            device
                .sample_rates
                .iter()
                .copied()
                .filter(|rate| *rate <= 192_000)
                .max()
                .or_else(|| device.sample_rates.first().copied())
        });
        let supported = platform_supported();
        let feature_enabled = feature_enabled();
        let music_app = self.music_app_status();
        let driver_telemetry = if driver_installed {
            driver_telemetry()
        } else {
            DriverTelemetry::default()
        };
        let message = if !feature_enabled {
            Some("Apple Music capture was compiled out. Enable the apple_music_capture Cargo feature.".to_string())
        } else if !supported {
            Some("Apple Music capture is available only on macOS.".to_string())
        } else if !driver_installed {
            Some(
                "Fozmo Capture HAL driver is not installed or is not visible to CoreAudio."
                    .to_string(),
            )
        } else {
            None
        };
        let detected_track_rate_hz = control.detected_rate();
        let rate_switch_pending = control.rate_switch_pending();
        let music_app_sound_volume = control.music_volume();
        let warnings = build_warnings(
            running,
            detected_track_rate_hz,
            music_app_sound_volume,
            music_app.player_state.as_deref(),
            control.poll_error(),
        );

        AppleMusicCaptureStatus {
            supported,
            feature_enabled,
            platform: std::env::consts::OS,
            driver_installed,
            driver_loaded: driver_installed,
            driver_version: driver_telemetry.version.clone(),
            capture_device_input_visible,
            capture_device_output_visible,
            capture_running: running,
            capture_device_name,
            output_device_name: runtime_output_device_name
                .or_else(|| settings.output_device_name.clone()),
            capture_rate_hz: session_rate_hz
                .or(metrics.observed_rate_hz)
                .or(driver_telemetry.nominal_rate_hz)
                .or(capture_rate_hz),
            capture_format: "f32",
            channels: capture_device.map(|device| device.channels).unwrap_or(2),
            buffer_ms: normalized_buffer_ms(settings.buffer_ms),
            buffer_frames: driver_telemetry.buffer_frames,
            ring_fill_frames: driver_telemetry.ring_fill_frames.unwrap_or(0),
            ring_fill_ms: driver_telemetry.ring_fill_ms.unwrap_or(0.0),
            underruns: driver_telemetry.underruns.unwrap_or(0),
            overruns: driver_telemetry.overruns.unwrap_or(0),
            snaps: driver_telemetry.snaps.unwrap_or(0),
            capture_ring_overruns: metrics.ring_overruns,
            last_sample_rate_change_unix_ms: driver_telemetry.last_rate_change_unix_ms,
            last_io_start_unix_ms: driver_telemetry.last_start_unix_ms.or(started_unix_ms),
            last_io_stop_unix_ms: driver_telemetry.last_stop_unix_ms.or(stopped_unix_ms),
            frames_received: metrics.frames_received,
            callbacks_received: metrics.callbacks_received,
            diagnostic_dropouts: metrics.dropouts,
            rms_l: metrics.rms_l,
            rms_r: metrics.rms_r,
            diagnostic_observed_rate_hz: metrics.observed_rate_hz,
            last_callback_unix_ms: metrics.last_callback_unix_ms,
            detected_track_rate_hz,
            rate_switch_pending,
            auto_route_system_output: settings.auto_route_system_output,
            music_app_running: music_app.running,
            music_app_player_state: music_app.player_state,
            music_app_track_title: music_app.track_title,
            music_app_track_artist: music_app.track_artist,
            music_app_track_album: music_app.track_album,
            music_app_sound_volume,
            music_app_message: music_app.message,
            warnings,
            message,
        }
    }

    pub fn settings_payload(
        &self,
        settings: &AppleMusicCaptureSettings,
    ) -> AppleMusicCaptureSettingsPayload {
        AppleMusicCaptureSettingsPayload {
            enabled: settings.enabled,
            capture_device_name: normalize_capture_device_name(
                settings.capture_device_name.as_deref(),
            ),
            output_device_name: normalize_optional(settings.output_device_name.as_deref()),
            buffer_ms: normalized_buffer_ms(settings.buffer_ms),
            auto_route_system_output: settings.auto_route_system_output,
        }
    }

    pub fn devices(&self) -> AppleMusicCaptureDevices {
        device_snapshot()
    }

    pub fn start(
        self: &Arc<Self>,
        settings: &AppleMusicCaptureSettings,
        request: StartAppleMusicCaptureRequest,
    ) -> Result<AppleMusicCaptureStatus, String> {
        if !feature_enabled() {
            return Err("Apple Music capture was compiled out.".to_string());
        }
        if !platform_supported() {
            return Err("Apple Music capture is available only on macOS.".to_string());
        }
        if !request.confirm_system_audio_capture {
            return Err(
                "Starting Apple Music capture requires explicit confirmation because it captures live macOS system audio."
                    .to_string(),
            );
        }
        #[cfg(target_os = "macos")]
        {
            self.start_macos(settings, request)?;
            Ok(self.status(settings))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            Err("Apple Music capture is available only on macOS.".to_string())
        }
    }

    #[cfg(target_os = "macos")]
    fn start_macos(
        self: &Arc<Self>,
        settings: &AppleMusicCaptureSettings,
        request: StartAppleMusicCaptureRequest,
    ) -> Result<(), String> {
        let devices = device_snapshot();
        let capture_device_name = resolve_capture_device_name(
            &devices,
            request.capture_device_name.as_deref(),
            settings.capture_device_name.as_deref(),
        )?;
        self.guard_against_feedback_loop()?;

        let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID).ok_or_else(|| {
            "Fozmo Capture HAL driver is not visible to CoreAudio. Install the driver first."
                .to_string()
        })?;

        // Auto-route: remember the user's default output so stop() can restore
        // it, then point macOS (and therefore Apple Music) at Fozmo Capture.
        let mut saved_default_output_uid = None;
        if settings.auto_route_system_output {
            let current_default = coreaudio::default_output_device_uid();
            if current_default.as_deref() != Some(CAPTURE_DEVICE_UID) {
                saved_default_output_uid = current_default;
            }
            coreaudio::set_default_output_device(device_id)
                .map_err(|err| format!("Could not route macOS output to Fozmo Capture: {err}"))?;
        }

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

        let metrics = Arc::new(DiagnosticMetrics::default());
        let params = LiveSessionParams {
            device_name: capture_device_name.clone(),
            rate_hz,
            buffer_ms: normalized_buffer_ms(settings.buffer_ms),
        };
        let session =
            capture_session::start_live_session(&self.player, &params, Arc::clone(&metrics))
                .inspect_err(|_| restore_on_error(&saved_default_output_uid))?;

        let control = Arc::new(SessionControl::new_unknown());
        let poller = self.spawn_music_poller(Arc::clone(&control));

        let previous = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = true;
            runtime.capture_device_name = Some(capture_device_name);
            runtime.output_device_name = request
                .output_device_name
                .and_then(|name| normalize_optional(Some(&name)))
                .or_else(|| normalize_optional(settings.output_device_name.as_deref()));
            runtime.started_unix_ms = Some(now_unix_ms());
            runtime.metrics = metrics;
            runtime.control = control;
            runtime.session_params = Some(params);
            runtime.saved_default_output_uid = saved_default_output_uid;
            (
                runtime.session.replace(session),
                runtime.poller.replace(poller),
                runtime.diagnostic.take(),
            )
        };
        // Old session/poller/diagnostic (if a capture was already running)
        // shut down outside the lock — their Drop impls join worker threads.
        drop(previous);
        Ok(())
    }

    /// Hard capture-output guard: the local zone must use a local physical
    /// CoreAudio output. This prevents feedback through the capture device and
    /// prevents live system audio from being routed to a network sink.
    fn guard_against_feedback_loop(&self) -> Result<(), String> {
        match self.player.selected_device_name() {
            None => Err(
                "The local Fozmo zone is set to the system-default output. Select an explicit physical output device for the local zone before starting Apple Music capture."
                    .to_string(),
            ),
            Some(name) if name.trim() == CAPTURE_DEVICE_NAME => Err(
                "The local Fozmo zone is set to Fozmo Capture, which would create a feedback loop. Select a physical output device first."
                    .to_string(),
            ),
            Some(name) if is_remote_or_virtual_output_name(&name) => Err(
                "Apple Music capture can only play through a local physical CoreAudio output. Select a built-in, USB, HDMI, DisplayPort, Thunderbolt, PCI, or FireWire output before starting capture."
                    .to_string(),
            ),
            Some(name) if !selected_output_is_local_physical(&name) => Err(
                "Apple Music capture could not verify that the selected output is a local physical CoreAudio device. Select a physical output before starting capture."
                    .to_string(),
            ),
            Some(_) => Ok(()),
        }
    }

    pub fn stop(&self, settings: &AppleMusicCaptureSettings) -> AppleMusicCaptureStatus {
        let (session, poller, diagnostic, saved_default_output_uid) = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = false;
            runtime.stopped_unix_ms = Some(now_unix_ms());
            runtime.session_params = None;
            (
                runtime.session.take(),
                runtime.poller.take(),
                runtime.diagnostic.take(),
                runtime.saved_default_output_uid.take(),
            )
        };
        // Drop outside the lock: poller/worker Drop impls join their threads.
        drop(poller);
        drop(session);
        drop(diagnostic);
        self.player.stop();
        restore_default_output(saved_default_output_uid);
        self.status(settings)
    }

    /// Manual rate override for streams where Apple Music does not report a
    /// sample rate. Running capture is restarted at the new rate; otherwise
    /// only the driver's nominal rate changes.
    pub fn set_manual_rate(self: &Arc<Self>, rate_hz: u32) -> Result<(), String> {
        if !platform_supported() {
            return Err("Apple Music capture is available only on macOS.".to_string());
        }
        if !rate_control::is_supported_capture_rate(rate_hz) {
            return Err(format!(
                "{rate_hz} Hz is not a supported Fozmo Capture rate. Supported: {:?}.",
                rate_control::SUPPORTED_CAPTURE_RATES
            ));
        }
        #[cfg(target_os = "macos")]
        {
            let running = self.runtime.lock().unwrap().running;
            if running {
                self.perform_rate_switch(rate_hz)
            } else {
                let device_id =
                    coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID).ok_or_else(|| {
                        "Fozmo Capture HAL driver is not visible to CoreAudio.".to_string()
                    })?;
                rate_control::set_nominal_rate(device_id, rate_hz)
            }
        }
        #[cfg(not(target_os = "macos"))]
        Err("Apple Music capture is available only on macOS.".to_string())
    }

    /// Debounced rate switch: end the live session, apply the driver rate via
    /// the config-change handshake, then reopen capture and a fresh session.
    #[cfg(target_os = "macos")]
    fn perform_rate_switch(self: &Arc<Self>, rate_hz: u32) -> Result<(), String> {
        let (params, control) = {
            let runtime = self.runtime.lock().unwrap();
            if !runtime.running {
                return Err("Apple Music capture is not running.".to_string());
            }
            let Some(params) = runtime.session_params.clone() else {
                return Err("Apple Music capture has no active session.".to_string());
            };
            (params, Arc::clone(&runtime.control))
        };
        if params.rate_hz == rate_hz {
            return Ok(());
        }
        control.set_rate_switch_pending(true);
        let result = (|| {
            let old_session = self.runtime.lock().unwrap().session.take();
            drop(old_session);
            self.player.stop();
            let device_id = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID)
                .ok_or_else(|| "Fozmo Capture disappeared during the rate switch.".to_string())?;
            rate_control::set_nominal_rate(device_id, rate_hz)?;
            let params = LiveSessionParams { rate_hz, ..params };
            let metrics = Arc::new(DiagnosticMetrics::default());
            let session =
                capture_session::start_live_session(&self.player, &params, Arc::clone(&metrics))?;
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.running {
                runtime.session = Some(session);
                runtime.session_params = Some(params);
                runtime.metrics = metrics;
            }
            Ok(())
        })();
        control.set_rate_switch_pending(false);
        if let Err(err) = &result {
            control.set_poll_error(Some(format!("Rate switch to {rate_hz} Hz failed: {err}")));
            let mut runtime = self.runtime.lock().unwrap();
            runtime.running = runtime.session.is_some();
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn spawn_music_poller(self: &Arc<Self>, control: Arc<SessionControl>) -> MusicPoller {
        use std::time::Duration;

        let weak = Arc::downgrade(self);
        let (stop_tx, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("fozmo-music-poller".to_string())
            .spawn(move || {
                let mut debounce = rate_control::RateSwitchDebounce::default();
                loop {
                    match stop_rx.recv_timeout(Duration::from_millis(1000)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let Some(service) = weak.upgrade() else {
                        break;
                    };
                    if !music_app_running() {
                        control.observe_music_gone();
                        continue;
                    }
                    match run_osascript(rate_control::MUSIC_POLL_SCRIPT) {
                        Ok(output) => {
                            let info = rate_control::parse_music_track_info(&output);
                            control.observe_track_info(
                                info.sample_rate_hz,
                                info.sound_volume,
                                info.is_playing(),
                            );
                            if let Some(desired) = debounce.desired_rate_on_track_change(&info) {
                                let current = service
                                    .runtime
                                    .lock()
                                    .unwrap()
                                    .session
                                    .as_ref()
                                    .map(|session| session.rate_hz());
                                if current.is_some_and(|rate| rate != desired) {
                                    // Errors are recorded on the control state
                                    // and surfaced through status warnings.
                                    let _ = service.perform_rate_switch(desired);
                                }
                            }
                        }
                        Err(err) => control.set_poll_error(Some(format!(
                            "Could not poll the Apple Music app: {err}"
                        ))),
                    }
                }
            })
            .ok();
        MusicPoller {
            stop_tx: Some(stop_tx),
            worker,
        }
    }

    pub fn music_app_status(&self) -> AppleMusicAppStatus {
        music_app_status()
    }

    pub fn control_music_app(
        &self,
        command: AppleMusicAppCommand,
    ) -> Result<AppleMusicAppStatus, String> {
        if !platform_supported() {
            return Err("Apple Music app control is available only on macOS.".to_string());
        }
        match command {
            AppleMusicAppCommand::Open => open_music_app()?,
            AppleMusicAppCommand::PlayPause => run_music_app_script("playpause")?,
            AppleMusicAppCommand::Play => run_music_app_script("play")?,
            AppleMusicAppCommand::Pause => run_music_app_script("pause")?,
            AppleMusicAppCommand::Next => run_music_app_script("next track")?,
            AppleMusicAppCommand::Previous => run_music_app_script("previous track")?,
        }
        Ok(self.music_app_status())
    }
}

impl Drop for AppleMusicCaptureService {
    fn drop(&mut self) {
        // Graceful shutdown: put the user's default output back.
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

fn build_warnings(
    running: bool,
    detected_track_rate_hz: Option<u32>,
    music_volume: Option<u32>,
    music_player_state: Option<&str>,
    poll_error: Option<String>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if !running {
        return warnings;
    }
    if let Some(volume) = music_volume
        && volume != 100
    {
        warnings.push(format!(
            "Apple Music volume is {volume}%. Set it to 100% for bit-perfect capture."
        ));
    }
    if detected_track_rate_hz.is_none() && music_player_state == Some("playing") {
        warnings.push(
            "Apple Music did not report the current track's sample rate (common for streaming). Capture continues at the driver rate; use the manual rate override for hi-res streams."
                .to_string(),
        );
    }
    if let Some(error) = poll_error {
        warnings.push(error);
    }
    warnings
}

pub fn apply_settings_update(
    settings: &mut AppleMusicCaptureSettings,
    update: AppleMusicCaptureSettingsUpdate,
) {
    if let Some(enabled) = update.enabled {
        settings.enabled = enabled;
    }
    if let Some(capture_device_name) = update.capture_device_name {
        settings.capture_device_name =
            normalize_capture_device_name(capture_device_name.as_deref());
    }
    if let Some(output_device_name) = update.output_device_name {
        settings.output_device_name = normalize_optional(output_device_name.as_deref());
    }
    if let Some(buffer_ms) = update.buffer_ms {
        settings.buffer_ms = normalized_buffer_ms(buffer_ms);
    }
    if let Some(auto_route) = update.auto_route_system_output {
        settings.auto_route_system_output = auto_route;
    }
}

pub fn sanitize_settings(settings: &mut AppleMusicCaptureSettings) {
    settings.capture_device_name =
        normalize_capture_device_name(settings.capture_device_name.as_deref());
    settings.buffer_ms = normalized_buffer_ms(settings.buffer_ms);
}

fn music_app_status() -> AppleMusicAppStatus {
    if !cfg!(target_os = "macos") {
        return AppleMusicAppStatus {
            running: false,
            player_state: None,
            track_title: None,
            track_artist: None,
            track_album: None,
            message: Some("Apple Music app control is available only on macOS.".to_string()),
        };
    }

    let running = music_app_running();
    if !running {
        return AppleMusicAppStatus {
            running,
            player_state: None,
            track_title: None,
            track_artist: None,
            track_album: None,
            message: Some("Apple Music app is not running.".to_string()),
        };
    }

    match run_osascript(&[
        "tell application \"Music\"",
        "set playbackState to player state as string",
        "set trackName to \"\"",
        "set artistName to \"\"",
        "set albumName to \"\"",
        "if player state is not stopped then",
        "set trackName to name of current track",
        "set artistName to artist of current track",
        "set albumName to album of current track",
        "end if",
        "return playbackState & linefeed & trackName & linefeed & artistName & linefeed & albumName",
        "end tell",
    ]) {
        Ok(output) => {
            let mut lines = output.lines();
            AppleMusicAppStatus {
                running,
                player_state: normalize_optional(lines.next()),
                track_title: normalize_optional(lines.next()),
                track_artist: normalize_optional(lines.next()),
                track_album: normalize_optional(lines.next()),
                message: None,
            }
        }
        Err(message) => AppleMusicAppStatus {
            running,
            player_state: None,
            track_title: None,
            track_artist: None,
            track_album: None,
            message: Some(message),
        },
    }
}

fn music_app_running() -> bool {
    ProcessCommand::new("pgrep")
        .args(["-x", "Music"])
        .status()
        .is_ok_and(|status| status.success())
}

fn open_music_app() -> Result<(), String> {
    ProcessCommand::new("open")
        .args(["-a", "Music"])
        .status()
        .map_err(|err| format!("Failed to open Apple Music app: {err}"))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err("Apple Music app did not open successfully.".to_string())
            }
        })
}

fn run_music_app_script(command: &str) -> Result<(), String> {
    run_osascript(&["tell application \"Music\"", command, "end tell"]).map(|_| ())
}

fn run_osascript(lines: &[&str]) -> Result<String, String> {
    let mut command = ProcessCommand::new("osascript");
    for line in lines {
        command.arg("-e").arg(line);
    }
    let output = command
        .output()
        .map_err(|err| format!("Failed to talk to Apple Music app: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "Apple Music app command failed.".to_string()
        } else {
            stderr
        })
    }
}

fn device_snapshot() -> AppleMusicCaptureDevices {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let default_input_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let default_output_name = host
        .default_output_device()
        .and_then(|device| device.name().ok());
    let capture_devices = host
        .input_devices()
        .ok()
        .map(|devices| {
            devices
                .filter_map(|device| {
                    audio_device_summary(device, default_input_name.as_deref(), true)
                })
                .filter(is_allowed_capture_device)
                .collect()
        })
        .unwrap_or_default();
    let output_devices = host
        .output_devices()
        .ok()
        .map(|devices| {
            devices
                .filter_map(|device| {
                    audio_device_summary(device, default_output_name.as_deref(), false)
                })
                .collect()
        })
        .unwrap_or_default();

    AppleMusicCaptureDevices {
        capture_devices,
        output_devices,
        preferred_capture_device_name: CAPTURE_DEVICE_NAME.to_string(),
    }
}

fn audio_device_summary(
    device: cpal::Device,
    default_name: Option<&str>,
    input: bool,
) -> Option<AudioDeviceSummary> {
    use cpal::traits::DeviceTrait;

    let name = device.name().ok()?;
    let is_default = default_name.is_some_and(|default_name| default_name == name);
    let is_fozmo_capture = name.trim() == CAPTURE_DEVICE_NAME;
    let mut sample_rates = Vec::new();
    let mut channels = 0;
    if input {
        if let Ok(configs) = device.supported_input_configs() {
            collect_config_summaries(configs, &mut sample_rates, &mut channels);
        }
    } else if let Ok(configs) = device.supported_output_configs() {
        collect_config_summaries(configs, &mut sample_rates, &mut channels);
    }
    sample_rates.sort_unstable();
    sample_rates.dedup();

    Some(AudioDeviceSummary {
        name,
        is_default,
        is_fozmo_capture,
        sample_rates,
        channels,
    })
}

fn is_allowed_capture_device(device: &AudioDeviceSummary) -> bool {
    is_allowed_capture_device_name(&device.name) && fozmo_capture_uid_matches_name(&device.name)
}

fn is_allowed_capture_device_name(name: &str) -> bool {
    name.trim() == CAPTURE_DEVICE_NAME
}

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

fn resolve_capture_device_name(
    devices: &AppleMusicCaptureDevices,
    requested_name: Option<&str>,
    settings_name: Option<&str>,
) -> Result<String, String> {
    resolve_capture_device_name_with(
        devices,
        requested_name,
        settings_name,
        is_allowed_capture_device,
    )
}

fn resolve_capture_device_name_with(
    devices: &AppleMusicCaptureDevices,
    requested_name: Option<&str>,
    settings_name: Option<&str>,
    is_allowed: impl Fn(&AudioDeviceSummary) -> bool,
) -> Result<String, String> {
    let requested_capture_device_name = normalize_optional(requested_name);
    if requested_capture_device_name
        .as_deref()
        .is_some_and(|name| !is_allowed_capture_device_name(name))
    {
        return Err(CAPTURE_DEVICE_ONLY_MESSAGE.to_string());
    }
    let capture_device_name = requested_capture_device_name
        .or_else(|| normalize_capture_device_name(settings_name))
        .unwrap_or_else(|| CAPTURE_DEVICE_NAME.to_string());
    let approved_capture_device = devices
        .capture_devices
        .iter()
        .find(|device| device.name == capture_device_name && is_allowed(device))
        .or_else(|| {
            devices
                .capture_devices
                .iter()
                .find(|device| is_allowed(device))
        });
    approved_capture_device
        .map(|device| device.name.clone())
        .ok_or_else(|| {
            format!(
                "{CAPTURE_DEVICE_NAME} is not visible as an input device. Install the Fozmo Capture HAL driver first."
            )
        })
}

fn collect_config_summaries<I>(configs: I, sample_rates: &mut Vec<u32>, channels: &mut u16)
where
    I: IntoIterator<Item = cpal::SupportedStreamConfigRange>,
{
    for config in configs {
        sample_rates.push(config.min_sample_rate().0);
        sample_rates.push(config.max_sample_rate().0);
        *channels = (*channels).max(config.channels());
    }
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalize_capture_device_name(value: Option<&str>) -> Option<String> {
    normalize_optional(value).filter(|name| is_allowed_capture_device_name(name))
}

#[cfg(not(target_os = "macos"))]
fn fozmo_capture_uid_matches_name(_name: &str) -> bool {
    true
}

#[cfg(target_os = "macos")]
fn fozmo_capture_uid_matches_name(name: &str) -> bool {
    coreaudio::device_name_for_uid(CAPTURE_DEVICE_UID)
        .is_some_and(|device_name| device_name == name)
}

#[cfg(not(target_os = "macos"))]
fn driver_telemetry() -> DriverTelemetry {
    DriverTelemetry::default()
}

#[cfg(target_os = "macos")]
fn driver_telemetry() -> DriverTelemetry {
    let Some(device_id) = coreaudio::device_id_for_uid(CAPTURE_DEVICE_UID) else {
        return DriverTelemetry::default();
    };
    DriverTelemetry {
        version: coreaudio::read_cf_string(device_id, PROP_VERSION),
        nominal_rate_hz: coreaudio::read_f64(
            device_id,
            coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
        )
        .map(|rate| rate.round().max(0.0) as u32),
        buffer_frames: coreaudio::read_u32(device_id, PROP_BUFFER_FRAMES),
        ring_fill_frames: coreaudio::read_u64(device_id, PROP_RING_FILL_FRAMES),
        ring_fill_ms: coreaudio::read_f64(device_id, PROP_RING_FILL_MS).map(|value| value as f32),
        underruns: coreaudio::read_u64(device_id, PROP_UNDERRUNS),
        overruns: coreaudio::read_u64(device_id, PROP_OVERRUNS),
        snaps: coreaudio::read_u64(device_id, PROP_SNAPS),
        last_rate_change_unix_ms: coreaudio::read_u64(device_id, PROP_LAST_RATE_CHANGE_MS)
            .and_then(nonzero_u64),
        last_start_unix_ms: coreaudio::read_u64(device_id, PROP_LAST_START_MS)
            .and_then(nonzero_u64),
        last_stop_unix_ms: coreaudio::read_u64(device_id, PROP_LAST_STOP_MS).and_then(nonzero_u64),
    }
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn normalized_buffer_ms(buffer_ms: u32) -> u32 {
    if buffer_ms == 0 {
        250
    } else {
        buffer_ms.clamp(50, 2_000)
    }
}

fn feature_enabled() -> bool {
    cfg!(feature = "apple_music_capture")
}

fn platform_supported() -> bool {
    cfg!(all(target_os = "macos", feature = "apple_music_capture"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str) -> AudioDeviceSummary {
        AudioDeviceSummary {
            name: name.to_string(),
            is_default: false,
            is_fozmo_capture: name == CAPTURE_DEVICE_NAME,
            sample_rates: vec![48_000],
            channels: 2,
        }
    }

    fn devices(names: &[&str]) -> AppleMusicCaptureDevices {
        AppleMusicCaptureDevices {
            capture_devices: names.iter().map(|name| device(name)).collect(),
            output_devices: Vec::new(),
            preferred_capture_device_name: CAPTURE_DEVICE_NAME.to_string(),
        }
    }

    fn fake_allowed(device: &AudioDeviceSummary) -> bool {
        is_allowed_capture_device_name(&device.name)
    }

    #[test]
    fn settings_update_normalizes_strings_and_buffer() {
        let mut settings = AppleMusicCaptureSettings::default();

        apply_settings_update(
            &mut settings,
            AppleMusicCaptureSettingsUpdate {
                enabled: Some(true),
                capture_device_name: Some(Some(" ".to_string())),
                buffer_ms: Some(10_000),
                output_device_name: None,
                auto_route_system_output: None,
            },
        );

        assert!(settings.enabled);
        assert_eq!(settings.capture_device_name, None);
        assert_eq!(settings.buffer_ms, 2_000);
        assert!(!settings.auto_route_system_output);
    }

    #[test]
    fn settings_update_toggles_auto_route() {
        let mut settings = AppleMusicCaptureSettings::default();
        assert!(!settings.auto_route_system_output);

        apply_settings_update(
            &mut settings,
            AppleMusicCaptureSettingsUpdate {
                enabled: None,
                capture_device_name: None,
                output_device_name: None,
                buffer_ms: None,
                auto_route_system_output: Some(true),
            },
        );

        assert!(settings.auto_route_system_output);
    }

    #[test]
    fn settings_update_does_not_persist_non_fozmo_capture_device() {
        let mut settings = AppleMusicCaptureSettings {
            capture_device_name: Some(CAPTURE_DEVICE_NAME.to_string()),
            ..AppleMusicCaptureSettings::default()
        };

        apply_settings_update(
            &mut settings,
            AppleMusicCaptureSettingsUpdate {
                enabled: None,
                capture_device_name: Some(Some("Built-in Microphone".to_string())),
                output_device_name: None,
                buffer_ms: None,
                auto_route_system_output: None,
            },
        );

        assert_eq!(settings.capture_device_name, None);
    }

    #[test]
    fn settings_update_allows_fozmo_capture_device() {
        let mut settings = AppleMusicCaptureSettings::default();

        apply_settings_update(
            &mut settings,
            AppleMusicCaptureSettingsUpdate {
                enabled: None,
                capture_device_name: Some(Some(format!(" {CAPTURE_DEVICE_NAME} "))),
                output_device_name: None,
                buffer_ms: None,
                auto_route_system_output: None,
            },
        );

        assert_eq!(
            settings.capture_device_name.as_deref(),
            Some(CAPTURE_DEVICE_NAME)
        );
    }

    #[test]
    fn capture_selection_rejects_explicit_non_fozmo_request() {
        let devices = devices(&[CAPTURE_DEVICE_NAME, "Built-in Microphone"]);

        let err = resolve_capture_device_name_with(
            &devices,
            Some("Built-in Microphone"),
            None,
            fake_allowed,
        )
        .expect_err("non-Fozmo request should fail");

        assert_eq!(err, CAPTURE_DEVICE_ONLY_MESSAGE);
    }

    #[test]
    fn capture_selection_ignores_stale_non_fozmo_setting() {
        let devices = devices(&[CAPTURE_DEVICE_NAME, "Built-in Microphone"]);

        let selected = resolve_capture_device_name_with(
            &devices,
            None,
            Some("Built-in Microphone"),
            fake_allowed,
        )
        .expect("stale setting should fall back to Fozmo Capture");

        assert_eq!(selected, CAPTURE_DEVICE_NAME);
    }

    #[test]
    fn capture_selection_requires_approved_fozmo_device() {
        let devices = devices(&["Built-in Microphone"]);

        let err = resolve_capture_device_name_with(&devices, None, None, fake_allowed)
            .expect_err("missing Fozmo Capture should fail");

        assert!(err.contains(CAPTURE_DEVICE_NAME));
    }

    #[test]
    fn capture_output_guard_rejects_remote_and_virtual_outputs() {
        assert!(is_remote_or_virtual_output_name("AirPlay Helper:attacker"));
        assert!(is_remote_or_virtual_output_name("Sonos UPnP:attacker"));
        assert!(is_remote_or_virtual_output_name(
            "UPnP AV Renderer:attacker"
        ));
        assert!(is_remote_or_virtual_output_name(CAPTURE_DEVICE_NAME));
        assert!(!is_remote_or_virtual_output_name("Built-in Output"));
    }

    #[test]
    fn capture_start_request_requires_explicit_confirmation() {
        let unconfirmed: StartAppleMusicCaptureRequest =
            serde_json::from_value(serde_json::json!({
                "capture_device_name": CAPTURE_DEVICE_NAME
            }))
            .unwrap();
        assert!(!unconfirmed.confirm_system_audio_capture);

        let confirmed: StartAppleMusicCaptureRequest = serde_json::from_value(serde_json::json!({
            "confirm_system_audio_capture": true
        }))
        .unwrap();
        assert!(confirmed.confirm_system_audio_capture);
    }

    #[test]
    fn warnings_flag_low_music_volume_and_unknown_rate() {
        let warnings = build_warnings(true, None, Some(80), Some("playing"), None);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("80%"));
        assert!(warnings[1].contains("did not report"));
    }

    #[test]
    fn warnings_empty_when_not_running() {
        assert!(build_warnings(false, None, Some(50), Some("playing"), None).is_empty());
    }
}
