#[path = "dsd_public/analysis.rs"]
mod analysis;
#[path = "dsd_public/signals.rs"]
mod signals;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use analysis::{
    AUDIO_BAND, CarrierMetric, DeclaredToneMetrics, DensityMetrics, HIRES_BAND, MultiBandMetrics,
    NoiseMetrics, ReconstructionProfile, ToneMetrics,
};
use clap::Parser;
use fozmo::audio::dsd::delta_sigma::DsdModulator;
use fozmo::audio::dsd::dsd_render::{
    DSD_PRODUCTION_POLICY_VERSION, DsdRate, DsdRenderer, dsd_source_window_to_modulator_samples,
};
use fozmo::audio::dsd::native_dsd::NativeDsdOrder;
use fozmo::audio::dsp::resampler::{DEFAULT_FILTER_TYPE, FilterType};
use serde::Serialize;
use sha2::{Digest, Sha256};

const REPORT_SCHEMA_VERSION: &str = "dsd-public-quality-report-v4";
const MEASUREMENT_VERSION: &str = "dsd-public-quality-v4";
const MATRIX_VERSION: &str = "dsd-public-matrix-28-v6";
const SCORE_VERSION: &str = "dsd-public-production-score-v3";
const SCORE_CLAIM: &str = "Fozmo PCM-to-DSD production-path score using Split Phase E2v3";
const CANONICAL_PRODUCTION_CELL_COUNT: usize = 28;
const CHUNK_FRAMES: usize = 1024;
const SPECTRAL_ANALYSIS_FRAMES: usize = 65_536;
const CANONICAL_CARGO_FEATURES: &str =
    "airplay-helper,default,experimental-dsd256,hegel,local-library,pcm-output,qobuz,sonos,upnp";
const PRODUCTION_HEADROOM_DB: f64 = -4.0;
const SEARCH_HEADROOM_DB: f64 = -2.0;
const DENSITY_LIMIT: f64 = 0.010;
const RECONSTRUCTED_PEAK_LIMIT: f64 = 1.05;
const ROLLING_DENSITY_WINDOW_SECONDS: f64 = 0.020;
const SCORE_ANCHOR_LEVEL_DSD64: f64 = 117.395_283_345_419_12;
const SCORE_ANCHOR_IDLE_DSD64: f64 = 135.476_162_644_196_82;
const SCORE_ANCHOR_LEVEL_DSD128: f64 = 153.636_545_065_436_45;
const SCORE_ANCHOR_STRESS_DSD128: f64 = 174.825_549_056_608_54;
const SCORE_ANCHOR_TRANSITION_DSD128: f64 = 53.823_208_105_431_38;
const SCORE_ANCHOR_LEVEL_DSD256: f64 = 159.273_577_938_486_65;
const SCORE_ANCHOR_HIRES_DSD256: f64 = 151.116_117_271_164_46;
/// 32,769-tap first-stage half-support plus the later 2x cleanup stages.
const LINEAR_FILTER_GUARD_FRAMES: usize = 16_512;
/// Full 131,073-tap production Split Phase support plus cleanup margin.
const SPLIT_FILTER_GUARD_FRAMES: usize = 131_328;

#[derive(Debug, Parser)]
#[command(
    name = "dsd_public_quality",
    about = "Fixed public PCM-to-DSD quality measurement bench"
)]
struct Cli {
    /// Directory for dsd-public-quality.json and dsd-public-quality.md.
    #[arg(long, default_value = "target/dsd-public-quality")]
    out: PathBuf,

    /// Comma-separated production modulators.
    #[arg(long, default_value = "Standard,EcDepth2,EcBeam,EcBeam2")]
    modulator: String,

    /// Reconstruction filter under test. Non-default filters are noncanonical and unscored.
    #[arg(long, default_value = "SplitPhase128kE2v3")]
    filter: String,

    /// Comma-separated DSD rates to exercise: 64, 128, and/or 256.
    #[arg(long, default_value = "64,128,256")]
    rates: String,

    /// Add the non-scoring SincExtreme32k Linear Phase diagnostic matrix.
    #[arg(long)]
    include_linear_reference: bool,

    /// Add non-scoring DSD128 hi-res cells for a matched DSD128/DSD256 comparison.
    #[arg(long)]
    include_rate_comparison: bool,

    /// Run only DSD256 cells.
    #[arg(long)]
    dsd256_only: bool,

    /// Return a non-zero status when any structural hard gate fails.
    #[arg(long)]
    check: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    LevelSweep,
    IdleTinySignal,
    HighFrequencyRatedStress,
    HighFrequencyMatchedStress,
    HiresReconstruction,
}

impl Scenario {
    fn as_name(self) -> &'static str {
        match self {
            Self::LevelSweep => "coherent_level_sweep",
            Self::IdleTinySignal => "idle_tiny_signal",
            Self::HighFrequencyRatedStress => "high_frequency_rated_stress",
            Self::HighFrequencyMatchedStress => "high_frequency_matched_stress",
            Self::HiresReconstruction => "hires_reconstruction",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CellSpec {
    scenario: Scenario,
    source_rate: u32,
    dsd_rate: DsdRate,
    modulator: DsdModulator,
    filter: FilterType,
    diagnostic: bool,
}

#[derive(Debug, Serialize)]
struct BenchReport {
    schema_version: &'static str,
    measurement_version: &'static str,
    matrix_version: &'static str,
    reconstruction_algorithm_version: &'static str,
    quality_policy: &'static str,
    dbfs_reference: &'static str,
    sinad_definition: &'static str,
    unknown_spur_definition: &'static str,
    transition_definition: &'static str,
    rolling_density_window_seconds: f64,
    density_hard_gate_scope: &'static str,
    stress_clean_mute_source_frames: usize,
    spectral_policy: SpectralPolicy,
    check_requested: bool,
    matrix_complete: bool,
    provenance: Provenance,
    canonical_production_cell_count: usize,
    attempted_production_cell_count: usize,
    successful_production_cell_count: usize,
    failed_production_cell_count: usize,
    attempted_diagnostic_cell_count: usize,
    successful_diagnostic_cell_count: usize,
    failed_diagnostic_cell_count: usize,
    filter_policies: Vec<FilterPolicy>,
    reconstruction_profiles: Vec<ReconstructionProfile>,
    rated_headroom: Vec<HeadroomPolicy>,
    selected_modulators: Vec<String>,
    selected_filters: Vec<String>,
    include_linear_reference: bool,
    dsd256_only: bool,
    score_eligible: bool,
    score_policy: ScorePolicy,
    production_path_scores: Vec<ProductionPathScore>,
    cells: Vec<CellReport>,
    execution_failures: Vec<String>,
    /// Structural failures from production cells; this is what `--check` gates.
    hard_failure_count: usize,
    /// Structural failures from optional Linear Phase diagnostic cells.
    diagnostic_hard_failure_count: usize,
}

#[derive(Debug, Serialize)]
struct Provenance {
    git_commit: Option<String>,
    working_tree_dirty: Option<bool>,
    source_snapshot_sha256: Option<String>,
    rustc_version: Option<String>,
    target_os: &'static str,
    target_arch: &'static str,
    cpu_class: Option<String>,
    launch_rustflags: Option<String>,
    build_provenance_schema: &'static str,
    build_profile: &'static str,
    build_opt_level: &'static str,
    build_debug_assertions: bool,
    build_target: &'static str,
    build_host: &'static str,
    build_target_cpu: &'static str,
    build_native_cpu_requested: bool,
    build_rustc_version: &'static str,
    build_rustflags: &'static str,
    build_encoded_rustflags_hex: &'static str,
    build_target_features: &'static str,
    build_target_features_hex: &'static str,
    build_cargo_features: &'static str,
    build_git_commit: &'static str,
    build_git_dirty: Option<bool>,
    build_source_snapshot_schema: &'static str,
    build_source_snapshot_sha256: &'static str,
    runtime_source_matches_build: bool,
    executable_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct FilterPolicy {
    name: String,
    role: &'static str,
    production_default: bool,
}

#[derive(Debug, Serialize)]
struct HeadroomPolicy {
    modulator: String,
    headroom_db: f64,
}

#[derive(Debug, Serialize)]
struct SpectralPolicy {
    analysis_frames: usize,
    coherent_line_and_residual_window: &'static str,
    coherent_line_integration_half_width_bins: usize,
    unexpected_spur_window: &'static str,
    unexpected_spur_integration_half_width_bins: usize,
    unexpected_spur_window_nominal_enbw_bins: f64,
}

#[derive(Debug, Serialize)]
struct ScorePolicy {
    name: &'static str,
    claim: &'static str,
    canonical_filter: &'static str,
    normalization: &'static str,
    anchor_basis: &'static str,
    eligibility: &'static str,
    rated_stress_role: &'static str,
    anchors: Vec<ScoreAnchorPolicy>,
    categories: Vec<ScoreCategoryPolicy>,
}

#[derive(Debug, Serialize)]
struct ScoreAnchorPolicy {
    rate: &'static str,
    category: &'static str,
    quality_index_anchor_db: f64,
}

#[derive(Debug, Serialize)]
struct ScoreCategoryPolicy {
    rate: &'static str,
    category: &'static str,
    maximum_points: f64,
    metric_formula: &'static str,
}

#[derive(Debug, Serialize)]
struct ProductionPathScore {
    modulator: String,
    filter: String,
    rated_stress_qualified: bool,
    rates: Vec<RateScore>,
}

#[derive(Debug, Serialize)]
struct RateScore {
    rate: &'static str,
    total_points: f64,
    maximum_points: f64,
    categories: Vec<CategoryScore>,
}

#[derive(Debug, Serialize)]
struct CategoryScore {
    category: &'static str,
    maximum_points: f64,
    quality_index_db: f64,
    quality_index_anchor_db: f64,
    normalized_score: f64,
    awarded_points: f64,
}

#[derive(Debug, Serialize)]
struct CellReport {
    scenario: String,
    modulator: String,
    diagnostic: bool,
    comparison_class: &'static str,
    level_matched_across_modulators: bool,
    production_default_filter: bool,
    filter: String,
    headroom_db: f64,
    source_rate: u32,
    dsd_rate: String,
    wire_rate: u32,
    source_frames: usize,
    filter_guard_source_frames: usize,
    source_peak: f64,
    effective_source_peak: f64,
    modulator_input_peak: f64,
    expected_bits_per_channel: usize,
    native_bytes_before_idle: [usize; 2],
    native_bytes_after_idle: [usize; 2],
    full_fixture_density: [WholeDensityMetrics; 2],
    reconstruction_profile: String,
    spectral_analysis_frames: usize,
    spectral_bin_width_hz: f64,
    render_seconds: f64,
    renderer_configuration: RendererConfiguration,
    source_pcm_sha256: String,
    native_dsd_sha256: [String; 2],
    health: HealthReport,
    measurements: ScenarioMeasurements,
    structural_summary: StructuralSummary,
    hard_failures: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RendererConfiguration {
    production_policy_version: &'static str,
    coefficient_table: String,
    coefficient_osr: u32,
    coefficient_obg: f64,
    coefficient_input_peak: f64,
    lookahead_depth: usize,
    isi_penalty: f64,
    chunk_source_frames: usize,
    seeds_hex: [String; 2],
}

#[derive(Debug, Clone, Copy, Serialize)]
struct WholeDensityMetrics {
    bits: usize,
    density: f64,
    deviation: f64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScenarioMeasurements {
    LevelSweep {
        segments: Vec<LevelSegmentReport>,
    },
    IdleTinySignal {
        sections: Vec<IdleSectionReport>,
    },
    HighFrequencyStress {
        level_contract: &'static str,
        channels: Vec<StressChannelReport>,
    },
    HiresReconstruction {
        channels: Vec<HiresChannelReport>,
    },
}

#[derive(Debug, Serialize)]
struct LevelSegmentReport {
    name: String,
    source_level_dbfs: f64,
    effective_level_dbfs: f64,
    actual_frequency_hz: f64,
    channels: Vec<ChannelToneReport>,
}

#[derive(Debug, Serialize)]
struct ChannelToneReport {
    channel: &'static str,
    metrics: ToneMetrics,
    density: DensityMetrics,
    reconstructed_peak: f64,
}

#[derive(Debug, Serialize)]
struct IdleSectionReport {
    name: String,
    actual_tone_hz: Option<f64>,
    channel_spread: IdleChannelSpread,
    channels: Vec<IdleChannelReport>,
}

#[derive(Debug, Serialize)]
struct IdleChannelSpread {
    integrated_noise_spread_db: f64,
    worst_spur_spread_db: f64,
    reconstructed_dc_magnitude_spread: f64,
    bit_density_deviation_spread: f64,
    tone_gain_error_spread_db: Option<f64>,
}

#[derive(Debug, Serialize)]
struct IdleChannelReport {
    channel: &'static str,
    noise: NoiseMetrics,
    reconstructed_dc: f64,
    expected_dc: Option<f64>,
    dc_error: Option<f64>,
    dc_polarity_correct: Option<bool>,
    tone_recovery: Option<CarrierMetric>,
    density: DensityMetrics,
    reconstructed_peak: f64,
}

#[derive(Debug, Serialize)]
struct StressChannelReport {
    channel: &'static str,
    steady: DeclaredToneMetrics,
    recovery: DeclaredToneMetrics,
    settled_program_peak: f64,
    transition_waveform_peak: f64,
    transition_overshoot_above_settled: f64,
    zero_input_transition_peak: f64,
    zero_input_transition_peak_dbfs: f64,
    clean_mute_peak: f64,
    clean_mute_peak_dbfs: f64,
    clean_mute_rms_dbfs: f64,
    restart_residual_peak: f64,
    restart_residual_peak_dbfs: f64,
    restart_residual_rms_1ms_dbfs: f64,
    restart_residual_rms_10ms_dbfs: f64,
    restart_residual_rms_50ms_dbfs: f64,
    transition_residual_peak: f64,
    transition_residual_peak_dbfs: f64,
    end_to_end_recovery_time_ms: Option<f64>,
    steady_density_analysis_range: String,
    steady_density: DensityMetrics,
    clean_mute_density_analysis_range: String,
    clean_mute_density: DensityMetrics,
    reconstructed_peak: f64,
}

struct StressTransitionAnalysis {
    settled_program_peak: f64,
    transition_waveform_peak: f64,
    transition_overshoot_above_settled: f64,
    zero_input_transition_peak: f64,
    clean_mute_peak: f64,
    clean_mute_rms_dbfs: f64,
    restart_residual_peak: f64,
    restart_residual_rms_1ms_dbfs: f64,
    restart_residual_rms_10ms_dbfs: f64,
    restart_residual_rms_50ms_dbfs: f64,
    transition_residual_peak: f64,
}

#[derive(Debug, Serialize)]
struct HiresChannelReport {
    channel: &'static str,
    metrics: MultiBandMetrics,
    density: DensityMetrics,
    reconstructed_peak: f64,
}

#[derive(Debug, Default, Serialize)]
struct StructuralSummary {
    observed_max_density_deviation: f64,
    observed_max_reconstructed_peak: f64,
    health_pass: bool,
}

#[derive(Debug, Default, Serialize)]
struct HealthReport {
    stability_resets: u64,
    state_clamps: u64,
    limiter_peak_ratio_max: f64,
    limiter_limited_events: u64,
    limiter_limited_samples: u64,
    truncation_events: u64,
    discarded_left_bits: u64,
    discarded_right_bits: u64,
    beam_clamps: u64,
    beam_speculative_clamps: u64,
    beam_committed_clamps: u64,
    beam_rejected_hard_limits: u64,
    beam_all_children_rejected: u64,
    ecbeam2_constraint_escapes: u64,
    ecbeam2_state_repairs: u64,
    ecbeam2_nonfinite_resets: u64,
    ecbeam2_observer_desynchronizations: u64,
    ecbeam2_invalid_input_substitutions: u64,
    ecbeam2_output_length_events: u64,
}

struct RenderedCell {
    left: Vec<u8>,
    right: Vec<u8>,
    native_bytes_before_idle: [usize; 2],
    native_bytes_after_idle: [usize; 2],
    expected_bits: usize,
    wire_rate: u32,
    filter: FilterType,
    modulator_input_peak: f64,
    render_seconds: f64,
    renderer_configuration: RendererConfiguration,
    native_dsd_sha256: [String; 2],
    full_fixture_density: [WholeDensityMetrics; 2],
    health: HealthReport,
    hard_failures: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok((report, output_paths)) => {
            println!("wrote {}", output_paths.0.display());
            println!("wrote {}", output_paths.1.display());
            println!(
                "completed {} cells with {} canonical and {} diagnostic structural hard failure(s)",
                report.cells.len(),
                report.hard_failure_count,
                report.diagnostic_hard_failure_count
            );
            if report.check_requested && (!report.matrix_complete || report.hard_failure_count > 0)
            {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("dsd_public_quality: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<(BenchReport, (PathBuf, PathBuf)), String> {
    require_canonical_build()?;
    reject_filter_overrides()?;
    let selected = parse_modulators(&cli.modulator)?;
    let selected_filter = parse_filter(&cli.filter)?;
    let selected_rates = parse_rates(&cli.rates)?;
    let mut matrix = build_matrix(
        &selected,
        cli.include_linear_reference,
        selected_filter,
        &selected_rates,
    );
    if cli.include_rate_comparison {
        append_rate_comparison_diagnostics(&mut matrix, &selected, selected_filter);
    }
    if cli.dsd256_only {
        matrix.retain(|cell| cell.dsd_rate == DsdRate::Dsd256);
    }
    let mut selected_filters = Vec::new();
    for cell in &matrix {
        let filter = cell.filter.as_name().to_string();
        if !selected_filters.contains(&filter) {
            selected_filters.push(filter);
        }
    }
    let attempted_production_cell_count = matrix.iter().filter(|cell| !cell.diagnostic).count();
    let attempted_diagnostic_cell_count = matrix.iter().filter(|cell| cell.diagnostic).count();
    let mut cells = Vec::with_capacity(matrix.len());
    let mut execution_failures = Vec::new();
    let mut production_execution_failure_count = 0;
    let mut diagnostic_execution_failure_count = 0;
    for (index, spec) in matrix.iter().copied().enumerate() {
        eprintln!(
            "[{}/{}] {} {} {} {}",
            index + 1,
            matrix.len(),
            spec.scenario.as_name(),
            spec.filter.as_name(),
            dsd_rate_name(spec.dsd_rate),
            spec.modulator.as_name()
        );
        match run_cell(spec) {
            Ok(cell) => cells.push(cell),
            Err(error) => {
                if spec.diagnostic {
                    diagnostic_execution_failure_count += 1;
                } else {
                    production_execution_failure_count += 1;
                }
                execution_failures.push(format!(
                    "{}{} {} {} {}: {error}",
                    if spec.diagnostic { "diagnostic " } else { "" },
                    spec.scenario.as_name(),
                    spec.filter.as_name(),
                    dsd_rate_name(spec.dsd_rate),
                    spec.modulator.as_name()
                ));
            }
        }
    }

    let successful_production_cell_count = cells.iter().filter(|cell| !cell.diagnostic).count();
    let successful_diagnostic_cell_count = cells.iter().filter(|cell| cell.diagnostic).count();
    let failed_production_cell_count = production_execution_failure_count;
    let failed_diagnostic_cell_count = diagnostic_execution_failure_count;
    let matrix_complete = selected_filter == DEFAULT_FILTER_TYPE
        && canonical_selection(&selected)
        && attempted_production_cell_count == CANONICAL_PRODUCTION_CELL_COUNT
        && successful_production_cell_count == CANONICAL_PRODUCTION_CELL_COUNT
        && failed_production_cell_count == 0;
    let (hard_failure_count, diagnostic_hard_failure_count) = structural_failure_counts(
        cells
            .iter()
            .map(|cell| (cell.diagnostic, cell.hard_failures.len())),
        production_execution_failure_count,
        diagnostic_execution_failure_count,
    );
    let score_eligible = matrix_complete && hard_failure_count == 0;
    let production_path_scores = if score_eligible {
        score_production_path(&cells)?
    } else {
        Vec::new()
    };
    let report = BenchReport {
        schema_version: REPORT_SCHEMA_VERSION,
        measurement_version: MEASUREMENT_VERSION,
        matrix_version: MATRIX_VERSION,
        reconstruction_algorithm_version: analysis::RECONSTRUCTION_ALGORITHM_VERSION,
        quality_policy: "the versioned production-path scores are comparative presentation only; --check requires the complete canonical Split Phase E2v3 matrix and enforces whole-render health plus scoped structural gates",
        dbfs_reference: "full-scale-sine: peak amplitude 1.0 and RMS 1/sqrt(2) are both 0 dBFS",
        sinad_definition: "declared carrier power divided by every other in-band component, including declared distortion products",
        unknown_spur_definition: "Blackman-Harris-4 integrated main-lobe power after joint least-squares removal of declared carriers/products and DC; lines unresolved from a declared frequency are absorbed by that fit",
        transition_definition: "zero-input transition, clean-center mute, and recovered-carrier-model restart residuals are separate; fixed-window RMS, waveform peak, and excess above settled program peak are also reported",
        rolling_density_window_seconds: ROLLING_DENSITY_WINDOW_SECONDS,
        density_hard_gate_scope: "whole fixture plus idle silence, stress clean-mute, and declared DC sections; analyzed AC-section density remains diagnostic",
        stress_clean_mute_source_frames: signals::STRESS_MUTE_FRAMES,
        spectral_policy: SpectralPolicy {
            analysis_frames: SPECTRAL_ANALYSIS_FRAMES,
            coherent_line_and_residual_window: "rectangular; fixtures are exact-bin coherent",
            coherent_line_integration_half_width_bins: 0,
            unexpected_spur_window: "declared-tone/DC residual with four-term Blackman-Harris",
            unexpected_spur_integration_half_width_bins: 6,
            unexpected_spur_window_nominal_enbw_bins: 2.0044,
        },
        check_requested: cli.check,
        matrix_complete,
        provenance: provenance(),
        canonical_production_cell_count: CANONICAL_PRODUCTION_CELL_COUNT,
        attempted_production_cell_count,
        successful_production_cell_count,
        failed_production_cell_count,
        attempted_diagnostic_cell_count,
        successful_diagnostic_cell_count,
        failed_diagnostic_cell_count,
        filter_policies: vec![
            FilterPolicy {
                name: FilterType::Split128k.as_name().to_string(),
                role: "retired_split_phase_reference",
                production_default: false,
            },
            FilterPolicy {
                name: FilterType::SincExtreme32k.as_name().to_string(),
                role: "linear_phase_reference",
                production_default: false,
            },
            FilterPolicy {
                name: FilterType::SmoothPhase128k.as_name().to_string(),
                role: "smooth_phase_production_option",
                production_default: false,
            },
            FilterPolicy {
                name: FilterType::SplitPhase128kE2v3.as_name().to_string(),
                role: "production_default_split_phase",
                production_default: true,
            },
        ],
        reconstruction_profiles: vec![AUDIO_BAND, HIRES_BAND],
        rated_headroom: [
            DsdModulator::Standard,
            DsdModulator::EcDepth2,
            DsdModulator::EcBeam,
            DsdModulator::EcBeam2,
        ]
        .into_iter()
        .map(|modulator| HeadroomPolicy {
            modulator: modulator.as_name().to_string(),
            headroom_db: headroom_db(modulator),
        })
        .collect(),
        selected_modulators: selected
            .iter()
            .map(|modulator| modulator.as_name().to_string())
            .collect(),
        selected_filters,
        include_linear_reference: cli.include_linear_reference,
        dsd256_only: cli.dsd256_only,
        score_eligible,
        score_policy: score_policy(),
        production_path_scores,
        cells,
        execution_failures,
        hard_failure_count,
        diagnostic_hard_failure_count,
    };
    let paths = write_artifacts(&report, &cli.out)?;
    Ok((report, paths))
}

fn structural_failure_counts(
    cell_failures: impl IntoIterator<Item = (bool, usize)>,
    production_execution_failures: usize,
    diagnostic_execution_failures: usize,
) -> (usize, usize) {
    cell_failures.into_iter().fold(
        (production_execution_failures, diagnostic_execution_failures),
        |(production, diagnostic), (is_diagnostic, failures)| {
            if is_diagnostic {
                (production, diagnostic + failures)
            } else {
                (production + failures, diagnostic)
            }
        },
    )
}

fn require_canonical_build() -> Result<(), String> {
    let runtime_source = source_snapshot_sha256()
        .ok_or_else(|| "could not hash the runtime source snapshot".to_string())?;
    let mut failures = Vec::new();
    if env!("FOZMO_BUILD_PROFILE") != "release" {
        failures.push(format!(
            "build profile was {}, expected release",
            env!("FOZMO_BUILD_PROFILE")
        ));
    }
    if env!("FOZMO_BUILD_OPT_LEVEL") != "3" {
        failures.push(format!(
            "build optimization level was {}, expected 3",
            env!("FOZMO_BUILD_OPT_LEVEL")
        ));
    }
    if env!("FOZMO_BUILD_DEBUG_ASSERTIONS") != "false" {
        failures.push("debug assertions were enabled".to_string());
    }
    if env!("FOZMO_BUILD_TARGET_CPU") != "native"
        || env!("FOZMO_BUILD_NATIVE_CPU_REQUESTED") != "true"
    {
        failures.push(format!(
            "embedded target CPU was {}, expected native",
            env!("FOZMO_BUILD_TARGET_CPU")
        ));
    }
    if rustflags_disable_target_features(env!("FOZMO_BUILD_RUSTFLAGS_DISPLAY")) {
        failures.push(format!(
            "build RUSTFLAGS explicitly disabled target features: {}",
            env!("FOZMO_BUILD_RUSTFLAGS_DISPLAY")
        ));
    }
    if !env!("FOZMO_BUILD_CARGO_FEATURES").eq(CANONICAL_CARGO_FEATURES) {
        failures.push(format!(
            "build Cargo features were {}, expected {}",
            env!("FOZMO_BUILD_CARGO_FEATURES"),
            CANONICAL_CARGO_FEATURES
        ));
    }
    if env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256") == "unavailable" {
        failures.push("build-time source snapshot was unavailable".to_string());
    } else if env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256") != runtime_source {
        failures.push(format!(
            "binary/source mismatch (built {}, runtime {})",
            env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256"),
            runtime_source
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "canonical bench build requirements failed: {}. Rebuild and run with RUSTFLAGS=\"-C target-cpu=native\" cargo run --locked --release --bin dsd_public_quality --",
            failures.join("; ")
        ))
    }
}

fn rustflags_disable_target_features(flags: &str) -> bool {
    flags.split_whitespace().any(|argument| {
        argument
            .split_once("target-feature=")
            .is_some_and(|(_, features)| {
                features.split(',').any(|feature| feature.starts_with('-'))
            })
    })
}

fn canonical_selection(modulators: &[DsdModulator]) -> bool {
    let production = [
        DsdModulator::Standard,
        DsdModulator::EcDepth2,
        DsdModulator::EcBeam,
        DsdModulator::EcBeam2,
    ];
    production
        .iter()
        .all(|modulator| modulators.contains(modulator))
}

fn parse_modulators(value: &str) -> Result<Vec<DsdModulator>, String> {
    let mut selected = Vec::new();
    for name in value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let modulator = match name.to_ascii_lowercase().as_str() {
            "standard" => DsdModulator::Standard,
            "ecdepth2" => DsdModulator::EcDepth2,
            "ecbeam" => DsdModulator::EcBeam,
            "ecbeam2" => DsdModulator::EcBeam2,
            _ => {
                return Err(format!(
                    "unsupported modulator {name}; use Standard, EcDepth2, EcBeam, or EcBeam2"
                ));
            }
        };
        if !selected.contains(&modulator) {
            selected.push(modulator);
        }
    }
    if selected.is_empty() {
        return Err("--modulator must select at least one modulator".to_string());
    }
    Ok(selected)
}

fn parse_filter(value: &str) -> Result<FilterType, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "split128k" => Ok(FilterType::Split128k),
        "splitphase" | "split-phase" | "splitphase128ke2v3" | "splitphasee2v3"
        | "split-phase-e2v3" => Ok(FilterType::SplitPhase128kE2v3),
        _ => Err(format!(
            "unsupported filter {value}; use Split128k or SplitPhase128kE2v3"
        )),
    }
}

fn parse_rates(value: &str) -> Result<Vec<DsdRate>, String> {
    let mut rates = Vec::new();
    for token in value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let rate = match token.to_ascii_lowercase().as_str() {
            "64" | "dsd64" => DsdRate::Dsd64,
            "128" | "dsd128" => DsdRate::Dsd128,
            "256" | "dsd256" => DsdRate::Dsd256,
            _ => {
                return Err(format!(
                    "unsupported DSD rate {token}; use 64, 128, and/or 256"
                ));
            }
        };
        if !rates.contains(&rate) {
            rates.push(rate);
        }
    }
    if rates.is_empty() {
        return Err("--rates must select at least one DSD rate".to_string());
    }
    Ok(rates)
}

fn build_matrix(
    selected: &[DsdModulator],
    include_linear_reference: bool,
    production_filter: FilterType,
    rates: &[DsdRate],
) -> Vec<CellSpec> {
    let legacy = [
        DsdModulator::Standard,
        DsdModulator::EcDepth2,
        DsdModulator::EcBeam,
    ]
    .into_iter()
    .filter(|modulator| selected.contains(modulator))
    .collect::<Vec<_>>();
    let mut matrix = Vec::new();
    for (filter, diagnostic) in [(production_filter, false)]
        .into_iter()
        .chain(include_linear_reference.then_some((FilterType::SincExtreme32k, true)))
    {
        for rate in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256]
            .into_iter()
            .filter(|rate| rates.contains(rate))
        {
            for &modulator in &legacy {
                matrix.push(CellSpec {
                    scenario: Scenario::LevelSweep,
                    source_rate: signals::SOURCE_RATE_44K1_HZ,
                    dsd_rate: rate,
                    modulator,
                    filter,
                    diagnostic,
                });
            }
        }
        for scenario in [
            Scenario::IdleTinySignal,
            Scenario::HighFrequencyRatedStress,
            Scenario::HighFrequencyMatchedStress,
        ] {
            let dsd_rate = if scenario == Scenario::IdleTinySignal {
                DsdRate::Dsd64
            } else {
                DsdRate::Dsd128
            };
            if !rates.contains(&dsd_rate) {
                continue;
            }
            for &modulator in &legacy {
                matrix.push(CellSpec {
                    scenario,
                    source_rate: signals::SOURCE_RATE_44K1_HZ,
                    dsd_rate,
                    modulator,
                    filter,
                    diagnostic,
                });
            }
        }
        if rates.contains(&DsdRate::Dsd256) {
            for &modulator in &legacy {
                matrix.push(CellSpec {
                    scenario: Scenario::HiresReconstruction,
                    source_rate: signals::SOURCE_RATE_176K4_HZ,
                    dsd_rate: DsdRate::Dsd256,
                    modulator,
                    filter,
                    diagnostic,
                });
            }
        }
    }

    if selected.contains(&DsdModulator::EcBeam2) {
        for dsd_rate in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256]
            .into_iter()
            .filter(|rate| rates.contains(rate))
        {
            matrix.push(CellSpec {
                scenario: Scenario::LevelSweep,
                source_rate: signals::SOURCE_RATE_44K1_HZ,
                dsd_rate,
                modulator: DsdModulator::EcBeam2,
                filter: production_filter,
                diagnostic: false,
            });
        }
        if rates.contains(&DsdRate::Dsd64) {
            matrix.push(CellSpec {
                scenario: Scenario::IdleTinySignal,
                source_rate: signals::SOURCE_RATE_44K1_HZ,
                dsd_rate: DsdRate::Dsd64,
                modulator: DsdModulator::EcBeam2,
                filter: production_filter,
                diagnostic: false,
            });
        }
        if rates.contains(&DsdRate::Dsd128) {
            for scenario in [
                Scenario::HighFrequencyRatedStress,
                Scenario::HighFrequencyMatchedStress,
            ] {
                matrix.push(CellSpec {
                    scenario,
                    source_rate: signals::SOURCE_RATE_44K1_HZ,
                    dsd_rate: DsdRate::Dsd128,
                    modulator: DsdModulator::EcBeam2,
                    filter: production_filter,
                    diagnostic: false,
                });
            }
        }
        if rates.contains(&DsdRate::Dsd256) {
            matrix.push(CellSpec {
                scenario: Scenario::HiresReconstruction,
                source_rate: signals::SOURCE_RATE_176K4_HZ,
                dsd_rate: DsdRate::Dsd256,
                modulator: DsdModulator::EcBeam2,
                filter: production_filter,
                diagnostic: false,
            });
        }
    }
    matrix
}

fn append_rate_comparison_diagnostics(
    matrix: &mut Vec<CellSpec>,
    selected: &[DsdModulator],
    filter: FilterType,
) {
    for modulator in [
        DsdModulator::Standard,
        DsdModulator::EcDepth2,
        DsdModulator::EcBeam,
        DsdModulator::EcBeam2,
    ] {
        if selected.contains(&modulator) {
            matrix.push(CellSpec {
                scenario: Scenario::HiresReconstruction,
                source_rate: signals::SOURCE_RATE_176K4_HZ,
                dsd_rate: DsdRate::Dsd128,
                modulator,
                filter,
                diagnostic: true,
            });
        }
    }
}

fn score_policy() -> ScorePolicy {
    ScorePolicy {
        name: SCORE_VERSION,
        claim: SCORE_CLAIM,
        canonical_filter: "SplitPhase128kE2v3",
        normalization: "each category has a frozen v1 quality-index anchor; normalized score = clamp(100 - max(0, anchor_db - measured_quality_index_db), 0, 100), so one average decibel below the anchor costs one category point before the published category weight is applied",
        anchor_basis: "historical best Split128k category quality index in the clean native 42-cell v2 research comparison at Git commit 82a4395db0e0d3f85a08a0b8a8e700940f78f1f7",
        eligibility: "scores are emitted only when all 28 canonical Split Phase E2v3 cells complete with zero canonical structural hard failures; optional Linear Phase cells never affect scores; every production modulator is scored at DSD64, DSD128, and DSD256",
        rated_stress_role: "DSD128 rated stress is a structural qualification gate only; only matched-effective-peak stress contributes ranking points",
        anchors: vec![
            ScoreAnchorPolicy {
                rate: "DSD64",
                category: "coherent_level_sweep",
                quality_index_anchor_db: SCORE_ANCHOR_LEVEL_DSD64,
            },
            ScoreAnchorPolicy {
                rate: "DSD64",
                category: "idle_tiny_signal",
                quality_index_anchor_db: SCORE_ANCHOR_IDLE_DSD64,
            },
            ScoreAnchorPolicy {
                rate: "DSD128",
                category: "coherent_level_sweep",
                quality_index_anchor_db: SCORE_ANCHOR_LEVEL_DSD128,
            },
            ScoreAnchorPolicy {
                rate: "DSD128",
                category: "level_matched_stress_spectral_quality",
                quality_index_anchor_db: SCORE_ANCHOR_STRESS_DSD128,
            },
            ScoreAnchorPolicy {
                rate: "DSD128",
                category: "mute_restart_transition_quality",
                quality_index_anchor_db: SCORE_ANCHOR_TRANSITION_DSD128,
            },
            ScoreAnchorPolicy {
                rate: "DSD256",
                category: "coherent_level_sweep",
                quality_index_anchor_db: SCORE_ANCHOR_LEVEL_DSD256,
            },
            ScoreAnchorPolicy {
                rate: "DSD256",
                category: "hires_reconstruction_through_70khz",
                quality_index_anchor_db: SCORE_ANCHOR_HIRES_DSD256,
            },
        ],
        categories: vec![
            ScoreCategoryPolicy {
                rate: "DSD64",
                category: "coherent_level_sweep",
                maximum_points: 60.0,
                metric_formula: "mean over four levels and both channels of 45% SINAD + 20% negative unexpected-spur dBFS + 20% negative residual dBFS + 15% carrier-gain-error rejection capped at 100 dB",
            },
            ScoreCategoryPolicy {
                rate: "DSD64",
                category: "idle_tiny_signal",
                maximum_points: 40.0,
                metric_formula: "50% mean negative integrated-noise dBFS + 30% mean negative unexpected-spur dBFS + 10% mean relative DC-error rejection + 10% mean relative tiny-tone gain-error rejection; rejection terms are capped at 100 dB",
            },
            ScoreCategoryPolicy {
                rate: "DSD128",
                category: "coherent_level_sweep",
                maximum_points: 35.0,
                metric_formula: "mean over four levels and both channels of 45% SINAD + 20% negative unexpected-spur dBFS + 20% negative residual dBFS + 15% carrier-gain-error rejection capped at 100 dB",
            },
            ScoreCategoryPolicy {
                rate: "DSD128",
                category: "level_matched_stress_spectral_quality",
                maximum_points: 40.0,
                metric_formula: "mean over steady/recovery and both channels of 35% conventional SINAD + 15% mean carrier-gain-error rejection capped at 100 dB + 20% negative worst-declared-product dBFS + 15% negative product-excluded residual dBFS + 15% negative unexpected-spur dBFS",
            },
            ScoreCategoryPolicy {
                rate: "DSD128",
                category: "mute_restart_transition_quality",
                maximum_points: 25.0,
                metric_formula: "mean over channels of 10% negative zero-input peak dBFS + 15% negative clean-mute peak dBFS + 15% negative clean-mute RMS dBFS + 15% negative restart peak dBFS + 10/10/15% negative restart RMS at 1/10/50 ms + 10% negative 20log10(recovery_ms); absent recovery uses 1e9 ms",
            },
            ScoreCategoryPolicy {
                rate: "DSD256",
                category: "coherent_level_sweep",
                maximum_points: 35.0,
                metric_formula: "mean over four levels and both channels of 45% SINAD + 20% negative unexpected-spur dBFS + 20% negative residual dBFS + 15% carrier-gain-error rejection capped at 100 dB",
            },
            ScoreCategoryPolicy {
                rate: "DSD256",
                category: "hires_reconstruction_through_70khz",
                maximum_points: 65.0,
                metric_formula: "45% mean negative residual dBFS + 35% mean negative unexpected-spur dBFS across both channels and the 0-20 kHz and 20-80 kHz bands + 20% carrier-gain-error rejection capped at 100 dB through 70 kHz",
            },
        ],
    }
}

fn score_production_path(cells: &[CellReport]) -> Result<Vec<ProductionPathScore>, String> {
    [
        DsdModulator::Standard,
        DsdModulator::EcDepth2,
        DsdModulator::EcBeam,
        DsdModulator::EcBeam2,
    ]
    .into_iter()
    .map(|modulator| {
        let name = modulator.as_name();
        let dsd64 = rate_score(
            "DSD64",
            vec![
                category_score(
                    "coherent_level_sweep",
                    60.0,
                    level_quality_index(cells, name, "DSD64")?,
                    SCORE_ANCHOR_LEVEL_DSD64,
                ),
                category_score(
                    "idle_tiny_signal",
                    40.0,
                    idle_quality_index(cells, name)?,
                    SCORE_ANCHOR_IDLE_DSD64,
                ),
            ],
        )?;
        let dsd128 = rate_score(
            "DSD128",
            vec![
                category_score(
                    "coherent_level_sweep",
                    35.0,
                    level_quality_index(cells, name, "DSD128")?,
                    SCORE_ANCHOR_LEVEL_DSD128,
                ),
                category_score(
                    "level_matched_stress_spectral_quality",
                    40.0,
                    stress_quality_index(cells, name)?,
                    SCORE_ANCHOR_STRESS_DSD128,
                ),
                category_score(
                    "mute_restart_transition_quality",
                    25.0,
                    transition_quality_index(cells, name)?,
                    SCORE_ANCHOR_TRANSITION_DSD128,
                ),
            ],
        )?;
        let rates = vec![
            dsd64,
            dsd128,
            rate_score(
                "DSD256",
                vec![
                    category_score(
                        "coherent_level_sweep",
                        35.0,
                        level_quality_index(cells, name, "DSD256")?,
                        SCORE_ANCHOR_LEVEL_DSD256,
                    ),
                    category_score(
                        "hires_reconstruction_through_70khz",
                        65.0,
                        hires_quality_index(cells, name)?,
                        SCORE_ANCHOR_HIRES_DSD256,
                    ),
                ],
            )?,
        ];
        let rated =
            canonical_score_cell(cells, name, Scenario::HighFrequencyRatedStress, "DSD128")?;
        Ok(ProductionPathScore {
            modulator: name.to_string(),
            filter: DEFAULT_FILTER_TYPE.as_name().to_string(),
            rated_stress_qualified: rated.hard_failures.is_empty(),
            rates,
        })
    })
    .collect()
}

fn rate_score(rate: &'static str, categories: Vec<CategoryScore>) -> Result<RateScore, String> {
    let maximum_points = categories
        .iter()
        .map(|category| category.maximum_points)
        .sum::<f64>();
    if (maximum_points - 100.0).abs() > 1.0e-12 {
        return Err(format!(
            "{rate} score category weights summed to {maximum_points}, expected 100"
        ));
    }
    Ok(RateScore {
        rate,
        total_points: categories
            .iter()
            .map(|category| category.awarded_points)
            .sum(),
        maximum_points,
        categories,
    })
}

fn category_score(
    category: &'static str,
    maximum_points: f64,
    quality_index_db: f64,
    quality_index_anchor_db: f64,
) -> CategoryScore {
    let normalized_score =
        (100.0 - (quality_index_anchor_db - quality_index_db).max(0.0)).clamp(0.0, 100.0);
    CategoryScore {
        category,
        maximum_points,
        quality_index_db,
        quality_index_anchor_db,
        normalized_score,
        awarded_points: maximum_points * normalized_score / 100.0,
    }
}

fn canonical_score_cell<'a>(
    cells: &'a [CellReport],
    modulator: &str,
    scenario: Scenario,
    dsd_rate: &str,
) -> Result<&'a CellReport, String> {
    cells
        .iter()
        .find(|cell| {
            !cell.diagnostic
                && cell.filter == DEFAULT_FILTER_TYPE.as_name()
                && cell.modulator == modulator
                && cell.scenario == scenario.as_name()
                && cell.dsd_rate == dsd_rate
        })
        .ok_or_else(|| {
            format!(
                "score input missing: {} Split Phase E2v3 {dsd_rate} {modulator}",
                scenario.as_name()
            )
        })
}

fn level_quality_index(
    cells: &[CellReport],
    modulator: &str,
    dsd_rate: &str,
) -> Result<f64, String> {
    let cell = canonical_score_cell(cells, modulator, Scenario::LevelSweep, dsd_rate)?;
    let ScenarioMeasurements::LevelSweep { segments } = &cell.measurements else {
        return Err("level score cell had the wrong measurement kind".to_string());
    };
    average(
        segments
            .iter()
            .flat_map(|segment| segment.channels.iter())
            .map(|channel| {
                let metrics = &channel.metrics;
                0.45 * metrics.sinad_db
                    + 0.20 * -metrics.worst_nonharmonic_spur.level_dbfs
                    + 0.20 * -metrics.residual_noise_dbfs
                    + 0.15 * gain_error_rejection_db(metrics.carrier.gain_error_db)
            }),
        "level-sweep quality index",
    )
}

fn idle_quality_index(cells: &[CellReport], modulator: &str) -> Result<f64, String> {
    let cell = canonical_score_cell(cells, modulator, Scenario::IdleTinySignal, "DSD64")?;
    let ScenarioMeasurements::IdleTinySignal { sections } = &cell.measurements else {
        return Err("idle score cell had the wrong measurement kind".to_string());
    };
    let channels = sections
        .iter()
        .flat_map(|section| section.channels.iter())
        .collect::<Vec<_>>();
    let noise = average(
        channels
            .iter()
            .map(|channel| -channel.noise.integrated_noise_dbfs),
        "idle noise quality index",
    )?;
    let spur = average(
        channels
            .iter()
            .map(|channel| -channel.noise.worst_spur.level_dbfs),
        "idle spur quality index",
    )?;
    let dc = average(
        channels.iter().filter_map(|channel| {
            channel
                .dc_error
                .zip(channel.expected_dc)
                .map(|(error, expected)| rejection_db(error / expected))
        }),
        "idle DC accuracy quality index",
    )?;
    let tone = average(
        channels.iter().filter_map(|channel| {
            channel
                .tone_recovery
                .as_ref()
                .map(|carrier| gain_error_rejection_db(carrier.gain_error_db))
        }),
        "idle tiny-tone accuracy quality index",
    )?;
    Ok(0.50 * noise + 0.30 * spur + 0.10 * dc + 0.10 * tone)
}

fn stress_quality_index(cells: &[CellReport], modulator: &str) -> Result<f64, String> {
    let cell = canonical_score_cell(
        cells,
        modulator,
        Scenario::HighFrequencyMatchedStress,
        "DSD128",
    )?;
    let ScenarioMeasurements::HighFrequencyStress { channels, .. } = &cell.measurements else {
        return Err("stress score cell had the wrong measurement kind".to_string());
    };
    let mut values = Vec::with_capacity(channels.len() * 2);
    for channel in channels {
        for metrics in [&channel.steady, &channel.recovery] {
            let product = metrics
                .worst_declared_product
                .as_ref()
                .ok_or_else(|| "stress score was missing a declared product".to_string())?;
            let carrier_gain = average(
                metrics
                    .carriers
                    .iter()
                    .map(|carrier| gain_error_rejection_db(carrier.gain_error_db)),
                "stress carrier-gain quality index",
            )?;
            values.push(
                0.35 * metrics.sinad_db
                    + 0.15 * carrier_gain
                    + 0.20 * -product.level_dbfs
                    + 0.15 * -metrics.residual_excluding_declared_products_dbfs
                    + 0.15 * -metrics.worst_unexpected_spur.level_dbfs,
            );
        }
    }
    average(values, "stress spectral quality index")
}

fn transition_quality_index(cells: &[CellReport], modulator: &str) -> Result<f64, String> {
    let cell = canonical_score_cell(
        cells,
        modulator,
        Scenario::HighFrequencyMatchedStress,
        "DSD128",
    )?;
    let ScenarioMeasurements::HighFrequencyStress { channels, .. } = &cell.measurements else {
        return Err("transition score cell had the wrong measurement kind".to_string());
    };
    average(
        channels.iter().map(|channel| {
            let recovery_ms = channel.end_to_end_recovery_time_ms.unwrap_or(1.0e9);
            0.10 * -channel.zero_input_transition_peak_dbfs
                + 0.15 * -channel.clean_mute_peak_dbfs
                + 0.15 * -channel.clean_mute_rms_dbfs
                + 0.15 * -channel.restart_residual_peak_dbfs
                + 0.10 * -channel.restart_residual_rms_1ms_dbfs
                + 0.10 * -channel.restart_residual_rms_10ms_dbfs
                + 0.15 * -channel.restart_residual_rms_50ms_dbfs
                + 0.10 * (-20.0 * recovery_ms.log10())
        }),
        "stress transition quality index",
    )
}

fn hires_quality_index(cells: &[CellReport], modulator: &str) -> Result<f64, String> {
    let cell = canonical_score_cell(cells, modulator, Scenario::HiresReconstruction, "DSD256")?;
    let ScenarioMeasurements::HiresReconstruction { channels } = &cell.measurements else {
        return Err("hi-res score cell had the wrong measurement kind".to_string());
    };
    let bands = channels
        .iter()
        .flat_map(|channel| channel.metrics.bands.iter())
        .collect::<Vec<_>>();
    let residual = average(
        bands.iter().map(|band| -band.residual_dbfs),
        "hi-res residual quality index",
    )?;
    let spur = average(
        bands
            .iter()
            .map(|band| -band.worst_unexpected_spur.level_dbfs),
        "hi-res spur quality index",
    )?;
    let gain = average(
        channels.iter().flat_map(|channel| {
            channel
                .metrics
                .carriers
                .iter()
                .map(|carrier| gain_error_rejection_db(carrier.gain_error_db))
        }),
        "hi-res carrier-gain quality index",
    )?;
    Ok(0.45 * residual + 0.35 * spur + 0.20 * gain)
}

fn rejection_db(error_ratio: f64) -> f64 {
    (-20.0 * error_ratio.abs().max(1.0e-18).log10()).min(100.0)
}

fn gain_error_rejection_db(gain_error_db: f64) -> f64 {
    rejection_db(10.0f64.powf(gain_error_db / 20.0) - 1.0)
}

fn average(values: impl IntoIterator<Item = f64>, label: &str) -> Result<f64, String> {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(format!("{label} had no finite values"));
    }
    Ok(values.iter().sum::<f64>() / values.len() as f64)
}

fn run_cell(spec: CellSpec) -> Result<CellReport, String> {
    let headroom_db = headroom_db(spec.modulator);
    let filter = spec.filter;
    let filter_guard_frames = filter_guard_frames(filter);
    let signal = match spec.scenario {
        Scenario::LevelSweep => signals::coherent_level_sweep(headroom_db, filter_guard_frames),
        Scenario::IdleTinySignal => signals::idle_tiny_signal(headroom_db, filter_guard_frames),
        Scenario::HighFrequencyRatedStress => {
            signals::high_frequency_stress(headroom_db, filter_guard_frames)
        }
        Scenario::HighFrequencyMatchedStress => {
            signals::high_frequency_stress_level_matched(headroom_db, filter_guard_frames)
        }
        Scenario::HiresReconstruction => signals::hires_multitone(headroom_db, filter_guard_frames),
    }
    .map_err(|error| error.to_string())?;
    if signal.sample_rate_hz != spec.source_rate {
        return Err("fixture source rate does not match matrix".to_string());
    }
    let profile = profile_for(spec.scenario);
    let rendered = render_signal(&signal, spec.dsd_rate, spec.modulator, filter)?;
    let mut hard_failures = rendered.hard_failures.clone();
    let (measurements, mut structural_summary) = match spec.scenario {
        Scenario::LevelSweep => {
            analyze_level_sweep(&signal, &rendered, profile, &mut hard_failures)?
        }
        Scenario::IdleTinySignal => analyze_idle(&signal, &rendered, profile, &mut hard_failures)?,
        Scenario::HighFrequencyRatedStress | Scenario::HighFrequencyMatchedStress => {
            analyze_stress(&signal, &rendered, profile, &mut hard_failures)?
        }
        Scenario::HiresReconstruction => {
            analyze_hires(&signal, &rendered, profile, &mut hard_failures)?
        }
    };
    structural_summary.health_pass = hard_failures.is_empty();
    Ok(CellReport {
        scenario: spec.scenario.as_name().to_string(),
        modulator: spec.modulator.as_name().to_string(),
        diagnostic: spec.diagnostic,
        comparison_class: comparison_class(spec),
        level_matched_across_modulators: !matches!(
            spec.scenario,
            Scenario::HighFrequencyRatedStress
        ),
        production_default_filter: filter == DEFAULT_FILTER_TYPE,
        filter: filter.as_name().to_string(),
        headroom_db,
        source_rate: signal.sample_rate_hz,
        dsd_rate: dsd_rate_name(spec.dsd_rate).to_string(),
        wire_rate: rendered.wire_rate,
        source_frames: signal.frames(),
        filter_guard_source_frames: signal.filter_guard_frames,
        source_peak: signal.source_peak(),
        effective_source_peak: signal.effective_peak(),
        modulator_input_peak: rendered.modulator_input_peak,
        expected_bits_per_channel: rendered.expected_bits,
        native_bytes_before_idle: rendered.native_bytes_before_idle,
        native_bytes_after_idle: rendered.native_bytes_after_idle,
        full_fixture_density: rendered.full_fixture_density,
        reconstruction_profile: profile.id.to_string(),
        spectral_analysis_frames: SPECTRAL_ANALYSIS_FRAMES,
        spectral_bin_width_hz: profile.output_rate as f64 / SPECTRAL_ANALYSIS_FRAMES as f64,
        render_seconds: rendered.render_seconds,
        renderer_configuration: rendered.renderer_configuration,
        source_pcm_sha256: source_pcm_sha256(&signal),
        native_dsd_sha256: rendered.native_dsd_sha256,
        health: rendered.health,
        measurements,
        structural_summary,
        hard_failures,
    })
}

fn render_signal(
    signal: &signals::StereoSignal,
    dsd_rate: DsdRate,
    modulator: DsdModulator,
    filter: FilterType,
) -> Result<RenderedCell, String> {
    signal.validate().map_err(|error| error.to_string())?;
    analysis::validate_finite(&signal.left, "left source")?;
    analysis::validate_finite(&signal.right, "right source")?;
    let wire_rate = dsd_rate
        .wire_rate_for_source(signal.sample_rate_hz)
        .ok_or_else(|| "matrix selected an unsupported source/wire-rate pair".to_string())?;
    let ratio = (wire_rate / signal.sample_rate_hz) as usize;
    let expected_bits = signal
        .frames()
        .checked_mul(ratio)
        .ok_or_else(|| "expected output length overflowed".to_string())?;
    if !expected_bits.is_multiple_of(8) {
        return Err("fixed matrix unexpectedly produced a partial native byte".to_string());
    }
    let expected_bytes = expected_bits / 8;
    let mut renderer =
        DsdRenderer::new_with_dsd_modulator(filter, signal.sample_rate_hz, dsd_rate, modulator)
            .map_err(str::to_string)?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let input_gain = 10.0f64.powf(signal.headroom_db / 20.0);
    let mut left = Vec::with_capacity(expected_bytes);
    let mut right = Vec::with_capacity(expected_bytes);
    let started = Instant::now();
    for start in (0..signal.frames()).step_by(CHUNK_FRAMES) {
        let end = (start + CHUNK_FRAMES).min(signal.frames());
        renderer.upsample(&signal.left[start..end], &signal.right[start..end]);
        renderer.modulate_and_pack_native(input_gain, &mut left, &mut right);
    }
    renderer.drain_resampler_eof();
    renderer.modulate_and_pack_native(input_gain, &mut left, &mut right);
    renderer.flush_modulators_and_pack_native(&mut left, &mut right);
    let native_bytes_before_idle = [left.len(), right.len()];
    let native_dsd_sha256 = [sha256_bytes(&left), sha256_bytes(&right)];
    let full_fixture_density = [whole_density_metrics(&left), whole_density_metrics(&right)];
    renderer.flush_native_with_idle(&mut left, &mut right);
    let native_bytes_after_idle = [left.len(), right.len()];
    let render_seconds = started.elapsed().as_secs_f64();
    let modulator_input_peak = renderer.modulator_input_peak();
    let health = collect_health(&renderer);
    let seeds = renderer.effective_modulator_seeds();
    let renderer_configuration = RendererConfiguration {
        production_policy_version: DSD_PRODUCTION_POLICY_VERSION,
        coefficient_table: renderer.coefficient_table_name().to_string(),
        coefficient_osr: renderer.coefficient_osr(),
        coefficient_obg: renderer.coefficient_obg(),
        coefficient_input_peak: renderer.modulator_input_peak(),
        lookahead_depth: modulator.lookahead_depth(),
        isi_penalty: renderer.isi_penalty(),
        chunk_source_frames: CHUNK_FRAMES,
        seeds_hex: [
            format!("0x{:016x}", seeds[0]),
            format!("0x{:016x}", seeds[1]),
        ],
    };
    let mut hard_failures = health_failures(&health);
    if !modulator_input_peak.is_finite() || modulator_input_peak <= 0.0 {
        hard_failures.push(format!(
            "modulator input calibration peak was invalid: {modulator_input_peak}"
        ));
    }
    if native_bytes_before_idle != [expected_bytes, expected_bytes] {
        hard_failures.push(format!(
            "native output before idle was L={} R={} bytes; expected {expected_bytes}",
            native_bytes_before_idle[0], native_bytes_before_idle[1]
        ));
    }
    if native_bytes_after_idle != native_bytes_before_idle {
        hard_failures.push(format!(
            "idle flush changed byte lengths from {:?} to {:?}",
            native_bytes_before_idle, native_bytes_after_idle
        ));
    }
    if signal.source_peak() > 1.0 + 1.0e-12 {
        hard_failures.push(format!(
            "source peak {:.9} exceeded full scale",
            signal.source_peak()
        ));
    }
    for (channel, density) in ["left", "right"].into_iter().zip(full_fixture_density) {
        if !density.density.is_finite() || density.deviation > DENSITY_LIMIT {
            hard_failures.push(format!(
                "whole-fixture {channel} bit-density deviation {:.9} exceeded {:.6}",
                density.deviation, DENSITY_LIMIT
            ));
        }
    }
    Ok(RenderedCell {
        left,
        right,
        native_bytes_before_idle,
        native_bytes_after_idle,
        expected_bits,
        wire_rate,
        filter,
        modulator_input_peak,
        render_seconds,
        renderer_configuration,
        native_dsd_sha256,
        full_fixture_density,
        health,
        hard_failures,
    })
}

fn collect_health(renderer: &DsdRenderer) -> HealthReport {
    let limiter = renderer.limiter_telemetry();
    let truncation = renderer.truncation_telemetry();
    let mut report = HealthReport {
        stability_resets: renderer.stability_resets(),
        state_clamps: renderer.state_clamps(),
        limiter_peak_ratio_max: limiter.peak_ratio_max as f64,
        limiter_limited_events: limiter.limited_events,
        limiter_limited_samples: limiter.limited_samples,
        truncation_events: truncation.events,
        discarded_left_bits: truncation.discarded_left_bits,
        discarded_right_bits: truncation.discarded_right_bits,
        ..HealthReport::default()
    };
    for diagnostics in renderer.beam_diagnostics().into_iter().flatten() {
        report.beam_clamps += diagnostics.beam_clamp_total;
        report.beam_speculative_clamps += diagnostics.beam_speculative_clamp_total;
        report.beam_committed_clamps += diagnostics.beam_committed_clamp_total;
        report.beam_rejected_hard_limits += diagnostics.beam_rejected_hard_limit_total;
        report.beam_all_children_rejected += diagnostics.beam_all_children_rejected_total;
    }
    for diagnostics in renderer.ecbeam2_diagnostics().into_iter().flatten() {
        report.ecbeam2_constraint_escapes += diagnostics.constraint_escape;
        report.ecbeam2_state_repairs += diagnostics.state_repair_fallback;
        report.ecbeam2_nonfinite_resets += diagnostics.all_nonfinite_resets;
        report.ecbeam2_observer_desynchronizations += diagnostics.observer_desynchronizations;
        report.ecbeam2_invalid_input_substitutions += diagnostics.invalid_input_substitutions;
        report.ecbeam2_output_length_events += diagnostics.output_length_events;
    }
    report
}

fn health_failures(health: &HealthReport) -> Vec<String> {
    let mut failures = Vec::new();
    if !health.limiter_peak_ratio_max.is_finite() {
        failures.push("limiter peak ratio was nonfinite".to_string());
    }
    push_nonzero(&mut failures, "stability resets", health.stability_resets);
    push_nonzero(&mut failures, "state clamps", health.state_clamps);
    push_nonzero(
        &mut failures,
        "limiter events",
        health.limiter_limited_events,
    );
    push_nonzero(
        &mut failures,
        "limiter samples",
        health.limiter_limited_samples,
    );
    push_nonzero(&mut failures, "truncation events", health.truncation_events);
    push_nonzero(
        &mut failures,
        "discarded left bits",
        health.discarded_left_bits,
    );
    push_nonzero(
        &mut failures,
        "discarded right bits",
        health.discarded_right_bits,
    );
    push_nonzero(
        &mut failures,
        "EcBeam committed clamps",
        health.beam_committed_clamps,
    );
    push_nonzero(
        &mut failures,
        "EcBeam all-children-rejected events",
        health.beam_all_children_rejected,
    );
    push_nonzero(
        &mut failures,
        "EcBeam2 constraint escapes",
        health.ecbeam2_constraint_escapes,
    );
    push_nonzero(
        &mut failures,
        "EcBeam2 state repairs",
        health.ecbeam2_state_repairs,
    );
    push_nonzero(
        &mut failures,
        "EcBeam2 nonfinite resets",
        health.ecbeam2_nonfinite_resets,
    );
    push_nonzero(
        &mut failures,
        "EcBeam2 observer desynchronizations",
        health.ecbeam2_observer_desynchronizations,
    );
    push_nonzero(
        &mut failures,
        "EcBeam2 invalid-input substitutions",
        health.ecbeam2_invalid_input_substitutions,
    );
    push_nonzero(
        &mut failures,
        "EcBeam2 output-length events",
        health.ecbeam2_output_length_events,
    );
    failures
}

fn analyze_level_sweep(
    signal: &signals::StereoSignal,
    rendered: &RenderedCell,
    profile: ReconstructionProfile,
    hard_failures: &mut Vec<String>,
) -> Result<(ScenarioMeasurements, StructuralSummary), String> {
    let mut segments = Vec::new();
    let mut summary = StructuralSummary::default();
    for carrier in &signal.carriers {
        let source_range = signal
            .range(carrier.analysis_ranges[0])
            .ok_or_else(|| "level carrier references a missing range".to_string())?;
        let bits = source_to_bit_range(
            rendered.filter,
            source_range.frames(),
            signal.sample_rate_hz,
            rendered.wire_rate,
        )?;
        let (left, right) = analysis::reconstruct_stereo_window(
            &rendered.left,
            &rendered.right,
            rendered.wire_rate,
            bits.clone(),
            profile,
            rendered.modulator_input_peak,
        )?;
        let mut channels = Vec::new();
        for (channel, samples, bytes) in [
            ("left", left.as_slice(), rendered.left.as_slice()),
            ("right", right.as_slice(), rendered.right.as_slice()),
        ] {
            let metrics = analysis::analyze_single_tone(
                samples,
                profile.output_rate,
                carrier.name,
                carrier.actual_hz,
                carrier.effective_amplitude,
                5,
            )?;
            let density = analysis::density_metrics_for_duration(
                bytes,
                bits.clone(),
                rendered.wire_rate,
                ROLLING_DENSITY_WINDOW_SECONDS,
            )?;
            let peak = analysis::max_abs(samples);
            update_density_peak(&mut summary, &density, peak);
            gate_channel(channel, carrier.name, &density, peak, false, hard_failures);
            channels.push(ChannelToneReport {
                channel,
                metrics,
                density,
                reconstructed_peak: peak,
            });
        }
        segments.push(LevelSegmentReport {
            name: carrier.name.to_string(),
            source_level_dbfs: carrier.source_dbfs(),
            effective_level_dbfs: carrier.effective_dbfs(),
            actual_frequency_hz: carrier.actual_hz,
            channels,
        });
    }
    Ok((ScenarioMeasurements::LevelSweep { segments }, summary))
}

fn analyze_idle(
    signal: &signals::StereoSignal,
    rendered: &RenderedCell,
    profile: ReconstructionProfile,
    hard_failures: &mut Vec<String>,
) -> Result<(ScenarioMeasurements, StructuralSummary), String> {
    let tone = signal
        .carriers
        .first()
        .ok_or_else(|| "idle fixture is missing its low-level tone".to_string())?;
    let sections = [
        ("silence", signals::IDLE_SILENCE_ANALYSIS_RANGE, None),
        ("tiny_dc", signals::IDLE_DC_ANALYSIS_RANGE, None),
        (
            "tone_100hz_-120_dbfs",
            signals::IDLE_TONE_ANALYSIS_RANGE,
            Some(tone.actual_hz),
        ),
    ];
    let mut reports = Vec::new();
    let mut summary = StructuralSummary::default();
    for (name, range_name, tone_hz) in sections {
        let source_range = signal
            .range(range_name)
            .ok_or_else(|| format!("idle fixture is missing {range_name}"))?;
        let bits = source_to_bit_range(
            rendered.filter,
            source_range.frames(),
            signal.sample_rate_hz,
            rendered.wire_rate,
        )?;
        let (left, right) = analysis::reconstruct_stereo_window(
            &rendered.left,
            &rendered.right,
            rendered.wire_rate,
            bits.clone(),
            profile,
            rendered.modulator_input_peak,
        )?;
        let mut channels = Vec::new();
        for (channel, samples, bytes) in [
            ("left", left.as_slice(), rendered.left.as_slice()),
            ("right", right.as_slice(), rendered.right.as_slice()),
        ] {
            let exclusions = tone_hz.map_or_else(Vec::new, |frequency| vec![frequency]);
            let noise =
                analysis::analyze_noise(samples, profile.output_rate, 20.0, 20_000.0, &exclusions)?;
            let tone_recovery = tone_hz
                .map(|frequency| {
                    analysis::measure_windowed_carrier(
                        samples,
                        profile.output_rate,
                        tone.name,
                        frequency,
                        tone.effective_amplitude,
                    )
                })
                .transpose()?;
            let density = analysis::density_metrics_for_duration(
                bytes,
                bits.clone(),
                rendered.wire_rate,
                ROLLING_DENSITY_WINDOW_SECONDS,
            )?;
            let reconstructed_dc = analysis::mean(samples);
            let expected_dc = signal
                .dc_offsets
                .iter()
                .find(|offset| offset.analysis_range == range_name)
                .map(|offset| {
                    if channel == "left" {
                        offset.effective_left
                    } else {
                        offset.effective_right
                    }
                });
            let dc_error = expected_dc.map(|expected| reconstructed_dc - expected);
            let dc_polarity_correct = expected_dc.map(|expected| {
                expected == 0.0
                    || reconstructed_dc != 0.0 && expected.signum() == reconstructed_dc.signum()
            });
            let peak = analysis::max_abs(samples);
            update_density_peak(&mut summary, &density, peak);
            gate_channel(
                channel,
                name,
                &density,
                peak,
                matches!(name, "silence" | "tiny_dc"),
                hard_failures,
            );
            channels.push(IdleChannelReport {
                channel,
                noise,
                reconstructed_dc,
                expected_dc,
                dc_error,
                dc_polarity_correct,
                tone_recovery,
                density,
                reconstructed_peak: peak,
            });
        }
        let left = &channels[0];
        let right = &channels[1];
        let channel_spread = IdleChannelSpread {
            integrated_noise_spread_db: (left.noise.integrated_noise_dbfs
                - right.noise.integrated_noise_dbfs)
                .abs(),
            worst_spur_spread_db: (left.noise.worst_spur.level_dbfs
                - right.noise.worst_spur.level_dbfs)
                .abs(),
            reconstructed_dc_magnitude_spread: (left.reconstructed_dc.abs()
                - right.reconstructed_dc.abs())
            .abs(),
            bit_density_deviation_spread: (left.density.deviation - right.density.deviation).abs(),
            tone_gain_error_spread_db: left
                .tone_recovery
                .as_ref()
                .zip(right.tone_recovery.as_ref())
                .map(|(left, right)| (left.gain_error_db - right.gain_error_db).abs()),
        };
        reports.push(IdleSectionReport {
            name: name.to_string(),
            actual_tone_hz: tone_hz,
            channel_spread,
            channels,
        });
    }
    Ok((
        ScenarioMeasurements::IdleTinySignal { sections: reports },
        summary,
    ))
}

fn analyze_stress(
    signal: &signals::StereoSignal,
    rendered: &RenderedCell,
    profile: ReconstructionProfile,
    hard_failures: &mut Vec<String>,
) -> Result<(ScenarioMeasurements, StructuralSummary), String> {
    let (
        level_contract,
        steady_range_name,
        mute_range_name,
        clean_mute_range_name,
        recovery_range_name,
    ) = if signal.id == signals::STRESS_LEVEL_MATCHED_FIXTURE_ID {
        (
            "matched_effective_peak",
            signals::STRESS_LEVEL_MATCHED_STEADY_ANALYSIS_RANGE,
            signals::STRESS_LEVEL_MATCHED_MUTE_RANGE,
            signals::STRESS_LEVEL_MATCHED_CLEAN_MUTE_RANGE,
            signals::STRESS_LEVEL_MATCHED_RECOVERY_RANGE,
        )
    } else if signal.id == signals::STRESS_RATED_FIXTURE_ID {
        (
            "rated_source_peak",
            signals::STRESS_STEADY_ANALYSIS_RANGE,
            signals::STRESS_MUTE_RANGE,
            signals::STRESS_CLEAN_MUTE_RANGE,
            signals::STRESS_RECOVERY_RANGE,
        )
    } else {
        return Err(format!("unexpected stress fixture {}", signal.id));
    };
    let steady_range = signal
        .range(steady_range_name)
        .ok_or_else(|| "stress fixture is missing its steady range".to_string())?;
    let recovery_range = signal
        .range(recovery_range_name)
        .ok_or_else(|| "stress fixture is missing its recovery range".to_string())?;
    let mute_range = signal
        .range(mute_range_name)
        .ok_or_else(|| "stress fixture is missing its mute range".to_string())?;
    let clean_mute_range = signal
        .range(clean_mute_range_name)
        .ok_or_else(|| "stress fixture is missing its clean mute range".to_string())?;
    let steady_bits = source_to_bit_range(
        rendered.filter,
        steady_range.frames(),
        signal.sample_rate_hz,
        rendered.wire_rate,
    )?;
    let transition_source = mute_range.start..recovery_range.end;
    let transition_bits = source_to_bit_range(
        rendered.filter,
        transition_source,
        signal.sample_rate_hz,
        rendered.wire_rate,
    )?;
    let clean_mute_bits = source_to_bit_range(
        rendered.filter,
        clean_mute_range.frames(),
        signal.sample_rate_hz,
        rendered.wire_rate,
    )?;
    let (steady_left, steady_right) = analysis::reconstruct_stereo_window(
        &rendered.left,
        &rendered.right,
        rendered.wire_rate,
        steady_bits.clone(),
        profile,
        rendered.modulator_input_peak,
    )?;
    let (transition_left, transition_right) = analysis::reconstruct_stereo_window(
        &rendered.left,
        &rendered.right,
        rendered.wire_rate,
        transition_bits,
        profile,
        rendered.modulator_input_peak,
    )?;
    let output_per_source = (profile.output_rate / signal.sample_rate_hz) as usize;
    let steady_len = steady_range.len() * output_per_source;
    if steady_left.len() != steady_len || steady_right.len() != steady_len {
        return Err("stress steady reconstruction length mismatch".to_string());
    }
    let restart_index = (recovery_range.start - mute_range.start) * output_per_source;
    let clean_mute_output_range = (clean_mute_range.start - mute_range.start) * output_per_source
        ..(clean_mute_range.end - mute_range.start) * output_per_source;
    let recovery_analysis_len = signals::STRESS_STEADY_ANALYZE_FRAMES * output_per_source;
    let recovery_analysis_start =
        (recovery_range.end - mute_range.start) * output_per_source - recovery_analysis_len;
    let carriers = signal
        .carriers
        .iter()
        .map(|carrier| (carrier.name, carrier.actual_hz, carrier.effective_amplitude))
        .collect::<Vec<_>>();
    let f1 = signal.carriers[0].actual_hz;
    let f2 = signal.carriers[1].actual_hz;
    let products = [
        ("difference", (f2 - f1).abs()),
        ("lower_imd", 2.0 * f1 - f2),
        ("upper_imd", 2.0 * f2 - f1),
    ];
    let mut reports = Vec::new();
    let mut summary = StructuralSummary::default();
    for (channel, steady_samples, samples, bytes) in [
        (
            "left",
            steady_left.as_slice(),
            transition_left.as_slice(),
            rendered.left.as_slice(),
        ),
        (
            "right",
            steady_right.as_slice(),
            transition_right.as_slice(),
            rendered.right.as_slice(),
        ),
    ] {
        let steady = analysis::analyze_declared_tones(
            steady_samples,
            profile.output_rate,
            &carriers,
            &products,
            20.0,
            20_500.0,
        )?;
        let recovery = analysis::analyze_declared_tones(
            &samples[recovery_analysis_start..recovery_analysis_start + recovery_analysis_len],
            profile.output_rate,
            &carriers,
            &products,
            20.0,
            20_500.0,
        )?;
        let transition = analyze_stress_transition(
            steady_samples,
            samples,
            profile.output_rate,
            restart_index,
            clean_mute_output_range.clone(),
            recovery_analysis_start..recovery_analysis_start + recovery_analysis_len,
            &[f1, f2],
        )?;
        let transition_residual_peak_dbfs =
            analysis::peak_sine_dbfs(transition.transition_residual_peak);
        let end_to_end_recovery_time_ms =
            analysis::estimate_recovery_time_ms_from_separate_windows(
                steady_samples,
                &samples[restart_index..],
                profile.output_rate,
                recovery_analysis_start - restart_index,
                &[f1, f2],
            )?;
        let steady_density = analysis::density_metrics_for_duration(
            bytes,
            steady_bits.clone(),
            rendered.wire_rate,
            ROLLING_DENSITY_WINDOW_SECONDS,
        )?;
        let clean_mute_density = analysis::density_metrics_for_duration(
            bytes,
            clean_mute_bits.clone(),
            rendered.wire_rate,
            ROLLING_DENSITY_WINDOW_SECONDS,
        )?;
        let peak = analysis::max_abs(steady_samples).max(analysis::max_abs(samples));
        update_density_peak(&mut summary, &steady_density, peak);
        update_density_peak(
            &mut summary,
            &clean_mute_density,
            transition.clean_mute_peak,
        );
        gate_channel(
            channel,
            "stress steady",
            &steady_density,
            peak,
            false,
            hard_failures,
        );
        gate_channel(
            channel,
            "stress clean mute",
            &clean_mute_density,
            transition.clean_mute_peak,
            true,
            hard_failures,
        );
        reports.push(StressChannelReport {
            channel,
            steady,
            recovery,
            settled_program_peak: transition.settled_program_peak,
            transition_waveform_peak: transition.transition_waveform_peak,
            transition_overshoot_above_settled: transition.transition_overshoot_above_settled,
            zero_input_transition_peak: transition.zero_input_transition_peak,
            zero_input_transition_peak_dbfs: analysis::peak_sine_dbfs(
                transition.zero_input_transition_peak,
            ),
            clean_mute_peak: transition.clean_mute_peak,
            clean_mute_peak_dbfs: analysis::peak_sine_dbfs(transition.clean_mute_peak),
            clean_mute_rms_dbfs: transition.clean_mute_rms_dbfs,
            restart_residual_peak: transition.restart_residual_peak,
            restart_residual_peak_dbfs: analysis::peak_sine_dbfs(transition.restart_residual_peak),
            restart_residual_rms_1ms_dbfs: transition.restart_residual_rms_1ms_dbfs,
            restart_residual_rms_10ms_dbfs: transition.restart_residual_rms_10ms_dbfs,
            restart_residual_rms_50ms_dbfs: transition.restart_residual_rms_50ms_dbfs,
            transition_residual_peak: transition.transition_residual_peak,
            transition_residual_peak_dbfs,
            end_to_end_recovery_time_ms,
            steady_density_analysis_range: steady_range.name.to_string(),
            steady_density,
            clean_mute_density_analysis_range: clean_mute_range.name.to_string(),
            clean_mute_density,
            reconstructed_peak: peak,
        });
    }
    Ok((
        ScenarioMeasurements::HighFrequencyStress {
            level_contract,
            channels: reports,
        },
        summary,
    ))
}

fn analyze_stress_transition(
    steady_samples: &[f64],
    transition_samples: &[f64],
    sample_rate: u32,
    restart: usize,
    clean_mute_range: std::ops::Range<usize>,
    recovered_fit_range: std::ops::Range<usize>,
    carrier_frequencies: &[f64],
) -> Result<StressTransitionAnalysis, String> {
    if steady_samples.len() < 3
        || restart == 0
        || clean_mute_range.is_empty()
        || clean_mute_range.end > restart
        || restart >= recovered_fit_range.start
        || recovered_fit_range.is_empty()
        || recovered_fit_range.end > transition_samples.len()
    {
        return Err("invalid stress-transition analysis boundaries".to_string());
    }
    let recovery_fit = analysis::fit_tone_model(
        &transition_samples[recovered_fit_range.clone()],
        sample_rate,
        carrier_frequencies,
    )?;
    let restart_residual = analysis::residual_against_tone_model(
        &transition_samples[restart..recovered_fit_range.start],
        &recovery_fit,
        restart as isize - recovered_fit_range.start as isize,
    )?;
    let zero_input_transition_peak = analysis::max_abs(&transition_samples[..restart]);
    let clean_mute = &transition_samples[clean_mute_range];
    let clean_mute_peak = analysis::max_abs(clean_mute);
    let clean_mute_mean_square =
        clean_mute.iter().map(|sample| sample * sample).sum::<f64>() / clean_mute.len() as f64;
    let restart_residual_peak = analysis::max_abs(&restart_residual);
    let settled_program_peak = analysis::max_abs(steady_samples).max(analysis::max_abs(
        &transition_samples[recovered_fit_range.clone()],
    ));
    let transition_waveform_peak =
        analysis::max_abs(&transition_samples[..recovered_fit_range.start]);
    Ok(StressTransitionAnalysis {
        settled_program_peak,
        transition_waveform_peak,
        transition_overshoot_above_settled: (transition_waveform_peak - settled_program_peak)
            .max(0.0),
        zero_input_transition_peak,
        clean_mute_peak,
        clean_mute_rms_dbfs: analysis::rms_dbfs_full_scale_sine(clean_mute_mean_square.sqrt()),
        restart_residual_peak,
        restart_residual_rms_1ms_dbfs: residual_prefix_rms_dbfs(
            &restart_residual,
            sample_rate,
            0.001,
        )?,
        restart_residual_rms_10ms_dbfs: residual_prefix_rms_dbfs(
            &restart_residual,
            sample_rate,
            0.010,
        )?,
        restart_residual_rms_50ms_dbfs: residual_prefix_rms_dbfs(
            &restart_residual,
            sample_rate,
            0.050,
        )?,
        transition_residual_peak: zero_input_transition_peak.max(restart_residual_peak),
    })
}

fn residual_prefix_rms_dbfs(
    residual: &[f64],
    sample_rate: u32,
    duration_seconds: f64,
) -> Result<f64, String> {
    let frames = (sample_rate as f64 * duration_seconds).round().max(1.0) as usize;
    if residual.len() < frames {
        return Err(format!(
            "restart residual has {} frames, fewer than the requested {frames}",
            residual.len()
        ));
    }
    let mean_square = residual[..frames]
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>()
        / frames as f64;
    Ok(analysis::rms_dbfs_full_scale_sine(mean_square.sqrt()))
}

fn analyze_hires(
    signal: &signals::StereoSignal,
    rendered: &RenderedCell,
    profile: ReconstructionProfile,
    hard_failures: &mut Vec<String>,
) -> Result<(ScenarioMeasurements, StructuralSummary), String> {
    let source_range = signal
        .range(signals::HIRES_ANALYSIS_RANGE)
        .ok_or_else(|| "hi-res fixture is missing its analysis range".to_string())?;
    let bits = source_to_bit_range(
        rendered.filter,
        source_range.frames(),
        signal.sample_rate_hz,
        rendered.wire_rate,
    )?;
    let (left, right) = analysis::reconstruct_stereo_window(
        &rendered.left,
        &rendered.right,
        rendered.wire_rate,
        bits.clone(),
        profile,
        rendered.modulator_input_peak,
    )?;
    let carriers = signal
        .carriers
        .iter()
        .map(|carrier| (carrier.name, carrier.actual_hz, carrier.effective_amplitude))
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    let mut summary = StructuralSummary::default();
    for (channel, samples, bytes) in [
        ("left", left.as_slice(), rendered.left.as_slice()),
        ("right", right.as_slice(), rendered.right.as_slice()),
    ] {
        let metrics = analysis::analyze_multiband(
            samples,
            profile.output_rate,
            &carriers,
            &[(0.0, 20_000.0), (20_000.0, 80_000.0)],
        )?;
        let density = analysis::density_metrics_for_duration(
            bytes,
            bits.clone(),
            rendered.wire_rate,
            ROLLING_DENSITY_WINDOW_SECONDS,
        )?;
        let peak = analysis::max_abs(samples);
        update_density_peak(&mut summary, &density, peak);
        gate_channel(channel, "hires", &density, peak, false, hard_failures);
        reports.push(HiresChannelReport {
            channel,
            metrics,
            density,
            reconstructed_peak: peak,
        });
    }
    Ok((
        ScenarioMeasurements::HiresReconstruction { channels: reports },
        summary,
    ))
}

fn update_density_peak(summary: &mut StructuralSummary, density: &DensityMetrics, peak: f64) {
    summary.observed_max_density_deviation = summary
        .observed_max_density_deviation
        .max(density.deviation)
        .max(density.rolling_max_deviation);
    summary.observed_max_reconstructed_peak = summary.observed_max_reconstructed_peak.max(peak);
}

fn gate_channel(
    channel: &str,
    section: &str,
    density: &DensityMetrics,
    reconstructed_peak: f64,
    enforce_density_limit: bool,
    failures: &mut Vec<String>,
) {
    if !density.density.is_finite() || !density.deviation.is_finite() {
        failures.push(format!(
            "{section} {channel} bit-density measurement was nonfinite"
        ));
    } else if enforce_density_limit && density.deviation > DENSITY_LIMIT {
        failures.push(format!(
            "{section} {channel} bit-density deviation {:.9} exceeded {:.6}",
            density.deviation, DENSITY_LIMIT
        ));
    }
    if !density.rolling_max_deviation.is_finite() {
        failures.push(format!(
            "{section} {channel} rolling bit-density measurement was nonfinite"
        ));
    } else if enforce_density_limit && density.rolling_max_deviation > DENSITY_LIMIT {
        failures.push(format!(
            "{section} {channel} rolling bit-density deviation {:.9} exceeded {:.6}",
            density.rolling_max_deviation, DENSITY_LIMIT
        ));
    }
    if !reconstructed_peak.is_finite() || reconstructed_peak > RECONSTRUCTED_PEAK_LIMIT {
        failures.push(format!(
            "{section} {channel} reconstructed peak {:.9} exceeded {:.3}",
            reconstructed_peak, RECONSTRUCTED_PEAK_LIMIT
        ));
    }
}

fn source_to_bit_range(
    filter: FilterType,
    source: std::ops::Range<usize>,
    source_rate: u32,
    wire_rate: u32,
) -> Result<std::ops::Range<usize>, String> {
    let source_length = source
        .end
        .checked_sub(source.start)
        .filter(|length| *length > 0)
        .ok_or_else(|| "invalid source-to-bit mapping".to_string())?;
    dsd_source_window_to_modulator_samples(
        filter,
        source_rate,
        wire_rate,
        source.start,
        source_length,
    )
    .ok_or_else(|| "renderer rejected source-to-bit mapping".to_string())
}

fn profile_for(scenario: Scenario) -> ReconstructionProfile {
    match scenario {
        Scenario::HiresReconstruction => HIRES_BAND,
        Scenario::LevelSweep
        | Scenario::IdleTinySignal
        | Scenario::HighFrequencyRatedStress
        | Scenario::HighFrequencyMatchedStress => AUDIO_BAND,
    }
}

fn headroom_db(modulator: DsdModulator) -> f64 {
    match modulator {
        DsdModulator::Standard | DsdModulator::EcDepth2 => PRODUCTION_HEADROOM_DB,
        DsdModulator::EcBeam | DsdModulator::EcBeam2 => SEARCH_HEADROOM_DB,
        _ => PRODUCTION_HEADROOM_DB,
    }
}

fn filter_guard_frames(filter: FilterType) -> usize {
    match filter {
        FilterType::SincExtreme32k => LINEAR_FILTER_GUARD_FRAMES,
        FilterType::Split128k | FilterType::SplitPhase128kE2v3 => SPLIT_FILTER_GUARD_FRAMES,
        _ => LINEAR_FILTER_GUARD_FRAMES,
    }
}

fn comparison_class(spec: CellSpec) -> &'static str {
    match (
        spec.diagnostic,
        spec.filter == DEFAULT_FILTER_TYPE,
        spec.scenario,
    ) {
        (true, true, _) => "rate_comparison_level_matched",
        (true, _, Scenario::HighFrequencyRatedStress) => "linear_reference_rated_input",
        (true, _, _) => "linear_reference_level_matched",
        (false, true, Scenario::HighFrequencyRatedStress) => "production_path_rated_input",
        (false, true, _) => "production_path_level_matched",
        (false, false, _) => "noncanonical",
    }
}

fn dsd_rate_name(rate: DsdRate) -> &'static str {
    match rate {
        DsdRate::Dsd64 => "DSD64",
        DsdRate::Dsd128 => "DSD128",
        DsdRate::Dsd256 => "DSD256",
    }
}

fn reject_filter_overrides() -> Result<(), String> {
    let names = [
        "FOZMO_EXTREME32K_CUTOFF",
        "FOZMO_EXTREME32K_BETA",
        "FOZMO_SPLIT128K_CUTOFF",
        "FOZMO_SPLIT128K_BETA",
        "FOZMO_SPLIT128K_F_LO_HZ",
        "FOZMO_SPLIT128K_F_HI_HZ",
        "FOZMO_SPLIT128K_BLEND_FLOOR",
        "FOZMO_SPLIT128K_CAUSALITY_SHIFT_SCALE",
        "FOZMO_SPLIT128K_TAIL_FADE",
    ];
    let set = names
        .iter()
        .filter(|name| env::var_os(name).is_some())
        .copied()
        .collect::<Vec<_>>();
    if set.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fixed public bench rejects filter overrides: {}",
            set.join(", ")
        ))
    }
}

fn write_artifacts(report: &BenchReport, out: &Path) -> Result<(PathBuf, PathBuf), String> {
    fs::create_dir_all(out)
        .map_err(|error| format!("failed to create {}: {error}", out.display()))?;
    let json_path = out.join("dsd-public-quality.json");
    let markdown_path = out.join("dsd-public-quality.md");
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize report: {error}"))?;
    fs::write(&json_path, format!("{json}\n"))
        .map_err(|error| format!("failed to write {}: {error}", json_path.display()))?;
    fs::write(&markdown_path, markdown_summary(report))
        .map_err(|error| format!("failed to write {}: {error}", markdown_path.display()))?;
    Ok((json_path, markdown_path))
}

fn markdown_summary(report: &BenchReport) -> String {
    let mut output = String::new();
    output.push_str("# Public PCM-to-DSD quality measurements\n\n");
    let status = if !report.matrix_complete && report.hard_failure_count > 0 {
        "PARTIAL / FAIL"
    } else if !report.matrix_complete {
        "PARTIAL"
    } else if report.hard_failure_count == 0 {
        "PASS"
    } else {
        "FAIL"
    };
    output.push_str(&format!(
        "Status: **{status}**. Schema `{}`; matrix `{}`; {}/{} canonical production cells completed. Canonical structural failures: {}. Optional diagnostic cells: {}/{} with {} structural failures.\n\n",
        report.schema_version,
        report.matrix_version,
        report.successful_production_cell_count,
        report.canonical_production_cell_count,
        report.hard_failure_count,
        report.successful_diagnostic_cell_count,
        report.attempted_diagnostic_cell_count,
        report.diagnostic_hard_failure_count
    ));
    output.push_str(&format!(
        "Built as `{}`/opt `{}` for `{}` with target CPU `{}`; source/binary snapshot match: `{}`.\n\n",
        report.provenance.build_profile,
        report.provenance.build_opt_level,
        report.provenance.build_target,
        report.provenance.build_target_cpu,
        report.provenance.runtime_source_matches_build,
    ));
    output.push_str(&format!(
        "All dBFS figures use `{}`. Stress SINAD is conventional and includes declared IMD. Declared products, product-excluded residual, and unexpected Blackman-Harris-integrated spurs are shown separately. Density uses a {:.1} ms physical-time window.\n\n",
        report.dbfs_reference,
        report.rolling_density_window_seconds * 1000.0,
    ));
    output.push_str("Rated stress preserves each modulator's production headroom and is not a loudness-matched comparison. Use only `matched_effective_peak` stress rows for direct cross-modulator comparison. `SplitPhase128kE2v3` is the only canonical and scoring Split Phase path; optional `SincExtreme32k` cells are a non-scoring Linear Phase diagnostic limited to modulators that support it. Every production modulator is scored at DSD64, DSD128, and DSD256.\n\n");

    output.push_str("## Split Phase E2v3 production-path scores\n\n");
    output.push_str(&format!("Score system: `{}`. {}. Scores are comparative presentation, not `--check` quality gates.\n\n", report.score_policy.name, report.score_policy.claim));
    if report.score_eligible {
        output.push_str(
            "| Modulator | DSD64 | DSD128 | DSD256 | Rated DSD128 stress qualification |\n",
        );
        output.push_str("| --- | ---: | ---: | ---: | --- |\n");
        for score in &report.production_path_scores {
            let score_for = |rate: &str| {
                score
                    .rates
                    .iter()
                    .find(|score| score.rate == rate)
                    .map(|score| format!("{:.2}", score.total_points))
                    .unwrap_or_else(|| "—".to_string())
            };
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                score.modulator,
                score_for("DSD64"),
                score_for("DSD128"),
                score_for("DSD256"),
                if score.rated_stress_qualified {
                    "PASS"
                } else {
                    "FAIL"
                },
            ));
        }
        output.push_str("\n### Score category detail\n\n");
        output.push_str("| Modulator | Rate | Category | Quality index dB | Anchor dB | Normalized /100 | Awarded points | Maximum |\n");
        output.push_str("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |\n");
        for score in &report.production_path_scores {
            for rate in &score.rates {
                for category in &rate.categories {
                    output.push_str(&format!(
                        "| {} | {} | {} | {:.4} | {:.4} | {:.2} | {:.2} | {:.0} |\n",
                        score.modulator,
                        rate.rate,
                        category.category,
                        category.quality_index_db,
                        category.quality_index_anchor_db,
                        category.normalized_score,
                        category.awarded_points,
                        category.maximum_points,
                    ));
                }
            }
        }
    } else {
        output.push_str("Scores were withheld because the complete healthy 28-cell canonical Split Phase E2v3 matrix was not available. Optional diagnostic cells do not affect eligibility.\n");
    }
    output.push('\n');

    output.push_str("## Structural coverage\n\n");
    output.push_str(
        "| Scenario | Filter role | Filter | Rate | Modulator | Comparison class | Effective peak | Full density dev. | Health |\n",
    );
    output.push_str("| --- | --- | --- | --- | --- | --- | ---: | ---: | --- |\n");
    for cell in &report.cells {
        let full_density_deviation = cell
            .full_fixture_density
            .iter()
            .map(|density| density.deviation)
            .fold(0.0, f64::max);
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {:.6} | {:.6} | {} |\n",
            cell.scenario,
            if cell.diagnostic {
                "linear diagnostic"
            } else if cell.production_default_filter {
                "production default"
            } else {
                "noncanonical"
            },
            cell.filter,
            cell.dsd_rate,
            cell.modulator,
            cell.comparison_class,
            cell.effective_source_peak,
            full_density_deviation,
            if cell.hard_failures.is_empty() {
                "PASS"
            } else {
                "FAIL"
            },
        ));
    }

    output.push_str("\n## Coherent level sweep\n\n");
    output.push_str("| Filter | Rate | Modulator | Channel | Source dBFS | Effective dBFS | SINAD dB | Gain error dB | Unexpected spur dBFS | Residual dBFS |\n");
    output.push_str("| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for cell in &report.cells {
        let ScenarioMeasurements::LevelSweep { segments } = &cell.measurements else {
            continue;
        };
        for segment in segments {
            for channel in &segment.channels {
                output.push_str(&format!(
                    "| {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.4} | {:.2} | {:.2} |\n",
                    cell.filter,
                    cell.dsd_rate,
                    cell.modulator,
                    channel.channel,
                    segment.source_level_dbfs,
                    segment.effective_level_dbfs,
                    channel.metrics.sinad_db,
                    channel.metrics.carrier.gain_error_db,
                    channel.metrics.worst_nonharmonic_spur.level_dbfs,
                    channel.metrics.residual_noise_dbfs,
                ));
            }
        }
    }

    output.push_str("\n## Idle, tiny DC, and tiny tone\n\n");
    output.push_str("| Filter | Modulator | Section | Channel | Noise dBFS | Unexpected spur dBFS | Expected DC | Measured DC | DC error | Density dev. |\n");
    output.push_str("| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for cell in &report.cells {
        let ScenarioMeasurements::IdleTinySignal { sections } = &cell.measurements else {
            continue;
        };
        for section in sections {
            for channel in &section.channels {
                output.push_str(&format!(
                    "| {} | {} | {} | {} | {:.2} | {:.2} | {} | {:.9e} | {} | {:.6} |\n",
                    cell.filter,
                    cell.modulator,
                    section.name,
                    channel.channel,
                    channel.noise.integrated_noise_dbfs,
                    channel.noise.worst_spur.level_dbfs,
                    fmt_scientific_option(channel.expected_dc),
                    channel.reconstructed_dc,
                    fmt_scientific_option(channel.dc_error),
                    channel.density.deviation,
                ));
            }
        }
    }

    output.push_str("\n## High-frequency stress spectral metrics\n\n");
    output.push_str("| Filter | Modulator | Input contract | Phase | Channel | Conventional SINAD dB | Worst declared product | Product-excluded residual dBFS | Unexpected spur dBFS |\n");
    output.push_str("| --- | --- | --- | --- | --- | ---: | --- | ---: | ---: |\n");
    for cell in &report.cells {
        let ScenarioMeasurements::HighFrequencyStress {
            level_contract,
            channels,
        } = &cell.measurements
        else {
            continue;
        };
        for channel in channels {
            for (phase, metrics) in [("steady", &channel.steady), ("recovery", &channel.recovery)] {
                output.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {:.2} | {} | {:.2} | {:.2} |\n",
                    cell.filter,
                    cell.modulator,
                    level_contract,
                    phase,
                    channel.channel,
                    metrics.sinad_db,
                    fmt_measured_tone(metrics.worst_declared_product.as_ref()),
                    metrics.residual_excluding_declared_products_dbfs,
                    metrics.worst_unexpected_spur.level_dbfs,
                ));
            }
        }
    }

    output.push_str("\n## High-frequency stress transitions\n\n");
    output.push_str("| Filter | Modulator | Input contract | Channel | Settled peak | Waveform peak | Excess | Zero-input transition peak dBFS | Clean mute peak/RMS dBFS | Restart residual peak dBFS | Restart RMS 1/10/50 ms dBFS | Recovery ms |\n");
    output.push_str(
        "| --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- | ---: | --- | ---: |\n",
    );
    for cell in &report.cells {
        let ScenarioMeasurements::HighFrequencyStress {
            level_contract,
            channels,
        } = &cell.measurements
        else {
            continue;
        };
        for channel in channels {
            output.push_str(&format!(
                "| {} | {} | {} | {} | {:.6} | {:.6} | {:.6} | {:.2} | {:.2} / {:.2} | {:.2} | {:.2} / {:.2} / {:.2} | {} |\n",
                cell.filter,
                cell.modulator,
                level_contract,
                channel.channel,
                channel.settled_program_peak,
                channel.transition_waveform_peak,
                channel.transition_overshoot_above_settled,
                channel.zero_input_transition_peak_dbfs,
                channel.clean_mute_peak_dbfs,
                channel.clean_mute_rms_dbfs,
                channel.restart_residual_peak_dbfs,
                channel.restart_residual_rms_1ms_dbfs,
                channel.restart_residual_rms_10ms_dbfs,
                channel.restart_residual_rms_50ms_dbfs,
                fmt_option(channel.end_to_end_recovery_time_ms),
            ));
        }
    }

    output.push_str("\n## Hi-res reconstruction carriers\n\n");
    output.push_str("| Filter | Modulator | Channel | Carrier | Frequency Hz | Gain error dB |\n");
    output.push_str("| --- | --- | --- | --- | ---: | ---: |\n");
    for cell in &report.cells {
        let ScenarioMeasurements::HiresReconstruction { channels } = &cell.measurements else {
            continue;
        };
        for channel in channels {
            for carrier in &channel.metrics.carriers {
                output.push_str(&format!(
                    "| {} | {} | {} | {} | {:.3} | {:.4} |\n",
                    cell.filter,
                    cell.modulator,
                    channel.channel,
                    carrier.name,
                    carrier.frequency_hz,
                    carrier.gain_error_db,
                ));
            }
        }
    }

    output.push_str("\n## Hi-res reconstruction bands\n\n");
    output.push_str(
        "| Filter | Modulator | Channel | Band Hz | Residual dBFS | Unexpected spur dBFS |\n",
    );
    output.push_str("| --- | --- | --- | --- | ---: | ---: |\n");
    for cell in &report.cells {
        let ScenarioMeasurements::HiresReconstruction { channels } = &cell.measurements else {
            continue;
        };
        for channel in channels {
            for band in &channel.metrics.bands {
                output.push_str(&format!(
                    "| {} | {} | {} | {:.0}–{:.0} | {:.2} | {:.2} |\n",
                    cell.filter,
                    cell.modulator,
                    channel.channel,
                    band.low_hz,
                    band.high_hz,
                    band.residual_dbfs,
                    band.worst_unexpected_spur.level_dbfs,
                ));
            }
        }
    }

    if !report.execution_failures.is_empty()
        || report
            .cells
            .iter()
            .any(|cell| !cell.hard_failures.is_empty())
        || !report.matrix_complete
    {
        output.push_str("\n## Structural failures\n\n");
        if !report.matrix_complete {
            output.push_str("- Canonical matrix was incomplete; this report cannot receive canonical PASS status.\n");
        }
        for failure in &report.execution_failures {
            output.push_str(&format!("- {failure}\n"));
        }
        for cell in &report.cells {
            for failure in &cell.hard_failures {
                output.push_str(&format!(
                    "- {} {} {} {}: {}\n",
                    cell.scenario, cell.filter, cell.dsd_rate, cell.modulator, failure
                ));
            }
        }
    }
    output
}

fn fmt_measured_tone(tone: Option<&analysis::MeasuredTone>) -> String {
    tone.map_or_else(
        || "—".to_string(),
        |tone| format!("{} ({:.2} dBFS)", tone.name, tone.level_dbfs),
    )
}

fn fmt_scientific_option(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("{value:.9e}"))
}

fn provenance() -> Provenance {
    let source_snapshot_sha256 = source_snapshot_sha256();
    Provenance {
        git_commit: command_output("git", &["rev-parse", "HEAD"]),
        working_tree_dirty: command_output("git", &["status", "--porcelain"])
            .map(|status| !status.is_empty()),
        runtime_source_matches_build: source_snapshot_sha256
            .as_deref()
            .is_some_and(|digest| digest == env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256")),
        source_snapshot_sha256,
        rustc_version: command_output("rustc", &["--version"]),
        target_os: env::consts::OS,
        target_arch: env::consts::ARCH,
        cpu_class: cpu_class(),
        launch_rustflags: env::var("RUSTFLAGS").ok().filter(|value| !value.is_empty()),
        build_provenance_schema: env!("FOZMO_BUILD_PROVENANCE_SCHEMA"),
        build_profile: env!("FOZMO_BUILD_PROFILE"),
        build_opt_level: env!("FOZMO_BUILD_OPT_LEVEL"),
        build_debug_assertions: env!("FOZMO_BUILD_DEBUG_ASSERTIONS") == "true",
        build_target: env!("FOZMO_BUILD_TARGET"),
        build_host: env!("FOZMO_BUILD_HOST"),
        build_target_cpu: env!("FOZMO_BUILD_TARGET_CPU"),
        build_native_cpu_requested: env!("FOZMO_BUILD_NATIVE_CPU_REQUESTED") == "true",
        build_rustc_version: env!("FOZMO_BUILD_RUSTC_VERSION"),
        build_rustflags: env!("FOZMO_BUILD_RUSTFLAGS_DISPLAY"),
        build_encoded_rustflags_hex: env!("FOZMO_BUILD_ENCODED_RUSTFLAGS_HEX"),
        build_target_features: env!("FOZMO_BUILD_TARGET_FEATURES"),
        build_target_features_hex: env!("FOZMO_BUILD_TARGET_FEATURES_HEX"),
        build_cargo_features: env!("FOZMO_BUILD_CARGO_FEATURES"),
        build_git_commit: env!("FOZMO_BUILD_GIT_COMMIT"),
        build_git_dirty: match env!("FOZMO_BUILD_GIT_DIRTY") {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        build_source_snapshot_schema: env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SCHEMA"),
        build_source_snapshot_sha256: env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256"),
        executable_sha256: executable_sha256(),
    }
}

fn executable_sha256() -> Option<String> {
    let executable = env::current_exe().ok()?;
    let bytes = fs::read(executable).ok()?;
    Some(sha256_bytes(&bytes))
}

fn source_snapshot_sha256() -> Option<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut relative_files = vec![
        PathBuf::from("Cargo.toml"),
        PathBuf::from("Cargo.lock"),
        PathBuf::from("build.rs"),
        PathBuf::from("src/lib.rs"),
        PathBuf::from("audio_tests/dsd_public_quality.rs"),
        PathBuf::from("audio_tests/dsd_public/analysis.rs"),
        PathBuf::from("audio_tests/dsd_public/signals.rs"),
    ];
    let mut directories = vec![PathBuf::from("src/audio")];
    while let Some(relative_directory) = directories.pop() {
        for entry in fs::read_dir(root.join(&relative_directory)).ok()? {
            let entry = entry.ok()?;
            let absolute = entry.path();
            let relative = absolute.strip_prefix(root).ok()?.to_path_buf();
            if absolute.is_dir() {
                directories.push(relative);
            } else if absolute
                .extension()
                .is_some_and(|extension| extension == "rs")
            {
                relative_files.push(relative);
            }
        }
    }
    relative_files.sort();
    relative_files.dedup();

    let mut digest = Sha256::new();
    digest.update(b"fozmo-dsd-public-source-snapshot-v2\0");
    for relative in relative_files {
        let path = relative.to_string_lossy();
        let bytes = fs::read(root.join(&relative)).ok()?;
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Some(format!("{:x}", digest.finalize()))
}

fn source_pcm_sha256(signal: &signals::StereoSignal) -> String {
    let mut digest = Sha256::new();
    digest.update(b"fozmo-dsd-public-pcm-v1\0");
    digest.update(signal.id.as_bytes());
    digest.update(signal.sample_rate_hz.to_le_bytes());
    digest.update(signal.headroom_db.to_bits().to_le_bytes());
    digest.update((signal.frames() as u64).to_le_bytes());
    for channel in [&signal.left, &signal.right] {
        for sample in channel {
            digest.update(sample.to_bits().to_le_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn whole_density_metrics(bytes: &[u8]) -> WholeDensityMetrics {
    let bits = bytes.len().saturating_mul(8);
    let ones = bytes
        .iter()
        .map(|byte| byte.count_ones() as usize)
        .sum::<usize>();
    let density = if bits == 0 {
        f64::NAN
    } else {
        ones as f64 / bits as f64
    };
    WholeDensityMetrics {
        bits,
        density,
        deviation: (density - 0.5).abs(),
    }
}

fn cpu_class() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
    }
    #[cfg(not(target_os = "macos"))]
    {
        env::var("PROCESSOR_IDENTIFIER").ok()
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn push_nonzero(failures: &mut Vec<String>, label: &str, value: u64) {
    if value != 0 {
        failures.push(format!("{label}: {value}"));
    }
}

fn fmt_option(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("{value:.2}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matrix_has_the_declared_twenty_eight_split_cells() {
        let selected = vec![
            DsdModulator::Standard,
            DsdModulator::EcDepth2,
            DsdModulator::EcBeam,
            DsdModulator::EcBeam2,
        ];
        let matrix = build_matrix(
            &selected,
            false,
            DEFAULT_FILTER_TYPE,
            &[DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256],
        );
        assert_eq!(matrix.len(), CANONICAL_PRODUCTION_CELL_COUNT);
        assert!(canonical_selection(&selected));
        for (scenario, expected) in [
            (Scenario::LevelSweep, 12),
            (Scenario::IdleTinySignal, 4),
            (Scenario::HighFrequencyRatedStress, 4),
            (Scenario::HighFrequencyMatchedStress, 4),
            (Scenario::HiresReconstruction, 4),
        ] {
            assert_eq!(
                matrix
                    .iter()
                    .filter(|cell| cell.scenario == scenario)
                    .count(),
                expected,
                "{}",
                scenario.as_name()
            );
        }
        assert!(
            matrix
                .iter()
                .all(|cell| { cell.filter == DEFAULT_FILTER_TYPE && !cell.diagnostic })
        );

        let ecbeam2 = matrix
            .iter()
            .filter(|cell| cell.modulator == DsdModulator::EcBeam2)
            .collect::<Vec<_>>();
        assert_eq!(ecbeam2.len(), 7);
        assert_eq!(
            ecbeam2
                .iter()
                .filter(|cell| cell.scenario == Scenario::LevelSweep)
                .map(|cell| cell.dsd_rate)
                .collect::<Vec<_>>(),
            vec![DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256]
        );
        assert!(ecbeam2.iter().any(|cell| {
            cell.scenario == Scenario::HiresReconstruction && cell.dsd_rate == DsdRate::Dsd256
        }));
    }

    #[test]
    fn linear_reference_adds_twenty_one_legacy_diagnostic_cells() {
        let selected = vec![
            DsdModulator::Standard,
            DsdModulator::EcDepth2,
            DsdModulator::EcBeam,
            DsdModulator::EcBeam2,
        ];
        let matrix = build_matrix(
            &selected,
            true,
            DEFAULT_FILTER_TYPE,
            &[DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256],
        );
        assert_eq!(matrix.len(), 49);
        assert_eq!(
            matrix
                .iter()
                .filter(|cell| cell.filter == DEFAULT_FILTER_TYPE && !cell.diagnostic)
                .count(),
            28
        );
        assert_eq!(
            matrix
                .iter()
                .filter(|cell| cell.filter == FilterType::SincExtreme32k && cell.diagnostic)
                .count(),
            21
        );
        assert!(
            matrix
                .iter()
                .all(|cell| { cell.modulator != DsdModulator::EcBeam2 || !cell.diagnostic })
        );
    }

    #[test]
    fn partial_selection_keeps_only_supported_canonical_cells() {
        let ecbeam2 = build_matrix(
            &[DsdModulator::EcBeam2],
            true,
            DEFAULT_FILTER_TYPE,
            &[DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256],
        );
        assert_eq!(ecbeam2.len(), 7);
        assert!(ecbeam2.iter().all(|cell| !cell.diagnostic));
        assert!(!canonical_selection(&[DsdModulator::EcBeam2]));

        let standard = build_matrix(
            &[DsdModulator::Standard],
            false,
            DEFAULT_FILTER_TYPE,
            &[DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256],
        );
        assert_eq!(standard.len(), 7);
        assert!(
            standard
                .iter()
                .all(|cell| cell.modulator == DsdModulator::Standard)
        );
    }

    #[test]
    fn rate_comparison_adds_a_noncanonical_dsd128_hires_cell_per_selected_modulator() {
        let selected = [DsdModulator::Standard, DsdModulator::EcBeam2];
        let mut matrix = build_matrix(
            &selected,
            false,
            DEFAULT_FILTER_TYPE,
            &[DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256],
        );
        let production_cells = matrix.len();
        append_rate_comparison_diagnostics(&mut matrix, &selected, DEFAULT_FILTER_TYPE);
        assert_eq!(matrix.len(), production_cells + selected.len());
        let diagnostics = matrix
            .iter()
            .filter(|cell| cell.diagnostic)
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), selected.len());
        assert!(diagnostics.iter().all(|cell| {
            cell.scenario == Scenario::HiresReconstruction
                && cell.source_rate == signals::SOURCE_RATE_176K4_HZ
                && cell.dsd_rate == DsdRate::Dsd128
                && cell.filter == DEFAULT_FILTER_TYPE
                && comparison_class(**cell) == "rate_comparison_level_matched"
        }));
    }

    #[test]
    fn parser_accepts_each_production_modulator() {
        assert_eq!(
            parse_modulators("Standard,EcBeam2").unwrap(),
            vec![DsdModulator::Standard, DsdModulator::EcBeam2]
        );
        assert!(parse_modulators("EcDepth4").is_err());
        assert_eq!(parse_filter("Split128k").unwrap(), FilterType::Split128k);
        assert_eq!(
            parse_filter("SplitPhase128kE2v3").unwrap(),
            FilterType::SplitPhase128kE2v3
        );
        assert!(parse_filter("Linear").is_err());
        assert_eq!(
            parse_rates("64,128").unwrap(),
            vec![DsdRate::Dsd64, DsdRate::Dsd128]
        );
        assert!(parse_rates("").is_err());
        assert!(
            Cli::try_parse_from([
                "dsd_public_quality",
                "--filter",
                "SplitPhase128kE2v3",
                "--rates",
                "64,128"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["dsd_public_quality", "--include-linear-reference"]).is_ok());
        assert!(Cli::try_parse_from(["dsd_public_quality", "--include-rate-comparison"]).is_ok());
        assert!(Cli::try_parse_from(["dsd_public_quality", "--dsd256-only"]).is_ok());
    }

    #[test]
    fn rated_headroom_and_filter_policy_are_explicit() {
        assert_eq!(headroom_db(DsdModulator::Standard), -4.0);
        assert_eq!(headroom_db(DsdModulator::EcDepth2), -4.0);
        assert_eq!(headroom_db(DsdModulator::EcBeam), -2.0);
        assert_eq!(headroom_db(DsdModulator::EcBeam2), -2.0);
        assert_eq!(DEFAULT_FILTER_TYPE, FilterType::SplitPhase128kE2v3);
        assert_eq!(
            filter_guard_frames(FilterType::SincExtreme32k),
            LINEAR_FILTER_GUARD_FRAMES
        );
        assert_eq!(
            filter_guard_frames(FilterType::Split128k),
            SPLIT_FILTER_GUARD_FRAMES
        );
        assert_eq!(
            filter_guard_frames(FilterType::SplitPhase128kE2v3),
            SPLIT_FILTER_GUARD_FRAMES
        );
        assert_eq!(
            filter_guard_frames(FilterType::SmoothPhase128k),
            LINEAR_FILTER_GUARD_FRAMES
        );
    }

    #[test]
    fn e2v3_dsd64_dsd128_matrix_has_expected_public_bench_cells() {
        let matrix = build_matrix(
            &[DsdModulator::Standard, DsdModulator::EcBeam2],
            false,
            FilterType::SplitPhase128kE2v3,
            &[DsdRate::Dsd64, DsdRate::Dsd128],
        );
        assert_eq!(matrix.len(), 10);
        assert!(matrix.iter().all(|cell| {
            cell.filter == FilterType::SplitPhase128kE2v3
                && matches!(cell.dsd_rate, DsdRate::Dsd64 | DsdRate::Dsd128)
                && !cell.diagnostic
        }));
        assert_eq!(
            matrix
                .iter()
                .filter(|cell| cell.scenario == Scenario::LevelSweep)
                .count(),
            4
        );
        assert_eq!(
            matrix
                .iter()
                .filter(|cell| cell.scenario == Scenario::IdleTinySignal)
                .count(),
            2
        );
        assert_eq!(
            matrix
                .iter()
                .filter(|cell| matches!(
                    cell.scenario,
                    Scenario::HighFrequencyRatedStress | Scenario::HighFrequencyMatchedStress
                ))
                .count(),
            4
        );
    }

    #[test]
    fn partial_selections_are_never_canonical() {
        let production = [
            DsdModulator::Standard,
            DsdModulator::EcDepth2,
            DsdModulator::EcBeam,
            DsdModulator::EcBeam2,
        ];
        assert!(canonical_selection(&production));
        assert!(!canonical_selection(&[
            DsdModulator::Standard,
            DsdModulator::EcBeam
        ]));
    }

    #[test]
    fn canonical_build_rejects_explicit_target_feature_disables() {
        #[cfg(feature = "default")]
        assert_eq!(env!("FOZMO_BUILD_CARGO_FEATURES"), CANONICAL_CARGO_FEATURES);
        assert!(!rustflags_disable_target_features("-C target-cpu=native"));
        assert!(!rustflags_disable_target_features(
            "-Ctarget-cpu=native -Ctarget-feature=+aes,+neon"
        ));
        assert!(rustflags_disable_target_features(
            "-C target-cpu=native -C target-feature=-avx2"
        ));
        assert!(rustflags_disable_target_features(
            "-Ctarget-feature=+fma,-avx2"
        ));
    }

    #[test]
    fn source_range_maps_with_the_actual_wire_ratio() {
        assert_eq!(
            source_to_bit_range(FilterType::SincExtreme32k, 10..20, 176_400, 11_289_600,).unwrap(),
            640..1280
        );
    }

    #[test]
    fn diagnostic_failures_never_enter_the_canonical_check_count() {
        let (production, diagnostic) = structural_failure_counts([(false, 2), (true, 4)], 1, 2);
        assert_eq!(production, 3);
        assert_eq!(diagnostic, 6);
    }

    #[test]
    fn score_normalization_loses_one_point_per_decibel_below_anchor() {
        let at_anchor = category_score("test", 40.0, 123.0, 123.0);
        assert_eq!(at_anchor.normalized_score, 100.0);
        assert_eq!(at_anchor.awarded_points, 40.0);

        let ten_db_down = category_score("test", 40.0, 113.0, 123.0);
        assert_eq!(ten_db_down.normalized_score, 90.0);
        assert_eq!(ten_db_down.awarded_points, 36.0);

        let above_anchor = category_score("test", 40.0, 130.0, 123.0);
        assert_eq!(above_anchor.normalized_score, 100.0);
    }

    #[test]
    fn score_policy_weights_each_rate_to_one_hundred_points() {
        let policy = score_policy();
        for rate in ["DSD64", "DSD128", "DSD256"] {
            let points = policy
                .categories
                .iter()
                .filter(|category| category.rate == rate)
                .map(|category| category.maximum_points)
                .sum::<f64>();
            assert!((points - 100.0).abs() < 1.0e-12, "{rate}: {points}");
        }
    }

    #[test]
    fn provenance_source_snapshot_is_a_sha256_digest() {
        let digest = source_snapshot_sha256().expect("source snapshot should be readable");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(digest, env!("FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256"));
    }

    #[test]
    fn whole_density_is_fast_and_exact() {
        let density = whole_density_metrics(&[0b1111_0000, 0b1010_1010]);
        assert_eq!(density.bits, 16);
        assert!((density.density - 0.5).abs() < 1.0e-12);
        assert!(density.deviation < 1.0e-12);
    }

    #[test]
    fn all_spectral_windows_have_the_published_fft_length() {
        assert_eq!(
            signals::LEVEL_SWEEP_ANALYZE_FRAMES
                * (AUDIO_BAND.output_rate / signals::SOURCE_RATE_44K1_HZ) as usize,
            SPECTRAL_ANALYSIS_FRAMES
        );
        assert_eq!(
            signals::IDLE_ANALYZE_FRAMES
                * (AUDIO_BAND.output_rate / signals::SOURCE_RATE_44K1_HZ) as usize,
            SPECTRAL_ANALYSIS_FRAMES
        );
        assert_eq!(
            signals::STRESS_STEADY_ANALYZE_FRAMES
                * (AUDIO_BAND.output_rate / signals::SOURCE_RATE_44K1_HZ) as usize,
            SPECTRAL_ANALYSIS_FRAMES
        );
        assert_eq!(
            signals::HIRES_ANALYZE_FRAMES
                * (HIRES_BAND.output_rate / signals::SOURCE_RATE_176K4_HZ) as usize,
            SPECTRAL_ANALYSIS_FRAMES
        );
    }

    #[test]
    fn transition_metric_removes_programme_and_detects_restart_overshoot() {
        let sample_rate = 10_000;
        let frequency = 100.0;
        let mut samples = (0..3_000)
            .map(|index| {
                0.5 * (2.0 * std::f64::consts::PI * frequency * index as f64 / sample_rate as f64)
                    .sin()
            })
            .collect::<Vec<_>>();
        samples[1_000..1_500].fill(0.0);
        samples[1_500] += 1.0;
        let transition = analyze_stress_transition(
            &samples[..1_000],
            &samples[1_000..],
            sample_rate,
            500,
            100..400,
            1_500..2_000,
            &[frequency],
        )
        .unwrap();
        assert!(transition.zero_input_transition_peak < 1.0e-12);
        assert!(transition.clean_mute_peak < 1.0e-12);
        assert!((transition.restart_residual_peak - 1.0).abs() < 1.0e-10);
        assert!((transition.transition_overshoot_above_settled - 0.5).abs() < 1.0e-10);
        assert!(transition.restart_residual_rms_1ms_dbfs.is_finite());
        assert!(transition.restart_residual_rms_50ms_dbfs.is_finite());
    }
}
