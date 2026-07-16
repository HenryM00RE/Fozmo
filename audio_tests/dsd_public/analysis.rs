use std::f64::consts::PI;
use std::ops::Range;

use fozmo::audio::dsp::resampler::{FilterType, SincResampler};
use realfft::RealFftPlanner;
use serde::Serialize;

pub const RECONSTRUCTION_ALGORITHM_VERSION: &str =
    "reconstruction-v2-sinc-runtime-context-bh-spurs-sine-dbfs";
/// The SincExtreme32k downsampling cascade reports about 99 ms of latency for
/// the 176.4 kHz decoder. Keep a fixed floor as well as a margin over the
/// runtime value so a windowed decode never substitutes zeroes inside the
/// composite decoder's settled support.
const MIN_DECODER_CONTEXT_SECONDS: f64 = 0.100;
const DECODER_CONTEXT_MARGIN_SECONDS: f64 = 0.010;
/// Bound temporary unpacked one-bit PCM to roughly 1 Mi samples per channel.
/// Long transition windows can span tens of millions of DSD bits.
const DECODER_UNPACK_CHUNK_BITS: usize = 1 << 20;
/// Reflection context for the short raised-cosine reconstruction response.
const PROFILE_CONTEXT_SECONDS: f64 = 0.050;
const MIN_DB: f64 = -360.0;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReconstructionProfile {
    pub id: &'static str,
    pub output_rate: u32,
    pub pass_hz: f64,
    pub stop_hz: f64,
    pub passband_ripple_db: f64,
    pub declared_stopband_attenuation_db: f64,
}

pub const AUDIO_BAND: ReconstructionProfile = ReconstructionProfile {
    id: "reconstruction-v1-audio-20k-24k",
    output_rate: 176_400,
    pass_hz: 20_000.0,
    stop_hz: 24_000.0,
    passband_ripple_db: 0.0,
    declared_stopband_attenuation_db: 140.0,
};

pub const HIRES_BAND: ReconstructionProfile = ReconstructionProfile {
    id: "reconstruction-v1-hires-80k-96k",
    output_rate: 352_800,
    pass_hz: 80_000.0,
    stop_hz: 96_000.0,
    passband_ripple_db: 0.0,
    declared_stopband_attenuation_db: 140.0,
};

#[derive(Debug, Clone, Copy)]
pub enum WindowKind {
    Rectangular,
    BlackmanHarris4,
}

impl WindowKind {
    fn half_width_bins(self) -> usize {
        match self {
            Self::Rectangular => 0,
            Self::BlackmanHarris4 => 6,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SpurMetric {
    pub frequency_hz: f64,
    /// Equivalent peak-sine dBFS. A broadband or DC cluster is converted from
    /// RMS power using the same full-scale-sine reference.
    pub level_dbfs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CarrierMetric {
    pub name: String,
    pub frequency_hz: f64,
    /// Peak-sine dBFS; a sine with peak amplitude 1.0 is 0 dBFS.
    pub expected_level_dbfs: f64,
    /// Peak-sine dBFS; a sine with peak amplitude 1.0 is 0 dBFS.
    pub measured_level_dbfs: f64,
    pub gain_error_db: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeasuredTone {
    pub name: String,
    pub frequency_hz: f64,
    /// Peak-sine dBFS; a sine with peak amplitude 1.0 is 0 dBFS.
    pub level_dbfs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToneMetrics {
    pub carrier: CarrierMetric,
    pub sinad_db: f64,
    pub thd_db: f64,
    /// Integrated RMS power, referenced so a full-scale sine is 0 dBFS.
    pub residual_noise_dbfs: f64,
    pub worst_nonharmonic_spur: SpurMetric,
    pub reconstructed_dc: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoiseMetrics {
    /// Integrated RMS power, referenced so a full-scale sine is 0 dBFS.
    pub integrated_noise_dbfs: f64,
    pub worst_spur: SpurMetric,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeclaredToneMetrics {
    pub carriers: Vec<CarrierMetric>,
    pub declared_products: Vec<MeasuredTone>,
    /// Conventional SINAD: carrier power divided by every non-carrier bin in
    /// the requested band, including declared distortion products.
    pub sinad_db: f64,
    /// Integrated RMS residual after carriers and declared products are removed,
    /// referenced so a full-scale sine is 0 dBFS.
    pub residual_excluding_declared_products_dbfs: f64,
    pub worst_declared_product: Option<MeasuredTone>,
    pub worst_unexpected_spur: SpurMetric,
}

#[derive(Debug, Clone, Serialize)]
pub struct BandResidualMetrics {
    pub low_hz: f64,
    pub high_hz: f64,
    /// Integrated RMS power, referenced so a full-scale sine is 0 dBFS.
    pub residual_dbfs: f64,
    pub worst_unexpected_spur: SpurMetric,
}

#[derive(Debug, Clone, Serialize)]
pub struct MultiBandMetrics {
    pub carriers: Vec<CarrierMetric>,
    pub bands: Vec<BandResidualMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DensityMetrics {
    pub density: f64,
    pub deviation: f64,
    pub rolling_max_deviation: f64,
    pub rolling_window_bits: usize,
}

/// A sinusoidal least-squares model whose phase origin is the first sample of
/// the interval passed to `fit_tone_model`.
#[derive(Debug, Clone)]
pub struct FittedToneModel {
    sample_rate: u32,
    frequencies_hz: Vec<f64>,
    coefficients: Vec<(f64, f64)>,
}

pub fn fit_tone_model(
    samples: &[f64],
    sample_rate: u32,
    frequencies_hz: &[f64],
) -> Result<FittedToneModel, String> {
    validate_analysis_input(samples, sample_rate)?;
    Ok(FittedToneModel {
        sample_rate,
        frequencies_hz: frequencies_hz.to_vec(),
        coefficients: fit_tones(samples, sample_rate, frequencies_hz)?,
    })
}

/// Subtract a fitted tone model from `samples`. `first_index_from_fit_start`
/// lets a model fitted on a settled tail be evaluated earlier in the same
/// waveform, including with a negative offset.
pub fn residual_against_tone_model(
    samples: &[f64],
    model: &FittedToneModel,
    first_index_from_fit_start: isize,
) -> Result<Vec<f64>, String> {
    validate_analysis_input(samples, model.sample_rate)?;
    Ok(samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            sample
                - predict_tones(
                    first_index_from_fit_start + index as isize,
                    model.sample_rate,
                    &model.frequencies_hz,
                    &model.coefficients,
                )
        })
        .collect())
}

/// Remove all declared coherent lines and DC before applying a finite-window
/// unknown-spur estimator. Masking bins alone leaves the window sidelobes of a
/// loud carrier in the search floor, which can overwhelm very small real lines.
fn remove_tones_and_mean(
    samples: &[f64],
    sample_rate: u32,
    frequencies_hz: &[f64],
) -> Result<Vec<f64>, String> {
    validate_analysis_input(samples, sample_rate)?;
    if frequencies_hz.is_empty() {
        let dc = mean(samples);
        return Ok(samples.iter().map(|sample| sample - dc).collect());
    }
    let (dc, coefficients) = fit_tones_and_dc(samples, sample_rate, frequencies_hz)?;
    Ok(samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            sample - dc - predict_tones(index as isize, sample_rate, frequencies_hz, &coefficients)
        })
        .collect())
}

pub fn reconstruct_stereo_window(
    left_bytes: &[u8],
    right_bytes: &[u8],
    wire_rate: u32,
    bit_range: Range<usize>,
    profile: ReconstructionProfile,
    modulator_input_peak: f64,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    if left_bytes.len() != right_bytes.len() {
        return Err("native DSD channel byte lengths differ".to_string());
    }
    if profile.output_rate == 0 || !wire_rate.is_multiple_of(profile.output_rate) {
        return Err(format!(
            "wire rate {wire_rate} is not an integer multiple of reconstruction rate {}",
            profile.output_rate
        ));
    }
    if !modulator_input_peak.is_finite() || modulator_input_peak <= 0.0 {
        return Err("modulator input-peak normalization must be finite and positive".to_string());
    }
    let total_bits = left_bytes.len().saturating_mul(8);
    if bit_range.start >= bit_range.end || bit_range.end > total_bits {
        return Err(format!(
            "invalid reconstruction bit range {}..{} for {total_bits} bits",
            bit_range.start, bit_range.end
        ));
    }
    let ratio = (wire_rate / profile.output_rate) as usize;
    if !bit_range.start.is_multiple_of(ratio) || !bit_range.end.is_multiple_of(ratio) {
        return Err(format!(
            "reconstruction range {}..{} is not aligned to decimation ratio {ratio}",
            bit_range.start, bit_range.end
        ));
    }

    let mut decimator =
        SincResampler::new(FilterType::SincExtreme32k, wire_rate, profile.output_rate);
    let context_output = decoder_context_output_frames(&decimator, profile.output_rate);
    let context_bits = context_output.saturating_mul(ratio);
    let extended_start = bit_range.start.saturating_sub(context_bits) / ratio * ratio;
    let extended_end = bit_range
        .end
        .saturating_add(context_bits)
        .min(total_bits)
        .div_ceil(ratio)
        .saturating_mul(ratio)
        .min(total_bits);
    let expected_extended = (extended_end - extended_start) / ratio;
    let mut left = Vec::with_capacity(expected_extended);
    let mut right = Vec::with_capacity(expected_extended);
    let mut interleaved = Vec::new();
    let unpack_chunk_bits = (DECODER_UNPACK_CHUNK_BITS / ratio).max(1) * ratio;
    let mut cursor = extended_start;
    while cursor < extended_end {
        let chunk_end = cursor.saturating_add(unpack_chunk_bits).min(extended_end);
        let left_bits = unpack_native_msb_range(left_bytes, cursor..chunk_end)?;
        let right_bits = unpack_native_msb_range(right_bytes, cursor..chunk_end)?;
        decimator.input(&left_bits, &right_bits);
        decimator.process(&mut interleaved);
        take_interleaved_stereo(&mut interleaved, &mut left, &mut right)?;
        cursor = chunk_end;
    }
    decimator.drain_eof(&mut interleaved);
    take_interleaved_stereo(&mut interleaved, &mut left, &mut right)?;
    if left.len() != expected_extended || right.len() != expected_extended {
        return Err(format!(
            "decoder length mismatch: expected {expected_extended}, got L={} R={}",
            left.len(),
            right.len()
        ));
    }

    let left = apply_reconstruction_profile(&left, profile)?;
    let right = apply_reconstruction_profile(&right, profile)?;
    let crop_start = (bit_range.start - extended_start) / ratio;
    let crop_len = (bit_range.end - bit_range.start) / ratio;
    let crop_end = crop_start + crop_len;
    if crop_end > left.len() || crop_end > right.len() {
        return Err("reconstruction crop exceeded decoded samples".to_string());
    }
    let left = left[crop_start..crop_end]
        .iter()
        .map(|sample| sample / modulator_input_peak)
        .collect::<Vec<_>>();
    let right = right[crop_start..crop_end]
        .iter()
        .map(|sample| sample / modulator_input_peak)
        .collect::<Vec<_>>();
    validate_finite(&left, "left reconstructed samples")?;
    validate_finite(&right, "right reconstructed samples")?;
    Ok((left, right))
}

fn take_interleaved_stereo(
    interleaved: &mut Vec<f64>,
    left: &mut Vec<f64>,
    right: &mut Vec<f64>,
) -> Result<(), String> {
    if !interleaved.len().is_multiple_of(2) {
        return Err("decoder produced a partial stereo frame".to_string());
    }
    for frame in interleaved.chunks_exact(2) {
        left.push(frame[0]);
        right.push(frame[1]);
    }
    interleaved.clear();
    Ok(())
}

fn decoder_context_output_frames(decimator: &SincResampler, output_rate: u32) -> usize {
    let decoder_context_seconds = (decimator.latency_ms() / 1000.0
        + DECODER_CONTEXT_MARGIN_SECONDS)
        .max(MIN_DECODER_CONTEXT_SECONDS);
    (output_rate as f64 * decoder_context_seconds)
        .ceil()
        .max(1.0) as usize
}

pub fn unpack_native_msb_range(bytes: &[u8], range: Range<usize>) -> Result<Vec<f64>, String> {
    let total_bits = bytes.len().saturating_mul(8);
    if range.start > range.end || range.end > total_bits {
        return Err(format!(
            "invalid native-bit range {}..{} for {total_bits} bits",
            range.start, range.end
        ));
    }
    Ok(range
        .map(|bit| {
            let byte = bytes[bit / 8];
            if (byte >> (7 - bit % 8)) & 1 == 1 {
                1.0
            } else {
                -1.0
            }
        })
        .collect())
}

pub fn density_metrics(
    bytes: &[u8],
    range: Range<usize>,
    rolling_window_bits: usize,
) -> Result<DensityMetrics, String> {
    let total_bits = bytes.len().saturating_mul(8);
    if range.start >= range.end || range.end > total_bits {
        return Err("invalid density range".to_string());
    }
    let len = range.end - range.start;
    let ones = range.clone().filter(|bit| native_bit(bytes, *bit)).count();
    let density = ones as f64 / len as f64;
    let rolling_window_bits = rolling_window_bits.max(1).min(len);
    let mut rolling_max_deviation = 0.0f64;
    let mut window_ones = (range.start..range.start + rolling_window_bits)
        .filter(|bit| native_bit(bytes, *bit))
        .count();
    for start in range.start..=range.end - rolling_window_bits {
        let window_density = window_ones as f64 / rolling_window_bits as f64;
        rolling_max_deviation = rolling_max_deviation.max((window_density - 0.5).abs());
        if start < range.end - rolling_window_bits {
            if native_bit(bytes, start) {
                window_ones -= 1;
            }
            if native_bit(bytes, start + rolling_window_bits) {
                window_ones += 1;
            }
        }
    }
    Ok(DensityMetrics {
        density,
        deviation: (density - 0.5).abs(),
        rolling_max_deviation,
        rolling_window_bits,
    })
}

/// Measure density with a rolling window expressed in physical time. Every
/// possible bit-aligned window is inspected. This avoids the rate-dependent
/// 23.2/11.6/5.8 ms meaning of a fixed 65,536-bit window at DSD64/128/256.
pub fn density_metrics_for_duration(
    bytes: &[u8],
    range: Range<usize>,
    wire_rate: u32,
    rolling_window_seconds: f64,
) -> Result<DensityMetrics, String> {
    if wire_rate == 0 || !rolling_window_seconds.is_finite() || rolling_window_seconds <= 0.0 {
        return Err("density duration and wire rate must be positive and finite".to_string());
    }
    let rolling_window_bits = (wire_rate as f64 * rolling_window_seconds).round().max(1.0) as usize;
    density_metrics(bytes, range, rolling_window_bits)
}

pub fn analyze_single_tone(
    samples: &[f64],
    sample_rate: u32,
    name: &str,
    frequency_hz: f64,
    expected_amplitude: f64,
    harmonic_count: usize,
) -> Result<ToneMetrics, String> {
    validate_analysis_input(samples, sample_rate)?;
    let spectrum = Spectrum::new(samples, sample_rate, WindowKind::Rectangular)?;
    let fundamental = spectrum.tone_range(frequency_hz, 0);
    let signal_power = spectrum.power_in_ranges(std::slice::from_ref(&fundamental));
    if signal_power <= f64::MIN_POSITIVE {
        return Err("single-tone fundamental has no measurable power".to_string());
    }
    let mut harmonics = Vec::new();
    let mut harmonic_frequencies = Vec::new();
    for multiple in 2..=harmonic_count.max(1) {
        let frequency = frequency_hz * multiple as f64;
        if frequency <= 20_000.0 && frequency < sample_rate as f64 * 0.5 {
            harmonics.push(spectrum.tone_range(frequency, 0));
            harmonic_frequencies.push(frequency);
        }
    }
    let mut signal_only = vec![fundamental.clone()];
    let sinad_residual = spectrum.band_power(20.0, 20_000.0, &signal_only);
    let harmonic_power = spectrum.power_in_ranges(&harmonics);
    signal_only.extend(harmonics.iter().cloned());
    let noise_power = spectrum.band_power(20.0, 20_000.0, &signal_only);
    let amplitude = (2.0 * signal_power).sqrt();
    let spur_kind = WindowKind::BlackmanHarris4;
    let mut declared_frequencies = vec![frequency_hz];
    declared_frequencies.extend(harmonic_frequencies);
    let spur_residual = remove_tones_and_mean(samples, sample_rate, &declared_frequencies)?;
    let spur_spectrum = Spectrum::new(&spur_residual, sample_rate, spur_kind)?;
    Ok(ToneMetrics {
        carrier: carrier_metric(name, frequency_hz, expected_amplitude, amplitude)?,
        sinad_db: ratio_db(signal_power, sinad_residual),
        thd_db: ratio_db(harmonic_power, signal_power),
        residual_noise_dbfs: full_scale_sine_power_dbfs(noise_power),
        worst_nonharmonic_spur: spur_spectrum.worst_spur(
            20.0,
            20_000.0,
            &[],
            spur_kind.half_width_bins(),
        )?,
        reconstructed_dc: mean(samples),
    })
}

pub fn analyze_noise(
    samples: &[f64],
    sample_rate: u32,
    low_hz: f64,
    high_hz: f64,
    excluded_frequencies: &[f64],
) -> Result<NoiseMetrics, String> {
    validate_analysis_input(samples, sample_rate)?;
    let kind = WindowKind::BlackmanHarris4;
    let residual = remove_tones_and_mean(samples, sample_rate, excluded_frequencies)?;
    let spectrum = Spectrum::new(&residual, sample_rate, kind)?;
    Ok(NoiseMetrics {
        integrated_noise_dbfs: full_scale_sine_power_dbfs(spectrum.band_power(
            low_hz,
            high_hz,
            &[],
        )),
        worst_spur: spectrum.worst_spur(low_hz, high_hz, &[], kind.half_width_bins())?,
    })
}

pub fn measure_windowed_carrier(
    samples: &[f64],
    sample_rate: u32,
    name: &str,
    frequency_hz: f64,
    expected_amplitude: f64,
) -> Result<CarrierMetric, String> {
    validate_analysis_input(samples, sample_rate)?;
    let kind = WindowKind::BlackmanHarris4;
    let spectrum = Spectrum::new(samples, sample_rate, kind)?;
    let range = spectrum.tone_range(frequency_hz, kind.half_width_bins());
    let amplitude = (2.0 * spectrum.power_in_ranges(&[range])).sqrt();
    carrier_metric(name, frequency_hz, expected_amplitude, amplitude)
}

pub fn analyze_declared_tones(
    samples: &[f64],
    sample_rate: u32,
    carriers: &[(&str, f64, f64)],
    products: &[(&str, f64)],
    low_hz: f64,
    high_hz: f64,
) -> Result<DeclaredToneMetrics, String> {
    validate_analysis_input(samples, sample_rate)?;
    let spectrum = Spectrum::new(samples, sample_rate, WindowKind::Rectangular)?;
    let carrier_ranges = carriers
        .iter()
        .map(|(_, frequency, _)| spectrum.tone_range(*frequency, 0))
        .collect::<Vec<_>>();
    let product_ranges = products
        .iter()
        .map(|(_, frequency)| spectrum.tone_range(*frequency, 0))
        .collect::<Vec<_>>();
    let mut carrier_results = Vec::with_capacity(carriers.len());
    for ((name, frequency, expected), range) in carriers.iter().zip(carrier_ranges.iter()) {
        let amplitude = (2.0 * spectrum.power_in_ranges(std::slice::from_ref(range))).sqrt();
        carrier_results.push(carrier_metric(name, *frequency, *expected, amplitude)?);
    }
    let mut product_results = Vec::with_capacity(products.len());
    for ((name, frequency), range) in products.iter().zip(product_ranges.iter()) {
        let amplitude = (2.0 * spectrum.power_in_ranges(std::slice::from_ref(range))).sqrt();
        product_results.push(MeasuredTone {
            name: (*name).to_string(),
            frequency_hz: *frequency,
            level_dbfs: peak_sine_dbfs(amplitude),
        });
    }
    let worst_declared_product = product_results
        .iter()
        .cloned()
        .max_by(|left, right| left.level_dbfs.total_cmp(&right.level_dbfs));
    let signal_power = spectrum.power_in_ranges(&carrier_ranges);
    let sinad_residual_power = spectrum.band_power(low_hz, high_hz, &carrier_ranges);
    let mut exclusions = carrier_ranges;
    exclusions.extend(product_ranges);
    let residual_power = spectrum.band_power(low_hz, high_hz, &exclusions);
    let spur_kind = WindowKind::BlackmanHarris4;
    let mut declared_frequencies = carriers
        .iter()
        .map(|(_, frequency, _)| *frequency)
        .collect::<Vec<_>>();
    declared_frequencies.extend(products.iter().map(|(_, frequency)| *frequency));
    let spur_residual = remove_tones_and_mean(samples, sample_rate, &declared_frequencies)?;
    let spur_spectrum = Spectrum::new(&spur_residual, sample_rate, spur_kind)?;
    Ok(DeclaredToneMetrics {
        carriers: carrier_results,
        declared_products: product_results,
        sinad_db: ratio_db(signal_power, sinad_residual_power),
        residual_excluding_declared_products_dbfs: full_scale_sine_power_dbfs(residual_power),
        worst_declared_product,
        worst_unexpected_spur: spur_spectrum.worst_spur(
            low_hz,
            high_hz,
            &[],
            spur_kind.half_width_bins(),
        )?,
    })
}

pub fn analyze_multiband(
    samples: &[f64],
    sample_rate: u32,
    carriers: &[(&str, f64, f64)],
    bands: &[(f64, f64)],
) -> Result<MultiBandMetrics, String> {
    validate_analysis_input(samples, sample_rate)?;
    let spectrum = Spectrum::new(samples, sample_rate, WindowKind::Rectangular)?;
    let carrier_ranges = carriers
        .iter()
        .map(|(_, frequency, _)| spectrum.tone_range(*frequency, 0))
        .collect::<Vec<_>>();
    let mut carrier_results = Vec::with_capacity(carriers.len());
    for ((name, frequency, expected), range) in carriers.iter().zip(carrier_ranges.iter()) {
        let amplitude = (2.0 * spectrum.power_in_ranges(std::slice::from_ref(range))).sqrt();
        carrier_results.push(carrier_metric(name, *frequency, *expected, amplitude)?);
    }
    let spur_kind = WindowKind::BlackmanHarris4;
    let declared_frequencies = carriers
        .iter()
        .map(|(_, frequency, _)| *frequency)
        .collect::<Vec<_>>();
    let spur_residual = remove_tones_and_mean(samples, sample_rate, &declared_frequencies)?;
    let spur_spectrum = Spectrum::new(&spur_residual, sample_rate, spur_kind)?;
    let mut band_results = Vec::with_capacity(bands.len());
    for (low_hz, high_hz) in bands {
        let residual_power = spectrum.band_power(*low_hz, *high_hz, &carrier_ranges);
        band_results.push(BandResidualMetrics {
            low_hz: *low_hz,
            high_hz: *high_hz,
            residual_dbfs: full_scale_sine_power_dbfs(residual_power),
            worst_unexpected_spur: spur_spectrum.worst_spur(
                *low_hz,
                *high_hz,
                &[],
                spur_kind.half_width_bins(),
            )?,
        });
    }
    Ok(MultiBandMetrics {
        carriers: carrier_results,
        bands: band_results,
    })
}

/// Estimate when non-carrier residual returns to its pre-transition range.
///
/// The pre- and post-transition carrier models are intentionally fitted
/// independently, so this measures residual settling rather than recovery of
/// carrier gain or phase. Callers must assess carrier gain/phase separately.
#[cfg(test)]
pub fn estimate_recovery_time_ms(
    samples: &[f64],
    sample_rate: u32,
    reference_steady: Range<usize>,
    restart_index: usize,
    recovery_fit_start: usize,
    carrier_frequencies: &[f64],
) -> Result<Option<f64>, String> {
    validate_analysis_input(samples, sample_rate)?;
    if reference_steady.is_empty()
        || reference_steady.end > restart_index
        || restart_index >= recovery_fit_start
        || recovery_fit_start >= samples.len()
    {
        return Err("invalid recovery analysis boundaries".to_string());
    }
    estimate_recovery_time_ms_from_separate_windows(
        &samples[reference_steady],
        &samples[restart_index..],
        sample_rate,
        recovery_fit_start - restart_index,
        carrier_frequencies,
    )
}

/// Estimate residual recovery when steady reference and restart capture were
/// reconstructed as independent windows. This lets callers avoid decoding a
/// potentially multi-second guard interval solely to keep both in one vector.
pub fn estimate_recovery_time_ms_from_separate_windows(
    reference_steady: &[f64],
    recovery_from_restart: &[f64],
    sample_rate: u32,
    recovery_fit_start: usize,
    carrier_frequencies: &[f64],
) -> Result<Option<f64>, String> {
    validate_analysis_input(reference_steady, sample_rate)?;
    validate_analysis_input(recovery_from_restart, sample_rate)?;
    if recovery_fit_start == 0 || recovery_fit_start >= recovery_from_restart.len() {
        return Err("invalid separate-window recovery boundaries".to_string());
    }
    let reference_model = fit_tone_model(reference_steady, sample_rate, carrier_frequencies)?;
    let reference_residual = residual_against_tone_model(reference_steady, &reference_model, 0)?;
    let recovery_model = fit_tone_model(
        &recovery_from_restart[recovery_fit_start..],
        sample_rate,
        carrier_frequencies,
    )?;
    let recovery_residual = residual_against_tone_model(
        recovery_from_restart,
        &recovery_model,
        -(recovery_fit_start as isize),
    )?;
    let window = ((sample_rate as f64 * 0.002).round() as usize).max(16);
    let required = ((sample_rate as f64 * 0.010).round() as usize).max(window);
    if reference_residual.len() < window || recovery_residual.len() < required {
        return Err("recovery interval is too short".to_string());
    }
    let reference_prefix = squared_prefix(&reference_residual);
    let mut reference_windows = (0..=reference_residual.len() - window)
        .map(|start| window_rms(&reference_prefix, start, window))
        .collect::<Vec<_>>();
    reference_windows.sort_by(f64::total_cmp);
    let reference_ceiling = percentile(&reference_windows, 0.995).max(1.0e-15);
    let threshold = reference_ceiling * 1.05;
    let recovery_prefix = squared_prefix(&recovery_residual);
    let mut good_run = 0usize;
    for start in 0..=recovery_residual.len() - window {
        if window_rms(&recovery_prefix, start, window) <= threshold {
            good_run += 1;
            if good_run >= required.saturating_sub(window).saturating_add(1) {
                let first = start + 1 - good_run;
                return Ok(Some(first as f64 * 1000.0 / sample_rate as f64));
            }
        } else {
            good_run = 0;
        }
    }
    Ok(None)
}

fn squared_prefix(samples: &[f64]) -> Vec<f64> {
    let mut prefix = Vec::with_capacity(samples.len() + 1);
    prefix.push(0.0);
    for sample in samples {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + sample * sample);
    }
    prefix
}

fn window_rms(prefix: &[f64], start: usize, window: usize) -> f64 {
    ((prefix[start + window] - prefix[start]) / window as f64)
        .max(0.0)
        .sqrt()
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (quantile.clamp(0.0, 1.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[index]
}

pub fn max_abs(samples: &[f64]) -> f64 {
    samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0, f64::max)
}

pub fn mean(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<f64>() / samples.len() as f64
}

#[cfg(test)]
fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt()
}

/// Peak-sine dBFS: a sine whose peak amplitude is 1.0 reads 0 dBFS.
pub fn peak_sine_dbfs(amplitude: f64) -> f64 {
    20.0 * amplitude.abs().max(1.0e-18).log10()
}

/// RMS dBFS on the same full-scale-sine reference. An RMS value of
/// `1/sqrt(2)` therefore reads 0 dBFS.
pub fn rms_dbfs_full_scale_sine(rms: f64) -> f64 {
    (10.0 * (2.0 * rms.abs().powi(2)).max(1.0e-36).log10()).max(MIN_DB)
}

/// Compatibility alias for existing callers that pass peak tone amplitude.
#[cfg(test)]
pub fn amplitude_dbfs(amplitude: f64) -> f64 {
    peak_sine_dbfs(amplitude)
}

pub fn validate_finite(samples: &[f64], label: &str) -> Result<(), String> {
    if let Some((index, _)) = samples
        .iter()
        .enumerate()
        .find(|(_, sample)| !sample.is_finite())
    {
        Err(format!(
            "{label} contains a non-finite sample at index {index}"
        ))
    } else {
        Ok(())
    }
}

pub fn apply_reconstruction_profile(
    samples: &[f64],
    profile: ReconstructionProfile,
) -> Result<Vec<f64>, String> {
    validate_analysis_input(samples, profile.output_rate)?;
    if !(0.0 < profile.pass_hz
        && profile.pass_hz < profile.stop_hz
        && profile.stop_hz < profile.output_rate as f64 * 0.5)
    {
        return Err(format!("invalid reconstruction profile {}", profile.id));
    }
    let pad_len = ((profile.output_rate as f64 * PROFILE_CONTEXT_SECONDS).round() as usize)
        .max(1)
        .min(samples.len().saturating_sub(2));
    let mut padded = Vec::with_capacity(samples.len() + 2 * pad_len);
    for index in (1..=pad_len).rev() {
        padded.push(samples[index]);
    }
    padded.extend_from_slice(samples);
    for index in 0..pad_len {
        padded.push(samples[samples.len() - 2 - index]);
    }
    let fft_len = padded.len().next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut time = forward.make_input_vec();
    time[..padded.len()].copy_from_slice(&padded);
    let mut frequency = forward.make_output_vec();
    forward
        .process(&mut time, &mut frequency)
        .map_err(|err| format!("reconstruction forward FFT failed: {err}"))?;
    let bin_hz = profile.output_rate as f64 / fft_len as f64;
    for (bin, value) in frequency.iter_mut().enumerate() {
        let gain = reconstruction_gain(bin as f64 * bin_hz, profile);
        value.re *= gain;
        value.im *= gain;
    }
    let mut filtered = inverse.make_output_vec();
    inverse
        .process(&mut frequency, &mut filtered)
        .map_err(|err| format!("reconstruction inverse FFT failed: {err}"))?;
    let scale = fft_len as f64;
    Ok(filtered[pad_len..pad_len + samples.len()]
        .iter()
        .map(|sample| sample / scale)
        .collect())
}

fn reconstruction_gain(frequency_hz: f64, profile: ReconstructionProfile) -> f64 {
    if frequency_hz <= profile.pass_hz {
        1.0
    } else if frequency_hz >= profile.stop_hz {
        0.0
    } else {
        let position = (frequency_hz - profile.pass_hz) / (profile.stop_hz - profile.pass_hz);
        0.5 + 0.5 * (PI * position).cos()
    }
}

#[derive(Debug)]
struct Spectrum {
    powers: Vec<f64>,
    sample_rate: u32,
    fft_len: usize,
}

impl Spectrum {
    fn new(samples: &[f64], sample_rate: u32, kind: WindowKind) -> Result<Self, String> {
        if samples.len() < 128 {
            return Err("spectral analysis requires at least 128 samples".to_string());
        }
        let fft_len = samples.len();
        let mut window_sum_squares = 0.0;
        let mut time = Vec::with_capacity(fft_len);
        for (index, sample) in samples.iter().enumerate() {
            let window = match kind {
                WindowKind::Rectangular => 1.0,
                WindowKind::BlackmanHarris4 => blackman_harris4(index, fft_len),
            };
            window_sum_squares += window * window;
            time.push(sample * window);
        }
        let mut planner = RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(fft_len);
        let mut frequency = fft.make_output_vec();
        fft.process(&mut time, &mut frequency)
            .map_err(|err| format!("analysis FFT failed: {err}"))?;
        let powers = frequency
            .iter()
            .enumerate()
            .map(|(bin, value)| {
                let one_sided = if bin == 0 || (fft_len.is_multiple_of(2) && bin == fft_len / 2) {
                    1.0
                } else {
                    2.0
                };
                one_sided * value.norm_sqr()
                    / (fft_len as f64 * window_sum_squares.max(f64::MIN_POSITIVE))
            })
            .collect();
        Ok(Self {
            powers,
            sample_rate,
            fft_len,
        })
    }

    fn bin_hz(&self) -> f64 {
        self.sample_rate as f64 / self.fft_len as f64
    }

    fn tone_range(&self, frequency_hz: f64, half_width: usize) -> Range<usize> {
        let bin = (frequency_hz / self.bin_hz()).round() as usize;
        let start = bin.saturating_sub(half_width).min(self.powers.len());
        let end = (bin + half_width + 1).min(self.powers.len());
        start..end
    }

    fn power_in_ranges(&self, ranges: &[Range<usize>]) -> f64 {
        ranges
            .iter()
            .flat_map(|range| range.clone())
            .filter_map(|bin| self.powers.get(bin))
            .copied()
            .sum()
    }

    fn band_power(&self, low_hz: f64, high_hz: f64, excluded: &[Range<usize>]) -> f64 {
        let (low, high) = self.band_bins(low_hz, high_hz);
        (low..high)
            .filter(|bin| !is_excluded(*bin, excluded))
            .map(|bin| self.powers[bin])
            .sum()
    }

    fn worst_spur(
        &self,
        low_hz: f64,
        high_hz: f64,
        excluded: &[Range<usize>],
        half_width: usize,
    ) -> Result<SpurMetric, String> {
        let (low, high) = self.band_bins(low_hz, high_hz);
        let mut best: Option<(usize, f64)> = None;
        for bin in low..high {
            if is_excluded(bin, excluded) {
                continue;
            }
            let before = bin.checked_sub(1).map_or(0.0, |index| self.powers[index]);
            let after = self.powers.get(bin + 1).copied().unwrap_or(0.0);
            if self.powers[bin] < before || self.powers[bin] < after {
                continue;
            }
            let start = bin.saturating_sub(half_width).max(low);
            let end = (bin + half_width + 1).min(high);
            let power = (start..end)
                .filter(|candidate| !is_excluded(*candidate, excluded))
                .map(|candidate| self.powers[candidate])
                .sum::<f64>();
            if best.is_none_or(|(_, best_power)| power > best_power) {
                best = Some((bin, power));
            }
        }
        let (bin, power) = best.ok_or_else(|| "no unmasked spur bin in band".to_string())?;
        Ok(SpurMetric {
            frequency_hz: bin as f64 * self.bin_hz(),
            level_dbfs: peak_sine_dbfs((2.0 * power).sqrt()),
        })
    }

    fn band_bins(&self, low_hz: f64, high_hz: f64) -> (usize, usize) {
        let low = (low_hz.max(0.0) / self.bin_hz()).ceil() as usize;
        let high = ((high_hz.min(self.sample_rate as f64 * 0.5) / self.bin_hz()).floor() as usize
            + 1)
        .min(self.powers.len());
        (low.min(high), high)
    }
}

fn carrier_metric(
    name: &str,
    frequency_hz: f64,
    expected_amplitude: f64,
    measured_amplitude: f64,
) -> Result<CarrierMetric, String> {
    if !expected_amplitude.is_finite() || expected_amplitude <= 0.0 {
        return Err(format!("invalid expected amplitude for {name}"));
    }
    if !measured_amplitude.is_finite() || measured_amplitude < 0.0 {
        return Err(format!("invalid measured amplitude for {name}"));
    }
    let expected_level_dbfs = peak_sine_dbfs(expected_amplitude);
    let measured_level_dbfs = peak_sine_dbfs(measured_amplitude);
    Ok(CarrierMetric {
        name: name.to_string(),
        frequency_hz,
        expected_level_dbfs,
        measured_level_dbfs,
        gain_error_db: measured_level_dbfs - expected_level_dbfs,
    })
}

fn fit_tones(
    samples: &[f64],
    sample_rate: u32,
    frequencies: &[f64],
) -> Result<Vec<(f64, f64)>, String> {
    if samples.is_empty() || frequencies.is_empty() {
        return Err("tone fit requires samples and frequencies".to_string());
    }
    if frequencies.iter().any(|frequency| {
        !frequency.is_finite() || *frequency <= 0.0 || *frequency >= sample_rate as f64 * 0.5
    }) {
        return Err("tone fit contains an invalid frequency".to_string());
    }
    let width = frequencies.len() * 2;
    let mut gram = vec![vec![0.0; width]; width];
    let mut rhs = vec![0.0; width];
    let mut basis = vec![0.0; width];
    for (index, sample) in samples.iter().enumerate() {
        for (tone, frequency) in frequencies.iter().enumerate() {
            let phase = 2.0 * PI * frequency * index as f64 / sample_rate as f64;
            basis[2 * tone] = phase.sin();
            basis[2 * tone + 1] = phase.cos();
        }
        for row in 0..width {
            rhs[row] += sample * basis[row];
            for column in 0..=row {
                gram[row][column] += basis[row] * basis[column];
            }
        }
    }
    for row in 0..width {
        let (through_row, following_rows) = gram.split_at_mut(row + 1);
        let current_row = &mut through_row[row];
        for (column, following_row) in (row + 1..width).zip(following_rows.iter()) {
            current_row[column] = following_row[row];
        }
    }
    let coefficients = solve_linear_system(gram, rhs)
        .ok_or_else(|| "tone-fit normal matrix is singular".to_string())?;
    Ok(coefficients
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect())
}

/// Fit a constant and every declared sinusoid in one least-squares system.
/// A sequential tone fit followed by de-meaning is not equivalent when a
/// finite-window tone is off-bin, and can leave a phantom declared-frequency
/// line in the residual.
fn fit_tones_and_dc(
    samples: &[f64],
    sample_rate: u32,
    frequencies: &[f64],
) -> Result<(f64, Vec<(f64, f64)>), String> {
    if samples.is_empty() || frequencies.is_empty() {
        return Err("tone and DC fit requires samples and frequencies".to_string());
    }
    if frequencies.iter().any(|frequency| {
        !frequency.is_finite() || *frequency <= 0.0 || *frequency >= sample_rate as f64 * 0.5
    }) {
        return Err("tone and DC fit contains an invalid frequency".to_string());
    }
    let width = 1 + frequencies.len() * 2;
    let mut gram = vec![vec![0.0; width]; width];
    let mut rhs = vec![0.0; width];
    let mut basis = vec![0.0; width];
    basis[0] = 1.0;
    for (index, sample) in samples.iter().enumerate() {
        for (tone, frequency) in frequencies.iter().enumerate() {
            let phase = 2.0 * PI * frequency * index as f64 / sample_rate as f64;
            basis[1 + 2 * tone] = phase.sin();
            basis[2 + 2 * tone] = phase.cos();
        }
        for row in 0..width {
            rhs[row] += sample * basis[row];
            for column in 0..=row {
                gram[row][column] += basis[row] * basis[column];
            }
        }
    }
    for row in 0..width {
        let (through_row, following_rows) = gram.split_at_mut(row + 1);
        let current_row = &mut through_row[row];
        for (column, following_row) in (row + 1..width).zip(following_rows.iter()) {
            current_row[column] = following_row[row];
        }
    }
    let solution = solve_linear_system(gram, rhs)
        .ok_or_else(|| "tone and DC fit normal matrix is singular".to_string())?;
    Ok((
        solution[0],
        solution[1..]
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .collect(),
    ))
}

fn solve_linear_system(mut matrix: Vec<Vec<f64>>, mut rhs: Vec<f64>) -> Option<Vec<f64>> {
    let size = rhs.len();
    for pivot in 0..size {
        let best = (pivot..size).max_by(|left, right| {
            matrix[*left][pivot]
                .abs()
                .total_cmp(&matrix[*right][pivot].abs())
        })?;
        if matrix[best][pivot].abs() <= f64::EPSILON {
            return None;
        }
        matrix.swap(pivot, best);
        rhs.swap(pivot, best);
        let divisor = matrix[pivot][pivot];
        for value in &mut matrix[pivot][pivot..size] {
            *value /= divisor;
        }
        rhs[pivot] /= divisor;
        let pivot_values = matrix[pivot].clone();
        for (row, row_values) in matrix.iter_mut().enumerate() {
            if row == pivot {
                continue;
            }
            let factor = row_values[pivot];
            for (value, pivot_value) in row_values[pivot..size]
                .iter_mut()
                .zip(&pivot_values[pivot..size])
            {
                *value -= factor * pivot_value;
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }
    Some(rhs)
}

fn predict_tones(
    index_from_fit_start: isize,
    sample_rate: u32,
    frequencies: &[f64],
    coefficients: &[(f64, f64)],
) -> f64 {
    frequencies
        .iter()
        .zip(coefficients.iter())
        .map(|(frequency, (sin_coefficient, cos_coefficient))| {
            let phase = 2.0 * PI * frequency * index_from_fit_start as f64 / sample_rate as f64;
            sin_coefficient * phase.sin() + cos_coefficient * phase.cos()
        })
        .sum()
}

fn blackman_harris4(index: usize, len: usize) -> f64 {
    if len <= 1 {
        return 1.0;
    }
    let phase = 2.0 * PI * index as f64 / (len - 1) as f64;
    0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos() - 0.01168 * (3.0 * phase).cos()
}

fn native_bit(bytes: &[u8], bit: usize) -> bool {
    (bytes[bit / 8] >> (7 - bit % 8)) & 1 == 1
}

fn is_excluded(bin: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|range| range.contains(&bin))
}

fn full_scale_sine_power_dbfs(power: f64) -> f64 {
    (10.0 * (2.0 * power.max(0.0)).max(1.0e-36).log10()).max(MIN_DB)
}

fn ratio_db(numerator: f64, denominator: f64) -> f64 {
    if numerator <= f64::MIN_POSITIVE {
        MIN_DB
    } else if denominator <= f64::MIN_POSITIVE {
        -MIN_DB
    } else {
        10.0 * (numerator.log10() - denominator.log10())
    }
}

fn validate_analysis_input(samples: &[f64], sample_rate: u32) -> Result<(), String> {
    if sample_rate == 0 {
        return Err("analysis sample rate must be nonzero".to_string());
    }
    if samples.len() < 3 {
        return Err("analysis requires at least three samples".to_string());
    }
    validate_finite(samples, "analysis input")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(len: usize, sample_rate: u32, frequency: f64, amplitude: f64) -> Vec<f64> {
        (0..len)
            .map(|index| {
                amplitude * (2.0 * PI * frequency * index as f64 / sample_rate as f64).sin()
            })
            .collect()
    }

    fn first_order_pdm(
        output_frames: usize,
        ratio: usize,
        wire_rate: u32,
        frequency_hz: f64,
        amplitude: f64,
    ) -> Vec<u8> {
        let bits = output_frames * ratio;
        assert!(bits.is_multiple_of(8));
        let mut bytes = vec![0u8; bits / 8];
        let mut integrator = 0.0;
        for bit in 0..bits {
            let target =
                amplitude * (2.0 * PI * frequency_hz * bit as f64 / wire_rate as f64).sin();
            integrator += target;
            let one = integrator >= 0.0;
            integrator -= if one { 1.0 } else { -1.0 };
            if one {
                bytes[bit / 8] |= 1 << (7 - bit % 8);
            }
        }
        bytes
    }

    #[test]
    fn native_unpack_is_msb_first() {
        let unpacked = unpack_native_msb_range(&[0b1010_0110], 0..8).unwrap();
        assert_eq!(unpacked, vec![1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0]);
    }

    #[test]
    fn density_reports_whole_and_rolling_bias() {
        let metrics = density_metrics(&[0b1111_0000, 0b1010_1010], 0..16, 8).unwrap();
        assert!((metrics.density - 0.5).abs() < 1.0e-12);
        assert!((metrics.rolling_max_deviation - 0.25).abs() < 1.0e-12);
    }

    #[test]
    fn fixed_time_density_uses_wire_rate_and_full_sliding_windows() {
        let bytes = [0b1111_0000, 0b1010_1010];
        let metrics = density_metrics_for_duration(&bytes, 0..16, 8, 1.0).unwrap();
        assert_eq!(metrics.rolling_window_bits, 8);
        assert!((metrics.rolling_max_deviation - 0.25).abs() < 1.0e-12);
    }

    #[test]
    fn decoder_context_covers_audio_and_hires_runtime_latency() {
        for wire_rate in [2_822_400, 5_644_800, 11_289_600] {
            for profile in [AUDIO_BAND, HIRES_BAND] {
                let decoder =
                    SincResampler::new(FilterType::SincExtreme32k, wire_rate, profile.output_rate);
                let context = decoder_context_output_frames(&decoder, profile.output_rate);
                let context_ms = context as f64 * 1000.0 / profile.output_rate as f64;
                assert!(
                    context_ms >= decoder.latency_ms() + DECODER_CONTEXT_MARGIN_SECONDS * 1000.0,
                    "wire={wire_rate} {profile:?}: context={context_ms} latency={}",
                    decoder.latency_ms()
                );
                assert!(context_ms >= MIN_DECODER_CONTEXT_SECONDS * 1000.0);
            }
        }
    }

    #[test]
    fn cropped_reconstruction_matches_whole_stream_interior() {
        let wire_rate = 2_822_400;
        let ratio = (wire_rate / AUDIO_BAND.output_rate) as usize;
        let output_frames = 100_000;
        let bytes = first_order_pdm(output_frames, ratio, wire_rate, 1_000.0, 0.2);
        let (whole, _) = reconstruct_stereo_window(
            &bytes,
            &bytes,
            wire_rate,
            0..output_frames * ratio,
            AUDIO_BAND,
            1.0,
        )
        .unwrap();
        let inner = 40_000..60_000;
        let (cropped, _) = reconstruct_stereo_window(
            &bytes,
            &bytes,
            wire_rate,
            inner.start * ratio..inner.end * ratio,
            AUDIO_BAND,
            1.0,
        )
        .unwrap();
        let error = whole[inner]
            .iter()
            .zip(&cropped)
            .map(|(whole, cropped)| (whole - cropped).abs())
            .fold(0.0, f64::max);
        assert!(error < 1.0e-8, "cropped/whole max error was {error:e}");
    }

    #[test]
    fn coherent_tone_normalization_recovers_level_and_thd() {
        let sample_rate = 176_400;
        let len = 65_536;
        let frequency = 372.0 * sample_rate as f64 / len as f64;
        let amplitude = 10.0f64.powf(-6.0 / 20.0);
        let samples = sine(len, sample_rate, frequency, amplitude);
        let metrics =
            analyze_single_tone(&samples, sample_rate, "tone", frequency, amplitude, 5).unwrap();
        assert!(metrics.carrier.gain_error_db.abs() < 1.0e-9);
        assert!(metrics.sinad_db > 250.0);
        assert!(metrics.thd_db < -250.0);
        assert!(metrics.residual_noise_dbfs < -250.0);
    }

    #[test]
    fn blackman_harris_tone_power_uses_window_energy() {
        let sample_rate = 176_400;
        let len = 65_536;
        let frequency = 37.0 * sample_rate as f64 / len as f64;
        let amplitude = 1.0e-6;
        let samples = sine(len, sample_rate, frequency, amplitude);
        let metric =
            measure_windowed_carrier(&samples, sample_rate, "tiny", frequency, amplitude).unwrap();
        assert!(metric.gain_error_db.abs() < 0.01, "{metric:?}");
    }

    #[test]
    fn blackman_harris_spur_recovers_a_half_bin_offset_tone() {
        let sample_rate = 176_400;
        let len = 65_536;
        let frequency = 137.5 * sample_rate as f64 / len as f64;
        let amplitude = 1.0e-3;
        let samples = sine(len, sample_rate, frequency, amplitude);
        let metrics = analyze_noise(&samples, sample_rate, 20.0, 20_000.0, &[]).unwrap();
        assert!(
            (metrics.worst_spur.level_dbfs - peak_sine_dbfs(amplitude)).abs() < 0.02,
            "{metrics:?}"
        );
        assert!(
            (metrics.worst_spur.frequency_hz - frequency).abs() <= sample_rate as f64 / len as f64
        );
    }

    #[test]
    fn joint_declared_tone_and_dc_subtraction_exposes_a_tiny_half_bin_spur() {
        let sample_rate = 176_400;
        let len = 65_536;
        let bin_hz = sample_rate as f64 / len as f64;
        let carrier_hz = 371.25 * bin_hz;
        let spur_hz = 743.5 * bin_hz;
        let spur_amplitude = 1.0e-7;
        let mut samples = sine(len, sample_rate, carrier_hz, 0.3);
        samples.iter_mut().for_each(|sample| *sample += 0.1);
        for (sample, spur) in
            samples
                .iter_mut()
                .zip(sine(len, sample_rate, spur_hz, spur_amplitude))
        {
            *sample += spur;
        }
        let metrics = analyze_declared_tones(
            &samples,
            sample_rate,
            &[("carrier", carrier_hz, 0.3)],
            &[],
            20.0,
            20_000.0,
        )
        .unwrap();
        assert!(
            (metrics.worst_unexpected_spur.level_dbfs - peak_sine_dbfs(spur_amplitude)).abs()
                < 0.05,
            "{metrics:?}"
        );
        assert!(
            (metrics.worst_unexpected_spur.frequency_hz - spur_hz).abs() <= bin_hz,
            "{metrics:?}"
        );
    }

    #[test]
    fn rms_and_peak_dbfs_share_the_full_scale_sine_reference() {
        assert!(peak_sine_dbfs(1.0).abs() < 1.0e-12);
        assert!(rms_dbfs_full_scale_sine(1.0 / 2.0f64.sqrt()).abs() < 1.0e-12);
        assert!(full_scale_sine_power_dbfs(0.5).abs() < 1.0e-12);
    }

    #[test]
    fn reconstruction_profile_preserves_passband_and_rejects_stopband() {
        let len = 65_536;
        let pass = sine(len, AUDIO_BAND.output_rate, 10_000.0, 0.25);
        let stop = sine(len, AUDIO_BAND.output_rate, 30_000.0, 0.25);
        let pass_filtered = apply_reconstruction_profile(&pass, AUDIO_BAND).unwrap();
        let stop_filtered = apply_reconstruction_profile(&stop, AUDIO_BAND).unwrap();
        let edge = 4096;
        let pass_gain = rms(&pass_filtered[edge..len - edge]) / rms(&pass[edge..len - edge]);
        let stop_gain = rms(&stop_filtered[edge..len - edge]) / rms(&stop[edge..len - edge]);
        assert!(amplitude_dbfs(pass_gain).abs() < 0.001);
        assert!(amplitude_dbfs(stop_gain) < -130.0);
        assert_eq!(pass_filtered.len(), len);
    }

    #[test]
    fn multiband_excludes_declared_carriers() {
        let sample_rate = 352_800;
        let len = 65_536;
        let f1 = 186.0 * sample_rate as f64 / len as f64;
        let f2 = 3344.0 * sample_rate as f64 / len as f64;
        let mut samples = sine(len, sample_rate, f1, 0.1);
        for (sample, other) in samples.iter_mut().zip(sine(len, sample_rate, f2, 0.05)) {
            *sample += other;
        }
        let metrics = analyze_multiband(
            &samples,
            sample_rate,
            &[("one", f1, 0.1), ("two", f2, 0.05)],
            &[(0.0, 20_000.0), (20_000.0, 80_000.0)],
        )
        .unwrap();
        assert!(
            metrics
                .carriers
                .iter()
                .all(|carrier| carrier.gain_error_db.abs() < 1.0e-8)
        );
        assert!(metrics.bands.iter().all(|band| band.residual_dbfs < -250.0));
    }

    #[test]
    fn declared_product_is_in_sinad_but_excluded_from_unexpected_residual() {
        let sample_rate = 176_400;
        let len = 65_536;
        let carrier_hz = 371.0 * sample_rate as f64 / len as f64;
        let product_hz = 743.0 * sample_rate as f64 / len as f64;
        let mut samples = sine(len, sample_rate, carrier_hz, 0.1);
        for (sample, product) in samples
            .iter_mut()
            .zip(sine(len, sample_rate, product_hz, 0.01))
        {
            *sample += product;
        }
        let metrics = analyze_declared_tones(
            &samples,
            sample_rate,
            &[("carrier", carrier_hz, 0.1)],
            &[("product", product_hz)],
            20.0,
            20_000.0,
        )
        .unwrap();
        assert!((metrics.sinad_db - 20.0).abs() < 1.0e-8, "{metrics:?}");
        assert!(metrics.residual_excluding_declared_products_dbfs < -250.0);
        let worst = metrics.worst_declared_product.unwrap();
        assert_eq!(worst.name, "product");
        assert!((worst.level_dbfs + 40.0).abs() < 1.0e-8);
    }

    #[test]
    fn recovery_uses_the_pre_transition_residual_as_its_reference() {
        let sample_rate = 10_000;
        let frequency = 100.0;
        let mut samples = sine(4_000, sample_rate, frequency, 0.5);
        for sample in &mut samples[1_500..1_600] {
            *sample += 0.01;
        }
        let recovery =
            estimate_recovery_time_ms(&samples, sample_rate, 0..1_000, 1_500, 3_000, &[frequency])
                .unwrap()
                .unwrap();
        assert!((9.0..=11.0).contains(&recovery), "{recovery}");
    }

    #[test]
    fn recovery_is_absent_when_the_reference_residual_never_returns() {
        let sample_rate = 10_000;
        let frequency = 100.0;
        let mut samples = sine(4_000, sample_rate, frequency, 0.5);
        for sample in &mut samples[1_500..] {
            *sample += 0.01;
        }
        let recovery =
            estimate_recovery_time_ms(&samples, sample_rate, 0..1_000, 1_500, 3_000, &[frequency])
                .unwrap();
        assert_eq!(recovery, None);
    }
}
