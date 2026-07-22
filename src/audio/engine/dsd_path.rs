#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
use std::collections::HashMap;
#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
use std::time::{Duration, Instant};

use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
use crate::audio::dsp::resampler::FilterType;

use super::buffers::{
    DsdDebugState, DsdWorkerState, dsd_boundary_fade_in_frames, dsd_render_quantum_frames,
    new_dop_ring,
};
use super::signal_path::OutputMode;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
use super::signal_path::source_uses_44k_family_dsd256_compat;

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
const NATIVE_DSD_TIMEOUT_RECOVERY_DELAY: Duration = Duration::from_secs(5);
#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
const NATIVE_DSD_TEMPORARY_FAILURE_TTL: Duration = Duration::from_secs(90);

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
type NativeDsdAttemptKey = (Option<String>, OutputMode, u32);

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
#[derive(Clone, Copy)]
enum NativeDsdFailure {
    Permanent,
    RetryAfter(Instant),
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
impl NativeDsdFailure {
    fn is_active(self, now: Instant) -> bool {
        match self {
            Self::Permanent => true,
            Self::RetryAfter(retry_at) => now < retry_at,
        }
    }

    fn describe(self, mode: OutputMode, wire_rate: u32, now: Instant) -> String {
        let label = format!("{} at {} Hz", mode.as_name().to_uppercase(), wire_rate);
        match self {
            Self::Permanent => format!("{label} previously failed"),
            Self::RetryAfter(retry_at) => {
                let seconds = retry_at.saturating_duration_since(now).as_secs().max(1);
                format!("{label} recently timed out (retrying in {seconds}s)")
            }
        }
    }
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
#[derive(Default)]
pub(super) struct NativeDsdFailureCache {
    failures: HashMap<NativeDsdAttemptKey, NativeDsdFailure>,
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
impl NativeDsdFailureCache {
    pub(super) fn clear(&mut self) {
        self.failures.clear();
    }

    pub(super) fn active_failure_description(
        &mut self,
        device_name: Option<String>,
        mode: OutputMode,
        wire_rate: u32,
        now: Instant,
    ) -> Option<String> {
        let key = (device_name, mode, wire_rate);
        let failure = self.failures.get(&key).copied()?;
        if failure.is_active(now) {
            return Some(failure.describe(mode, wire_rate, now));
        }
        self.failures.remove(&key);
        None
    }

    pub(super) fn record_permanent(
        &mut self,
        device_name: Option<String>,
        mode: OutputMode,
        wire_rate: u32,
    ) {
        self.failures
            .insert((device_name, mode, wire_rate), NativeDsdFailure::Permanent);
    }

    pub(super) fn record_timeout(
        &mut self,
        device_name: Option<String>,
        mode: OutputMode,
        wire_rate: u32,
        now: Instant,
    ) -> Instant {
        self.failures.insert(
            (device_name, mode, wire_rate),
            NativeDsdFailure::RetryAfter(now + NATIVE_DSD_TEMPORARY_FAILURE_TTL),
        );
        now + NATIVE_DSD_TIMEOUT_RECOVERY_DELAY
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DsdFallbackKey {
    device_name: Option<String>,
    mode: OutputMode,
    source_rate: u32,
}

impl DsdFallbackKey {
    pub(super) fn new(device_name: Option<String>, mode: OutputMode, source_rate: u32) -> Self {
        Self {
            device_name,
            mode,
            source_rate,
        }
    }
}

pub(super) fn dsd_rate_for_mode(mode: OutputMode) -> DsdRate {
    match mode {
        OutputMode::Dsd64 => DsdRate::Dsd64,
        OutputMode::Dsd128 => DsdRate::Dsd128,
        OutputMode::Dsd256 => DsdRate::Dsd256,
        OutputMode::Pcm => unreachable!("PCM has no DSD renderer rate"),
    }
}

pub(super) fn build_renderer(
    filter_type: FilterType,
    source_rate: u32,
    mode: OutputMode,
    force_44k_family: bool,
    dsd_modulator: DsdModulator,
    dsd_isi_penalty: f32,
) -> Result<DsdRenderer, &'static str> {
    let dsd_rate = dsd_rate_for_mode(mode);
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: build DSD renderer: source={}Hz mode={} filter={} force_44k_family={} modulator={} lookahead={} isi_penalty={:.5}",
            source_rate,
            mode.as_name(),
            filter_type.as_name(),
            force_44k_family,
            dsd_modulator.as_name(),
            dsd_modulator.lookahead_depth(),
            dsd_isi_penalty,
        );
    }
    if force_44k_family {
        DsdRenderer::new_44k_family_with_dsd_modulator_and_isi_penalty(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            dsd_isi_penalty as f64,
        )
    } else {
        DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            dsd_isi_penalty as f64,
        )
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(super) fn dop_wire_rate_for_mode(
    mode: OutputMode,
    source_rate: u32,
    force_44k_family: bool,
) -> Option<u32> {
    if force_44k_family {
        return Some(dsd_rate_for_mode(mode).wire_rate_44k_family());
    }
    mode.dsd_wire_rate(source_rate)
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(super) fn should_force_44k_family_dsd256(mode: OutputMode, source_rate: u32) -> bool {
    mode == OutputMode::Dsd256 && source_uses_44k_family_dsd256_compat(source_rate)
}

pub(super) fn new_dop_worker_state(
    renderer: DsdRenderer,
    source_rate: u32,
    wire_rate: u32,
    mode: OutputMode,
    dsp_buffer_ms: u32,
) -> DsdWorkerState {
    let dop_frame_rate = DsdRate::dop_frame_rate(wire_rate);
    let (dop_prod, dop_cons) = new_dop_ring(dop_frame_rate, dsp_buffer_ms);
    let ring_capacity = dop_prod.free_len() + dop_prod.len();
    let fade_in_frames = dsd_boundary_fade_in_frames(wire_rate);
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: new DoP worker state: source={}Hz wire={}Hz dop_frame={}Hz mode={} ring_capacity={} samples",
            source_rate,
            wire_rate,
            dop_frame_rate,
            mode.as_name(),
            ring_capacity,
        );
    }
    DsdWorkerState {
        renderer,
        prod: dop_prod,
        cons_opt: Some(dop_cons),
        output_buf: Vec::with_capacity(16384),
        staged_pcm_l: Vec::with_capacity(dsd_render_quantum_frames(source_rate, mode)),
        staged_pcm_r: Vec::with_capacity(dsd_render_quantum_frames(source_rate, mode)),
        render_quantum_l: Vec::with_capacity(dsd_render_quantum_frames(source_rate, mode)),
        render_quantum_r: Vec::with_capacity(dsd_render_quantum_frames(source_rate, mode)),
        eq_scratch_l: Vec::with_capacity(dsd_render_quantum_frames(source_rate, mode)),
        eq_scratch_r: Vec::with_capacity(dsd_render_quantum_frames(source_rate, mode)),
        render_quantum_frames: dsd_render_quantum_frames(source_rate, mode),
        recent_render_loads: Vec::new(),
        recent_render_load_cursor: 0,
        dop_frame_rate,
        source_rate,
        wire_rate,
        mode,
        dsp_buffer_ms,
        fade_in_total_frames: fade_in_frames,
        fade_in_remaining_frames: fade_in_frames,
        debug: DsdDebugState::new(),
        #[cfg(all(target_os = "windows", feature = "asio"))]
        native: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::audio::dsp::resampler::FilterType;

    use super::*;

    #[test]
    fn dsd_rate_maps_output_modes() {
        assert_eq!(dsd_rate_for_mode(OutputMode::Dsd64), DsdRate::Dsd64);
        assert_eq!(dsd_rate_for_mode(OutputMode::Dsd128), DsdRate::Dsd128);
        assert_eq!(dsd_rate_for_mode(OutputMode::Dsd256), DsdRate::Dsd256);
    }

    #[test]
    fn dsd_fallback_key_tracks_device_mode_and_source_rate() {
        let key = DsdFallbackKey::new(Some("Device A".to_string()), OutputMode::Dsd128, 44_100);

        assert_eq!(
            key,
            DsdFallbackKey::new(Some("Device A".to_string()), OutputMode::Dsd128, 44_100)
        );
        assert_ne!(
            key,
            DsdFallbackKey::new(Some("Device B".to_string()), OutputMode::Dsd128, 44_100)
        );
        assert_ne!(
            key,
            DsdFallbackKey::new(Some("Device A".to_string()), OutputMode::Dsd256, 44_100)
        );
        assert_ne!(
            key,
            DsdFallbackKey::new(Some("Device A".to_string()), OutputMode::Dsd128, 48_000)
        );
    }

    #[test]
    fn dsd64_dop_wire_rates_use_standard_pcm_carriers() {
        assert_eq!(
            dop_wire_rate_for_mode(OutputMode::Dsd64, 44_100, false),
            Some(2_822_400)
        );
        assert_eq!(
            dop_wire_rate_for_mode(OutputMode::Dsd64, 48_000, false),
            Some(3_072_000)
        );
        // DoP frame rates land on 176.4 / 192 kHz PCM carriers.
        assert_eq!(DsdRate::dop_frame_rate(2_822_400), 176_400);
        assert_eq!(DsdRate::dop_frame_rate(3_072_000), 192_000);
    }

    #[test]
    fn dop_wire_rate_can_force_dsd256_to_44k_family() {
        assert_eq!(
            dop_wire_rate_for_mode(OutputMode::Dsd256, 48_000, true),
            Some(11_289_600)
        );
        assert_eq!(
            dop_wire_rate_for_mode(OutputMode::Dsd256, 48_000, false),
            Some(12_288_000)
        );
    }

    #[test]
    fn dop_worker_state_uses_dsd_frame_rate() {
        let renderer = build_renderer(
            FilterType::LinearPhase128k,
            44_100,
            OutputMode::Dsd128,
            false,
            DsdModulator::default(),
            0.0,
        )
        .expect("DSD128 renderer");
        let state = new_dop_worker_state(renderer, 44_100, 5_644_800, OutputMode::Dsd128, 0);

        assert_eq!(state.dop_frame_rate, 352_800);
        assert_eq!(state.source_rate, 44_100);
        assert_eq!(state.wire_rate, 5_644_800);
        assert_eq!(state.mode, OutputMode::Dsd128);
        assert_eq!(state.dsp_buffer_ms, 0);
        assert!(state.cons_opt.is_some());
    }

    #[test]
    fn only_selected_48k_family_sources_force_dsd256_compat() {
        assert!(should_force_44k_family_dsd256(OutputMode::Dsd256, 48_000));
        assert!(should_force_44k_family_dsd256(OutputMode::Dsd256, 192_000));
        assert!(!should_force_44k_family_dsd256(OutputMode::Dsd256, 384_000));
        assert!(!should_force_44k_family_dsd256(OutputMode::Dsd128, 48_000));
    }

    #[test]
    fn native_dsd_permanent_failure_stays_active_until_cleared() {
        let mut cache = NativeDsdFailureCache::default();
        let now = Instant::now();

        cache.record_permanent(
            Some("ASIO: Driver".to_string()),
            OutputMode::Dsd256,
            12_288_000,
        );

        let description = cache
            .active_failure_description(
                Some("ASIO: Driver".to_string()),
                OutputMode::Dsd256,
                12_288_000,
                now + Duration::from_secs(3600),
            )
            .expect("permanent failure remains active");
        assert!(description.contains("previously failed"));

        cache.clear();
        assert!(
            cache
                .active_failure_description(
                    Some("ASIO: Driver".to_string()),
                    OutputMode::Dsd256,
                    12_288_000,
                    now,
                )
                .is_none()
        );
    }

    #[test]
    fn native_dsd_timeout_reports_retry_window_then_expires() {
        let mut cache = NativeDsdFailureCache::default();
        let now = Instant::now();

        let pcm_retry_at = cache.record_timeout(None, OutputMode::Dsd128, 5_644_800, now);
        assert_eq!(
            pcm_retry_at.duration_since(now),
            NATIVE_DSD_TIMEOUT_RECOVERY_DELAY
        );

        let description = cache
            .active_failure_description(
                None,
                OutputMode::Dsd128,
                5_644_800,
                now + Duration::from_secs(10),
            )
            .expect("timeout should still be active");
        assert!(description.contains("recently timed out"));

        assert!(
            cache
                .active_failure_description(
                    None,
                    OutputMode::Dsd128,
                    5_644_800,
                    now + NATIVE_DSD_TEMPORARY_FAILURE_TTL + Duration::from_secs(1),
                )
                .is_none()
        );
    }
}
