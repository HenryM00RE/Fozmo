use realfft::RealFftPlanner;
use serde::Serialize;

const DB_FLOOR: f64 = -300.0;

#[derive(Debug, Clone, Serialize)]
pub struct ImpulseTimingMetrics {
    pub peak_index: usize,
    pub peak_amplitude: f64,
    pub pre_peak_energy_db_total: f64,
    pub post_peak_energy_db_total: f64,
    pub maximum_pre_ringing_lobe_db_peak: f64,
    pub maximum_post_ringing_lobe_db_peak: f64,
    pub decay_time_to_minus_80_db_ms: Option<f64>,
    pub decay_time_to_minus_120_db_ms: Option<f64>,
    pub decay_minus_80_db_censored: bool,
    pub decay_minus_120_db_censored: bool,
    pub main_lobe_width_us: f64,
    pub step_response_overshoot_percent: f64,
    pub step_response_undershoot_percent: f64,
    pub energy_centroid_relative_to_peak_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupDelayPoint {
    pub frequency_hz: f64,
    pub magnitude_db: f64,
    pub absolute_group_delay_ms: f64,
    pub group_delay_relative_to_peak_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MagnitudeResponseMetrics {
    pub gain_5khz_db_dc: f64,
    pub gain_10khz_db_dc: f64,
    pub gain_15khz_db_dc: f64,
    pub gain_18khz_db_dc: f64,
    pub gain_20khz_db_dc: f64,
    pub gain_20_5khz_db_dc: f64,
    pub gain_21khz_db_dc: f64,
    pub gain_21_5khz_db_dc: f64,
    pub gain_22khz_db_dc: f64,
    pub gain_22_05khz_db_dc: f64,
    pub passband_ripple_20hz_18khz_db: f64,
    pub passband_ripple_20hz_20khz_db: f64,
    pub bandwidth_minus_0_1_db_hz: Option<f64>,
    pub bandwidth_minus_1_db_hz: Option<f64>,
    pub bandwidth_minus_3_db_hz: Option<f64>,
    pub bandwidth_minus_6_db_hz: Option<f64>,
    pub transition_minus_0_1_to_minus_100_db_hz: Option<f64>,
    pub stopband_rejection_24_1khz_to_nyquist_db: f64,
    pub first_image_rejection_24_1khz_to_64_1khz_db: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PacketTimingMetrics {
    pub frequency_hz: f64,
    pub cycles: f64,
    pub nominal_duration_ms: f64,
    pub alignment: &'static str,
    pub energy_centroid_index: f64,
    pub pre_echo_energy_db_total: f64,
    pub post_echo_energy_db_total: f64,
    pub maximum_pre_echo_db_peak: f64,
    pub maximum_post_echo_db_peak: f64,
    pub onset_reference: &'static str,
    pub nominal_onset_index: f64,
    pub onset_pre_echo_energy_db_total: f64,
    pub onset_post_decay_energy_db_total: f64,
    pub maximum_onset_pre_echo_db_peak: f64,
    pub maximum_onset_post_decay_db_peak: f64,
}

#[derive(Debug, Clone, Copy)]
struct MainLobe {
    left: f64,
    right: f64,
}

pub fn normalize_interpolator_dc(response: &mut [f64], rate_ratio: f64) -> f64 {
    let sum = response.iter().sum::<f64>();
    if sum.abs() <= f64::EPSILON {
        return 1.0;
    }
    let scale = rate_ratio / sum;
    for sample in response {
        *sample *= scale;
    }
    scale
}

pub fn analyze_impulse(response: &[f64], sample_rate: f64) -> ImpulseTimingMetrics {
    assert!(!response.is_empty());
    assert!(sample_rate > 0.0);

    let peak_index = response
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .map(|(index, _)| index)
        .unwrap_or(0);
    let peak_amplitude = response[peak_index].abs();
    let total_energy = response.iter().map(|value| value * value).sum::<f64>();
    let pre_energy = response[..peak_index]
        .iter()
        .map(|value| value * value)
        .sum::<f64>();
    let post_energy = response[peak_index.saturating_add(1)..]
        .iter()
        .map(|value| value * value)
        .sum::<f64>();
    let lobe = main_lobe(response, peak_index);
    let pre_end = lobe.left.ceil().max(0.0) as usize;
    let post_start = (lobe.right.floor() as usize)
        .saturating_add(1)
        .min(response.len());
    let pre_lobe = response[..pre_end]
        .iter()
        .map(|value| value.abs())
        .fold(0.0, f64::max);
    let post_lobe = response[post_start..]
        .iter()
        .map(|value| value.abs())
        .fold(0.0, f64::max);
    let centroid = if total_energy > 0.0 {
        response
            .iter()
            .enumerate()
            .map(|(index, value)| index as f64 * value * value)
            .sum::<f64>()
            / total_energy
    } else {
        peak_index as f64
    };
    let (decay80, censored80) =
        decay_time(response, peak_index, peak_amplitude, -80.0, sample_rate);
    let (decay120, censored120) =
        decay_time(response, peak_index, peak_amplitude, -120.0, sample_rate);

    ImpulseTimingMetrics {
        peak_index,
        peak_amplitude,
        pre_peak_energy_db_total: power_ratio_db(pre_energy, total_energy),
        post_peak_energy_db_total: power_ratio_db(post_energy, total_energy),
        maximum_pre_ringing_lobe_db_peak: amplitude_ratio_db(pre_lobe, peak_amplitude),
        maximum_post_ringing_lobe_db_peak: amplitude_ratio_db(post_lobe, peak_amplitude),
        decay_time_to_minus_80_db_ms: decay80,
        decay_time_to_minus_120_db_ms: decay120,
        decay_minus_80_db_censored: censored80,
        decay_minus_120_db_censored: censored120,
        main_lobe_width_us: (lobe.right - lobe.left) / sample_rate * 1_000_000.0,
        step_response_overshoot_percent: 0.0,
        step_response_undershoot_percent: 0.0,
        energy_centroid_relative_to_peak_ms: (centroid - peak_index as f64) / sample_rate * 1000.0,
    }
}

pub fn group_delay_curve(
    response: &[f64],
    sample_rate: f64,
    peak_index: usize,
    frequencies_hz: &[f64],
) -> Vec<GroupDelayPoint> {
    if response.is_empty() {
        return Vec::new();
    }
    let fft_len = response.len().next_power_of_two().max(1024);
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut input = fft.make_input_vec();
    input[..response.len()].copy_from_slice(response);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum)
        .expect("real FFT dimensions should match");

    let peak_magnitude = spectrum
        .iter()
        .map(|value| value.norm())
        .fold(0.0, f64::max);
    let bin_hz = sample_rate / fft_len as f64;
    let bin_omega = 2.0 * std::f64::consts::PI / fft_len as f64;
    let mut phase = Vec::with_capacity(spectrum.len());
    let mut previous = spectrum[0].arg();
    phase.push(previous);
    for transfer in spectrum.iter().skip(1) {
        let raw = transfer.arg();
        let delta = (raw - previous + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI)
            - std::f64::consts::PI;
        previous += delta;
        phase.push(previous);
    }

    frequencies_hz
        .iter()
        .copied()
        .filter(|frequency| *frequency > 0.0 && *frequency < sample_rate * 0.5)
        .filter_map(|frequency| {
            let bin = (frequency / bin_hz).round() as usize;
            let bin = bin.min(spectrum.len() - 1);
            let transfer = spectrum[bin];
            let magnitude = transfer.norm();
            if magnitude <= peak_magnitude * 1e-5 {
                return None;
            }
            let first = bin.saturating_sub(4).max(1);
            let last = bin.saturating_add(4).min(spectrum.len().saturating_sub(2));
            if first >= last {
                return None;
            }
            let center = 0.5 * (first + last) as f64;
            let mut covariance = 0.0;
            let mut variance = 0.0;
            for index in first..=last {
                let x = index as f64 - center;
                covariance += x * phase[index];
                variance += x * x;
            }
            let absolute_delay_samples = -(covariance / variance) / bin_omega;
            let relative_delay_samples = absolute_delay_samples - peak_index as f64;
            let relative_delay_ms = relative_delay_samples / sample_rate * 1000.0;
            Some(GroupDelayPoint {
                frequency_hz: frequency,
                magnitude_db: amplitude_ratio_db(magnitude, peak_magnitude),
                absolute_group_delay_ms: absolute_delay_samples / sample_rate * 1000.0,
                group_delay_relative_to_peak_ms: relative_delay_ms,
            })
        })
        .collect()
}

pub fn magnitude_response_metrics(response: &[f64], output_rate: f64) -> MagnitudeResponseMetrics {
    assert!(!response.is_empty());
    assert!(output_rate > 0.0);
    let fft_len = response.len().next_power_of_two().max(1024);
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut input = fft.make_input_vec();
    input[..response.len()].copy_from_slice(response);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum)
        .expect("real FFT dimensions should match");
    let dc = spectrum[0].norm().max(f64::MIN_POSITIVE);
    let bin_hz = output_rate / fft_len as f64;
    let magnitude_db: Vec<f64> = spectrum
        .iter()
        .map(|value| amplitude_ratio_db(value.norm(), dc))
        .collect();
    let gain = |frequency: f64| -> f64 {
        let index = (frequency / bin_hz).round() as usize;
        magnitude_db[index.min(magnitude_db.len() - 1)]
    };
    let ripple = |low: f64, high: f64| -> f64 {
        let first = (low / bin_hz).ceil() as usize;
        let last = ((high / bin_hz).floor() as usize).min(magnitude_db.len() - 1);
        let band = &magnitude_db[first.min(last)..=last];
        band.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - band.iter().copied().fold(f64::INFINITY, f64::min)
    };
    let crossing = |threshold: f64| -> Option<f64> {
        let first = (10_000.0 / bin_hz).floor() as usize;
        (first + 1..magnitude_db.len()).find_map(|index| {
            let before = magnitude_db[index - 1];
            let after = magnitude_db[index];
            if before > threshold && after <= threshold {
                let fraction = (threshold - before) / (after - before);
                Some((index as f64 - 1.0 + fraction) * bin_hz)
            } else {
                None
            }
        })
    };
    let rejection = |low: f64, high: f64| -> f64 {
        let first = (low / bin_hz).ceil() as usize;
        let last = ((high / bin_hz).floor() as usize).min(magnitude_db.len() - 1);
        -magnitude_db[first.min(last)..=last]
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let edge_0_1 = crossing(-0.1);
    let edge_100 = crossing(-100.0);

    MagnitudeResponseMetrics {
        gain_5khz_db_dc: gain(5_000.0),
        gain_10khz_db_dc: gain(10_000.0),
        gain_15khz_db_dc: gain(15_000.0),
        gain_18khz_db_dc: gain(18_000.0),
        gain_20khz_db_dc: gain(20_000.0),
        gain_20_5khz_db_dc: gain(20_500.0),
        gain_21khz_db_dc: gain(21_000.0),
        gain_21_5khz_db_dc: gain(21_500.0),
        gain_22khz_db_dc: gain(22_000.0),
        gain_22_05khz_db_dc: gain(22_050.0),
        passband_ripple_20hz_18khz_db: ripple(20.0, 18_000.0),
        passband_ripple_20hz_20khz_db: ripple(20.0, 20_000.0),
        bandwidth_minus_0_1_db_hz: edge_0_1,
        bandwidth_minus_1_db_hz: crossing(-1.0),
        bandwidth_minus_3_db_hz: crossing(-3.0),
        bandwidth_minus_6_db_hz: crossing(-6.0),
        transition_minus_0_1_to_minus_100_db_hz: edge_0_1
            .zip(edge_100)
            .map(|(start, stop)| stop - start),
        stopband_rejection_24_1khz_to_nyquist_db: rejection(24_100.0, output_rate * 0.5),
        first_image_rejection_24_1khz_to_64_1khz_db: rejection(24_100.0, 64_100.0),
    }
}

pub fn analyze_quadrature_packet(
    frequency_hz: f64,
    cycles: f64,
    nominal_duration_seconds: f64,
    left: &[f64],
    right: &[f64],
    sample_rate: f64,
    nominal_onset_index: f64,
) -> PacketTimingMetrics {
    assert_eq!(left.len(), right.len());
    assert!(!left.is_empty());
    let envelope: Vec<f64> = left.iter().zip(right).map(|(i, q)| i.hypot(*q)).collect();
    let energy = envelope.iter().map(|value| value * value).sum::<f64>();
    let centroid = if energy > 0.0 {
        envelope
            .iter()
            .enumerate()
            .map(|(index, value)| index as f64 * value * value)
            .sum::<f64>()
            / energy
    } else {
        0.0
    };
    let peak = envelope.iter().copied().fold(0.0, f64::max);
    let nominal_output_samples = nominal_duration_seconds * sample_rate;
    let half = nominal_output_samples * 0.5;
    let core_start = (centroid - half).ceil().max(0.0) as usize;
    let core_end = (centroid + half).floor().max(0.0) as usize;
    let core_end = core_end.min(envelope.len().saturating_sub(1));
    let pre = &envelope[..core_start.min(envelope.len())];
    let post = if core_end + 1 < envelope.len() {
        &envelope[core_end + 1..]
    } else {
        &[]
    };
    let pre_energy = pre.iter().map(|value| value * value).sum::<f64>();
    let post_energy = post.iter().map(|value| value * value).sum::<f64>();
    let max_pre = pre.iter().copied().fold(0.0, f64::max);
    let max_post = post.iter().copied().fold(0.0, f64::max);
    let onset_start = nominal_onset_index.ceil().clamp(0.0, envelope.len() as f64) as usize;
    let onset_end = (nominal_onset_index + nominal_output_samples)
        .floor()
        .clamp(0.0, envelope.len().saturating_sub(1) as f64) as usize;
    let onset_pre = &envelope[..onset_start];
    let onset_post = if onset_end + 1 < envelope.len() {
        &envelope[onset_end + 1..]
    } else {
        &[]
    };
    let onset_pre_energy = onset_pre.iter().map(|value| value * value).sum::<f64>();
    let onset_post_energy = onset_post.iter().map(|value| value * value).sum::<f64>();
    let maximum_onset_pre = onset_pre.iter().copied().fold(0.0, f64::max);
    let maximum_onset_post = onset_post.iter().copied().fold(0.0, f64::max);

    PacketTimingMetrics {
        frequency_hz,
        cycles,
        nominal_duration_ms: nominal_duration_seconds * 1000.0,
        alignment: "energy_centroid (historical envelope split)",
        energy_centroid_index: centroid,
        pre_echo_energy_db_total: power_ratio_db(pre_energy, energy),
        post_echo_energy_db_total: power_ratio_db(post_energy, energy),
        maximum_pre_echo_db_peak: amplitude_ratio_db(max_pre, peak),
        maximum_post_echo_db_peak: amplitude_ratio_db(max_post, peak),
        onset_reference: "principal impulse peak plus nominal source packet bounds",
        nominal_onset_index,
        onset_pre_echo_energy_db_total: power_ratio_db(onset_pre_energy, energy),
        onset_post_decay_energy_db_total: power_ratio_db(onset_post_energy, energy),
        maximum_onset_pre_echo_db_peak: amplitude_ratio_db(maximum_onset_pre, peak),
        maximum_onset_post_decay_db_peak: amplitude_ratio_db(maximum_onset_post, peak),
    }
}

pub fn step_response_excursions(
    response: &[f64],
    baseline_samples: usize,
    settled_samples: usize,
) -> (f64, f64) {
    assert!(!response.is_empty());
    let baseline_count = baseline_samples.clamp(1, response.len());
    let settled_count = settled_samples.clamp(1, response.len());
    let baseline = response[..baseline_count].iter().sum::<f64>() / baseline_count as f64;
    let settled = response[response.len() - settled_count..]
        .iter()
        .sum::<f64>()
        / settled_count as f64;
    let span = settled - baseline;
    if span.abs() <= f64::EPSILON {
        return (0.0, 0.0);
    }
    let mut maximum = f64::NEG_INFINITY;
    let mut minimum = f64::INFINITY;
    for sample in response {
        let normalized = (sample - baseline) / span;
        maximum = maximum.max(normalized);
        minimum = minimum.min(normalized);
    }
    (
        (maximum - 1.0).max(0.0) * 100.0,
        (-minimum).max(0.0) * 100.0,
    )
}

pub fn convolve_upsampled(
    source: &[f64],
    impulse_response: &[f64],
    integer_ratio: usize,
) -> Vec<f64> {
    assert!(integer_ratio > 0);
    if source.is_empty() || impulse_response.is_empty() {
        return Vec::new();
    }
    let upsampled_len = (source.len() - 1) * integer_ratio + 1;
    let output_len = upsampled_len + impulse_response.len() - 1;
    let fft_len = output_len.next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);

    let mut a = forward.make_input_vec();
    let mut b = forward.make_input_vec();
    for (index, value) in source.iter().copied().enumerate() {
        a[index * integer_ratio] = value;
    }
    b[..impulse_response.len()].copy_from_slice(impulse_response);
    let mut spectrum_a = forward.make_output_vec();
    let mut spectrum_b = forward.make_output_vec();
    forward
        .process(&mut a, &mut spectrum_a)
        .expect("real FFT dimensions should match");
    forward
        .process(&mut b, &mut spectrum_b)
        .expect("real FFT dimensions should match");
    for (a_bin, b_bin) in spectrum_a.iter_mut().zip(&spectrum_b) {
        *a_bin *= *b_bin;
    }
    let mut output = inverse.make_output_vec();
    inverse
        .process(&mut spectrum_a, &mut output)
        .expect("inverse real FFT dimensions should match");
    let scale = 1.0 / fft_len as f64;
    output.truncate(output_len);
    for value in &mut output {
        *value *= scale;
    }
    output
}

pub fn convolve_upsampled_pair(
    source_a: &[f64],
    source_b: &[f64],
    impulse_response: &[f64],
    integer_ratio: usize,
) -> (Vec<f64>, Vec<f64>) {
    assert!(integer_ratio > 0);
    assert_eq!(source_a.len(), source_b.len());
    if source_a.is_empty() || impulse_response.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let upsampled_len = (source_a.len() - 1) * integer_ratio + 1;
    let output_len = upsampled_len + impulse_response.len() - 1;
    let fft_len = output_len.next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);

    let mut impulse = forward.make_input_vec();
    impulse[..impulse_response.len()].copy_from_slice(impulse_response);
    let mut impulse_spectrum = forward.make_output_vec();
    forward
        .process(&mut impulse, &mut impulse_spectrum)
        .expect("real FFT dimensions should match");

    let convolve_one = |source: &[f64]| {
        let mut input = forward.make_input_vec();
        for (index, value) in source.iter().copied().enumerate() {
            input[index * integer_ratio] = value;
        }
        let mut spectrum = forward.make_output_vec();
        forward
            .process(&mut input, &mut spectrum)
            .expect("real FFT dimensions should match");
        for (bin, impulse_bin) in spectrum.iter_mut().zip(&impulse_spectrum) {
            *bin *= *impulse_bin;
        }
        let mut output = inverse.make_output_vec();
        inverse
            .process(&mut spectrum, &mut output)
            .expect("inverse real FFT dimensions should match");
        output.truncate(output_len);
        let scale = 1.0 / fft_len as f64;
        for value in &mut output {
            *value *= scale;
        }
        output
    };

    (convolve_one(source_a), convolve_one(source_b))
}

fn main_lobe(response: &[f64], peak: usize) -> MainLobe {
    let peak_sign = response[peak].signum();
    let mut left = 0.0;
    for index in (1..=peak).rev() {
        if response[index - 1] == 0.0 || response[index - 1].signum() != peak_sign {
            left = interpolated_zero(index - 1, response[index - 1], index, response[index]);
            break;
        }
    }
    let mut right = response.len().saturating_sub(1) as f64;
    for index in peak..response.len().saturating_sub(1) {
        if response[index + 1] == 0.0 || response[index + 1].signum() != peak_sign {
            right = interpolated_zero(index, response[index], index + 1, response[index + 1]);
            break;
        }
    }
    MainLobe { left, right }
}

fn interpolated_zero(i0: usize, y0: f64, i1: usize, y1: f64) -> f64 {
    let denominator = y0.abs() + y1.abs();
    if denominator <= f64::EPSILON {
        return (i0 + i1) as f64 * 0.5;
    }
    i0 as f64 + y0.abs() / denominator
}

fn decay_time(
    response: &[f64],
    peak_index: usize,
    peak: f64,
    threshold_db: f64,
    sample_rate: f64,
) -> (Option<f64>, bool) {
    if peak <= f64::EPSILON {
        return (None, false);
    }
    let threshold = peak * 10.0_f64.powf(threshold_db / 20.0);
    let last = response
        .iter()
        .enumerate()
        .skip(peak_index + 1)
        .rfind(|(_, value)| value.abs() > threshold)
        .map(|(index, _)| index);
    let hold_samples = (sample_rate * 0.010).ceil() as usize;
    let censored = last.is_some_and(|index| index.saturating_add(hold_samples) >= response.len());
    (
        last.map(|index| (index - peak_index) as f64 / sample_rate * 1000.0),
        censored,
    )
}

fn power_ratio_db(value: f64, reference: f64) -> f64 {
    if value <= 0.0 || reference <= 0.0 {
        DB_FLOOR
    } else {
        (10.0 * (value / reference).log10()).max(DB_FLOOR)
    }
}

fn amplitude_ratio_db(value: f64, reference: f64) -> f64 {
    if value <= 0.0 || reference <= 0.0 {
        DB_FLOOR
    } else {
        (20.0 * (value / reference).log10()).max(DB_FLOOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_impulse_has_symmetric_energy_and_zero_centroid_offset() {
        let metrics = analyze_impulse(&[-0.1, 0.0, 1.0, 0.0, -0.1], 1000.0);
        assert!(
            (metrics.pre_peak_energy_db_total - metrics.post_peak_energy_db_total).abs() < 1e-12
        );
        assert!(metrics.energy_centroid_relative_to_peak_ms.abs() < 1e-12);
        assert_eq!(metrics.maximum_pre_ringing_lobe_db_peak, -20.0);
        assert_eq!(metrics.maximum_post_ringing_lobe_db_peak, -20.0);
    }

    #[test]
    fn direct_step_reports_overshoot() {
        let (overshoot, undershoot) = step_response_excursions(&[0.0, 0.0, 1.2, 1.0, 1.0], 2, 2);
        assert!((overshoot - 20.0).abs() < 1e-10);
        assert_eq!(undershoot, 0.0);
    }

    #[test]
    fn asymmetric_energy_uses_total_energy_as_denominator() {
        let metrics = analyze_impulse(&[0.25, 1.0, 0.5], 7_000.0);
        assert!((metrics.pre_peak_energy_db_total - 10.0 * (1.0_f64 / 21.0).log10()).abs() < 1e-12);
        assert!(
            (metrics.post_peak_energy_db_total - 10.0 * (4.0_f64 / 21.0).log10()).abs() < 1e-12
        );
        assert!(
            (metrics.energy_centroid_relative_to_peak_ms - 1.0 / 7.0 / 7_000.0 * 1000.0).abs()
                < 1e-12
        );
    }

    #[test]
    fn digital_zero_bounds_a_causal_main_lobe() {
        let metrics = analyze_impulse(&[0.0, 0.0, 0.5, 1.0, 0.5, 0.0, -0.1], 1_000.0);
        assert!((metrics.main_lobe_width_us - 4_000.0).abs() < 1e-12);
        assert_eq!(metrics.maximum_pre_ringing_lobe_db_peak, -300.0);
        assert_eq!(metrics.maximum_post_ringing_lobe_db_peak, -20.0);
    }

    #[test]
    fn group_delay_of_delayed_unit_impulse_matches_peak() {
        let mut impulse = vec![0.0; 256];
        impulse[37] = 1.0;
        let point = group_delay_curve(&impulse, 48_000.0, 37, &[10_000.0]).remove(0);
        assert!((point.absolute_group_delay_ms - 37.0 / 48_000.0 * 1000.0).abs() < 1e-9);
        assert!(point.group_delay_relative_to_peak_ms.abs() < 1e-9);
    }

    #[test]
    fn unit_impulse_has_flat_magnitude_response() {
        let metrics = magnitude_response_metrics(&[1.0], 176_400.0);
        assert!(metrics.gain_20khz_db_dc.abs() < 1e-12);
        assert!(metrics.passband_ripple_20hz_20khz_db.abs() < 1e-12);
        assert!(metrics.bandwidth_minus_3_db_hz.is_none());
        assert!(metrics.first_image_rejection_24_1khz_to_64_1khz_db.abs() < 1e-12);
    }

    #[test]
    fn fft_convolution_respects_integer_input_spacing() {
        let output = convolve_upsampled(&[1.0, 2.0], &[1.0, 0.5], 3);
        let expected = [1.0, 0.5, 0.0, 2.0, 1.0];
        assert_eq!(output.len(), expected.len());
        for (actual, expected) in output.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn packet_onset_metrics_use_nominal_bounds_not_centroid() {
        let metrics =
            analyze_quadrature_packet(1.0, 1.0, 1.0, &[0.1, 1.0, 1.0, 0.2], &[0.0; 4], 1.0, 1.0);
        let total_energy = 2.05_f64;
        assert!(
            (metrics.onset_pre_echo_energy_db_total - 10.0 * (0.01_f64 / total_energy).log10())
                .abs()
                < 1e-12
        );
        assert!(
            (metrics.onset_post_decay_energy_db_total - 10.0 * (0.04_f64 / total_energy).log10())
                .abs()
                < 1e-12
        );
        assert!((metrics.maximum_onset_pre_echo_db_peak + 20.0).abs() < 1e-12);
        assert!((metrics.maximum_onset_post_decay_db_peak - 20.0 * 0.2_f64.log10()).abs() < 1e-12);
    }
}
