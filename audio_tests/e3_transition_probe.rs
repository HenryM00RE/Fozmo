#[path = "dsd_public/analysis.rs"]
mod analysis;
#[path = "dsd_public/signals.rs"]
mod signals;

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use fozmo::audio::dsd::delta_sigma::DsdModulator;
use fozmo::audio::dsd::dsd_render::{DsdRate, DsdRenderer, dsd_source_window_to_modulator_samples};
use fozmo::audio::dsp::resampler::{FilterType, install_research_e3_character_file};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "e3-filter-only-transition-probe-v1";
const CHUNK_SOURCE_FRAMES: usize = 1024;
const SPLIT_FILTER_GUARD_FRAMES: usize = 131_328;
const TRANSITION_TRACE_SECONDS: f64 = 0.050;
const TRANSITION_WINDOW_SECONDS: f64 = 0.002;

#[derive(Debug, Parser)]
#[command(
    name = "e3_transition_probe",
    about = "Research-only capture of the exact PCM stream entering a DSD128 modulator"
)]
struct Cli {
    #[arg(long, default_value = "SplitPhase128kE3")]
    filter: String,

    #[arg(long, default_value = "Standard")]
    modulator: String,

    #[arg(long)]
    experimental_character_file: Option<PathBuf>,

    #[arg(long)]
    experimental_character_sha256: Option<String>,

    #[arg(long)]
    reference: Option<PathBuf>,

    #[arg(long, default_value_t = 2.0e-9)]
    tolerance_rms: f64,

    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Clone)]
struct Capture {
    name: &'static str,
    range: Range<usize>,
    samples: Vec<f64>,
}

impl Capture {
    fn new(name: &'static str, range: Range<usize>) -> Self {
        Self {
            name,
            samples: Vec::with_capacity(range.len()),
            range,
        }
    }

    fn append_overlap(&mut self, block_start: usize, block: &[f64]) {
        let block_end = block_start + block.len();
        let start = self.range.start.max(block_start);
        let end = self.range.end.min(block_end);
        if start < end {
            self.samples
                .extend_from_slice(&block[start - block_start..end - block_start]);
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.samples.len() == self.range.len() {
            Ok(())
        } else {
            Err(format!(
                "{} captured {} samples, expected {} for {}..{}",
                self.name,
                self.samples.len(),
                self.range.len(),
                self.range.start,
                self.range.end
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReferenceReport {
    transition_envelope: analysis::TransitionEnvelopeMetrics,
}

#[derive(Debug, Serialize)]
struct SourceMetadata {
    fixture: &'static str,
    source_rate_hz: u32,
    wire_rate_hz: u32,
    source_frames: usize,
    chunk_source_frames: usize,
    headroom_db: f64,
    effective_peak: f64,
    carrier_frequencies_hz: Vec<f64>,
    source_pcm_sha256: String,
    restart_wire_sample: usize,
    recovered_fit_wire_range: [usize; 2],
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: &'static str,
    filter: &'static str,
    modulator: &'static str,
    character_file: Option<String>,
    character_sha256: Option<String>,
    source: SourceMetadata,
    transition_envelope: analysis::TransitionEnvelopeMetrics,
    transition_envelope_excess_vs_reference: Option<analysis::TransitionEnvelopeExcessMetrics>,
    residual_peak_dbfs: f64,
    residual_rms_1ms_dbfs: f64,
    residual_rms_10ms_dbfs: f64,
    residual_rms_50ms_dbfs: f64,
    rendered_wire_samples: usize,
    elapsed_seconds: f64,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("e3_transition_probe: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    if !cli.tolerance_rms.is_finite() || cli.tolerance_rms < 0.0 {
        return Err("--tolerance-rms must be finite and non-negative".into());
    }
    let filter = FilterType::from_name(&cli.filter)
        .ok_or_else(|| format!("unknown filter {}", cli.filter))?;
    if !matches!(
        filter,
        FilterType::SplitPhase128kE2v3 | FilterType::SplitPhase128kE3
    ) {
        return Err("the focused probe accepts only SplitPhase128kE2v3 or SplitPhase128kE3".into());
    }
    let modulator = DsdModulator::from_name(&cli.modulator)
        .ok_or_else(|| format!("unknown modulator {}", cli.modulator))?;
    if !matches!(modulator, DsdModulator::Standard | DsdModulator::EcBeam2) {
        return Err("the focused probe accepts only Standard or EcBeam2".into());
    }
    let character = match (
        cli.experimental_character_file.as_deref(),
        cli.experimental_character_sha256.as_deref(),
    ) {
        (None, None) => None,
        (Some(path), Some(hash)) => {
            if filter != FilterType::SplitPhase128kE3 {
                return Err("research character files require SplitPhase128kE3".into());
            }
            let metadata = install_research_e3_character_file(path, hash)?;
            Some((path.to_path_buf(), metadata.sha256))
        }
        _ => {
            return Err(
                "--experimental-character-file and --experimental-character-sha256 are required together"
                    .into(),
            );
        }
    };

    let headroom_db = match modulator {
        DsdModulator::Standard => -4.0,
        DsdModulator::EcBeam2 => -2.0,
        _ => unreachable!(),
    };
    let signal =
        signals::high_frequency_stress_level_matched(headroom_db, SPLIT_FILTER_GUARD_FRAMES)
            .map_err(|error| error.to_string())?;
    signal.validate().map_err(|error| error.to_string())?;
    let recovery = signal
        .range(signals::STRESS_LEVEL_MATCHED_RECOVERY_RANGE)
        .ok_or_else(|| "stress fixture is missing its recovery range".to_string())?;
    let wire_rate = DsdRate::Dsd128
        .wire_rate_for_source(signal.sample_rate_hz)
        .ok_or_else(|| "DSD128 rejected the stress source rate".to_string())?;
    let ratio = usize::try_from(wire_rate / signal.sample_rate_hz)
        .map_err(|_| "source/wire ratio does not fit usize")?;
    let restart_wire = dsd_source_window_to_modulator_samples(
        filter,
        signal.sample_rate_hz,
        wire_rate,
        recovery.start,
        1,
    )
    .ok_or_else(|| "could not map recovery start into the wire domain".to_string())?
    .start;
    let trace_samples = (wire_rate as f64 * TRANSITION_TRACE_SECONDS).round() as usize;
    let window_samples = (wire_rate as f64 * TRANSITION_WINDOW_SECONDS).round() as usize;
    let restart_required = trace_samples + window_samples - 1;
    let fit_source_start = recovery.end - signals::STRESS_STEADY_ANALYZE_FRAMES;
    let fit_wire = dsd_source_window_to_modulator_samples(
        filter,
        signal.sample_rate_hz,
        wire_rate,
        fit_source_start,
        signals::STRESS_STEADY_ANALYZE_FRAMES,
    )
    .ok_or_else(|| "could not map recovered fit interval into the wire domain".to_string())?;

    let mut restart_capture =
        Capture::new("restart", restart_wire..restart_wire + restart_required);
    let mut fit_capture = Capture::new("recovered fit", fit_wire.clone());
    let mut renderer = DsdRenderer::new_with_dsd_modulator(
        filter,
        signal.sample_rate_hz,
        DsdRate::Dsd128,
        modulator,
    )
    .map_err(str::to_string)?;
    let mut normalized_left = Vec::new();
    let mut normalized_right = Vec::new();
    let mut output_cursor = 0usize;
    let started = Instant::now();
    for start in (0..signal.frames()).step_by(CHUNK_SOURCE_FRAMES) {
        let end = (start + CHUNK_SOURCE_FRAMES).min(signal.frames());
        renderer.upsample(&signal.left[start..end], &signal.right[start..end]);
        renderer.research_normalized_modulator_input_block(
            signal.headroom_gain,
            &mut normalized_left,
            &mut normalized_right,
        );
        append_captures(
            output_cursor,
            &normalized_left,
            &mut restart_capture,
            &mut fit_capture,
        );
        output_cursor += normalized_left.len();
    }
    renderer.drain_resampler_eof();
    renderer.research_normalized_modulator_input_block(
        signal.headroom_gain,
        &mut normalized_left,
        &mut normalized_right,
    );
    append_captures(
        output_cursor,
        &normalized_left,
        &mut restart_capture,
        &mut fit_capture,
    );
    output_cursor += normalized_left.len();
    let elapsed_seconds = started.elapsed().as_secs_f64();
    let expected_wire_samples = signal.frames() * ratio;
    if output_cursor != expected_wire_samples {
        return Err(format!(
            "resampler emitted {output_cursor} wire samples, expected {expected_wire_samples}"
        ));
    }
    restart_capture.validate()?;
    fit_capture.validate()?;

    let frequencies = signal
        .carriers
        .iter()
        .map(|carrier| carrier.actual_hz)
        .collect::<Vec<_>>();
    let fit_model = analysis::fit_tone_model(&fit_capture.samples, wire_rate, &frequencies)?;
    let fit_offset = isize::try_from(restart_capture.range.start)
        .and_then(|restart| isize::try_from(fit_capture.range.start).map(|fit| restart - fit))
        .map_err(|_| "fit/restart offset does not fit isize")?;
    let residual =
        analysis::residual_against_tone_model(&restart_capture.samples, &fit_model, fit_offset)?;
    let transition_envelope = analysis::analyze_transition_envelope(&residual, wire_rate)?;
    let reference = cli.reference.as_deref().map(load_reference).transpose()?;
    let excess = reference
        .as_ref()
        .map(|reference| {
            analysis::compare_transition_envelopes(
                &transition_envelope,
                &reference.transition_envelope,
                cli.tolerance_rms,
            )
        })
        .transpose()?;

    let report = Report {
        schema_version: SCHEMA_VERSION,
        filter: filter.as_name(),
        modulator: modulator.as_name(),
        character_file: character
            .as_ref()
            .map(|(path, _)| path.display().to_string()),
        character_sha256: character.as_ref().map(|(_, hash)| hash.clone()),
        source: SourceMetadata {
            fixture: signal.id,
            source_rate_hz: signal.sample_rate_hz,
            wire_rate_hz: wire_rate,
            source_frames: signal.frames(),
            chunk_source_frames: CHUNK_SOURCE_FRAMES,
            headroom_db,
            effective_peak: signal.effective_peak(),
            carrier_frequencies_hz: frequencies,
            source_pcm_sha256: source_pcm_sha256(&signal),
            restart_wire_sample: restart_wire,
            recovered_fit_wire_range: [fit_wire.start, fit_wire.end],
        },
        transition_envelope,
        transition_envelope_excess_vs_reference: excess,
        residual_peak_dbfs: analysis::peak_sine_dbfs(analysis::max_abs(&residual)),
        residual_rms_1ms_dbfs: prefix_rms_dbfs(&residual, wire_rate, 0.001)?,
        residual_rms_10ms_dbfs: prefix_rms_dbfs(&residual, wire_rate, 0.010)?,
        residual_rms_50ms_dbfs: prefix_rms_dbfs(&residual, wire_rate, 0.050)?,
        rendered_wire_samples: output_cursor,
        elapsed_seconds,
    };
    if let Some(parent) = cli.out.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        &cli.out,
        serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    println!(
        "{} {}: 0-2 ms {:.6} dBFS, 2-5 ms {:.6} dBFS, elapsed {:.3} s",
        report.filter,
        report.modulator,
        report.transition_envelope.intervals[0].residual_rms_dbfs,
        report.transition_envelope.intervals[1].residual_rms_dbfs,
        report.elapsed_seconds,
    );
    Ok(())
}

fn append_captures(block_start: usize, samples: &[f64], restart: &mut Capture, fit: &mut Capture) {
    restart.append_overlap(block_start, samples);
    fit.append_overlap(block_start, samples);
}

fn load_reference(path: &Path) -> Result<ReferenceReport, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read reference {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse reference {}: {error}", path.display()))
}

fn prefix_rms_dbfs(residual: &[f64], sample_rate: u32, seconds: f64) -> Result<f64, String> {
    let frames = (sample_rate as f64 * seconds).round().max(1.0) as usize;
    if residual.len() < frames {
        return Err("restart residual is shorter than a requested prefix".into());
    }
    let mean_square = residual[..frames]
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>()
        / frames as f64;
    Ok(analysis::rms_dbfs_full_scale_sine(mean_square.sqrt()))
}

fn source_pcm_sha256(signal: &signals::StereoSignal) -> String {
    let mut digest = Sha256::new();
    for samples in [&signal.left, &signal.right] {
        for sample in samples {
            digest.update(sample.to_le_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}
