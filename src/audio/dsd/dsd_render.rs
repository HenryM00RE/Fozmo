//! End-to-end PCM → DoP renderer.
//!
//! Chains together:
//!   1. [`SincResampler`] — integer cascade pushing 44.1/48 kHz f64 PCM up to the DSD rate
//!      (2.8224 MHz for DSD64 through 45.1584 MHz for measurement-only DSD1024).
//!   2. [`CrfbModulator`] — one per channel, runs the upsampled f64 through a 7th-order
//!      delta-sigma loop and emits a 1-bit stream.
//!   3. [`DopPacker`] — repacks the bit streams into DoP frames (24-bit values with
//!      0x05/0xFA marker in the top 8 bits).
//!
//! Output is interleaved stereo `i32` at DSD_rate/16 — the same wire format that a
//! standard 24-bit/176.4 kHz (DSD64) through 24-bit/2.8224 MHz (DSD1024) PCM endpoint
//! expects. DSD512 and DSD1024 are currently exposed only to measurement callers.
//!
//! The modulate stage is pipelined one block deep: each `modulate_*` call hands the
//! freshly upsampled block to two persistent per-channel worker threads and packs the
//! *previous* block's bits. Decode + upsample of block N+1 therefore overlaps
//! modulation of block N, so real-time throughput is bounded by the slowest stage
//! instead of the sum of stages. The held block is emitted by the end-of-stream
//! flush, which the engine already calls at EOF for both modulators.

// Staged DSD renderer paths compile before every transport enables them by default.
#![allow(dead_code)]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::audio::dsd::delta_sigma::{
    CrfbModulator, DsdModulator, SeventhOrderSearchDiagnostics, SeventhOrderSearchExperimentConfig,
    SeventhOrderSearchModulator, seventh_order_search_production_config,
};
use crate::audio::dsd::dop::DopPacker;
use crate::audio::dsd::dsd_coeffs::{
    CRFB7_STANDARD_OSR64, CRFB7_STANDARD_OSR128, CRFB7_STANDARD_OSR256, CRFB7_STANDARD_OSR512,
    CRFB7_STANDARD_OSR1024, ModulatorCoeffs,
};
use crate::audio::dsd::native_dsd::{NativeDsdOrder, NativeDsdPacker};
use crate::audio::dsp::resampler::{FilterType, SincResampler};

const DSD_LIMITER_KNEE_RATIO: f64 = 0.95;
const DEFAULT_DSD_ISI_PENALTY: f64 = 0.0;
/// Version identifier for the effective production-policy defaults applied by
/// [`DsdExperimentTweaks::with_production_policy_defaults`]. Measurement tools
/// record this separately from their own schema so policy-only changes cannot
/// masquerade as an equivalent renderer configuration.
pub const DSD_PRODUCTION_POLICY_VERSION: &str = "dsd-production-policy-v4";
pub const DSD64_SEVENTH_ORDER_SEARCH_REQUIRED_HEADROOM_DB: f64 = -2.0;

/// Map a source-PCM window onto the wire-rate sample domain emitted by the DSD
/// resampler. The FIR engines pre-pad their kernels to compensate group delay,
/// and EOF draining emits exactly `input_frames * ratio`, so source sample zero
/// is wire sequence zero. Quality tools use this single boundary for both
/// frozen-corpus diagnostics and 7th Order Search exact-oracle state prefixes.
pub fn dsd_source_window_to_modulator_samples(
    filter_type: FilterType,
    source_rate: u32,
    wire_rate: u32,
    source_start: usize,
    source_length: usize,
) -> Option<std::ops::Range<usize>> {
    if source_rate == 0
        || wire_rate == 0
        || !wire_rate.is_multiple_of(source_rate)
        || source_length == 0
    {
        return None;
    }
    let ratio = usize::try_from(wire_rate / source_rate).ok()?;
    // Keep this exhaustive so a future filter with a different alignment
    // contract must be handled explicitly.
    match filter_type {
        FilterType::LinearPhase128k
        | FilterType::Minimum16k
        | FilterType::SplitPhase128kE3
        | FilterType::MinimumPhaseCompact128k => {}
    }
    let start = source_start.checked_mul(ratio)?;
    let length = source_length.checked_mul(ratio)?;
    Some(start..start.checked_add(length)?)
}

const DSD_MOD_SEED_LEFT: u64 = 0xA5A5_F00F_DEAD_BEEF;
const DSD_MOD_SEED_RIGHT: u64 = 0xE99B_C2D7_05F8_D3E1;

fn sanitize_isi_penalty(penalty: f64) -> f64 {
    if penalty.is_finite() {
        penalty.clamp(0.0, 0.05)
    } else {
        DEFAULT_DSD_ISI_PENALTY
    }
}

fn effective_modulator_input_gain(
    dsd_modulator: DsdModulator,
    input_gain: f64,
    experiment_gain_db: f64,
) -> f64 {
    let requested = input_gain * 10.0f64.powf(experiment_gain_db / 20.0);
    if dsd_modulator == DsdModulator::SeventhOrderSearch {
        // Playback settings already supply -2 dB, so cap instead of multiplying
        // by a second headroom factor.  This also protects direct renderer
        // callers while preserving deliberately quieter input gain.
        requested.min(10.0f64.powf(DSD64_SEVENTH_ORDER_SEARCH_REQUIRED_HEADROOM_DB / 20.0))
    } else {
        requested
    }
}

/// Choice of DSD output rate. Selects both the cascade target rate and the modulator
/// coefficient table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdRate {
    Dsd64,
    Dsd128,
    Dsd256,
    /// Measurement-only until the transport/UI capability work is complete.
    Dsd512,
    /// Measurement-only until the transport/UI capability work is complete.
    Dsd1024,
}

#[derive(Clone, Copy)]
struct NamedModulatorCoeffs {
    coeffs: &'static ModulatorCoeffs,
    name: &'static str,
}

#[derive(Clone, Copy)]
struct SeventhOrderSearchProductionPolicy {
    config: SeventhOrderSearchExperimentConfig,
    coefficients: NamedModulatorCoeffs,
}

fn seventh_order_search_production_policy(
    dsd_rate: DsdRate,
) -> Option<SeventhOrderSearchProductionPolicy> {
    let coefficients = match dsd_rate {
        DsdRate::Dsd64 => NamedModulatorCoeffs {
            coeffs:
                crate::audio::dsd::delta_sigma::seventh_order_search_dsd64_production_coefficients(),
            name: "SEVENTH_ORDER_SEARCH_OSR64_OBG164_INPUT468_V1",
        },
        DsdRate::Dsd128 => NamedModulatorCoeffs {
            coeffs:
                crate::audio::dsd::delta_sigma::seventh_order_search_dsd128_production_coefficients(
                ),
            name: "SEVENTH_ORDER_SEARCH_OSR128_OBG164_INPUT468_V1",
        },
        DsdRate::Dsd256 => NamedModulatorCoeffs {
            coeffs:
                crate::audio::dsd::delta_sigma::seventh_order_search_dsd256_production_coefficients(
                ),
            name: "SEVENTH_ORDER_SEARCH_OSR256_OBG164_INPUT468_V1",
        },
        DsdRate::Dsd512 | DsdRate::Dsd1024 => return None,
    };
    Some(SeventhOrderSearchProductionPolicy {
        config: seventh_order_search_production_config(),
        coefficients,
    })
}

fn standard_coeffs_for_rate(rate: DsdRate) -> NamedModulatorCoeffs {
    match rate {
        DsdRate::Dsd64 => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR64,
            name: "CRFB7_STANDARD_OSR64",
        },
        DsdRate::Dsd128 => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR128,
            name: "CRFB7_STANDARD_OSR128",
        },
        DsdRate::Dsd256 => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR256,
            name: "CRFB7_STANDARD_OSR256",
        },
        DsdRate::Dsd512 => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR512,
            name: "CRFB7_STANDARD_OSR512",
        },
        DsdRate::Dsd1024 => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR1024,
            name: "CRFB7_STANDARD_OSR1024",
        },
    }
}

impl DsdRate {
    pub fn wire_rate_44k_family(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 2_822_400,
            DsdRate::Dsd128 => 5_644_800,
            DsdRate::Dsd256 => 11_289_600,
            DsdRate::Dsd512 => 22_579_200,
            DsdRate::Dsd1024 => 45_158_400,
        }
    }

    pub fn oversample(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 64,
            DsdRate::Dsd128 => 128,
            DsdRate::Dsd256 => 256,
            DsdRate::Dsd512 => 512,
            DsdRate::Dsd1024 => 1024,
        }
    }

    /// Whether this rate has a calibrated table for the requested production
    /// modulator. The two experimental high rates intentionally support only
    /// the plain hard-sign Standard path.
    pub fn supports_modulator(self, modulator: DsdModulator) -> bool {
        !matches!(self, DsdRate::Dsd512 | DsdRate::Dsd1024) || modulator == DsdModulator::Standard
    }

    /// DSD wire rate for a given PCM source rate. DSD rates are *fixed* per family:
    ///
    /// * 44.1 kHz family (44.1 / 88.2 / 176.4 / 352.8 kHz) → 2.8224 MHz (DSD64),
    ///   5.6448 MHz (DSD128) or 11.2896 MHz (DSD256).
    /// * 48 kHz family (48 / 96 / 192 / 384 kHz) → 3.072 MHz (DSD64), 6.144 MHz
    ///   (DSD128), 12.288 MHz (DSD256), 24.576 MHz (DSD512), or 49.152 MHz
    ///   (DSD1024).
    ///
    /// Returns `None` if the source is in neither family or the implied upsample
    /// ratio isn't a power of two ≥ 2 (the cascade can't reach it). High-rate
    /// sources like 176.4 kHz that already exceed the DSD modulator's notional
    /// in-band coverage (the OSR=128/256 tables are tuned for ~22.05 kHz audio
    /// bandwidth) still work — the noise-shaping isn't optimal above ~22 kHz
    /// but doesn't actively break.
    pub fn wire_rate_for_source(self, source_rate: u32) -> Option<u32> {
        if source_rate == 0 {
            return None;
        }
        let base = if source_rate.is_multiple_of(44_100) {
            2_822_400
        } else if source_rate.is_multiple_of(48_000) {
            3_072_000
        } else {
            return None;
        };
        let target = match self {
            DsdRate::Dsd64 => base,
            DsdRate::Dsd128 => base * 2,
            DsdRate::Dsd256 => base * 4,
            DsdRate::Dsd512 => base * 8,
            DsdRate::Dsd1024 => base * 16,
        };
        if target <= source_rate {
            return None;
        }
        if !target.is_multiple_of(source_rate) {
            return None;
        }
        let ratio = target / source_rate;
        if !ratio.is_power_of_two() {
            return None;
        }
        Some(target)
    }

    /// DoP frame rate (i.e. WASAPI exclusive PCM rate) for the given wire rate.
    pub fn dop_frame_rate(wire_rate: u32) -> u32 {
        wire_rate / 16
    }
}

pub struct DsdRenderer {
    upsampler: DsdUpsampler,
    worker_l: ModulatorWorker,
    worker_r: ModulatorWorker,
    /// A block has been handed to the workers and not yet collected.
    in_flight: bool,
    dop_packer: DopPacker,
    native_packer: NativeDsdPacker,
    /// Interleaved f64 PCM at the DSD rate, produced by the resampler each call.
    pcm_scratch: Vec<f64>,
    /// Source-rate scratch used only for NaN/Inf scrubbing before upsampling.
    source_scratch_l: Vec<f64>,
    source_scratch_r: Vec<f64>,
    /// Per-channel deinterleaved buffers (recycled through the worker channels).
    pcm_l: Vec<f64>,
    pcm_r: Vec<f64>,
    /// Per-channel 1-bit DSD output of the *previous* block, returned by the workers.
    bits_l: Vec<u8>,
    bits_r: Vec<u8>,
    /// Spare bit buffers cycled into the next worker job.
    spare_bits_l: Vec<u8>,
    spare_bits_r: Vec<u8>,
    /// Per-channel modulator health counters, refreshed each time a worker
    /// result is collected.
    stability_resets_lr: [u64; 2],
    state_clamps_lr: [u64; 2],
    seventh_order_search_diagnostics_lr: [Option<SeventhOrderSearchDiagnostics>; 2],
    /// Wall time spent by the slower channel worker for the most recently
    /// collected process block. This is measured inside the workers so paced
    /// live sources cannot hide modulation work between render calls.
    last_collected_modulation_time: Duration,
    limiter_telemetry: DsdLimiterTelemetry,
    truncation_telemetry: DsdTruncationTelemetry,
    source_rate: u32,
    dsd_rate: DsdRate,
    coeffs: &'static ModulatorCoeffs,
    coefficient_table_name: &'static str,
    modulator_seeds: [u64; 2],
    dsd_modulator: DsdModulator,
    isi_penalty: f64,
    experiment_tweaks: DsdExperimentTweaks,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DsdLimiterTelemetry {
    pub current_block_peak_ratio: f32,
    pub peak_ratio_max: f32,
    pub current_block_gain: f32,
    pub current_block_limited_samples: u64,
    pub limited_events: u64,
    pub limited_samples: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DsdTruncationTelemetry {
    pub events: u64,
    pub discarded_left_bits: u64,
    pub discarded_right_bits: u64,
    pub last_left_len: usize,
    pub last_right_len: usize,
    pub last_kept_len: usize,
}

impl Default for DsdLimiterTelemetry {
    fn default() -> Self {
        Self {
            current_block_peak_ratio: 0.0,
            peak_ratio_max: 0.0,
            current_block_gain: 1.0,
            current_block_limited_samples: 0,
            limited_events: 0,
            limited_samples: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DsdRenderTiming {
    pub upsample: Duration,
    pub modulate_submit_collect: Duration,
    pub pack: Duration,
    pub flush_modulators: Duration,
    pub flush_pack: Duration,
}

impl DsdRenderTiming {
    pub fn block_total(self) -> Duration {
        self.upsample + self.modulate_submit_collect + self.pack
    }

    pub fn flush_total(self) -> Duration {
        self.flush_modulators + self.flush_pack
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DsdExperimentTweaks {
    /// Isolated 7th Order Search controls. `None` selects its versioned production defaults.
    pub seventh_order_search_config: Option<SeventhOrderSearchExperimentConfig>,
    /// `Some(true)` retains qualification telemetry, while `Some(false)` uses
    /// the bit-identical lean playback path. `None` resolves to full telemetry
    /// for explicit research configurations and lean telemetry for ordinary
    /// playback.
    pub seventh_order_search_full_diagnostics: Option<bool>,
    pub seed_left: Option<u64>,
    pub seed_right: Option<u64>,
    pub input_gain_db: f64,
}

impl DsdExperimentTweaks {
    fn with_production_policy_defaults(
        mut self,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Self {
        if dsd_modulator == DsdModulator::SeventhOrderSearch {
            let explicit_research_config = self.seventh_order_search_config.is_some();
            if self.seventh_order_search_full_diagnostics.is_none() {
                self.seventh_order_search_full_diagnostics = Some(explicit_research_config);
            }
            if self.seventh_order_search_config.is_none() {
                self.seventh_order_search_config =
                    seventh_order_search_production_policy(dsd_rate).map(|policy| policy.config);
            }
        }
        self
    }
}

fn select_modulator_coeffs(
    _filter_type: FilterType,
    dsd_rate: DsdRate,
    dsd_modulator: DsdModulator,
    coeffs_override: Option<&'static ModulatorCoeffs>,
    _experiment_tweaks: DsdExperimentTweaks,
) -> Result<NamedModulatorCoeffs, &'static str> {
    if let Some(coeffs) = coeffs_override {
        if dsd_modulator == DsdModulator::SeventhOrderSearch {
            return Err("7th Order Search coefficient overrides are not supported");
        }
        if coeffs.osr != dsd_rate.oversample() {
            return Err("DSD coefficient override OSR does not match the selected DSD rate");
        }
        return Ok(NamedModulatorCoeffs {
            coeffs,
            name: "custom_override",
        });
    }
    if dsd_modulator == DsdModulator::SeventhOrderSearch {
        return seventh_order_search_production_policy(dsd_rate)
            .map(|policy| policy.coefficients)
            .ok_or("7th Order Search has no production policy for the selected DSD rate");
    }
    Ok(standard_coeffs_for_rate(dsd_rate))
}

enum ModJob {
    /// Modulate one block of gained, limited per-channel PCM into bits.
    Process {
        input: Vec<f64>,
        bits: Vec<u8>,
        submitted_at: Instant,
    },
    /// Emit 7th Order Search's delayed tail (no-op for Standard).
    Flush {
        bits: Vec<u8>,
        submitted_at: Instant,
    },
    /// Reset integrator state (keeps the dither RNG running). No response.
    Reset,
}

enum WorkerModulator {
    Crfb(Box<CrfbModulator>),
    SeventhOrderSearch(Box<SeventhOrderSearchModulator>),
}

impl WorkerModulator {
    fn process_into_bits(&mut self, input: &[f64], bits: &mut Vec<u8>) {
        match self {
            Self::Crfb(modulator) => modulator.process_into_bits(input, bits),
            Self::SeventhOrderSearch(modulator) => modulator.process_into_bits(input, bits),
        }
    }

    fn flush_into_bits(&mut self, bits: &mut Vec<u8>) {
        match self {
            Self::Crfb(modulator) => modulator.flush_into_bits(bits),
            Self::SeventhOrderSearch(modulator) => modulator.flush_into_bits(bits),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Crfb(modulator) => modulator.reset(),
            Self::SeventhOrderSearch(modulator) => modulator.reset(),
        }
    }

    fn stability_resets(&self) -> u64 {
        match self {
            Self::Crfb(modulator) => modulator.stability_resets(),
            Self::SeventhOrderSearch(modulator) => modulator.stability_resets(),
        }
    }

    fn state_clamps(&self) -> u64 {
        match self {
            Self::Crfb(modulator) => modulator.state_clamps(),
            Self::SeventhOrderSearch(modulator) => modulator.state_clamps(),
        }
    }

    fn seventh_order_search_diagnostics(&self) -> Option<SeventhOrderSearchDiagnostics> {
        match self {
            Self::Crfb(_) => None,
            Self::SeventhOrderSearch(modulator) => Some(modulator.diagnostics()),
        }
    }
}

struct ModOutput {
    bits: Vec<u8>,
    /// The input buffer of a `Process` job, returned for recycling.
    input: Option<Vec<f64>>,
    turnaround_time: Duration,
    stability_resets: u64,
    state_clamps: u64,
    seventh_order_search_diagnostics: Option<SeventhOrderSearchDiagnostics>,
}

/// Persistent single-channel modulator thread. Owning the `CrfbModulator` on a
/// long-lived thread (rather than spawning per block) lets modulation of block N
/// overlap upsampling of block N+1 and keeps the integrator state warm in one
/// core's cache.
struct ModulatorWorker {
    jobs: mpsc::Sender<ModJob>,
    results: mpsc::Receiver<ModOutput>,
}

impl ModulatorWorker {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
        tweaks: DsdExperimentTweaks,
        wire_rate: u32,
        name: &str,
    ) -> Result<Self, &'static str> {
        if tweaks.seventh_order_search_config.is_some()
            && dsd_modulator != DsdModulator::SeventhOrderSearch
        {
            return Err("7th Order Search controls require the 7th Order Search modulator");
        }
        let mut modulator = if dsd_modulator == DsdModulator::SeventhOrderSearch {
            if isi_penalty != 0.0 {
                return Err("7th Order Search requires zero ISI compensation");
            }
            WorkerModulator::SeventhOrderSearch(Box::new(
                SeventhOrderSearchModulator::new_with_diagnostics(
                    coeffs,
                    seed,
                    wire_rate,
                    tweaks.seventh_order_search_config.unwrap_or_default(),
                    tweaks
                        .seventh_order_search_full_diagnostics
                        .unwrap_or(false),
                )?,
            ))
        } else {
            WorkerModulator::Crfb(Box::new(CrfbModulator::new(coeffs, seed)?))
        };
        let (job_tx, job_rx) = mpsc::channel::<ModJob>();
        let (result_tx, result_rx) = mpsc::channel::<ModOutput>();
        let thread_name = name.to_string();
        let log_name = thread_name.clone();
        if crate::audio::debug::audio_debug_enabled() {
            eprintln!(
                "AudioWorker DEBUG: spawning DSD modulator worker name={} modulator={} isi_penalty={:.5} coeff_osr={} coeff_obg={:.2} input_peak={:.6}",
                log_name,
                dsd_modulator.as_name(),
                isi_penalty,
                coeffs.osr,
                coeffs.obg,
                coeffs.input_peak,
            );
        }
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                promote_thread_to_audio_qos();
                if crate::audio::debug::audio_debug_enabled() {
                    eprintln!(
                        "AudioWorker DEBUG: DSD modulator worker online name={} modulator={}",
                        log_name,
                        dsd_modulator.as_name()
                    );
                }
                while let Ok(job) = job_rx.recv() {
                    let output = match job {
                        ModJob::Process {
                            input,
                            mut bits,
                            submitted_at,
                        } => {
                            bits.clear();
                            modulator.process_into_bits(&input, &mut bits);
                            ModOutput {
                                bits,
                                input: Some(input),
                                turnaround_time: submitted_at.elapsed(),
                                stability_resets: modulator.stability_resets(),
                                state_clamps: modulator.state_clamps(),
                                seventh_order_search_diagnostics: modulator
                                    .seventh_order_search_diagnostics(),
                            }
                        }
                        ModJob::Flush {
                            mut bits,
                            submitted_at,
                        } => {
                            bits.clear();
                            modulator.flush_into_bits(&mut bits);
                            ModOutput {
                                bits,
                                input: None,
                                turnaround_time: submitted_at.elapsed(),
                                stability_resets: modulator.stability_resets(),
                                state_clamps: modulator.state_clamps(),
                                seventh_order_search_diagnostics: modulator
                                    .seventh_order_search_diagnostics(),
                            }
                        }
                        ModJob::Reset => {
                            modulator.reset();
                            continue;
                        }
                    };
                    if result_tx.send(output).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| "failed to spawn DSD modulator worker thread")?;
        Ok(Self {
            jobs: job_tx,
            results: result_rx,
        })
    }

    fn submit(&self, job: ModJob) {
        self.jobs
            .send(job)
            .expect("DSD modulator worker thread exited unexpectedly");
    }

    fn collect(&self) -> ModOutput {
        self.results
            .recv()
            .expect("DSD modulator worker thread exited unexpectedly")
    }
}

/// The modulator workers sit on the real-time audio path: ask the scheduler to
/// treat them accordingly so they aren't parked on efficiency cores or preempted
/// by background work. (macOS has no hard core pinning; QoS is the supported way
/// to keep a thread on performance cores.)
#[cfg(target_os = "macos")]
fn promote_thread_to_audio_qos() {
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn promote_thread_to_audio_qos() {}

// Both variants are stateful DSP pipelines; boxing would add indirection in the render loop.
#[allow(clippy::large_enum_variant)]
enum DsdUpsampler {
    Direct(SincResampler),
    CrossFamily(CrossFamilyDsdChain),
}

impl DsdUpsampler {
    fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Self {
        // Any 48k-family source forced onto a 44.1-family DSD wire rate
        // (DSD64, DSD128 or DSD256) needs the cross-family hop; a Direct
        // resampler would see a non-integer ratio and silently degrade to the
        // capped fractional polyphase path.
        let is_44k_dsd_target = target_rate == DsdRate::Dsd64.wire_rate_44k_family()
            || target_rate == DsdRate::Dsd128.wire_rate_44k_family()
            || target_rate == DsdRate::Dsd256.wire_rate_44k_family()
            || target_rate == DsdRate::Dsd512.wire_rate_44k_family()
            || target_rate == DsdRate::Dsd1024.wire_rate_44k_family();
        if is_44k_dsd_target && matches!(source_rate, 48_000 | 96_000 | 192_000) {
            Self::CrossFamily(CrossFamilyDsdChain::new(
                filter_type,
                source_rate,
                target_rate,
            ))
        } else {
            Self::Direct(SincResampler::new(filter_type, source_rate, target_rate))
        }
    }

    fn target_rate(&self) -> u32 {
        match self {
            Self::Direct(resampler) => resampler.target_rate(),
            Self::CrossFamily(chain) => chain.target_rate(),
        }
    }

    fn debug_name(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct-integer-family",
            Self::CrossFamily(_) => "cross-family-48k-to-44k",
        }
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        match self {
            Self::Direct(resampler) => resampler.input(samples_l, samples_r),
            Self::CrossFamily(chain) => chain.input(samples_l, samples_r),
        }
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        match self {
            Self::Direct(resampler) => resampler.process(output),
            Self::CrossFamily(chain) => chain.process(output),
        }
    }

    fn drain_eof(&mut self, output: &mut Vec<f64>) -> usize {
        match self {
            Self::Direct(resampler) => resampler.drain_eof(output),
            Self::CrossFamily(chain) => chain.drain_eof(output),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Direct(resampler) => resampler.reset(),
            Self::CrossFamily(chain) => chain.reset(),
        }
    }
}

struct CrossFamilyDsdChain {
    stage1: Option<SincResampler>,
    stage2: SincResampler,
    stage3: SincResampler,
    stage1_out: Vec<f64>,
    stage2_out: Vec<f64>,
    plane_l: Vec<f64>,
    plane_r: Vec<f64>,
}

impl CrossFamilyDsdChain {
    const HOP_RATE_48K: u32 = 192_000;
    const HOP_RATE_44K: u32 = 176_400;

    fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Self {
        let stage1 = (source_rate != Self::HOP_RATE_48K)
            .then(|| SincResampler::new(filter_type, source_rate, Self::HOP_RATE_48K));
        Self {
            stage1,
            stage2: SincResampler::new_exact_160_147_without_capped_polyphase_warning(
                filter_type,
                Self::HOP_RATE_48K,
                Self::HOP_RATE_44K,
            ),
            stage3: SincResampler::new(filter_type, Self::HOP_RATE_44K, target_rate),
            stage1_out: Vec::new(),
            stage2_out: Vec::new(),
            plane_l: Vec::new(),
            plane_r: Vec::new(),
        }
    }

    fn target_rate(&self) -> u32 {
        self.stage3.target_rate()
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        self.stage1_out.clear();
        if let Some(stage1) = &mut self.stage1 {
            stage1.input(samples_l, samples_r);
            stage1.process(&mut self.stage1_out);
        } else {
            interleave_stereo(samples_l, samples_r, &mut self.stage1_out);
        }

        if self.stage1_out.is_empty() {
            return;
        }
        feed_interleaved(
            &mut self.stage2,
            &self.stage1_out,
            &mut self.plane_l,
            &mut self.plane_r,
        );
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        self.stage2_out.clear();
        self.stage2.process(&mut self.stage2_out);
        if !self.stage2_out.is_empty() {
            feed_interleaved(
                &mut self.stage3,
                &self.stage2_out,
                &mut self.plane_l,
                &mut self.plane_r,
            );
        }
        self.stage3.process(output)
    }

    fn drain_eof(&mut self, output: &mut Vec<f64>) -> usize {
        self.stage1_out.clear();
        if let Some(stage1) = &mut self.stage1 {
            stage1.drain_eof(&mut self.stage1_out);
            if !self.stage1_out.is_empty() {
                feed_interleaved(
                    &mut self.stage2,
                    &self.stage1_out,
                    &mut self.plane_l,
                    &mut self.plane_r,
                );
            }
        }

        self.stage2_out.clear();
        self.stage2.drain_eof(&mut self.stage2_out);
        if !self.stage2_out.is_empty() {
            feed_interleaved(
                &mut self.stage3,
                &self.stage2_out,
                &mut self.plane_l,
                &mut self.plane_r,
            );
        }

        self.stage3.drain_eof(output)
    }

    fn reset(&mut self) {
        if let Some(stage1) = &mut self.stage1 {
            stage1.reset();
        }
        self.stage2.reset();
        self.stage3.reset();
        self.stage1_out.clear();
        self.stage2_out.clear();
        self.plane_l.clear();
        self.plane_r.clear();
    }
}

impl DsdRenderer {
    /// `source_rate` is the decoded media rate (44100, 48000, etc.). Returns an
    /// error if the modulator coefficient tables haven't been calibrated yet
    /// (i.e. `tools/gen_crfb.py` has not been run).
    pub fn new(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator(filter_type, source_rate, dsd_rate, DsdModulator::default())
    }

    pub fn new_with_dsd_modulator(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
        )
    }

    pub fn new_with_dsd_modulator_and_coeffs(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        coeffs_override: Option<&'static ModulatorCoeffs>,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty_and_coeffs(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
            coeffs_override,
            DsdExperimentTweaks::default(),
        )
    }

    pub fn new_with_dsd_modulator_and_experiment_tweaks(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        coeffs_override: Option<&'static ModulatorCoeffs>,
        experiment_tweaks: DsdExperimentTweaks,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty_and_coeffs(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
            coeffs_override,
            experiment_tweaks,
        )
    }

    pub fn new_with_dsd_modulator_and_isi_penalty(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty_and_coeffs(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            isi_penalty,
            None,
            DsdExperimentTweaks::default(),
        )
    }

    pub fn new_with_dsd_modulator_and_isi_penalty_and_coeffs(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
        coeffs_override: Option<&'static ModulatorCoeffs>,
        experiment_tweaks: DsdExperimentTweaks,
    ) -> Result<Self, &'static str> {
        let target_rate = dsd_rate.wire_rate_for_source(source_rate).ok_or(
            "DSD output requires a 44.1 kHz- or 48 kHz-family source rate \
             (44.1/88.2/176.4 or 48/96/192 kHz)",
        )?;
        Self::new_with_wire_rate(
            filter_type,
            source_rate,
            dsd_rate,
            target_rate,
            dsd_modulator,
            isi_penalty,
            coeffs_override,
            experiment_tweaks,
        )
    }

    pub fn new_44k_family(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
    ) -> Result<Self, &'static str> {
        Self::new_44k_family_with_dsd_modulator(
            filter_type,
            source_rate,
            dsd_rate,
            DsdModulator::default(),
        )
    }

    pub fn new_44k_family_with_dsd_modulator(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Result<Self, &'static str> {
        Self::new_44k_family_with_dsd_modulator_and_isi_penalty(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
        )
    }

    pub fn new_44k_family_with_dsd_modulator_and_isi_penalty(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
    ) -> Result<Self, &'static str> {
        if !is_44k_family(source_rate) && !is_48k_family(source_rate) {
            return Err(
                "DSD output requires a 44.1 kHz- or 48 kHz-family source rate \
                 (44.1/88.2/176.4 or 48/96/192 kHz)",
            );
        }
        if is_48k_family(source_rate) && !matches!(source_rate, 48_000 | 96_000 | 192_000) {
            return Err("44.1-family DSD forcing supports 48/96/192 kHz sources");
        }
        let target_rate = dsd_rate.wire_rate_44k_family();
        if target_rate <= source_rate {
            return Err("DSD output target rate must be above the source rate");
        }
        Self::new_with_wire_rate(
            filter_type,
            source_rate,
            dsd_rate,
            target_rate,
            dsd_modulator,
            isi_penalty,
            None,
            DsdExperimentTweaks::default(),
        )
    }

    // DSD construction keeps rate, modulator, and experiment inputs explicit at the mode boundary.
    #[allow(clippy::too_many_arguments)]
    fn new_with_wire_rate(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        target_rate: u32,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
        coeffs_override: Option<&'static ModulatorCoeffs>,
        experiment_tweaks: DsdExperimentTweaks,
    ) -> Result<Self, &'static str> {
        if !dsd_rate.supports_modulator(dsd_modulator) {
            return Err("DSD512 and DSD1024 currently support only the Standard modulator");
        }
        if dsd_modulator == DsdModulator::SeventhOrderSearch {
            // Reject negative and non-finite values rather than silently
            // normalizing either one to zero.
            if isi_penalty != 0.0 {
                return Err("7th Order Search requires zero ISI compensation");
            }
        }
        let upsampler = DsdUpsampler::new(filter_type, source_rate, target_rate);
        let lookahead_depth = dsd_modulator.lookahead_depth();
        let isi_penalty = sanitize_isi_penalty(isi_penalty);
        let experiment_tweaks =
            experiment_tweaks.with_production_policy_defaults(dsd_rate, dsd_modulator);
        let coefficient_table = select_modulator_coeffs(
            filter_type,
            dsd_rate,
            dsd_modulator,
            coeffs_override,
            experiment_tweaks,
        )?;
        let coeffs = coefficient_table.coeffs;
        if crate::audio::debug::audio_debug_enabled() {
            eprintln!(
                "AudioWorker DEBUG: DSD renderer init: source={}Hz wire={}Hz dop_frame={}Hz rate={:?} filter={} upsampler={} modulator={} lookahead={} isi_penalty={:.5} coeff_osr={} coeff_obg={:.2} input_peak={:.6}",
                source_rate,
                upsampler.target_rate(),
                upsampler.target_rate() / 16,
                dsd_rate,
                filter_type.as_name(),
                upsampler.debug_name(),
                dsd_modulator.as_name(),
                lookahead_depth,
                isi_penalty,
                coeffs.osr,
                coeffs.obg,
                coeffs.input_peak,
            );
        }
        // Use two distinct seeds so L and R dither streams are independent.
        let modulator_seeds = [
            experiment_tweaks.seed_left.unwrap_or(DSD_MOD_SEED_LEFT),
            experiment_tweaks.seed_right.unwrap_or(DSD_MOD_SEED_RIGHT),
        ];
        let worker_l = ModulatorWorker::spawn(
            coeffs,
            modulator_seeds[0],
            dsd_modulator,
            isi_penalty,
            experiment_tweaks,
            upsampler.target_rate(),
            "dsd-mod-l",
        )?;
        let worker_r = ModulatorWorker::spawn(
            coeffs,
            modulator_seeds[1],
            dsd_modulator,
            isi_penalty,
            experiment_tweaks,
            upsampler.target_rate(),
            "dsd-mod-r",
        )?;
        Ok(Self {
            upsampler,
            worker_l,
            worker_r,
            in_flight: false,
            dop_packer: DopPacker::new(),
            native_packer: NativeDsdPacker::new(NativeDsdOrder::MsbFirst),
            pcm_scratch: Vec::new(),
            source_scratch_l: Vec::new(),
            source_scratch_r: Vec::new(),
            pcm_l: Vec::new(),
            pcm_r: Vec::new(),
            bits_l: Vec::new(),
            bits_r: Vec::new(),
            spare_bits_l: Vec::new(),
            spare_bits_r: Vec::new(),
            stability_resets_lr: [0; 2],
            state_clamps_lr: [0; 2],
            seventh_order_search_diagnostics_lr: [None, None],
            last_collected_modulation_time: Duration::ZERO,
            limiter_telemetry: DsdLimiterTelemetry::default(),
            truncation_telemetry: DsdTruncationTelemetry::default(),
            source_rate,
            dsd_rate,
            coeffs,
            coefficient_table_name: coefficient_table.name,
            modulator_seeds,
            dsd_modulator,
            isi_penalty,
            experiment_tweaks,
        })
    }

    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    /// Full-scale PCM is mapped to this coefficient-table input before the
    /// one-bit loop. Measurement decoders divide their reconstructed output by
    /// the same declared gain to return to the post-headroom PCM domain.
    pub fn modulator_input_peak(&self) -> f64 {
        self.coeffs.input_peak
    }

    /// Stable identity of the effective coefficient table. Explicit coefficient
    /// overrides report `"custom_override"` instead of impersonating a built-in
    /// table with the same numeric contents.
    pub fn coefficient_table_name(&self) -> &'static str {
        self.coefficient_table_name
    }

    /// Nominal oversampling ratio for which the effective coefficient table was
    /// designed. This is not necessarily the wire/source-rate ratio for hi-res PCM.
    pub fn coefficient_osr(&self) -> u32 {
        self.coeffs.osr
    }

    /// Out-of-band gain of the effective coefficient table.
    pub fn coefficient_obg(&self) -> f64 {
        self.coeffs.obg
    }

    /// Effective policy values after production defaults have been applied.
    /// This is an inspection boundary for deterministic measurement reports.
    pub fn effective_experiment_tweaks(&self) -> DsdExperimentTweaks {
        self.experiment_tweaks
    }

    /// Initial per-channel seeds actually passed to the modulator workers after
    /// filling any omitted seed with the production default.
    pub fn effective_modulator_seeds(&self) -> [u64; 2] {
        self.modulator_seeds
    }

    /// Output sample rate as seen on the wire by a DoP-capable DAC.
    /// DSD64 → 176.4 kHz, DSD128 → 352.8 kHz, DSD256 → 705.6 kHz.
    pub fn dop_frame_rate(&self) -> u32 {
        self.upsampler.target_rate() / 16
    }

    pub fn last_collected_modulation_time(&self) -> Duration {
        self.last_collected_modulation_time
    }

    pub fn reset(&mut self) {
        // Discard any block still in flight before resetting the modulators.
        self.collect_in_flight_into_bits();
        self.worker_l.submit(ModJob::Reset);
        self.worker_r.submit(ModJob::Reset);
        self.upsampler.reset();
        self.dop_packer.reset();
        self.native_packer.reset();
        self.pcm_scratch.clear();
        self.source_scratch_l.clear();
        self.source_scratch_r.clear();
        self.pcm_l.clear();
        self.pcm_r.clear();
        self.bits_l.clear();
        self.bits_r.clear();
        self.limiter_telemetry.current_block_peak_ratio = 0.0;
        self.limiter_telemetry.peak_ratio_max = 0.0;
        self.limiter_telemetry.current_block_gain = 1.0;
        self.limiter_telemetry.current_block_limited_samples = 0;
        self.seventh_order_search_diagnostics_lr = [None, None];
        self.last_collected_modulation_time = Duration::ZERO;
        self.truncation_telemetry = DsdTruncationTelemetry::default();
    }

    /// Counters lag by one block: they're refreshed each time a worker result is
    /// collected, which is exactly when its bits become observable downstream.
    pub fn stability_resets(&self) -> u64 {
        self.stability_resets_lr[0] + self.stability_resets_lr[1]
    }

    pub fn state_clamps(&self) -> u64 {
        self.state_clamps_lr[0] + self.state_clamps_lr[1]
    }

    pub fn seventh_order_search_diagnostics(&self) -> [Option<SeventhOrderSearchDiagnostics>; 2] {
        self.seventh_order_search_diagnostics_lr
    }

    pub fn limiter_telemetry(&self) -> DsdLimiterTelemetry {
        self.limiter_telemetry
    }

    pub fn truncation_telemetry(&self) -> DsdTruncationTelemetry {
        self.truncation_telemetry
    }

    /// Fold a collected worker result back into the renderer's buffer pools and
    /// health counters without touching `bits_l`/`bits_r`.
    fn recycle_collected(&mut self, output: ModOutput, left: bool) {
        let channel = if left { 0 } else { 1 };
        self.stability_resets_lr[channel] = output.stability_resets;
        self.state_clamps_lr[channel] = output.state_clamps;
        self.seventh_order_search_diagnostics_lr[channel] = output.seventh_order_search_diagnostics;
        if left {
            self.spare_bits_l = output.bits;
        } else {
            self.spare_bits_r = output.bits;
        }
        if let Some(input) = output.input {
            if left {
                self.pcm_l = input;
            } else {
                self.pcm_r = input;
            }
        }
    }

    pub fn dsd_modulator(&self) -> DsdModulator {
        self.dsd_modulator
    }

    pub fn isi_penalty(&self) -> f64 {
        self.isi_penalty
    }

    /// Stage 1: upsample decoded PCM up to the DSD rate.
    ///
    /// Returns a mutable view of an interleaved-stereo f64 buffer the caller can
    /// modify in place — e.g. to apply EQ or pre-modulator volume — before the
    /// modulate step. The buffer is owned by the renderer and will be reused on
    /// the next call.
    pub fn upsample(&mut self, samples_l: &[f64], samples_r: &[f64]) -> &mut Vec<f64> {
        if source_needs_sanitize(samples_l) || source_needs_sanitize(samples_r) {
            fill_sanitized_source_scratch(&mut self.source_scratch_l, samples_l);
            fill_sanitized_source_scratch(&mut self.source_scratch_r, samples_r);
            self.upsampler
                .input(&self.source_scratch_l, &self.source_scratch_r);
        } else {
            self.upsampler.input(samples_l, samples_r);
        }
        self.pcm_scratch.clear();
        self.upsampler.process(&mut self.pcm_scratch);
        &mut self.pcm_scratch
    }

    pub fn drain_resampler_eof(&mut self) -> &mut Vec<f64> {
        self.pcm_scratch.clear();
        self.upsampler.drain_eof(&mut self.pcm_scratch);
        &mut self.pcm_scratch
    }

    /// Materialize the current upsampler block in the exact scalar domain seen
    /// by 7th Order Search (`u`): post coefficient-table gain, mandatory headroom, block
    /// rider, and soft limiter. This is a quality-tool inspection boundary; it
    /// neither submits work to the modulators nor changes renderer telemetry.
    ///
    /// Keeping this helper on the renderer makes exact-oracle tooling share the
    /// production normalization implementation instead of approximating it in
    /// a second command-line program.
    #[doc(hidden)]
    pub fn seventh_order_search_oracle_modulator_input_block(
        &self,
        input_gain: f64,
    ) -> Result<(Vec<f64>, Vec<f64>), &'static str> {
        if self.dsd_modulator != DsdModulator::SeventhOrderSearch || self.dsd_rate != DsdRate::Dsd64
        {
            return Err(
                "7th Order Search oracle input inspection requires the DSD64 7th Order Search renderer",
            );
        }
        let mut left = Vec::new();
        let mut right = Vec::new();
        prepare_modulator_input_planes(
            &self.pcm_scratch,
            self.coeffs,
            self.dsd_modulator,
            self.experiment_tweaks,
            input_gain,
            &mut left,
            &mut right,
        );
        Ok((left, right))
    }

    /// Materialize the current upsampler block in the exact normalized PCM
    /// domain entering either production modulator. This research-only probe
    /// shares coefficient gain, mandatory headroom, block riding, and limiting
    /// with production, then divides out the coefficient-table input peak so
    /// Standard and 7th Order Search captures remain directly comparable in PCM units.
    #[cfg(feature = "research-filter-assets")]
    #[doc(hidden)]
    pub fn research_normalized_modulator_input_block(
        &self,
        input_gain: f64,
        left: &mut Vec<f64>,
        right: &mut Vec<f64>,
    ) {
        prepare_modulator_input_planes(
            &self.pcm_scratch,
            self.coeffs,
            self.dsd_modulator,
            self.experiment_tweaks,
            input_gain,
            left,
            right,
        );
        let inverse_peak = self.coeffs.input_peak.recip();
        for sample in left.iter_mut().chain(right.iter_mut()) {
            *sample *= inverse_peak;
        }
    }

    /// Stage 2 (pipelined): hand the upsampled buffer produced by [`upsample`] to
    /// the modulator workers and surface the *previous* block's bits for packing.
    /// Output therefore lags input by one block; the end-of-stream flush emits the
    /// held block.
    ///
    /// `input_gain` is multiplied after mapping PCM full scale to the selected
    /// coefficient table's measured modulator input peak.
    /// Use this to apply user volume (and any EQ-related makeup gain) — DoP bytes
    /// cannot be scaled downstream without scrambling the 0x05/0xFA markers.
    fn modulate(&mut self, input_gain: f64) -> bool {
        let frames = self.pcm_scratch.len() / 2;
        if frames == 0 {
            // Preserve any already-collected block until the next non-empty block
            // or EOF flush. Collecting here would make an empty upsample call
            // unexpectedly surface delayed DSD bits.
            return false;
        }
        // Collect the previous block first so its input planes are free for reuse.
        self.collect_in_flight_into_bits();
        let has_packable_bits = self.truncate_current_bits_to_equal_len();

        let prepared = prepare_modulator_input_planes(
            &self.pcm_scratch,
            self.coeffs,
            self.dsd_modulator,
            self.experiment_tweaks,
            input_gain,
            &mut self.pcm_l,
            &mut self.pcm_r,
        );
        self.record_limiter_block(
            prepared.block_peak,
            prepared.headroom_gain,
            prepared.block_limited_samples,
        );

        self.worker_l.submit(ModJob::Process {
            input: std::mem::take(&mut self.pcm_l),
            bits: std::mem::take(&mut self.spare_bits_l),
            submitted_at: Instant::now(),
        });
        self.worker_r.submit(ModJob::Process {
            input: std::mem::take(&mut self.pcm_r),
            bits: std::mem::take(&mut self.spare_bits_r),
            submitted_at: Instant::now(),
        });
        self.in_flight = true;

        has_packable_bits
    }

    fn record_limiter_block(
        &mut self,
        block_peak: f64,
        block_gain: f64,
        block_limited_samples: u64,
    ) {
        record_limiter_telemetry(
            &mut self.limiter_telemetry,
            self.coeffs.input_peak,
            block_peak,
            block_gain,
            block_limited_samples,
        );
    }

    pub fn modulate_and_pack(&mut self, input_gain: f64, out: &mut Vec<i32>) {
        if self.modulate(input_gain) {
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
        }
    }

    pub fn render_profiled(
        &mut self,
        samples_l: &[f64],
        samples_r: &[f64],
        input_gain: f64,
        out: &mut Vec<i32>,
    ) -> DsdRenderTiming {
        let mut timing = DsdRenderTiming::default();

        let start = Instant::now();
        self.upsample(samples_l, samples_r);
        timing.upsample = start.elapsed();

        let start = Instant::now();
        let has_packable_bits = self.modulate(input_gain);
        timing.modulate_submit_collect = start.elapsed();

        if has_packable_bits {
            let start = Instant::now();
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
            timing.pack = start.elapsed();
        }

        timing
    }

    pub fn set_native_order(&mut self, order: NativeDsdOrder) {
        self.native_packer.set_order(order);
    }

    pub fn modulate_and_pack_native(
        &mut self,
        input_gain: f64,
        out_l: &mut Vec<u8>,
        out_r: &mut Vec<u8>,
    ) {
        if self.modulate(input_gain) {
            self.native_packer
                .push_stream(&self.bits_l, &self.bits_r, out_l, out_r);
        }
    }

    /// End-of-stream flush: emit the EC modulators' held lookahead tail through the
    /// DoP packer. No-op in Standard mode (the modulators hold no latency). Call once
    /// at track end, after the final [`modulate_and_pack`](Self::modulate_and_pack).
    pub fn flush_modulators_and_pack(&mut self, out: &mut Vec<i32>) {
        if self.flush_modulators() {
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
        }
    }

    pub fn flush_modulators_and_pack_profiled(&mut self, out: &mut Vec<i32>) -> DsdRenderTiming {
        let mut timing = DsdRenderTiming::default();

        let start = Instant::now();
        let has_packable_bits = self.flush_modulators();
        timing.flush_modulators = start.elapsed();

        if has_packable_bits {
            let start = Instant::now();
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
            timing.flush_pack = start.elapsed();
        }

        timing
    }

    /// Native-DSD counterpart of [`flush_modulators_and_pack`](Self::flush_modulators_and_pack).
    /// Call before [`flush_native_with_idle`](Self::flush_native_with_idle) so the tail
    /// bits land ahead of the idle padding.
    pub fn flush_modulators_and_pack_native(&mut self, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        if self.flush_modulators() {
            self.native_packer
                .push_stream(&self.bits_l, &self.bits_r, out_l, out_r);
        }
    }

    /// Pull the in-flight block's bits into `bits_l`/`bits_r` (clearing them if
    /// nothing is in flight) and recycle the freed buffers. Leaves `in_flight` false.
    fn collect_in_flight_into_bits(&mut self) {
        if !self.in_flight {
            self.bits_l.clear();
            self.bits_r.clear();
            return;
        }
        let out_l = self.worker_l.collect();
        let out_r = self.worker_r.collect();
        self.last_collected_modulation_time = out_l.turnaround_time.max(out_r.turnaround_time);
        let prev_bits_l = std::mem::replace(&mut self.bits_l, out_l.bits);
        let prev_bits_r = std::mem::replace(&mut self.bits_r, out_r.bits);
        self.recycle_collected(
            ModOutput {
                bits: prev_bits_l,
                input: out_l.input,
                turnaround_time: out_l.turnaround_time,
                stability_resets: out_l.stability_resets,
                state_clamps: out_l.state_clamps,
                seventh_order_search_diagnostics: out_l.seventh_order_search_diagnostics,
            },
            true,
        );
        self.recycle_collected(
            ModOutput {
                bits: prev_bits_r,
                input: out_r.input,
                turnaround_time: out_r.turnaround_time,
                stability_resets: out_r.stability_resets,
                state_clamps: out_r.state_clamps,
                seventh_order_search_diagnostics: out_r.seventh_order_search_diagnostics,
            },
            false,
        );
        self.in_flight = false;
    }

    fn flush_modulators(&mut self) -> bool {
        // First reel in the pipelined block still held by the workers…
        self.collect_in_flight_into_bits();

        // …then append 7th Order Search's delayed tail (empty in Standard mode).
        self.worker_l.submit(ModJob::Flush {
            bits: std::mem::take(&mut self.spare_bits_l),
            submitted_at: Instant::now(),
        });
        self.worker_r.submit(ModJob::Flush {
            bits: std::mem::take(&mut self.spare_bits_r),
            submitted_at: Instant::now(),
        });
        let tail_l = self.worker_l.collect();
        let tail_r = self.worker_r.collect();
        self.bits_l.extend_from_slice(&tail_l.bits);
        self.bits_r.extend_from_slice(&tail_r.bits);
        self.recycle_collected(tail_l, true);
        self.recycle_collected(tail_r, false);

        self.truncate_current_bits_to_equal_len()
    }

    fn truncate_current_bits_to_equal_len(&mut self) -> bool {
        // Both channels normally see the same frame count. If a worker ever returns
        // divergent lengths, keep the DoP/native packers from seeing an L/R desync.
        let left_len = self.bits_l.len();
        let right_len = self.bits_r.len();
        let len = self.bits_l.len().min(self.bits_r.len());
        if left_len != right_len {
            self.truncation_telemetry.events = self.truncation_telemetry.events.wrapping_add(1);
            self.truncation_telemetry.discarded_left_bits = self
                .truncation_telemetry
                .discarded_left_bits
                .wrapping_add(left_len.saturating_sub(len) as u64);
            self.truncation_telemetry.discarded_right_bits = self
                .truncation_telemetry
                .discarded_right_bits
                .wrapping_add(right_len.saturating_sub(len) as u64);
            self.truncation_telemetry.last_left_len = left_len;
            self.truncation_telemetry.last_right_len = right_len;
            self.truncation_telemetry.last_kept_len = len;
        }
        self.bits_l.truncate(len);
        self.bits_r.truncate(len);
        len > 0
    }

    pub fn flush_native_with_idle(&mut self, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        self.native_packer.flush_with_idle(out_l, out_r);
    }

    /// Convenience wrapper: upsample + modulate + pack with no in-band processing
    /// and unity volume. Kept for tests and callers that don't need EQ/volume.
    pub fn render(&mut self, samples_l: &[f64], samples_r: &[f64], out: &mut Vec<i32>) {
        self.upsample(samples_l, samples_r);
        self.modulate_and_pack(1.0, out);
    }
}

fn is_44k_family(source_rate: u32) -> bool {
    source_rate != 0 && source_rate.is_multiple_of(44_100)
}

fn is_48k_family(source_rate: u32) -> bool {
    source_rate != 0 && source_rate.is_multiple_of(48_000)
}

fn limit_modulator_input(sample: f64, input_peak: f64) -> f64 {
    ModulatorInputLimiter::new(input_peak).limit(sample)
}

fn block_headroom_gain(block_peak: f64, target_peak: f64) -> f64 {
    if block_peak.is_finite()
        && target_peak.is_finite()
        && block_peak > target_peak
        && target_peak > 0.0
    {
        target_peak / block_peak
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PreparedModulatorInput {
    block_peak: f64,
    headroom_gain: f64,
    block_limited_samples: u64,
}

#[allow(clippy::too_many_arguments)]
fn prepare_modulator_input_planes(
    pcm_scratch: &[f64],
    coeffs: &ModulatorCoeffs,
    dsd_modulator: DsdModulator,
    experiment_tweaks: DsdExperimentTweaks,
    input_gain: f64,
    pcm_l: &mut Vec<f64>,
    pcm_r: &mut Vec<f64>,
) -> PreparedModulatorInput {
    prepare_modulator_input_planes_for_peak(
        pcm_scratch,
        coeffs.input_peak,
        dsd_modulator,
        experiment_tweaks,
        input_gain,
        pcm_l,
        pcm_r,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_modulator_input_planes_for_peak(
    pcm_scratch: &[f64],
    input_peak: f64,
    dsd_modulator: DsdModulator,
    experiment_tweaks: DsdExperimentTweaks,
    input_gain: f64,
    pcm_l: &mut Vec<f64>,
    pcm_r: &mut Vec<f64>,
) -> PreparedModulatorInput {
    let frames = pcm_scratch.len() / 2;
    let effective_input_gain =
        effective_modulator_input_gain(dsd_modulator, input_gain, experiment_tweaks.input_gain_db);
    let gain = input_peak * effective_input_gain;
    let limiter = ModulatorInputLimiter::new(input_peak);
    let mut block_peak = 0.0f64;
    let mut block_limited_samples = 0_u64;
    pcm_l.clear();
    pcm_r.clear();
    pcm_l.reserve(frames);
    pcm_r.reserve(frames);
    for chunk in pcm_scratch.chunks_exact(2) {
        let raw_l = chunk[0] * gain;
        let raw_r = chunk[1] * gain;
        if raw_l.is_finite() {
            block_peak = block_peak.max(raw_l.abs());
        }
        if raw_r.is_finite() {
            block_peak = block_peak.max(raw_r.abs());
        }
        if limiter.knee_touched(raw_l) {
            block_limited_samples += 1;
        }
        if limiter.knee_touched(raw_r) {
            block_limited_samples += 1;
        }
        pcm_l.push(limiter.limit(raw_l));
        pcm_r.push(limiter.limit(raw_r));
    }
    let headroom_gain = block_headroom_gain(block_peak, limiter.knee_start());
    if headroom_gain < 1.0 {
        pcm_l.clear();
        pcm_r.clear();
        let ridden_gain = gain * headroom_gain;
        for chunk in pcm_scratch.chunks_exact(2) {
            let raw_l = chunk[0] * ridden_gain;
            let raw_r = chunk[1] * ridden_gain;
            pcm_l.push(limiter.limit(raw_l));
            pcm_r.push(limiter.limit(raw_r));
        }
    }
    PreparedModulatorInput {
        block_peak,
        headroom_gain,
        block_limited_samples,
    }
}

fn record_limiter_telemetry(
    telemetry: &mut DsdLimiterTelemetry,
    input_peak: f64,
    block_peak: f64,
    block_gain: f64,
    block_limited_samples: u64,
) {
    let peak_ratio = if input_peak > 0.0 {
        (block_peak / input_peak).min(f32::MAX as f64) as f32
    } else {
        0.0
    };
    telemetry.current_block_peak_ratio = peak_ratio;
    telemetry.peak_ratio_max = telemetry.peak_ratio_max.max(peak_ratio);
    telemetry.current_block_gain = block_gain.min(f32::MAX as f64) as f32;
    telemetry.current_block_limited_samples = block_limited_samples;
    if block_limited_samples > 0 {
        telemetry.limited_events += 1;
        telemetry.limited_samples += block_limited_samples;
    }
}

#[derive(Clone, Copy)]
struct ModulatorInputLimiter {
    input_limit: f64,
    knee_start: f64,
    knee_width: f64,
}

impl ModulatorInputLimiter {
    fn new(input_peak: f64) -> Self {
        if !input_peak.is_finite() || input_peak <= 0.0 {
            return Self {
                input_limit: 0.0,
                knee_start: 0.0,
                knee_width: 0.0,
            };
        }
        let input_limit = input_peak.abs();
        let knee_start = input_limit * DSD_LIMITER_KNEE_RATIO;
        Self {
            input_limit,
            knee_start,
            knee_width: input_limit - knee_start,
        }
    }

    fn limit(self, sample: f64) -> f64 {
        if !sample.is_finite() || self.input_limit <= 0.0 {
            return 0.0;
        }

        let magnitude = sample.abs();
        if magnitude <= self.knee_start {
            return sample;
        }

        if self.knee_width <= f64::EPSILON {
            return sample.signum() * self.input_limit;
        }

        let excess = (magnitude - self.knee_start) / self.knee_width;
        let limited = self.knee_start + self.knee_width * excess.tanh().min(1.0);
        sample.signum() * limited.min(self.input_limit)
    }

    fn knee_touched(self, sample: f64) -> bool {
        sample.is_finite() && self.input_limit > 0.0 && sample.abs() > self.knee_start
    }

    fn knee_start(self) -> f64 {
        self.knee_start
    }
}

fn source_abs_peak(samples: &[f64]) -> f64 {
    samples
        .iter()
        .copied()
        .map(|sample| sample.abs())
        .fold(0.0, f64::max)
}

fn source_needs_sanitize(samples: &[f64]) -> bool {
    samples.iter().any(|sample| !sample.is_finite())
}

fn fill_sanitized_source_scratch(dst: &mut Vec<f64>, src: &[f64]) {
    dst.clear();
    dst.reserve(src.len());
    dst.extend(src.iter().map(|&sample| sanitize_source_sample(sample)));
}

fn sanitize_source_sample(sample: f64) -> f64 {
    if sample.is_finite() { sample } else { 0.0 }
}

fn interleave_stereo(samples_l: &[f64], samples_r: &[f64], out: &mut Vec<f64>) {
    out.clear();
    let frames = samples_l.len().min(samples_r.len());
    out.reserve(frames * 2);
    for idx in 0..frames {
        out.push(samples_l[idx]);
        out.push(samples_r[idx]);
    }
}

fn feed_interleaved(
    resampler: &mut SincResampler,
    interleaved: &[f64],
    plane_l: &mut Vec<f64>,
    plane_r: &mut Vec<f64>,
) {
    plane_l.clear();
    plane_r.clear();
    plane_l.reserve(interleaved.len() / 2);
    plane_r.reserve(interleaved.len() / 2);
    for frame in interleaved.chunks_exact(2) {
        plane_l.push(frame[0]);
        plane_r.push(frame[1]);
    }
    resampler.input(plane_l, plane_r);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILTER: FilterType = FilterType::SplitPhase128kE3;

    #[test]
    fn rate_support_matches_the_two_active_modulators() {
        for rate in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256] {
            assert!(rate.supports_modulator(DsdModulator::Standard));
            assert!(rate.supports_modulator(DsdModulator::SeventhOrderSearch));
        }
        for rate in [DsdRate::Dsd512, DsdRate::Dsd1024] {
            assert!(rate.supports_modulator(DsdModulator::Standard));
            assert!(!rate.supports_modulator(DsdModulator::SeventhOrderSearch));
        }
    }

    #[test]
    fn standard_renderer_uses_the_standard_rate_table() {
        let renderer = DsdRenderer::new_with_dsd_modulator(
            FILTER,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::Standard,
        )
        .unwrap();

        assert_eq!(renderer.dsd_modulator(), DsdModulator::Standard);
        assert_eq!(renderer.coefficient_table_name(), "CRFB7_STANDARD_OSR128");
        assert_eq!(renderer.coefficient_osr(), 128);
        assert_eq!(renderer.seventh_order_search_diagnostics(), [None, None]);
    }

    #[test]
    fn seventh_order_search_renderer_uses_its_isolated_production_policy() {
        let renderer = DsdRenderer::new_with_dsd_modulator(
            FILTER,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::SeventhOrderSearch,
        )
        .unwrap();

        assert_eq!(renderer.dsd_modulator(), DsdModulator::SeventhOrderSearch);
        assert_eq!(
            renderer.coefficient_table_name(),
            "SEVENTH_ORDER_SEARCH_OSR128_OBG164_INPUT468_V1"
        );
        assert_eq!(
            renderer
                .effective_experiment_tweaks()
                .seventh_order_search_full_diagnostics,
            Some(false)
        );
    }

    #[test]
    fn explicit_seventh_order_search_research_config_enables_diagnostics_by_default() {
        let renderer = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FILTER,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::SeventhOrderSearch,
            None,
            DsdExperimentTweaks {
                seventh_order_search_config: Some(SeventhOrderSearchExperimentConfig::default()),
                ..DsdExperimentTweaks::default()
            },
        )
        .unwrap();

        assert_eq!(
            renderer
                .effective_experiment_tweaks()
                .seventh_order_search_full_diagnostics,
            Some(true)
        );
    }

    #[test]
    fn seventh_order_search_rejects_isi_and_coefficient_overrides() {
        assert_eq!(
            DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
                FILTER,
                44_100,
                DsdRate::Dsd64,
                DsdModulator::SeventhOrderSearch,
                0.01,
            )
            .err(),
            Some("7th Order Search requires zero ISI compensation")
        );
        assert_eq!(
            DsdRenderer::new_with_dsd_modulator_and_coeffs(
                FILTER,
                44_100,
                DsdRate::Dsd64,
                DsdModulator::SeventhOrderSearch,
                Some(&CRFB7_STANDARD_OSR64),
            )
            .err(),
            Some("7th Order Search coefficient overrides are not supported")
        );
    }

    #[test]
    fn source_windows_map_exactly_to_wire_samples() {
        assert_eq!(
            dsd_source_window_to_modulator_samples(FILTER, 44_100, 2_822_400, 10, 20,),
            Some(640..1920)
        );
        assert_eq!(
            dsd_source_window_to_modulator_samples(FILTER, 44_100, 3_000_000, 0, 1),
            None
        );
    }

    #[test]
    fn both_modulators_complete_a_small_native_render() {
        for modulator in [DsdModulator::Standard, DsdModulator::SeventhOrderSearch] {
            let mut renderer =
                DsdRenderer::new_with_dsd_modulator(FILTER, 44_100, DsdRate::Dsd64, modulator)
                    .unwrap();
            let left: Vec<f64> = (0..32)
                .map(|index| 0.1 * (index as f64 * 0.13).sin())
                .collect();
            let right = left.clone();
            let mut out_l = Vec::new();
            let mut out_r = Vec::new();

            renderer.upsample(&left, &right);
            renderer.modulate_and_pack_native(1.0, &mut out_l, &mut out_r);
            renderer.drain_resampler_eof();
            renderer.modulate_and_pack_native(1.0, &mut out_l, &mut out_r);
            renderer.flush_modulators_and_pack_native(&mut out_l, &mut out_r);
            assert!(
                renderer.last_collected_modulation_time() > Duration::ZERO,
                "{modulator:?} worker processing time was not retained"
            );

            assert_eq!(out_l.len(), out_r.len());
            assert!(!out_l.is_empty());
            assert_eq!(renderer.stability_resets(), 0);
            assert_eq!(renderer.state_clamps(), 0);
        }
    }
}
