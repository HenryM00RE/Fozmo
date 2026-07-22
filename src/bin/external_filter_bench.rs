use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use clap::Parser;
use fozmo::audio::dsp::resampler::{FilterType, SincResampler};
use fozmo::audio::dsp::timing::{
    GroupDelayPoint, ImpulseTimingMetrics, MagnitudeResponseMetrics, PacketTimingMetrics,
    analyze_impulse, analyze_quadrature_packet, group_delay_curve, magnitude_response_metrics,
    normalize_interpolator_dc, step_response_excursions,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const EXTERNAL_PRESETS: [(&str, &str); 3] = [
    ("megalinear", "External Product Linear 1M"),
    ("megaextreme", "External Product Hybrid 1M"),
    ("megaorganik", "External Product Minimum 1M"),
];
const FOZMO_FILTERS: [(FilterType, &str); 3] = [
    (FilterType::LinearPhase128k, "Fozmo Linear Phase"),
    (FilterType::MinimumPhaseCompact128k, "Fozmo Minimum Phase"),
    (FilterType::SplitPhase128kE3, "Fozmo Split Phase B"),
];
const TONES: [f64; 5] = [5_000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    external_dir: PathBuf,
    #[arg(long, default_value = "target/external-filter-comparison")]
    out: PathBuf,
    #[arg(long, default_value_t = 44_100)]
    source_rate: u32,
    #[arg(long, default_value_t = 176_400)]
    output_rate: u32,
    #[arg(long, default_value_t = -12.0, allow_hyphen_values = true)]
    headroom_db: f64,
    #[arg(long, default_value_t = 131_072)]
    guard_frames: usize,
    #[arg(long, default_value_t = 6_000.0)]
    tail_ms: f64,
    #[arg(long, default_value_t = 8.0)]
    packet_cycles: f64,
}

#[derive(Clone)]
struct Stimulus {
    id: String,
    samples: Vec<f64>,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    conditions: Conditions,
    external_executable_sha256: String,
    results: Vec<ResultRow>,
}

#[derive(Serialize)]
struct Conditions {
    source_rate_hz: u32,
    output_rate_hz: u32,
    input_and_comparison_domain: &'static str,
    headroom_dbfs: f64,
    guard_frames: usize,
    tail_ms: f64,
    packet_cycles: f64,
    external_dither_control: &'static str,
    dither_mitigation: &'static str,
    alignment: &'static str,
}

#[derive(Serialize)]
struct ResultRow {
    system: &'static str,
    id: String,
    display_name: String,
    class: &'static str,
    deterministic: bool,
    total_render_seconds: f64,
    silence_peak_dbfs: f64,
    silence_rms_dbfs: f64,
    impulse: ImpulseTimingMetrics,
    magnitude: MagnitudeResponseMetrics,
    packets: Vec<PacketTimingMetrics>,
    group_delay: Vec<GroupDelayPoint>,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    validate(&args)?;
    let exe = args.external_dir.join("upsampler.exe");
    if !exe.is_file() {
        return Err(format!("missing {}", exe.display()));
    }
    let stimuli = make_stimuli(&args);
    let stimulus_dir = args.out.join("stimuli");
    let render_root = args.out.join("renders");
    fs::create_dir_all(&stimulus_dir).map_err(|e| e.to_string())?;
    for stimulus in &stimuli {
        write_wav24(
            &stimulus_dir.join(format!("{}.wav", stimulus.id)),
            args.source_rate,
            &stimulus.samples,
        )
        .map_err(|e| e.to_string())?;
    }

    let mut rows = Vec::new();
    for (preset, display) in EXTERNAL_PRESETS {
        eprintln!("rendering {display}...");
        let dir = render_root.join(preset);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut rendered = BTreeMap::new();
        let mut total_seconds = 0.0;
        for stimulus in &stimuli {
            let input = stimulus_dir.join(format!("{}.wav", stimulus.id));
            let output = dir.join(format!("{}.wav", stimulus.id));
            total_seconds += render_external(&exe, &input, &output, args.output_rate, preset)?;
            rendered.insert(
                stimulus.id.clone(),
                read_wav24(&output).map_err(|e| e.to_string())?,
            );
        }
        let mut deterministic = true;
        for id in ["silence", "impulse"] {
            let input = stimulus_dir.join(format!("{id}.wav"));
            let repeat = dir.join(format!("{id}-repeat.wav"));
            total_seconds += render_external(&exe, &input, &repeat, args.output_rate, preset)?;
            deterministic &= sha256_file(&dir.join(format!("{id}.wav")))
                .map_err(|e| e.to_string())?
                == sha256_file(&repeat).map_err(|e| e.to_string())?;
        }
        rows.push(analyze_result(
            "ExternalProduct",
            preset,
            display,
            deterministic,
            total_seconds,
            &rendered,
            &args,
        )?);
    }

    for (filter, display) in FOZMO_FILTERS {
        eprintln!("rendering {display}...");
        let started = Instant::now();
        let rendered = render_fozmo(filter, &stimuli, &args);
        rows.push(analyze_result(
            "Fozmo",
            filter.as_name(),
            display,
            true,
            started.elapsed().as_secs_f64(),
            &rendered,
            &args,
        )?);
    }

    let report = Report {
        schema_version: 1,
        conditions: Conditions {
            source_rate_hz: args.source_rate,
            output_rate_hz: args.output_rate,
            input_and_comparison_domain: "stereo dual-mono signed PCM24 WAV",
            headroom_dbfs: args.headroom_db,
            guard_frames: args.guard_frames,
            tail_ms: args.tail_ms,
            packet_cycles: args.packet_cycles,
            external_dither_control: "not exposed; CLI reports TPDF at one 24-bit LSB",
            dither_mitigation: "subtract a same-length rendered-silence control; flag -120 dB decay as PCM24-floor-sensitive",
            alignment: "impulse principal peak; packet historical centroid plus principal-peak nominal onset bounds",
        },
        external_executable_sha256: sha256_file(&exe).map_err(|e| e.to_string())?,
        results: rows,
    };
    fs::write(
        args.out.join("report.json"),
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| e.to_string())?;
    fs::write(args.out.join("report.md"), markdown(&report)).map_err(|e| e.to_string())?;
    println!("wrote {}", args.out.join("report.md").display());
    Ok(())
}

fn validate(args: &Args) -> Result<(), String> {
    if !args.output_rate.is_multiple_of(args.source_rate)
        || args.output_rate / args.source_rate != 4
    {
        return Err("this comparison is fixed to an exact 4x integer path".into());
    }
    if args.guard_frames < 131_072 || args.tail_ms < 6_000.0 {
        return Err("1M-tap filters require at least 131072 guard frames and 6000 ms tail".into());
    }
    Ok(())
}

fn make_stimuli(args: &Args) -> Vec<Stimulus> {
    let tail = (args.tail_ms * args.source_rate as f64 / 1000.0).ceil() as usize;
    let len = args.guard_frames + tail;
    let amp = quantize24(10.0_f64.powf(args.headroom_db / 20.0));
    let mut out = vec![Stimulus {
        id: "silence".into(),
        samples: vec![0.0; len],
    }];
    let mut impulse = vec![0.0; len];
    impulse[args.guard_frames] = amp;
    out.push(Stimulus {
        id: "impulse".into(),
        samples: impulse,
    });
    let mut step = vec![0.0; len];
    step[args.guard_frames..].fill(amp);
    out.push(Stimulus {
        id: "step".into(),
        samples: step,
    });
    for frequency in TONES {
        let count = ((args.packet_cycles / frequency) * args.source_rate as f64).round() as usize;
        for (suffix, cosine) in [("i", true), ("q", false)] {
            let mut samples = vec![0.0; len];
            for n in 0..count {
                let window =
                    0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / (count - 1) as f64).cos();
                let phase = std::f64::consts::TAU * frequency * n as f64 / args.source_rate as f64;
                samples[args.guard_frames + n] =
                    quantize24(amp * window * if cosine { phase.cos() } else { phase.sin() });
            }
            out.push(Stimulus {
                id: format!("tone-{frequency:.0}-{suffix}"),
                samples,
            });
        }
    }
    out
}

fn render_external(
    exe: &Path,
    input: &Path,
    output: &Path,
    rate: u32,
    preset: &str,
) -> Result<f64, String> {
    let started = Instant::now();
    let result = Command::new(exe)
        .arg(input)
        .arg("-o")
        .arg(output)
        .arg("-r")
        .arg(rate.to_string())
        .arg("-p")
        .arg(preset)
        .output()
        .map_err(|e| e.to_string())?;
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    if !result.status.success() {
        return Err(format!(
            "external product failed for {preset}: {diagnostic}"
        ));
    }
    if diagnostic.to_ascii_lowercase().contains("unknown preset") {
        return Err(format!(
            "external product rejected preset {preset} and would have used a fallback: {diagnostic}"
        ));
    }
    if !output.is_file() {
        return Err(format!(
            "external product reported success but did not create {}: {diagnostic}",
            output.display()
        ));
    }
    Ok(started.elapsed().as_secs_f64())
}

fn render_fozmo(
    filter: FilterType,
    stimuli: &[Stimulus],
    args: &Args,
) -> BTreeMap<String, Vec<f64>> {
    let mut out = BTreeMap::new();
    for stimulus in stimuli {
        let mut resampler = SincResampler::new(filter, args.source_rate, args.output_rate);
        let mut mono = Vec::new();
        let mut interleaved = Vec::new();
        for chunk in stimulus.samples.chunks(4096) {
            resampler.input(chunk, chunk);
            interleaved.clear();
            resampler.process(&mut interleaved);
            mono.extend(interleaved.iter().step_by(2).copied().map(quantize24));
        }
        out.insert(stimulus.id.clone(), mono);
    }
    out
}

fn analyze_result(
    system: &'static str,
    id: &str,
    display: &str,
    deterministic: bool,
    total_render_seconds: f64,
    rendered: &BTreeMap<String, Vec<f64>>,
    args: &Args,
) -> Result<ResultRow, String> {
    let silence = rendered.get("silence").ok_or("missing silence")?;
    let silence_peak = silence.iter().map(|x| x.abs()).fold(0.0, f64::max);
    let silence_rms = (silence.iter().map(|x| x * x).sum::<f64>() / silence.len() as f64).sqrt();
    let clean = |name: &str| -> Result<Vec<f64>, String> {
        let signal = rendered
            .get(name)
            .ok_or_else(|| format!("missing {name}"))?;
        Ok(signal.iter().zip(silence).map(|(a, b)| a - b).collect())
    };
    let amp = quantize24(10.0_f64.powf(args.headroom_db / 20.0));
    let mut response: Vec<f64> = clean("impulse")?.into_iter().map(|x| x / amp).collect();
    normalize_interpolator_dc(&mut response, 4.0);
    let mut impulse = analyze_impulse(&response, args.output_rate as f64);
    let step = clean("step")?;
    let plateau = args.output_rate as usize / 10;
    let (over, under) = step_response_excursions(&step, plateau, plateau);
    impulse.step_response_overshoot_percent = over;
    impulse.step_response_undershoot_percent = under;
    let frequencies: Vec<f64> = std::iter::once(20.0)
        .chain((100..=20_000).step_by(100).map(|x| x as f64))
        .collect();
    let group_delay = group_delay_curve(
        &response,
        args.output_rate as f64,
        impulse.peak_index,
        &frequencies,
    );
    let magnitude = magnitude_response_metrics(&response, args.output_rate as f64);
    let mut packets = Vec::new();
    for frequency in TONES {
        let i = clean(&format!("tone-{frequency:.0}-i"))?;
        let q = clean(&format!("tone-{frequency:.0}-q"))?;
        let source_samples =
            ((args.packet_cycles / frequency) * args.source_rate as f64).round() as usize;
        packets.push(analyze_quadrature_packet(
            frequency,
            args.packet_cycles,
            source_samples as f64 / args.source_rate as f64,
            &i,
            &q,
            args.output_rate as f64,
            impulse.peak_index as f64,
        ));
    }
    Ok(ResultRow {
        system,
        id: id.into(),
        display_name: display.into(),
        class: "static",
        deterministic,
        total_render_seconds,
        silence_peak_dbfs: amp_db(silence_peak),
        silence_rms_dbfs: amp_db(silence_rms),
        impulse,
        magnitude,
        packets,
        group_delay,
    })
}

fn markdown(report: &Report) -> String {
    let mut s = String::from("# Fozmo vs external-product static filter timing\n\n");
    s.push_str("## Magnitude response (relative to normalized DC gain)\n\n| Filter | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz | Ripple 20 Hz-18 kHz | Ripple 20 Hz-20 kHz |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        let m = &row.magnitude;
        s.push_str(&format!(
            "| {} | {:.4} dB | {:.4} dB | {:.4} dB | {:.4} dB | {:.4} dB | {:.4} dB | {:.4} dB |\n",
            row.display_name,
            m.gain_5khz_db_dc,
            m.gain_10khz_db_dc,
            m.gain_15khz_db_dc,
            m.gain_18khz_db_dc,
            m.gain_20khz_db_dc,
            m.passband_ripple_20hz_18khz_db,
            m.passband_ripple_20hz_20khz_db,
        ));
    }
    s.push_str("\n## Cutoff, transition, and reconstruction images\n\n| Filter | 20.5 kHz | 21 kHz | 21.5 kHz | 22 kHz | 22.05 kHz | -0.1 dB edge | -3 dB edge | -6 dB edge | -0.1 to first -100 dB crossing | Stopband rejection 24.1-88.2 kHz | First-image rejection 24.1-64.1 kHz |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        let m = &row.magnitude;
        s.push_str(&format!(
            "| {} | {:.3} dB | {:.3} dB | {:.3} dB | {:.3} dB | {:.3} dB | {} | {} | {} | {} | {:.2} dB | {:.2} dB |\n",
            row.display_name,
            m.gain_20_5khz_db_dc,
            m.gain_21khz_db_dc,
            m.gain_21_5khz_db_dc,
            m.gain_22khz_db_dc,
            m.gain_22_05khz_db_dc,
            frequency(m.bandwidth_minus_0_1_db_hz),
            frequency(m.bandwidth_minus_3_db_hz),
            frequency(m.bandwidth_minus_6_db_hz),
            frequency(m.transition_minus_0_1_to_minus_100_db_hz),
            m.stopband_rejection_24_1khz_to_nyquist_db,
            m.first_image_rejection_24_1khz_to_64_1khz_db,
        ));
    }
    s.push('\n');
    s.push_str("## Impulse and step response\n\n");
    s.push_str("| Filter | Deterministic | Pre energy | Max pre lobe | Post energy | Max post lobe | Decay -80 | Decay -120 | Main lobe | Step over | Step under | Centroid |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        let m = &row.impulse;
        s.push_str(&format!("| {} | {} | {:.2} dB | {:.2} dB | {:.2} dB | {:.2} dB | {} | {} | {:.2} us | {:.3}% | {:.3}% | {:.4} ms |\n", row.display_name, row.deterministic, m.pre_peak_energy_db_total, m.maximum_pre_ringing_lobe_db_peak, m.post_peak_energy_db_total, m.maximum_post_ringing_lobe_db_peak, decay(m.decay_time_to_minus_80_db_ms, m.decay_minus_80_db_censored), decay(m.decay_time_to_minus_120_db_ms, m.decay_minus_120_db_censored), m.main_lobe_width_us, m.step_response_overshoot_percent, m.step_response_undershoot_percent, m.energy_centroid_relative_to_peak_ms));
    }
    s.push_str("\n## Tone packets: energy before/after envelope centroid (dB total)\n\n| Filter | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |\n| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        s.push_str(&format!("| {}", row.display_name));
        for p in &row.packets {
            s.push_str(&format!(
                " | {:.2} / {:.2}",
                p.pre_echo_energy_db_total, p.post_echo_energy_db_total
            ));
        }
        s.push_str(" |\n");
    }
    s.push_str("\n## Tone packets: maximum before/after-centroid lobe (dB peak)\n\n| Filter | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |\n| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        s.push_str(&format!("| {}", row.display_name));
        for p in &row.packets {
            s.push_str(&format!(
                " | {:.2} / {:.2}",
                p.maximum_pre_echo_db_peak, p.maximum_post_echo_db_peak
            ));
        }
        s.push_str(" |\n");
    }
    s.push_str("\n## Tone packets: onset-referenced pre-echo/post-decay energy (dB total)\n\n| Filter | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |\n| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        s.push_str(&format!("| {}", row.display_name));
        for p in &row.packets {
            s.push_str(&format!(
                " | {:.2} / {:.2}",
                p.onset_pre_echo_energy_db_total, p.onset_post_decay_energy_db_total
            ));
        }
        s.push_str(" |\n");
    }
    s.push_str("\n## Group delay relative to principal peak (ms)\n\n| Filter | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |\n| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for row in &report.results {
        s.push_str(&format!("| {}", row.display_name));
        for frequency in TONES {
            let value = row
                .group_delay
                .iter()
                .min_by(|a, b| {
                    (a.frequency_hz - frequency)
                        .abs()
                        .total_cmp(&(b.frequency_hz - frequency).abs())
                })
                .map(|point| point.group_delay_relative_to_peak_ms);
            match value {
                Some(value) => s.push_str(&format!(" | {value:.5}")),
                None => s.push_str(" | n/a"),
            }
        }
        s.push_str(" |\n");
    }
    s.push_str("\n## Controls and elapsed render time\n\n| Filter | Deterministic | Silence peak | Silence RMS | Batch elapsed |\n| --- | --- | ---: | ---: | ---: |\n");
    for row in &report.results {
        s.push_str(&format!(
            "| {} | {} | {:.2} dBFS | {:.2} dBFS | {:.3} s |\n",
            row.display_name,
            row.deterministic,
            row.silence_peak_dbfs,
            row.silence_rms_dbfs,
            row.total_render_seconds
        ));
    }
    s.push_str("\nAll comparison samples are signed PCM24. The external-product CLI reports mandatory one-LSB TPDF, but its rendered-silence controls were digital zero in this run. Silence controls were still subtracted; -120 dB decay is sensitive to the PCM24 quantization floor. Elapsed time is an offline batch diagnostic, not a controlled startup/throughput benchmark. The complete 20 Hz/100 Hz-spaced group-delay curves are in `report.json`.\n");
    s
}

fn frequency(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.2} kHz", value / 1000.0))
        .unwrap_or_else(|| "n/a".into())
}

fn decay(value: Option<f64>, censored: bool) -> String {
    match value {
        Some(v) if censored => format!(">{v:.2} ms"),
        Some(v) => format!("{v:.2} ms"),
        None => "0 ms".into(),
    }
}

fn quantize24(value: f64) -> f64 {
    ((value.clamp(-1.0, 1.0 - 1.0 / 8_388_608.0) * 8_388_608.0).round()) / 8_388_608.0
}

fn amp_db(value: f64) -> f64 {
    if value > 0.0 {
        20.0 * value.log10()
    } else {
        -300.0
    }
}

fn write_wav24(path: &Path, rate: u32, mono: &[f64]) -> io::Result<()> {
    let data_len = mono.len() * 2 * 3;
    let mut f = fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36u32 + data_len as u32).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 6).to_le_bytes())?;
    f.write_all(&6u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&(data_len as u32).to_le_bytes())?;
    for sample in mono {
        let n = (quantize24(*sample) * 8_388_608.0) as i32;
        let b = n.to_le_bytes();
        f.write_all(&b[..3])?;
        f.write_all(&b[..3])?;
    }
    Ok(())
}

fn read_wav24(path: &Path) -> io::Result<Vec<f64>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not RIFF/WAVE"));
    }
    let mut pos = 12;
    let mut channels = 0usize;
    let mut bits = 0u16;
    let mut format_tag = 0u16;
    let mut data = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        if pos + len > bytes.len() {
            break;
        }
        if id == b"fmt " {
            if len < 16 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short WAV fmt chunk",
                ));
            }
            format_tag = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
            channels = u16::from_le_bytes(bytes[pos + 2..pos + 4].try_into().unwrap()) as usize;
            bits = u16::from_le_bytes(bytes[pos + 14..pos + 16].try_into().unwrap());
        } else if id == b"data" {
            data = Some(&bytes[pos..pos + len]);
        }
        pos += len + (len & 1);
    }
    if format_tag != 1 || channels != 2 || bits != 24 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected stereo integer PCM24, got format {format_tag}, {channels}ch/{bits}bit"
            ),
        ));
    }
    let data = data.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing data"))?;
    Ok(data
        .chunks_exact(6)
        .map(|f| {
            let mut n = (f[0] as i32) | ((f[1] as i32) << 8) | ((f[2] as i32) << 16);
            if n & 0x800000 != 0 {
                n |= !0xffffff;
            }
            n as f64 / 8_388_608.0
        })
        .collect())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut h = Sha256::new();
    h.update(fs::read(path)?);
    Ok(format!("{:x}", h.finalize()))
}
