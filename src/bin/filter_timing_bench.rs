use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use fozmo::audio::dsp::resampler::{FilterType, SincResampler};
use fozmo::audio::dsp::timing::{
    GroupDelayPoint, ImpulseTimingMetrics, PacketTimingMetrics, analyze_impulse,
    analyze_quadrature_packet, convolve_upsampled_pair, group_delay_curve,
    normalize_interpolator_dc, step_response_excursions,
};
use serde::Serialize;

const PRODUCTION_FILTERS: [FilterType; 4] = [
    FilterType::LinearPhase128k,
    FilterType::MinimumPhaseCompact128k,
    FilterType::SplitPhase128kE2v3,
    FilterType::SmoothPhase128k,
];
const PACKET_FREQUENCIES_HZ: [f64; 5] = [5_000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0];
const SUMMARY_GROUP_DELAY_HZ: [f64; 7] = [
    100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0,
];
const CHUNK_FRAMES: usize = 4096;

#[derive(Debug, Parser)]
#[command(about = "Measure temporal behavior of Fozmo's production reconstruction filters")]
struct Args {
    #[arg(long, default_value_t = 44_100)]
    source_rate: u32,
    #[arg(long, default_value_t = 176_400)]
    output_rate: u32,
    #[arg(long, default_value_t = -12.0, allow_hyphen_values = true)]
    headroom_db: f64,
    #[arg(long, default_value_t = 8.0)]
    packet_cycles: f64,
    #[arg(long, default_value_t = 131_072)]
    guard_source_frames: usize,
    #[arg(long, default_value_t = 4_000.0)]
    tail_ms: f64,
    #[arg(long, value_name = "NAME")]
    filter: Vec<String>,
    #[arg(long, default_value = "target/filter-timing")]
    out: PathBuf,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    configuration: Configuration,
    filters: Vec<FilterReport>,
}

#[derive(Debug, Serialize)]
struct Configuration {
    source_rate_hz: u32,
    output_rate_hz: u32,
    integer_ratio: usize,
    stimulus_headroom_dbfs: f64,
    packet_window: &'static str,
    packet_cycles: f64,
    packet_frequencies_hz: Vec<f64>,
    guard_source_frames: usize,
    analysis_tail_ms: f64,
    passband_gain_control: &'static str,
    transition_bandwidth_control: &'static str,
    impulse_alignment: &'static str,
    packet_alignment: &'static str,
    reconstruction_filter: &'static str,
    packet_reconstruction_floor_db_peak: f64,
    analysis_window: &'static str,
    environment_overrides: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct FilterReport {
    filter: &'static str,
    display_name: &'static str,
    runtime: RuntimeMetadata,
    dc_normalization_scale: f64,
    measured_peak_offset_from_input_ms: f64,
    impulse: ImpulseTimingMetrics,
    tone_packets: Vec<PacketTimingMetrics>,
    group_delay: Vec<GroupDelayPoint>,
}

#[derive(Debug, Serialize)]
struct RuntimeMetadata {
    path: &'static str,
    phase_profile_preserved: bool,
    uses_capped_fallback: bool,
    ratio_num: u32,
    ratio_den: u32,
    nominal_buffer_latency_ms: f64,
    estimated_memory_bytes: usize,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let ratio = validate_args(&args)?;
    let filters = selected_filters(&args.filter)?;
    let amplitude = 10.0_f64.powf(args.headroom_db / 20.0);
    let tail_frames = ((args.tail_ms / 1000.0) * args.source_rate as f64).ceil() as usize;
    let total_frames = args.guard_source_frames + tail_frames;
    let event_output_index = args.guard_source_frames * ratio;
    let group_delay_frequencies = group_delay_grid(args.source_rate);

    let mut reports = Vec::with_capacity(filters.len());
    for filter in filters {
        eprintln!("measuring {}...", filter.as_name());
        let mut resampler = SincResampler::new(filter, args.source_rate, args.output_rate);
        let runtime = resampler.runtime_info();
        if !runtime.phase_profile_preserved || runtime.uses_capped_fallback {
            return Err(format!(
                "{} selected a non-production fallback for {} -> {}",
                filter.as_name(),
                args.source_rate,
                args.output_rate
            ));
        }

        let mut impulse_input = vec![0.0; total_frames];
        impulse_input[args.guard_source_frames] = amplitude;
        let impulse_output = run_mono(&mut resampler, &impulse_input);
        let mut impulse_response: Vec<f64> = impulse_output
            .into_iter()
            .map(|value| value / amplitude)
            .collect();
        let normalization_scale = normalize_interpolator_dc(&mut impulse_response, ratio as f64);
        let mut impulse = analyze_impulse(&impulse_response, args.output_rate as f64);

        resampler.reset();
        let mut step_input = vec![0.0; total_frames];
        step_input[args.guard_source_frames..].fill(amplitude);
        let step_output = run_mono(&mut resampler, &step_input);
        let plateau_samples = (args.output_rate as usize / 10).max(1);
        let (overshoot, undershoot) =
            step_response_excursions(&step_output, plateau_samples, plateau_samples);
        impulse.step_response_overshoot_percent = overshoot;
        impulse.step_response_undershoot_percent = undershoot;

        let convolution_response = trim_below(&impulse_response, -160.0, 512);
        let mut packets = Vec::with_capacity(PACKET_FREQUENCIES_HZ.len());
        for frequency in PACKET_FREQUENCIES_HZ {
            let (packet_i, packet_q) =
                make_quadrature_packet(args.source_rate, frequency, args.packet_cycles, amplitude);
            let duration_seconds = packet_i.len() as f64 / args.source_rate as f64;
            let (output_i, output_q) =
                convolve_upsampled_pair(&packet_i, &packet_q, convolution_response, ratio);
            packets.push(analyze_quadrature_packet(
                frequency,
                args.packet_cycles,
                duration_seconds,
                &output_i,
                &output_q,
                args.output_rate as f64,
            ));
        }

        let group_delay = group_delay_curve(
            &impulse_response,
            args.output_rate as f64,
            impulse.peak_index,
            &group_delay_frequencies,
        );
        let peak_offset_ms = (impulse.peak_index as f64 - event_output_index as f64)
            / args.output_rate as f64
            * 1000.0;
        reports.push(FilterReport {
            filter: filter.as_name(),
            display_name: display_name(filter),
            runtime: RuntimeMetadata {
                path: runtime.path_kind.as_name(),
                phase_profile_preserved: runtime.phase_profile_preserved,
                uses_capped_fallback: runtime.uses_capped_fallback,
                ratio_num: runtime.ratio_num,
                ratio_den: runtime.ratio_den,
                nominal_buffer_latency_ms: runtime.latency_ms,
                estimated_memory_bytes: runtime.estimated_memory_bytes,
            },
            dc_normalization_scale: normalization_scale,
            measured_peak_offset_from_input_ms: peak_offset_ms,
            impulse,
            tone_packets: packets,
            group_delay,
        });
    }

    let report = Report {
        schema_version: 1,
        configuration: Configuration {
            source_rate_hz: args.source_rate,
            output_rate_hz: args.output_rate,
            integer_ratio: ratio,
            stimulus_headroom_dbfs: args.headroom_db,
            packet_window: "Hann",
            packet_cycles: args.packet_cycles,
            packet_frequencies_hz: PACKET_FREQUENCIES_HZ.to_vec(),
            guard_source_frames: args.guard_source_frames,
            analysis_tail_ms: args.tail_ms,
            passband_gain_control: "each measured impulse is DC-normalized to the common integer interpolation ratio",
            transition_bandwidth_control: "intrinsic production response; common rates and 20 kHz analysis band, with no additional equalizer",
            impulse_alignment: "principal absolute-energy peak",
            packet_alignment: "quadrature-envelope energy centroid",
            reconstruction_filter: "the production SincResampler path only; no secondary reconstruction filter",
            packet_reconstruction_floor_db_peak: -160.0,
            analysis_window: "full fixed guard plus tail capture; 10 ms threshold hold for decay censoring",
            environment_overrides: relevant_environment_overrides(),
        },
        filters: reports,
    };

    write_report(&args.out, &report).map_err(|error| error.to_string())?;
    println!("wrote {}", args.out.join("report.md").display());
    println!("wrote {}", args.out.join("report.json").display());
    println!("wrote {}", args.out.join("group-delay.csv").display());
    Ok(())
}

fn validate_args(args: &Args) -> Result<usize, String> {
    if args.source_rate < 40_000 || args.source_rate > 192_000 {
        return Err("source rate must be between 40 kHz and 192 kHz".to_string());
    }
    if args.output_rate % args.source_rate != 0 {
        return Err("output rate must be an integer multiple of source rate".to_string());
    }
    let ratio = (args.output_rate / args.source_rate) as usize;
    if ratio < 2 || !ratio.is_power_of_two() {
        return Err(
            "the fair-comparison bench requires a power-of-two integer ratio >= 2".to_string(),
        );
    }
    if args.source_rate as f64 * 0.5 <= 20_000.0 {
        return Err("source Nyquist must exceed the 20 kHz packet".to_string());
    }
    if !args.headroom_db.is_finite() || args.headroom_db > 0.0 {
        return Err("headroom must be a finite value at or below 0 dBFS".to_string());
    }
    if !args.packet_cycles.is_finite() || args.packet_cycles < 2.0 {
        return Err("packet cycles must be finite and at least 2".to_string());
    }
    if args.guard_source_frames < 131_072 {
        return Err(
            "guard-source-frames must be at least 131072 for the production FIRs".to_string(),
        );
    }
    if !args.tail_ms.is_finite() || args.tail_ms < 3_500.0 {
        return Err(
            "tail-ms must be at least 3500 ms to expose the long production FIR tails".to_string(),
        );
    }
    Ok(ratio)
}

fn selected_filters(names: &[String]) -> Result<Vec<FilterType>, String> {
    if names.is_empty() {
        return Ok(PRODUCTION_FILTERS.to_vec());
    }
    let mut filters = Vec::new();
    for name in names {
        let filter = PRODUCTION_FILTERS
            .iter()
            .copied()
            .find(|filter| filter.as_name().eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("{name} is not a current production filter"))?;
        if !filters.contains(&filter) {
            filters.push(filter);
        }
    }
    Ok(filters)
}

fn run_mono(resampler: &mut SincResampler, input: &[f64]) -> Vec<f64> {
    let mut mono = Vec::new();
    let mut interleaved = Vec::new();
    for chunk in input.chunks(CHUNK_FRAMES) {
        resampler.input(chunk, chunk);
        interleaved.clear();
        resampler.process(&mut interleaved);
        mono.extend(interleaved.iter().step_by(2).copied());
    }
    mono
}

fn make_quadrature_packet(
    source_rate: u32,
    frequency_hz: f64,
    cycles: f64,
    amplitude: f64,
) -> (Vec<f64>, Vec<f64>) {
    let samples = ((cycles / frequency_hz) * source_rate as f64)
        .round()
        .max(3.0) as usize;
    let mut i = Vec::with_capacity(samples);
    let mut q = Vec::with_capacity(samples);
    for index in 0..samples {
        let window =
            0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / (samples - 1) as f64).cos();
        let phase = 2.0 * std::f64::consts::PI * frequency_hz * index as f64 / source_rate as f64;
        i.push(amplitude * window * phase.cos());
        q.push(amplitude * window * phase.sin());
    }
    (i, q)
}

fn trim_below(response: &[f64], threshold_db: f64, margin: usize) -> &[f64] {
    let peak = response.iter().map(|value| value.abs()).fold(0.0, f64::max);
    let threshold = peak * 10.0_f64.powf(threshold_db / 20.0);
    let first = response
        .iter()
        .position(|value| value.abs() > threshold)
        .unwrap_or(0)
        .saturating_sub(margin);
    let last = response
        .iter()
        .rposition(|value| value.abs() > threshold)
        .unwrap_or(response.len().saturating_sub(1))
        .saturating_add(margin)
        .min(response.len().saturating_sub(1));
    &response[first..=last]
}

fn group_delay_grid(source_rate: u32) -> Vec<f64> {
    let maximum = 20_000.0_f64.min(source_rate as f64 * 0.5 - 1.0);
    let mut frequencies = vec![20.0];
    frequencies.extend(
        (100..=(maximum as usize))
            .step_by(100)
            .map(|value| value as f64),
    );
    frequencies
}

fn relevant_environment_overrides() -> BTreeMap<String, String> {
    [
        "FOZMO_LINEAR128K_CUTOFF",
        "FOZMO_LINEAR128K_BETA",
        "FOZMO_MINIMUM16K_CUTOFF",
        "FOZMO_MINIMUM16K_BETA",
        "FOZMO_SPLIT128K_CUTOFF",
        "FOZMO_SPLIT128K_BETA",
    ]
    .into_iter()
    .filter_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| (name.to_string(), value))
    })
    .collect()
}

fn display_name(filter: FilterType) -> &'static str {
    match filter {
        FilterType::LinearPhase128k => "Linear Phase",
        FilterType::MinimumPhaseCompact128k => "Minimum Phase",
        FilterType::SplitPhase128kE2v3 => "Split Phase",
        FilterType::SmoothPhase128k => "Smooth Phase",
        _ => unreachable!("production filter list is closed"),
    }
}

fn write_report(out: &Path, report: &Report) -> std::io::Result<()> {
    fs::create_dir_all(out)?;
    fs::write(
        out.join("report.json"),
        serde_json::to_string_pretty(report).expect("report is JSON serializable") + "\n",
    )?;
    fs::write(out.join("report.md"), markdown_report(report))?;
    fs::write(out.join("group-delay.csv"), group_delay_csv(report))?;
    Ok(())
}

fn markdown_report(report: &Report) -> String {
    let c = &report.configuration;
    let mut text = format!(
        "# Production filter timing\n\nSource: {} Hz  \nOutput: {} Hz  \nHeadroom: {:.1} dBFS  \nPacket: {:.1}-cycle Hann window  \nAlignment: impulse by principal peak; packets by energy centroid  \n\n",
        c.source_rate_hz, c.output_rate_hz, c.stimulus_headroom_dbfs, c.packet_cycles
    );
    text.push_str("## Impulse and step metrics\n\n");
    text.push_str("| Filter | Pre energy (dB total) | Max pre lobe (dB peak) | Post energy (dB total) | Max post lobe (dB peak) | Decay -80 (ms) | Decay -120 (ms) | Main lobe (us) | Step overshoot (%) | Centroid vs peak (ms) |\n");
    text.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for filter in &report.filters {
        let m = &filter.impulse;
        text.push_str(&format!(
            "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {:.2} | {:.3} | {:.4} |\n",
            filter.display_name,
            m.pre_peak_energy_db_total,
            m.maximum_pre_ringing_lobe_db_peak,
            m.post_peak_energy_db_total,
            m.maximum_post_ringing_lobe_db_peak,
            format_decay(m.decay_time_to_minus_80_db_ms, m.decay_minus_80_db_censored),
            format_decay(
                m.decay_time_to_minus_120_db_ms,
                m.decay_minus_120_db_censored
            ),
            m.main_lobe_width_us,
            m.step_response_overshoot_percent,
            m.energy_centroid_relative_to_peak_ms,
        ));
    }
    text.push_str("\n## Windowed tone packets\n\nEnergy outside the nominal packet window is measured after aligning its quadrature envelope by energy centroid.\n\n");
    text.push_str("| Filter | Frequency (Hz) | Pre-echo energy (dB total) | Max pre-echo (dB peak) | Post-echo energy (dB total) | Max post-echo (dB peak) |\n");
    text.push_str("| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for filter in &report.filters {
        for packet in &filter.tone_packets {
            text.push_str(&format!(
                "| {} | {:.0} | {:.2} | {:.2} | {:.2} | {:.2} |\n",
                filter.display_name,
                packet.frequency_hz,
                packet.pre_echo_energy_db_total,
                packet.maximum_pre_echo_db_peak,
                packet.post_echo_energy_db_total,
                packet.maximum_post_echo_db_peak,
            ));
        }
    }
    text.push_str("\n## Group delay relative to principal peak\n\n");
    text.push_str("| Filter | 100 Hz | 1 kHz | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |\n");
    text.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for filter in &report.filters {
        text.push_str(&format!("| {}", filter.display_name));
        for frequency in SUMMARY_GROUP_DELAY_HZ {
            let point = filter
                .group_delay
                .iter()
                .min_by(|a, b| {
                    (a.frequency_hz - frequency)
                        .abs()
                        .total_cmp(&(b.frequency_hz - frequency).abs())
                })
                .expect("group delay grid is populated");
            text.push_str(&format!(
                " | {:.4} ms",
                point.group_delay_relative_to_peak_ms
            ));
        }
        text.push_str(" |\n");
    }
    text.push_str("\nThe full 20 Hz-20 kHz group-delay curves are in `group-delay.csv`. The production filters retain their intrinsic transition shapes; the bench does not add a compensating equalizer that would change the filters under test.\n");
    text
}

fn group_delay_csv(report: &Report) -> String {
    let mut csv = String::from(
        "filter,source_rate_hz,output_rate_hz,frequency_hz,magnitude_db,absolute_group_delay_ms,group_delay_relative_to_peak_ms\n",
    );
    for filter in &report.filters {
        for point in &filter.group_delay {
            csv.push_str(&format!(
                "{},{},{},{:.3},{:.8},{:.8},{:.8}\n",
                filter.filter,
                report.configuration.source_rate_hz,
                report.configuration.output_rate_hz,
                point.frequency_hz,
                point.magnitude_db,
                point.absolute_group_delay_ms,
                point.group_delay_relative_to_peak_ms,
            ));
        }
    }
    csv
}

fn format_decay(value: Option<f64>, censored: bool) -> String {
    match (value, censored) {
        (Some(value), true) => format!(">{value:.2}"),
        (Some(value), false) => format!("{value:.2}"),
        (None, _) => "0.00".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_filter_selection_defaults_to_picker_set() {
        assert_eq!(selected_filters(&[]).unwrap(), PRODUCTION_FILTERS);
    }

    #[test]
    fn packet_has_common_headroom_and_zero_endpoints() {
        let (i, q) = make_quadrature_packet(44_100, 5_000.0, 8.0, 0.25);
        assert_eq!(i.len(), q.len());
        assert!(i[0].abs() < 1e-15 && q[0].abs() < 1e-15);
        assert!(i.last().unwrap().abs() < 1e-15 && q.last().unwrap().abs() < 1e-15);
        assert!(i.iter().chain(&q).all(|sample| sample.abs() <= 0.25));
    }
}
