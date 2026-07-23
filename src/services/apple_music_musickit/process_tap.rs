//! Provisional Music.app process tap used to evaluate the Fozmo DSP path
//! without requiring a MusicKit provisioning profile.
//!
//! Core Audio owns the real-time callback thread. The callback performs no
//! allocation, locking, logging, or IPC: it only updates atomics and writes
//! stereo F32 samples into the existing lock-free live-source ring.

use super::live_source::{
    CaptureProducer, LIVE_CHANNELS, LIVE_SAMPLE_CONTAINER_BITS, LIVE_SAMPLE_PRECISION_BITS,
    LiveCaptureSource, live_capture_ring, ring_capacity_samples,
};
use super::model::{AppleMusicMvpError, AppleMusicProcessTapMetrics, AppleMusicProcessTapStatus};
use crate::audio::player::{Player, TrackTags};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

const PROCESS_TAP_BUFFER_MS: u32 = 500;
const LIVE_DISPLAY_NAME: &str = "Apple Music (Music app tap)";
const LAYOUT_INTERLEAVED: u32 = 0;
const LAYOUT_PLANAR: u32 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct NativeTapInfo {
    pid: i32,
    process_object_id: u32,
    tap_object_id: u32,
    aggregate_device_id: u32,
    sample_rate_hz: f64,
    channels: u32,
    format_flags: u32,
    interleaved: u32,
    bits_per_channel: u32,
    bytes_per_frame: u32,
    format_settable_known: u32,
    format_settable: u32,
}

type NativeAudioCallback = unsafe extern "C" fn(
    buffer0: *const f32,
    buffer1: *const f32,
    frames: u32,
    layout: u32,
    host_time: u64,
    context: *mut c_void,
);

unsafe extern "C" {
    fn fozmo_process_tap_supported() -> u32;
    fn fozmo_music_app_pid() -> i32;
    fn fozmo_process_tap_create(
        pid: i32,
        mute_original: u32,
        out_info: *mut NativeTapInfo,
        out_status: *mut i32,
        out_stage: *mut u32,
    ) -> *mut c_void;
    fn fozmo_process_tap_start(
        handle: *mut c_void,
        callback: Option<NativeAudioCallback>,
        context: *mut c_void,
        out_stage: *mut u32,
    ) -> i32;
    fn fozmo_process_tap_stop(handle: *mut c_void);
}

#[derive(Default)]
struct TapMetrics {
    callbacks_received: AtomicU64,
    frames_received: AtomicU64,
    ring_overruns: AtomicU64,
    invalid_callbacks: AtomicU64,
    rms_l_bits: AtomicU32,
    rms_r_bits: AtomicU32,
    last_callback_host_time: AtomicU64,
}

impl TapMetrics {
    fn snapshot(&self) -> AppleMusicProcessTapMetrics {
        AppleMusicProcessTapMetrics {
            callbacks_received: self.callbacks_received.load(Ordering::Relaxed),
            frames_received: self.frames_received.load(Ordering::Relaxed),
            ring_overruns: self.ring_overruns.load(Ordering::Relaxed),
            invalid_callbacks: self.invalid_callbacks.load(Ordering::Relaxed),
            rms_l: f32::from_bits(self.rms_l_bits.load(Ordering::Relaxed)),
            rms_r: f32::from_bits(self.rms_r_bits.load(Ordering::Relaxed)),
            last_callback_age_ms: host_time_age_ms(
                self.last_callback_host_time.load(Ordering::Relaxed),
            ),
        }
    }
}

struct CallbackContext {
    producer: CaptureProducer,
    metrics: Arc<TapMetrics>,
}

struct NativeTap(NonNull<c_void>);

// The handle is an opaque Core Audio registration. All access is serialized
// through ProcessTapController, while the IOProc only touches CallbackContext.
unsafe impl Send for NativeTap {}

impl NativeTap {
    fn create(pid: i32, mute_original: bool) -> Result<(Self, NativeTapInfo), TapFailure> {
        let mut info = NativeTapInfo::default();
        let mut status = 0;
        let mut stage = 0;
        let handle = unsafe {
            fozmo_process_tap_create(
                pid,
                u32::from(mute_original),
                &mut info,
                &mut status,
                &mut stage,
            )
        };
        let handle = NonNull::new(handle).ok_or(TapFailure { status, stage })?;
        Ok((Self(handle), info))
    }

    fn start(&self, context: *mut CallbackContext) -> Result<(), TapFailure> {
        let mut stage = 0;
        let status = unsafe {
            fozmo_process_tap_start(
                self.0.as_ptr(),
                Some(process_tap_callback),
                context.cast(),
                &mut stage,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(TapFailure { status, stage })
        }
    }
}

impl Drop for NativeTap {
    fn drop(&mut self) {
        unsafe {
            fozmo_process_tap_stop(self.0.as_ptr());
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TapFailure {
    status: i32,
    stage: u32,
}

struct ProcessTapSession {
    // Must be stopped before callback_context is freed.
    native: Option<NativeTap>,
    _callback_context: Box<CallbackContext>,
    shutdown: Arc<AtomicBool>,
    metrics: Arc<TapMetrics>,
    info: NativeTapInfo,
    player: Arc<Player>,
    playback_epoch: u64,
    mute_original: bool,
}

impl ProcessTapSession {
    fn start(
        player: Arc<Player>,
        pid: i32,
        mute_original: bool,
    ) -> Result<Self, AppleMusicMvpError> {
        let (native, info) = NativeTap::create(pid, mute_original).map_err(process_tap_error)?;
        let rate_hz = validated_rate(info.sample_rate_hz)?;
        if info.channels != u32::from(LIVE_CHANNELS) {
            return Err(mvp_error(
                "process_tap_format_unsupported",
                format!(
                    "The Music app tap exposed {} channels; this experiment requires stereo.",
                    info.channels
                ),
                false,
                "tap_format",
                true,
            ));
        }

        let capacity = ring_capacity_samples(rate_hz, PROCESS_TAP_BUFFER_MS);
        let (producer, consumer) = live_capture_ring(capacity);
        let metrics = Arc::new(TapMetrics::default());
        let mut callback_context = Box::new(CallbackContext {
            producer,
            metrics: Arc::clone(&metrics),
        });
        native
            .start(callback_context.as_mut())
            .map_err(process_tap_error)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let source = LiveCaptureSource::new(rate_hz, consumer, Arc::clone(&shutdown));
        let tags = TrackTags {
            title: Some(LIVE_DISPLAY_NAME.to_string()),
            artist: Some("Apple Music".to_string()),
            sample_rate: Some(rate_hz),
            channels: Some(LIVE_CHANNELS),
            // This describes the native tap container. Core Audio does not
            // expose the Apple Music asset's original integer bit depth.
            bits_per_sample: Some(LIVE_SAMPLE_CONTAINER_BITS),
            ..TrackTags::default()
        };
        let epoch = player.reserve_playback_change();
        let started = player.play_stream_if_epoch(
            epoch,
            Box::new(source),
            Some("wav".to_string()),
            LIVE_DISPLAY_NAME.to_string(),
            None,
            Some(tags),
            Vec::new(),
        );
        if !started {
            shutdown.store(true, Ordering::Release);
            return Err(mvp_error(
                "process_tap_playback_changed",
                "Fozmo playback changed while the Music app tap was starting.",
                true,
                "dsp_handoff",
                true,
            ));
        }
        let playback_epoch = player.playback_epoch();

        Ok(Self {
            native: Some(native),
            _callback_context: callback_context,
            shutdown,
            metrics,
            info,
            player,
            playback_epoch,
            mute_original,
        })
    }

    fn owns_player_session(&self) -> bool {
        self.player.playback_epoch() == self.playback_epoch
    }

    fn status(&self, music_app_pid: Option<u32>) -> AppleMusicProcessTapStatus {
        AppleMusicProcessTapStatus {
            supported: process_tap_supported(),
            minimum_macos_version: "14.2".to_string(),
            state: "running".to_string(),
            music_app_running: music_app_pid.is_some(),
            music_app_pid,
            audio_process_object_id: nonzero(self.info.process_object_id),
            tap_object_id: nonzero(self.info.tap_object_id),
            aggregate_device_id: nonzero(self.info.aggregate_device_id),
            sample_rate_hz: Some(self.info.sample_rate_hz.round() as u32),
            channels: Some(self.info.channels),
            interleaved: Some(self.info.interleaved != 0),
            sample_format: Some("pcm_f32".to_string()),
            sample_container_bits: Some(self.info.bits_per_channel),
            sample_precision_bits: Some(LIVE_SAMPLE_PRECISION_BITS),
            source_bit_depth_bits: None,
            format_settable: (self.info.format_settable_known != 0)
                .then_some(self.info.format_settable != 0),
            sample_values_preserved: true,
            original_audio_muted_while_tapped: self.mute_original,
            dsp_handoff_active: self.owns_player_session(),
            output_device: self.player.selected_device_name(),
            metrics: self.metrics.snapshot(),
            last_error: None,
        }
    }
}

impl Drop for ProcessTapSession {
    fn drop(&mut self) {
        // Reverse the start order: stop/destroy Core Audio first so its
        // callback can no longer touch the ring, then let the live source EOF.
        drop(self.native.take());
        self.shutdown.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct ProcessTapController {
    session: Option<ProcessTapSession>,
    last_error: Option<AppleMusicMvpError>,
}

impl ProcessTapController {
    pub(super) fn status(&mut self) -> AppleMusicProcessTapStatus {
        if self
            .session
            .as_ref()
            .is_some_and(|session| !session.owns_player_session())
        {
            // Another Fozmo source replaced the live stream. Stop reading the
            // tap immediately so Music.app's direct path is restored.
            self.session.take();
        }

        let music_app_pid = music_app_pid();
        if let Some(session) = self.session.as_ref() {
            return session.status(music_app_pid);
        }
        AppleMusicProcessTapStatus {
            supported: process_tap_supported(),
            music_app_running: music_app_pid.is_some(),
            music_app_pid,
            last_error: self.last_error.clone(),
            ..AppleMusicProcessTapStatus::default()
        }
    }

    pub(super) fn start(
        &mut self,
        player: Arc<Player>,
        confirm_system_audio_capture: bool,
        mute_original: bool,
    ) -> Result<AppleMusicProcessTapStatus, AppleMusicMvpError> {
        if !confirm_system_audio_capture {
            return Err(mvp_error(
                "process_tap_confirmation_required",
                "Confirm the macOS system-audio capture prompt before starting the experiment.",
                false,
                "permission",
                true,
            ));
        }
        if !process_tap_supported() {
            return Err(mvp_error(
                "process_tap_unsupported",
                "Music app process capture requires macOS 14.2 or newer.",
                false,
                "os_support",
                true,
            ));
        }
        if self.session.is_some() {
            return Ok(self.status());
        }
        let pid = music_app_pid().ok_or_else(|| {
            mvp_error(
                "music_app_not_running",
                "Open the Music app and start a song before enabling the DSP experiment.",
                true,
                "music_app",
                true,
            )
        })?;
        let result = ProcessTapSession::start(player, pid as i32, mute_original);
        match result {
            Ok(session) => {
                self.session = Some(session);
                self.last_error = None;
                Ok(self.status())
            }
            Err(failure) => {
                self.last_error = Some(failure.clone());
                Err(failure)
            }
        }
    }

    pub(super) fn stop(&mut self) -> AppleMusicProcessTapStatus {
        self.session.take();
        self.last_error = None;
        self.status()
    }
}

fn process_tap_supported() -> bool {
    unsafe { fozmo_process_tap_supported() != 0 }
}

fn music_app_pid() -> Option<u32> {
    u32::try_from(unsafe { fozmo_music_app_pid() })
        .ok()
        .filter(|pid| *pid != 0)
}

fn validated_rate(rate_hz: f64) -> Result<u32, AppleMusicMvpError> {
    let rounded = rate_hz.round();
    if !rate_hz.is_finite()
        || !(8_000.0..=384_000.0).contains(&rounded)
        || (rate_hz - rounded).abs() > 0.01
    {
        return Err(mvp_error(
            "process_tap_format_unsupported",
            format!("The Music app tap exposed an unsupported sample rate ({rate_hz} Hz)."),
            false,
            "tap_format",
            true,
        ));
    }
    Ok(rounded as u32)
}

fn process_tap_error(failure: TapFailure) -> AppleMusicMvpError {
    let stage = stage_name(failure.stage);
    let message = match failure.stage {
        2 => "Music.app is open but is not currently visible as a Core Audio process. Start a song, then retry.".to_string(),
        3 | 6 | 7 | 8 | 9 => format!(
            "macOS could not start the Music app audio tap at {stage} (OSStatus {}). Check System Settings → Privacy & Security → Screen & System Audio Recording, then retry.",
            display_os_status(failure.status)
        ),
        4 => format!(
            "The Music app tap exposed a PCM format this experiment does not support (OSStatus {}).",
            display_os_status(failure.status)
        ),
        _ => format!(
            "The Music app audio tap failed at {stage} (OSStatus {}).",
            display_os_status(failure.status)
        ),
    };
    mvp_error("process_tap_start_failed", message, true, stage, true)
}

fn stage_name(stage: u32) -> &'static str {
    match stage {
        1 => "os_support",
        2 => "audio_process",
        3 => "create_tap",
        4 => "tap_format",
        5 => "tap_uid",
        6 => "create_aggregate",
        7 => "attach_tap",
        8 => "create_io_proc",
        9 => "start_io",
        _ => "process_tap",
    }
}

fn display_os_status(status: i32) -> String {
    let bytes = (status as u32).to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        format!("'{}' / {status}", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

fn mvp_error(
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

fn nonzero(value: u32) -> Option<u32> {
    (value != 0).then_some(value)
}

fn host_time_age_ms(last_host_time: u64) -> Option<u64> {
    if last_host_time == 0 {
        return None;
    }
    let now = unsafe { coreaudio_sys::AudioGetCurrentHostTime() };
    let elapsed = now.saturating_sub(last_host_time);
    let nanos = unsafe { coreaudio_sys::AudioConvertHostTimeToNanos(elapsed) };
    Some(nanos / 1_000_000)
}

unsafe extern "C" fn process_tap_callback(
    buffer0: *const f32,
    buffer1: *const f32,
    frames: u32,
    layout: u32,
    host_time: u64,
    context: *mut c_void,
) {
    if context.is_null() || buffer0.is_null() || frames == 0 {
        return;
    }
    let context = unsafe { &mut *context.cast::<CallbackContext>() };
    let frames = frames as usize;
    let (sum_l, sum_r, pushed_frames) = match layout {
        LAYOUT_INTERLEAVED => {
            let samples = unsafe {
                slice::from_raw_parts(buffer0, frames.saturating_mul(usize::from(LIVE_CHANNELS)))
            };
            let writable_samples = context.producer.free_len().min(samples.len()) & !1;
            let pushed = context.producer.push_slice(&samples[..writable_samples]);
            let mut sum_l = 0.0_f64;
            let mut sum_r = 0.0_f64;
            for frame in samples.chunks_exact(2) {
                sum_l += f64::from(frame[0]) * f64::from(frame[0]);
                sum_r += f64::from(frame[1]) * f64::from(frame[1]);
            }
            (sum_l, sum_r, pushed / 2)
        }
        LAYOUT_PLANAR if !buffer1.is_null() => {
            let left = unsafe { slice::from_raw_parts(buffer0, frames) };
            let right = unsafe { slice::from_raw_parts(buffer1, frames) };
            let writable_frames = (context.producer.free_len() / 2).min(frames);
            let mut sum_l = 0.0_f64;
            let mut sum_r = 0.0_f64;
            for index in 0..frames {
                let l = left[index];
                let r = right[index];
                sum_l += f64::from(l) * f64::from(l);
                sum_r += f64::from(r) * f64::from(r);
                if index < writable_frames {
                    let _ = context.producer.push(l);
                    let _ = context.producer.push(r);
                }
            }
            (sum_l, sum_r, writable_frames)
        }
        _ => {
            context
                .metrics
                .invalid_callbacks
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let metrics = &context.metrics;
    metrics.callbacks_received.fetch_add(1, Ordering::Relaxed);
    metrics
        .frames_received
        .fetch_add(frames as u64, Ordering::Relaxed);
    if pushed_frames < frames {
        metrics.ring_overruns.fetch_add(1, Ordering::Relaxed);
    }
    let divisor = frames as f64;
    metrics.rms_l_bits.store(
        ((sum_l / divisor).sqrt() as f32).to_bits(),
        Ordering::Relaxed,
    );
    metrics.rms_r_bits.store(
        ((sum_r / divisor).sqrt() as f32).to_bits(),
        Ordering::Relaxed,
    );
    metrics
        .last_callback_host_time
        .store(host_time, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_status_fourcc_is_readable() {
        assert_eq!(display_os_status(0x7072633f), "'prc?' / 1886544703");
        assert_eq!(display_os_status(-50), "-50");
    }

    #[test]
    fn validates_integral_audio_rates() {
        assert_eq!(validated_rate(48_000.0).unwrap(), 48_000);
        assert!(validated_rate(0.0).is_err());
        assert!(validated_rate(44_100.5).is_err());
    }
}
