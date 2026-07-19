#[path = "ecbeam2/harness.rs"]
mod harness;

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use fozmo::audio::dsd::delta_sigma::{
    DitherPrng, DitherShape, DsdModulator, Ec2LongFilterPolicy, Ec2PolicyWeights,
    EcBeam2ExperimentConfig, EcBeam2ProfileId, EcBeam2RunMode, EcFutureScorer,
};
use fozmo::audio::dsd::dsd_render::DsdRate;
use fozmo::audio::dsp::resampler::FilterType;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const SOURCE_RATE: u32 = 44_100;
const CANDIDATE_SCHEMA_VERSION: &str = "dsd-candidate-schema-v1";
const DEFAULT_RANKING_CELL_CAP: usize = 1_500;

#[derive(Clone, Copy)]
enum Ec2SweepRate {
    Dsd64,
    Dsd128,
    Dsd256,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return ExitCode::SUCCESS;
    }
    let out_dir = match parse_path_arg(&args, "--out") {
        Ok(path) => path,
        Err(err) => return exit_arg_error(err),
    };

    if args.iter().any(|arg| arg == "--selectable-dsd-matrix") {
        return run_selectable_dsd_matrix_cli(&args, out_dir);
    }
    if args.iter().any(|arg| arg == "--ecbeam2-qualification") {
        return run_ecbeam2_qualification_cli(&args, out_dir);
    }
    exit_arg_error("expected --ecbeam2-qualification or --selectable-dsd-matrix".to_string())
}

fn print_usage() {
    println!(
        "usage: ecbeam2_quality --ecbeam2-qualification --mode scale-probe|stability|budget [--source-rates 44100,48000] [--filters MinimumPhase,SplitPhase] --candidate-config PATH --corpus-manifest PATH --out PATH\n\
         ecbeam2_quality --selectable-dsd-matrix [--selectable-filter LIST] [--selectable-modulator EcBeam|EcBeam2] [--source-rates 44100,48000] --rates 64 --candidate-config PATH --ecbeam2-corpus-manifest PATH --out PATH"
    );
}

fn run_ecbeam2_qualification_cli(args: &[String], out_dir: Option<PathBuf>) -> ExitCode {
    if let Err(err) = reject_unknown_args(
        args,
        &[
            "--ecbeam2-qualification",
            "--mode",
            "--source-rates",
            "--filters",
            "--candidate-config",
            "--modulator",
            "--corpus-manifest",
            "--allow-exploratory",
            "--out",
        ],
    ) {
        return exit_arg_error(err);
    }
    let mode = match parse_required_value_arg(args, "--mode") {
        Ok(mode) if matches!(mode.as_str(), "scale-probe" | "stability" | "budget") => mode,
        Ok(mode) => {
            return exit_arg_error(format!(
                "--mode must be scale-probe, stability, or budget, got {mode}"
            ));
        }
        Err(err) => return exit_arg_error(err),
    };
    let source_rates = if args.iter().any(|arg| arg == "--source-rates") {
        match parse_source_rates_arg(args, "--source-rates") {
            Ok(rates) => rates,
            Err(err) => return exit_arg_error(err),
        }
    } else {
        vec![44_100, 48_000]
    };
    let filter_args = args
        .iter()
        .map(|arg| {
            if arg == "--filters" {
                "--selectable-filter".to_string()
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>();
    let filters = if args.iter().any(|arg| arg == "--filters") {
        match parse_selectable_filters_arg(&filter_args) {
            Ok(filters) => filters,
            Err(err) => return exit_arg_error(err.replace("--selectable-filter", "--filters")),
        }
    } else {
        vec![
            harness::SelectableDsdFilter {
                name: "MinimumPhase",
                filter: FilterType::Minimum16k,
            },
            harness::SelectableDsdFilter {
                name: "SplitPhase",
                filter: FilterType::Split128k,
            },
        ]
    };
    let candidate_path = match parse_required_path_arg(args, "--candidate-config") {
        Ok(path) => path,
        Err(err) => return exit_arg_error(err),
    };
    let modulator = match parse_candidate_modulator_arg(args, "--modulator") {
        Ok(Some(modulator @ (DsdModulator::EcBeam | DsdModulator::EcBeam2))) => modulator,
        Ok(Some(_)) => {
            return exit_arg_error("--modulator must be EcBeam or EcBeam2".to_string());
        }
        Ok(None) => DsdModulator::EcBeam2,
        Err(err) => return exit_arg_error(err),
    };
    let corpus_path = match parse_required_path_arg(args, "--corpus-manifest") {
        Ok(path) => path,
        Err(err) => return exit_arg_error(err),
    };
    let Some(out_dir) = out_dir else {
        return exit_arg_error("--ecbeam2-qualification requires --out".to_string());
    };
    let config = match candidate_config_arg(
        harness::DsdExperimentConfig::default(),
        &[DsdRate::Dsd64],
        args,
    ) {
        Ok(config) => config,
        Err(err) => return exit_arg_error(err),
    };
    let hash_file = |path: &std::path::Path| -> Result<String, String> {
        fs::read(path)
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .map_err(|err| format!("failed to hash {}: {err}", path.display()))
    };
    let candidate_config_sha256 = match hash_file(&candidate_path) {
        Ok(digest) => Some(digest),
        Err(err) => return exit_arg_error(err),
    };
    let binary_sha256 = env::current_exe()
        .ok()
        .and_then(|path| hash_file(&path).ok());
    match harness::run_ecbeam2_qualification(
        &mode,
        &corpus_path,
        &filters,
        &source_rates,
        modulator,
        config,
        binary_sha256,
        candidate_config_sha256,
    ) {
        Ok(report) => {
            if let Err(err) = harness::write_ecbeam2_qualification_artifact(&report, &out_dir) {
                eprintln!("ecbeam2_quality: failed to write EcBeam2 qualification: {err}");
                return ExitCode::from(1);
            }
            println!(
                "wrote {} lightweight EcBeam2 measurements to {}",
                report.measurements.len(),
                out_dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("ecbeam2_quality: EcBeam2 qualification failed: {err}");
            ExitCode::from(1)
        }
    }
}

fn exit_arg_error(err: String) -> ExitCode {
    eprintln!("ecbeam2_quality: {err}");
    ExitCode::from(2)
}

fn run_selectable_dsd_matrix_cli(args: &[String], out_dir: Option<PathBuf>) -> ExitCode {
    if let Err(err) = reject_unknown_args(
        args,
        &[
            "--selectable-dsd-matrix",
            "--selectable-filter",
            "--selectable-modulator",
            "--source-rates",
            "--rates",
            "--candidate-config",
            "--ecbeam2-corpus-manifest",
            "--allow-exploratory",
            "--budget-cell-cap",
            "--out",
        ],
    ) {
        return exit_arg_error(err);
    }

    let filters = match parse_selectable_filters_arg(args) {
        Ok(filters) => filters,
        Err(err) => return exit_arg_error(err),
    };
    let modulators = match parse_selectable_modulators_arg(args) {
        Ok(modulators) => modulators,
        Err(err) => return exit_arg_error(err),
    };
    let rates = match parse_selectable_dsd_rates_arg(args) {
        Ok(rates) => rates,
        Err(err) => return exit_arg_error(err),
    };
    let source_rates = match parse_source_rates_arg(args, "--source-rates") {
        Ok(rates) => rates,
        Err(err) => return exit_arg_error(err),
    };
    if modulators
        .iter()
        .any(|modulator| !matches!(modulator, DsdModulator::EcBeam | DsdModulator::EcBeam2))
    {
        return exit_arg_error(
            "EcBeam2 selectable runs support only EcBeam and EcBeam2".to_string(),
        );
    }
    if rates != [DsdRate::Dsd64] {
        return exit_arg_error("EcBeam2 selectable runs require --rates 64".to_string());
    }
    let config = match candidate_config_arg(harness::DsdExperimentConfig::default(), &rates, args) {
        Ok(config) => config,
        Err(err) => return exit_arg_error(err),
    };
    let corpus_manifest = match parse_path_arg(args, "--ecbeam2-corpus-manifest") {
        Ok(path) => path,
        Err(err) => return exit_arg_error(err),
    };
    if corpus_manifest.is_some() && out_dir.is_none() {
        return exit_arg_error(
            "--ecbeam2-corpus-manifest requires --out so native corpus evidence is persisted"
                .to_string(),
        );
    }
    if let Err(err) = enforce_budget(
        "selectable-dsd-matrix",
        filters.len() * rates.len() * modulators.len() * source_rates.len(),
        args,
    ) {
        return exit_arg_error(err);
    }

    match harness::run_selectable_dsd_matrix_with_config_and_source_rates(
        &filters,
        &rates,
        &modulators,
        &source_rates,
        config,
    ) {
        Ok(mut report) => {
            if let Some(manifest_path) = corpus_manifest.as_deref() {
                let corpus = match harness::run_ecbeam2_corpus_manifest(
                    manifest_path,
                    &filters,
                    &rates,
                    &modulators,
                    &source_rates,
                    config,
                ) {
                    Ok(report) => report,
                    Err(err) => {
                        eprintln!("ecbeam2_quality: EcBeam2 corpus execution failed: {err}");
                        return ExitCode::from(1);
                    }
                };
                if let Err(err) = harness::apply_ecbeam2_corpus_report(&mut report, &corpus) {
                    eprintln!("ecbeam2_quality: EcBeam2 corpus integration failed: {err}");
                    return ExitCode::from(1);
                }
                if let Some(out_dir) = out_dir.as_deref()
                    && let Err(err) = harness::write_ecbeam2_corpus_artifacts(&corpus, out_dir)
                {
                    eprintln!("ecbeam2_quality: failed to write EcBeam2 corpus evidence: {err}");
                    return ExitCode::from(1);
                }
                if !corpus.hard_failures.is_empty() {
                    eprintln!(
                        "ecbeam2_quality: EcBeam2 corpus recorded {} hard failure(s)",
                        corpus.hard_failures.len()
                    );
                }
            }
            harness::print_console_report(&report);
            if let Some(out_dir) = out_dir {
                if let Err(err) = harness::write_report_artifacts(&report, &out_dir) {
                    eprintln!("ecbeam2_quality: failed to write report artifacts: {err}");
                    return ExitCode::from(1);
                }
                println!("wrote report artifacts to {}", out_dir.display());
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("ecbeam2_quality: {err}");
            ExitCode::from(1)
        }
    }
}

fn reject_unknown_args(args: &[String], known: &[impl AsRef<str>]) -> Result<(), String> {
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if !arg.starts_with("--") {
            idx += 1;
            continue;
        }
        if !known.iter().any(|known| known.as_ref() == arg) {
            return Err(format!("unsupported option {arg}"));
        }
        if option_takes_value(arg) {
            idx += 1;
            if idx >= args.len() {
                return Err(format!("{arg} requires a value"));
            }
        }
        idx += 1;
    }
    Ok(())
}

fn option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "--out"
            | "--rates"
            | "--candidate-config"
            | "--ecbeam2-corpus-manifest"
            | "--candidate-label"
            | "--candidate-modulator"
            | "--fixture-manifest"
            | "--baseline-cache"
            | "--target-wall-seconds"
            | "--workers"
            | "--budget-cell-cap"
            | "--ec2-decision-trace-window-bits"
            | "--selectable-filter"
            | "--selectable-modulator"
            | "--source-rates"
            | "--roundtrip-fixtures"
    ) || {
        arg.contains("-input-gain-db")
            || arg.contains("-seed-")
            || arg.contains("-ec-dither-")
            || arg.contains("-ec-dc-corner-hz")
            || arg.contains("-ec-future-scorer")
            || arg.contains("-ec-long-filter-policy")
            || arg.contains("-ec-quantizer-weight")
            || arg.contains("-ec-pressure-weight")
            || arg.contains("-ec-limit-weight")
            || arg.contains("-ec-transition-weight")
            || arg.contains("-ec-dc-weight")
            || arg.contains("-ec-lookahead-discount")
            || arg.contains("-ec-ambiguity-margin")
            || arg.contains("-ec-pressure-taper-start")
            || arg.contains("-ec-pressure-taper-strength")
            || arg.contains("-ec-pressure-stage-weights")
            || arg.contains("-ec-gated-dither-margin")
            || arg.contains("-ec-gated-dither-scale")
    }
}

fn set_input_gain(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    gain_db: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_input_gain_db(gain_db),
        Ec2SweepRate::Dsd128 => config.with_dsd128_input_gain_db(gain_db),
        Ec2SweepRate::Dsd256 => config.with_dsd256_input_gain_db(gain_db),
    }
}

fn set_dither_scale(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    scale: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_dither_scale_multiplier(scale),
        Ec2SweepRate::Dsd128 => config.with_dsd128_dither_scale_multiplier(scale),
        Ec2SweepRate::Dsd256 => config.with_dsd256_dither_scale_multiplier(scale),
    }
}

fn set_dither_shape(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    shape: DitherShape,
) -> harness::DsdExperimentConfig {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_dither_shape(shape),
        Ec2SweepRate::Dsd128 => config.with_dsd128_dither_shape(shape),
        Ec2SweepRate::Dsd256 => config.with_dsd256_dither_shape(shape),
    }
}

fn set_dither_prng(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    prng: DitherPrng,
) -> harness::DsdExperimentConfig {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_dither_prng(prng),
        Ec2SweepRate::Dsd128 => config.with_dsd128_dither_prng(prng),
        Ec2SweepRate::Dsd256 => config.with_dsd256_dither_prng(prng),
    }
}

fn set_dither_leak_alpha(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    alpha: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_dither_leak_alpha(alpha),
        Ec2SweepRate::Dsd128 => config.with_dsd128_dither_leak_alpha(alpha),
        Ec2SweepRate::Dsd256 => config.with_dsd256_dither_leak_alpha(alpha),
    }
}

fn set_dither_lf_floor_gamma(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    gamma: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_dither_lf_floor_gamma(gamma),
        Ec2SweepRate::Dsd128 => config.with_dsd128_dither_lf_floor_gamma(gamma),
        Ec2SweepRate::Dsd256 => config.with_dsd256_dither_lf_floor_gamma(gamma),
    }
}

fn set_ec_dc_corner_hz(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    corner_hz: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_ec_dc_corner_hz(corner_hz),
        Ec2SweepRate::Dsd128 => config.with_dsd128_ec_dc_corner_hz(corner_hz),
        Ec2SweepRate::Dsd256 => config.with_dsd256_ec_dc_corner_hz(corner_hz),
    }
}

fn set_future_scorer(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    scorer: EcFutureScorer,
) -> harness::DsdExperimentConfig {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_future_scorer(scorer),
        Ec2SweepRate::Dsd128 => config.with_dsd128_future_scorer(scorer),
        Ec2SweepRate::Dsd256 => config.with_dsd256_future_scorer(scorer),
    }
}

fn set_ec2_policy(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    policy: Ec2LongFilterPolicy,
) -> harness::DsdExperimentConfig {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_ec2_long_filter_policy(policy),
        Ec2SweepRate::Dsd128 => config.with_dsd128_ec2_long_filter_policy(policy),
        Ec2SweepRate::Dsd256 => config.with_dsd256_ec2_long_filter_policy(policy),
    }
}

fn set_ec2_weights(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    weights: Ec2PolicyWeights,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_ec2_policy_weights(weights),
        Ec2SweepRate::Dsd128 => config.with_dsd128_ec2_policy_weights(weights),
        Ec2SweepRate::Dsd256 => config.with_dsd256_ec2_policy_weights(weights),
    }
}

fn set_ec2_pressure_stage_weights(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    weights: [f64; 7],
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_ec2_pressure_stage_weights(weights),
        Ec2SweepRate::Dsd128 => config.with_dsd128_ec2_pressure_stage_weights(weights),
        Ec2SweepRate::Dsd256 => config.with_dsd256_ec2_pressure_stage_weights(weights),
    }
}

fn set_gated_dither(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    margin: f64,
    scale: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_gated_dither(margin, scale),
        Ec2SweepRate::Dsd128 => config.with_dsd128_gated_dither(margin, scale),
        Ec2SweepRate::Dsd256 => config.with_dsd256_gated_dither(margin, scale),
    }
}

fn set_seed_pair(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    left: Option<u64>,
    right: Option<u64>,
) -> harness::DsdExperimentConfig {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_seed_pair(left, right),
        Ec2SweepRate::Dsd128 => config.with_dsd_seed_pair(left, right),
        Ec2SweepRate::Dsd256 => config.with_dsd256_seed_pair(left, right),
    }
}

fn set_ec_beam_search(
    config: harness::DsdExperimentConfig,
    rate: Ec2SweepRate,
    m: usize,
    n: usize,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        Ec2SweepRate::Dsd64 => config.with_dsd64_ec_beam_search(m, n),
        Ec2SweepRate::Dsd128 => config.with_dsd128_ec_beam_search(m, n),
        Ec2SweepRate::Dsd256 => config.with_dsd256_ec_beam_search(m, n),
    }
}

fn sweep_rate_from_dsd_rate(rate: DsdRate) -> Ec2SweepRate {
    match rate {
        DsdRate::Dsd64 => Ec2SweepRate::Dsd64,
        DsdRate::Dsd128 => Ec2SweepRate::Dsd128,
        DsdRate::Dsd256 => Ec2SweepRate::Dsd256,
    }
}

fn set_input_gain_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    gain_db: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    set_input_gain(config, sweep_rate_from_dsd_rate(rate), gain_db)
}

fn set_dither_scale_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    scale: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    set_dither_scale(config, sweep_rate_from_dsd_rate(rate), scale)
}

fn set_dither_shape_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    shape: DitherShape,
) -> Result<harness::DsdExperimentConfig, String> {
    Ok(set_dither_shape(
        config,
        sweep_rate_from_dsd_rate(rate),
        shape,
    ))
}

fn set_dither_prng_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    prng: DitherPrng,
) -> Result<harness::DsdExperimentConfig, String> {
    Ok(set_dither_prng(
        config,
        sweep_rate_from_dsd_rate(rate),
        prng,
    ))
}

fn set_dither_leak_alpha_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    alpha: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    set_dither_leak_alpha(config, sweep_rate_from_dsd_rate(rate), alpha)
}

fn set_dither_lf_floor_gamma_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    gamma: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    set_dither_lf_floor_gamma(config, sweep_rate_from_dsd_rate(rate), gamma)
}

fn set_ec_dc_corner_hz_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    corner_hz: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    set_ec_dc_corner_hz(config, sweep_rate_from_dsd_rate(rate), corner_hz)
}

fn set_future_scorer_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    scorer: EcFutureScorer,
) -> Result<harness::DsdExperimentConfig, String> {
    Ok(set_future_scorer(
        config,
        sweep_rate_from_dsd_rate(rate),
        scorer,
    ))
}

fn set_ec2_policy_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    policy: Ec2LongFilterPolicy,
) -> Result<harness::DsdExperimentConfig, String> {
    Ok(set_ec2_policy(
        config,
        sweep_rate_from_dsd_rate(rate),
        policy,
    ))
}

fn set_ec2_weights_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weights: Ec2PolicyWeights,
) -> Result<harness::DsdExperimentConfig, String> {
    set_ec2_weights(config, sweep_rate_from_dsd_rate(rate), weights)
}

fn set_ec2_pressure_stage_weights_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weights: [f64; 7],
) -> Result<harness::DsdExperimentConfig, String> {
    set_ec2_pressure_stage_weights(config, sweep_rate_from_dsd_rate(rate), weights)
}

fn set_gated_dither_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    margin: f64,
    scale: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    set_gated_dither(config, sweep_rate_from_dsd_rate(rate), margin, scale)
}

fn set_seed_pair_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    left: Option<u64>,
    right: Option<u64>,
) -> harness::DsdExperimentConfig {
    set_seed_pair(config, sweep_rate_from_dsd_rate(rate), left, right)
}

fn set_ec_beam_search_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    m: usize,
    n: usize,
) -> Result<harness::DsdExperimentConfig, String> {
    set_ec_beam_search(config, sweep_rate_from_dsd_rate(rate), m, n)
}

fn set_ec_beam_terminal_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_terminal_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_terminal_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_terminal_weight(weight),
    }
}

fn set_ec_beam_alternation_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_alternation_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_alternation_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_alternation_weight(weight),
    }
}

fn set_ec_beam_alternation_rank_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_alternation_rank_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_alternation_rank_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_alternation_rank_weight(weight),
    }
}

fn set_ec_beam_alternation_threshold_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    threshold: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_alternation_threshold(threshold),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_alternation_threshold(threshold),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_alternation_threshold(threshold),
    }
}

fn set_ec_beam_periodicity_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_periodicity_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_periodicity_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_periodicity_weight(weight),
    }
}

fn set_ec_beam_periodicity_lags_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    lags: &[u8],
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_periodicity_lags(lags),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_periodicity_lags(lags),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_periodicity_lags(lags),
    }
}

fn set_ec_beam_periodicity_window_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    window: usize,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_periodicity_window(window),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_periodicity_window(window),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_periodicity_window(window),
    }
}

fn set_ec_beam_filtered_error_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_filtered_error_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_filtered_error_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_filtered_error_weight(weight),
    }
}

fn set_ec_beam_filtered_error_rank_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_filtered_error_rank_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_filtered_error_rank_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_filtered_error_rank_weight(weight),
    }
}

fn set_ec_beam_reconstruction_error_weight_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    weight: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_reconstruction_error_weight(weight),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_reconstruction_error_weight(weight),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_reconstruction_error_weight(weight),
    }
}

fn set_ec_beam_pressure_deadzone_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    deadzone: f64,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_pressure_deadzone(deadzone),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_pressure_deadzone(deadzone),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_pressure_deadzone(deadzone),
    }
}

fn set_ec_beam_metric_diagnostics_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    enabled: bool,
) -> harness::DsdExperimentConfig {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_metric_diagnostics(enabled),
        DsdRate::Dsd128 => config.with_dsd128_ec_beam_metric_diagnostics(enabled),
        DsdRate::Dsd256 => config.with_dsd256_ec_beam_metric_diagnostics(enabled),
    }
}

fn set_ec_beam_auxiliary_metric_scales_for_dsd_rate(
    config: harness::DsdExperimentConfig,
    rate: DsdRate,
    pressure_accum_scale: Option<f64>,
    pressure_rank_scale: Option<f64>,
    dc_accum_scale: Option<f64>,
    dc_rank_scale: Option<f64>,
) -> Result<harness::DsdExperimentConfig, String> {
    match rate {
        DsdRate::Dsd64 => config.with_dsd64_ec_beam_auxiliary_metric_scales(
            pressure_accum_scale,
            pressure_rank_scale,
            dc_accum_scale,
            dc_rank_scale,
        ),
        DsdRate::Dsd128 | DsdRate::Dsd256 => {
            Err("beam auxiliary metric scales are only wired for DSD64".to_string())
        }
    }
}

fn enforce_budget(label: &str, cells: usize, args: &[String]) -> Result<(), String> {
    let cap = parse_usize_arg(args, "--budget-cell-cap")?.unwrap_or(DEFAULT_RANKING_CELL_CAP);
    eprintln!("ecbeam2_quality: {label} estimated cells {cells} (cap {cap})");
    if cells > cap {
        Err(format!(
            "{label} manifest estimate {cells} cells exceeds cap {cap}; raise --budget-cell-cap to run"
        ))
    } else {
        Ok(())
    }
}

fn parse_usize_arg(args: &[String], flag: &str) -> Result<Option<usize>, String> {
    parse_value_arg(args, flag)?
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| format!("{flag} requires a positive integer"))
        })
        .transpose()
}

fn parse_path_arg(args: &[String], flag: &str) -> Result<Option<PathBuf>, String> {
    Ok(parse_value_arg(args, flag)?.map(PathBuf::from))
}

fn parse_required_value_arg(args: &[String], flag: &str) -> Result<String, String> {
    parse_value_arg(args, flag)?
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{flag} is required"))
}

fn parse_required_path_arg(args: &[String], flag: &str) -> Result<PathBuf, String> {
    parse_path_arg(args, flag)?.ok_or_else(|| format!("{flag} is required"))
}

fn parse_candidate_modulator_arg(
    args: &[String],
    flag: &str,
) -> Result<Option<DsdModulator>, String> {
    let Some(value) = parse_value_arg(args, flag)? else {
        return Ok(None);
    };
    let modulator = DsdModulator::from_name(value).ok_or_else(|| {
        format!("{flag} must be EcDepth2, EcBeam, EcBeam2, or Standard, got {value}")
    })?;
    match modulator {
        DsdModulator::EcDepth2
        | DsdModulator::EcBeam
        | DsdModulator::EcBeam2
        | DsdModulator::Standard => Ok(Some(modulator)),
        _ => Err(format!(
            "{flag} must be EcDepth2, EcBeam, EcBeam2, or Standard, got {value}"
        )),
    }
}

#[derive(Debug, Deserialize)]
struct CandidateConfigFile {
    candidate_schema_version: Option<String>,
    #[serde(default)]
    baseline: bool,
    params: CandidateParams,
}

#[derive(Debug, Default, Deserialize)]
struct CandidateParams {
    headroom_db: Option<f64>,
    expected_gain_db: Option<f64>,
    ec_obg: Option<f64>,
    dither_scale: Option<f64>,
    dither_shape: Option<String>,
    dither_prng: Option<String>,
    future_scorer: Option<String>,
    leak_alpha: Option<f64>,
    lf_floor_gamma: Option<f64>,
    ec_dc_bias_corner_hz: Option<f64>,
    ec2_policy: Option<String>,
    ec2_quantizer_weight: Option<f64>,
    ec2_pressure_weight: Option<f64>,
    ec2_limit_weight: Option<f64>,
    ec2_transition_weight: Option<f64>,
    ec2_dc_weight: Option<f64>,
    ec2_lookahead_discount: Option<f64>,
    ec2_ambiguity_margin: Option<f64>,
    ec2_pressure_taper_start: Option<f64>,
    ec2_pressure_taper_strength: Option<f64>,
    ec2_pressure_stage_weights: Option<Vec<f64>>,
    beam_quantizer_weight: Option<f64>,
    beam_pressure_weight: Option<f64>,
    beam_limit_weight: Option<f64>,
    beam_transition_weight: Option<f64>,
    beam_dc_weight: Option<f64>,
    beam_pressure_stage_weights: Option<Vec<f64>>,
    beam_terminal_weight: Option<f64>,
    beam_alternation_weight: Option<f64>,
    beam_alternation_rank_weight: Option<f64>,
    beam_alternation_threshold: Option<f64>,
    beam_filtered_error_weight: Option<f64>,
    beam_filtered_error_rank_weight: Option<f64>,
    beam_reconstruction_error_weight: Option<f64>,
    beam_pressure_deadzone: Option<f64>,
    beam_metric_diagnostics: Option<bool>,
    beam_periodicity_weight: Option<f64>,
    beam_periodicity_lags: Option<Vec<u8>>,
    beam_periodicity_window: Option<usize>,
    beam_pressure_accum_scale: Option<f64>,
    beam_pressure_rank_scale: Option<f64>,
    beam_dc_accum_scale: Option<f64>,
    beam_dc_rank_scale: Option<f64>,
    beam_dither_scale: Option<f64>,
    ec_gated_dither_margin: Option<f64>,
    ec_gated_dither_scale: Option<f64>,
    ec_beam_m: Option<usize>,
    ec_beam_n: Option<usize>,
    ecbeam2_run_mode: Option<String>,
    ecbeam2_profile: Option<String>,
    ecbeam2_state_terminal_weight: Option<f64>,
    ecbeam2_state_deadzone: Option<f64>,
    ecbeam2_state_deadzone_weight: Option<f64>,
    ecbeam2_quantizer_regularizer: Option<f64>,
    ecbeam2_ultrasonic_budget: Option<f64>,
    ecbeam2_signed_error_budget: Option<f64>,
    seed_left: Option<SeedValue>,
    seed_right: Option<SeedValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum SeedValue {
    Text(String),
    Number(u64),
}

fn candidate_config_arg(
    config: harness::DsdExperimentConfig,
    rates: &[DsdRate],
    args: &[String],
) -> Result<harness::DsdExperimentConfig, String> {
    let Some(path) = parse_path_arg(args, "--candidate-config")? else {
        return Ok(config);
    };
    let text = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    let file: CandidateConfigFile = serde_json::from_str(&text)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;
    let allow_exploratory = args.iter().any(|arg| arg == "--allow-exploratory");
    apply_candidate_config_file(config, rates, file, allow_exploratory)
}

fn apply_candidate_config_file(
    mut config: harness::DsdExperimentConfig,
    rates: &[DsdRate],
    file: CandidateConfigFile,
    allow_exploratory: bool,
) -> Result<harness::DsdExperimentConfig, String> {
    if file
        .candidate_schema_version
        .as_deref()
        .is_some_and(|version| version != CANDIDATE_SCHEMA_VERSION)
    {
        return Err(format!(
            "candidate config must use {CANDIDATE_SCHEMA_VERSION}"
        ));
    }
    if !file.baseline {
        validate_candidate_tiers(&file.params, allow_exploratory)?;
    }
    if let Some(obg) = file.params.ec_obg {
        config = config.with_ec_obg(obg)?;
    }
    if let Some(gain_db) = file.params.expected_gain_db {
        config = config.with_expected_gain_db(gain_db)?;
    }
    for &rate in rates {
        config = apply_candidate_params_for_rate(config, rate, &file.params)?;
    }
    Ok(config)
}

fn apply_candidate_params_for_rate(
    mut config: harness::DsdExperimentConfig,
    rate: DsdRate,
    params: &CandidateParams,
) -> Result<harness::DsdExperimentConfig, String> {
    if let Some(gain) = params.headroom_db {
        config = set_input_gain_for_dsd_rate(config, rate, gain)?;
    }
    if candidate_has_ecbeam2_config(params) {
        if rate != DsdRate::Dsd64 {
            return Err("EcBeam2 candidate controls support DSD64 only".to_string());
        }
        let mut ecbeam2 = EcBeam2ExperimentConfig::default();
        if let Some(run_mode) = &params.ecbeam2_run_mode {
            ecbeam2.run_mode = parse_ecbeam2_run_mode(run_mode)?;
        }
        if let Some(profile) = &params.ecbeam2_profile {
            ecbeam2.profile = parse_ecbeam2_profile(profile)?;
        }
        ecbeam2.state_terminal_weight = params.ecbeam2_state_terminal_weight.unwrap_or(0.0);
        ecbeam2.state_deadzone = params.ecbeam2_state_deadzone.unwrap_or(0.0);
        ecbeam2.state_deadzone_weight = params.ecbeam2_state_deadzone_weight.unwrap_or(0.0);
        ecbeam2.quantizer_regularizer = params.ecbeam2_quantizer_regularizer.unwrap_or(0.0);
        ecbeam2.ultrasonic_budget = params
            .ecbeam2_ultrasonic_budget
            .filter(|value| *value > 0.0);
        ecbeam2.signed_error_budget = params
            .ecbeam2_signed_error_budget
            .filter(|value| *value > 0.0);
        config = config.with_dsd64_ecbeam2_config(ecbeam2)?;
    }
    let beam_candidate = params.ec_beam_m.is_some() || params.ec_beam_n.is_some();
    if params.ec_beam_m.is_some() || params.ec_beam_n.is_some() {
        config = set_ec_beam_search_for_dsd_rate(
            config,
            rate,
            params
                .ec_beam_m
                .ok_or_else(|| "ec_beam_m is required when ec_beam_n is set".to_string())?,
            params
                .ec_beam_n
                .ok_or_else(|| "ec_beam_n is required when ec_beam_m is set".to_string())?,
        )?;
    }
    if let Some(scale) = params
        .beam_dither_scale
        .filter(|_| beam_candidate)
        .or(params.dither_scale)
    {
        config = set_dither_scale_for_dsd_rate(config, rate, scale)?;
    }
    if let Some(shape) = &params.dither_shape {
        config = set_dither_shape_for_dsd_rate(config, rate, parse_dither_shape(shape)?)?;
    }
    if let Some(prng) = &params.dither_prng {
        config = set_dither_prng_for_dsd_rate(config, rate, parse_dither_prng(prng)?)?;
    }
    if let Some(alpha) = params.leak_alpha {
        config = set_dither_leak_alpha_for_dsd_rate(config, rate, alpha)?;
    }
    if let Some(gamma) = params.lf_floor_gamma {
        config = set_dither_lf_floor_gamma_for_dsd_rate(config, rate, gamma)?;
    }
    if let Some(scorer) = &params.future_scorer {
        config = set_future_scorer_for_dsd_rate(config, rate, parse_future_scorer(scorer)?)?;
    }
    if let Some(corner_hz) = params.ec_dc_bias_corner_hz {
        config = set_ec_dc_corner_hz_for_dsd_rate(config, rate, corner_hz)?;
    }
    if let Some(policy) = &params.ec2_policy {
        config = set_ec2_policy_for_dsd_rate(config, rate, parse_ec2_policy(policy)?)?;
    }
    if candidate_has_ec2_weights(params) || candidate_has_beam_weights(params) {
        let defaults = Ec2PolicyWeights::default();
        config = set_ec2_weights_for_dsd_rate(
            config,
            rate,
            Ec2PolicyWeights {
                quantizer_weight: params
                    .beam_quantizer_weight
                    .filter(|_| beam_candidate)
                    .or(params.ec2_quantizer_weight)
                    .unwrap_or(defaults.quantizer_weight),
                pressure_weight: params
                    .beam_pressure_weight
                    .filter(|_| beam_candidate)
                    .or(params.ec2_pressure_weight)
                    .unwrap_or(defaults.pressure_weight),
                limit_weight: params
                    .beam_limit_weight
                    .filter(|_| beam_candidate)
                    .or(params.ec2_limit_weight)
                    .unwrap_or(defaults.limit_weight),
                transition_weight: params
                    .beam_transition_weight
                    .filter(|_| beam_candidate)
                    .or(params.ec2_transition_weight)
                    .unwrap_or(defaults.transition_weight),
                dc_weight: params
                    .beam_dc_weight
                    .filter(|_| beam_candidate)
                    .or(params.ec2_dc_weight)
                    .unwrap_or(defaults.dc_weight),
                lookahead_discount: params
                    .ec2_lookahead_discount
                    .unwrap_or(defaults.lookahead_discount),
                ambiguity_margin: params
                    .ec2_ambiguity_margin
                    .unwrap_or(defaults.ambiguity_margin),
                pressure_taper_start: params
                    .ec2_pressure_taper_start
                    .unwrap_or(defaults.pressure_taper_start),
                pressure_taper_strength: params
                    .ec2_pressure_taper_strength
                    .unwrap_or(defaults.pressure_taper_strength),
            },
        )?;
    }
    if let Some(weights) = params
        .beam_pressure_stage_weights
        .as_ref()
        .filter(|_| beam_candidate)
        .or(params.ec2_pressure_stage_weights.as_ref())
    {
        config = set_ec2_pressure_stage_weights_for_dsd_rate(
            config,
            rate,
            parse_stage_weight_vec(weights)?,
        )?;
    }
    if params.ec_gated_dither_margin.is_some() || params.ec_gated_dither_scale.is_some() {
        config = set_gated_dither_for_dsd_rate(
            config,
            rate,
            params.ec_gated_dither_margin.unwrap_or(0.0),
            params.ec_gated_dither_scale.unwrap_or(0.0),
        )?;
    }
    if let Some(weight) = params.beam_terminal_weight.filter(|_| beam_candidate) {
        config = set_ec_beam_terminal_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(weight) = params.beam_alternation_weight.filter(|_| beam_candidate) {
        config = set_ec_beam_alternation_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(weight) = params
        .beam_alternation_rank_weight
        .filter(|_| beam_candidate)
    {
        config = set_ec_beam_alternation_rank_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(threshold) = params.beam_alternation_threshold.filter(|_| beam_candidate) {
        config = set_ec_beam_alternation_threshold_for_dsd_rate(config, rate, threshold)?;
    }
    if let Some(weight) = params.beam_periodicity_weight.filter(|_| beam_candidate) {
        config = set_ec_beam_periodicity_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(lags) = params
        .beam_periodicity_lags
        .as_ref()
        .filter(|_| beam_candidate)
    {
        config = set_ec_beam_periodicity_lags_for_dsd_rate(config, rate, lags)?;
    }
    if let Some(window) = params.beam_periodicity_window.filter(|_| beam_candidate) {
        config = set_ec_beam_periodicity_window_for_dsd_rate(config, rate, window)?;
    }
    if let Some(weight) = params.beam_filtered_error_weight.filter(|_| beam_candidate) {
        config = set_ec_beam_filtered_error_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(weight) = params
        .beam_filtered_error_rank_weight
        .filter(|_| beam_candidate)
    {
        config = set_ec_beam_filtered_error_rank_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(weight) = params
        .beam_reconstruction_error_weight
        .filter(|_| beam_candidate)
    {
        config = set_ec_beam_reconstruction_error_weight_for_dsd_rate(config, rate, weight)?;
    }
    if let Some(deadzone) = params.beam_pressure_deadzone.filter(|_| beam_candidate) {
        config = set_ec_beam_pressure_deadzone_for_dsd_rate(config, rate, deadzone)?;
    }
    if let Some(enabled) = params.beam_metric_diagnostics.filter(|_| beam_candidate) {
        config = set_ec_beam_metric_diagnostics_for_dsd_rate(config, rate, enabled);
    }
    if beam_candidate
        && (params.beam_pressure_accum_scale.is_some()
            || params.beam_pressure_rank_scale.is_some()
            || params.beam_dc_accum_scale.is_some()
            || params.beam_dc_rank_scale.is_some())
    {
        config = set_ec_beam_auxiliary_metric_scales_for_dsd_rate(
            config,
            rate,
            params.beam_pressure_accum_scale,
            params.beam_pressure_rank_scale,
            params.beam_dc_accum_scale,
            params.beam_dc_rank_scale,
        )?;
    }
    if params.seed_left.is_some() || params.seed_right.is_some() {
        config = set_seed_pair_for_dsd_rate(
            config,
            rate,
            params
                .seed_left
                .as_ref()
                .map(parse_seed_value)
                .transpose()?,
            params
                .seed_right
                .as_ref()
                .map(parse_seed_value)
                .transpose()?,
        );
    }
    Ok(config)
}

fn candidate_has_ec2_weights(params: &CandidateParams) -> bool {
    params.ec2_quantizer_weight.is_some()
        || params.ec2_pressure_weight.is_some()
        || params.ec2_limit_weight.is_some()
        || params.ec2_transition_weight.is_some()
        || params.ec2_dc_weight.is_some()
        || params.ec2_lookahead_discount.is_some()
        || params.ec2_ambiguity_margin.is_some()
        // Candidate canonicalization records the default taper start even when
        // tapering is disabled. That inert marker must not replace production
        // A1's complete policy with generic EC defaults.
        || params
            .ec2_pressure_taper_strength
            .is_some_and(|strength| strength != 0.0)
}

fn candidate_has_ecbeam2_config(params: &CandidateParams) -> bool {
    params.ecbeam2_run_mode.is_some()
        || params.ecbeam2_profile.is_some()
        || params.ecbeam2_state_terminal_weight.is_some()
        || params.ecbeam2_state_deadzone.is_some()
        || params.ecbeam2_state_deadzone_weight.is_some()
        || params.ecbeam2_quantizer_regularizer.is_some()
        || params.ecbeam2_ultrasonic_budget.is_some()
        || params.ecbeam2_signed_error_budget.is_some()
}

fn candidate_has_beam_weights(params: &CandidateParams) -> bool {
    params.beam_quantizer_weight.is_some()
        || params.beam_pressure_weight.is_some()
        || params.beam_limit_weight.is_some()
        || params.beam_transition_weight.is_some()
        || params.beam_dc_weight.is_some()
        || params.beam_pressure_accum_scale.is_some()
        || params.beam_pressure_rank_scale.is_some()
        || params.beam_dc_accum_scale.is_some()
        || params.beam_dc_rank_scale.is_some()
}

fn validate_candidate_tiers(
    params: &CandidateParams,
    allow_exploratory: bool,
) -> Result<(), String> {
    let ec_beam_candidate = params.ec_beam_m.is_some() || params.ec_beam_n.is_some();
    if candidate_has_beam_weights(params) && !ec_beam_candidate {
        return Err("beam weights require ec_beam_m/ec_beam_n".to_string());
    }
    tier_range(
        "dither_scale",
        params.dither_scale,
        0.0,
        0.25,
        0.30,
        allow_exploratory,
    )?;
    tier_min(
        "leak_alpha",
        params.leak_alpha,
        0.99,
        0.98,
        allow_exploratory,
    )?;
    tier_range(
        "lf_floor_gamma",
        params.lf_floor_gamma,
        0.0,
        0.03,
        0.05,
        allow_exploratory,
    )?;
    if ec_beam_candidate && allow_exploratory {
        strict_range(
            "ec2_quantizer_weight",
            params.ec2_quantizer_weight,
            0.0,
            4.0,
        )?;
        strict_range(
            "beam_quantizer_weight",
            params.beam_quantizer_weight,
            0.0,
            4.0,
        )?;
        strict_range("ec2_pressure_weight", params.ec2_pressure_weight, 0.0, 7.5)?;
        strict_range(
            "beam_pressure_weight",
            params.beam_pressure_weight,
            0.0,
            7.5,
        )?;
    } else {
        tier_inner_range(
            "ec2_quantizer_weight",
            params.ec2_quantizer_weight,
            0.60,
            1.00,
            0.50,
            1.00,
            allow_exploratory,
        )?;
        tier_inner_range(
            "ec2_pressure_weight",
            params.ec2_pressure_weight,
            0.75,
            5.0,
            0.375,
            7.5,
            allow_exploratory,
        )?;
    }
    tier_range(
        "ec_dc_bias_corner_hz",
        params.ec_dc_bias_corner_hz,
        0.0,
        250.0,
        2_000.0,
        allow_exploratory,
    )?;
    tier_range(
        "expected_gain_db",
        params.expected_gain_db,
        -24.0,
        6.0,
        12.0,
        allow_exploratory,
    )?;
    if ec_beam_candidate && allow_exploratory {
        strict_range("ec2_limit_weight", params.ec2_limit_weight, 0.0, 320.0)?;
        strict_range("beam_limit_weight", params.beam_limit_weight, 0.0, 320.0)?;
    } else if params
        .ec2_limit_weight
        .is_some_and(|value| (value - 80.0).abs() > 1.0e-9)
    {
        return Err("ec2_limit_weight is pinned at 80.0".to_string());
    }
    tier_range(
        "ec2_transition_weight",
        params.ec2_transition_weight,
        0.0,
        0.006,
        0.010,
        allow_exploratory,
    )?;
    tier_range(
        "beam_transition_weight",
        params.beam_transition_weight,
        0.0,
        0.006,
        0.010,
        allow_exploratory,
    )?;
    if ec_beam_candidate && allow_exploratory {
        strict_range("ec2_dc_weight", params.ec2_dc_weight, 0.0, 0.20)?;
        strict_range("beam_dc_weight", params.beam_dc_weight, 0.0, 0.20)?;
    } else {
        tier_range(
            "ec2_dc_weight",
            params.ec2_dc_weight,
            0.0,
            0.10,
            0.10,
            allow_exploratory,
        )?;
    }
    tier_inner_range(
        "ec2_lookahead_discount",
        params.ec2_lookahead_discount,
        0.4,
        0.8,
        0.3,
        0.9,
        allow_exploratory,
    )?;
    tier_range(
        "ec2_ambiguity_margin",
        params.ec2_ambiguity_margin,
        0.0,
        0.01,
        0.02,
        allow_exploratory,
    )?;
    strict_range(
        "ec2_pressure_taper_start",
        params.ec2_pressure_taper_start,
        0.45,
        0.72,
    )?;
    tier_range(
        "ec2_pressure_taper_strength",
        params.ec2_pressure_taper_strength,
        0.0,
        2.0,
        2.0,
        allow_exploratory,
    )?;
    if params
        .dither_prng
        .as_deref()
        .is_some_and(|prng| !matches!(prng, "splitmix64" | "splitmix"))
        && !allow_exploratory
    {
        return Err("dither_prng is exploratory; pass --allow-exploratory".to_string());
    }
    if let Some(weights) = &params.ec2_pressure_stage_weights {
        let weights = parse_stage_weight_vec(weights)?;
        if !allow_exploratory {
            return Err(
                "ec2_pressure_stage_weights is exploratory; pass --allow-exploratory".to_string(),
            );
        }
        for weight in weights {
            if !(0.1..=4.0).contains(&weight) {
                return Err(
                    "ec2_pressure_stage_weights is outside the allowed search space".to_string(),
                );
            }
        }
    }
    if let Some(weights) = &params.beam_pressure_stage_weights {
        let weights = parse_stage_weight_vec(weights)?;
        if !ec_beam_candidate {
            return Err("beam_pressure_stage_weights requires ec_beam_m/ec_beam_n".to_string());
        }
        if !allow_exploratory {
            return Err(
                "beam_pressure_stage_weights is exploratory; pass --allow-exploratory".to_string(),
            );
        }
        for weight in weights {
            if !(0.1..=4.0).contains(&weight) {
                return Err(
                    "beam_pressure_stage_weights is outside the allowed search space".to_string(),
                );
            }
        }
    }
    if params.beam_terminal_weight.is_some() && !ec_beam_candidate {
        return Err("beam_terminal_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    strict_range(
        "beam_terminal_weight",
        params.beam_terminal_weight,
        0.0,
        1.0,
    )?;
    if params.beam_alternation_weight.is_some() && !ec_beam_candidate {
        return Err("beam_alternation_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    strict_range(
        "beam_alternation_weight",
        params.beam_alternation_weight,
        0.0,
        0.05,
    )?;
    if params.beam_alternation_rank_weight.is_some() && !ec_beam_candidate {
        return Err("beam_alternation_rank_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    strict_range(
        "beam_alternation_rank_weight",
        params.beam_alternation_rank_weight,
        0.0,
        0.05,
    )?;
    if params.beam_alternation_threshold.is_some() && !ec_beam_candidate {
        return Err("beam_alternation_threshold requires ec_beam_m/ec_beam_n".to_string());
    }
    strict_range(
        "beam_alternation_threshold",
        params.beam_alternation_threshold,
        0.0,
        1.0,
    )?;
    if params.beam_metric_diagnostics.is_some() && !ec_beam_candidate {
        return Err("beam_metric_diagnostics requires ec_beam_m/ec_beam_n".to_string());
    }
    if params.beam_filtered_error_weight.is_some() && !ec_beam_candidate {
        return Err("beam_filtered_error_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    strict_range(
        "beam_filtered_error_weight",
        params.beam_filtered_error_weight,
        0.0,
        4.0,
    )?;
    if params.beam_filtered_error_rank_weight.is_some() && !ec_beam_candidate {
        return Err("beam_filtered_error_rank_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    strict_range(
        "beam_filtered_error_rank_weight",
        params.beam_filtered_error_rank_weight,
        0.0,
        4.0,
    )?;
    if params.beam_reconstruction_error_weight.is_some() && !ec_beam_candidate {
        return Err("beam_reconstruction_error_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    if params.beam_reconstruction_error_weight.is_some() && !allow_exploratory {
        return Err(
            "beam_reconstruction_error_weight is exploratory; pass --allow-exploratory".to_string(),
        );
    }
    strict_range(
        "beam_reconstruction_error_weight",
        params.beam_reconstruction_error_weight,
        0.0,
        1000.0,
    )?;
    if params.beam_pressure_deadzone.is_some() && !ec_beam_candidate {
        return Err("beam_pressure_deadzone requires ec_beam_m/ec_beam_n".to_string());
    }
    if params.beam_pressure_deadzone.is_some() && !allow_exploratory {
        return Err("beam_pressure_deadzone is exploratory; pass --allow-exploratory".to_string());
    }
    strict_range(
        "beam_pressure_deadzone",
        params.beam_pressure_deadzone,
        0.0,
        1.0,
    )?;
    if params.beam_periodicity_weight.is_some() && !ec_beam_candidate {
        return Err("beam_periodicity_weight requires ec_beam_m/ec_beam_n".to_string());
    }
    if params.beam_periodicity_weight.is_some() && !allow_exploratory {
        return Err("beam_periodicity_weight is exploratory; pass --allow-exploratory".to_string());
    }
    strict_range(
        "beam_periodicity_weight",
        params.beam_periodicity_weight,
        0.0,
        0.05,
    )?;
    if params.beam_periodicity_lags.is_some() && !ec_beam_candidate {
        return Err("beam_periodicity_lags requires ec_beam_m/ec_beam_n".to_string());
    }
    if params.beam_periodicity_lags.is_some() && !allow_exploratory {
        return Err("beam_periodicity_lags is exploratory; pass --allow-exploratory".to_string());
    }
    if let Some(lags) = &params.beam_periodicity_lags {
        if lags.is_empty() || lags.len() > 4 {
            return Err("beam_periodicity_lags requires 1 to 4 lags".to_string());
        }
        for (index, &lag) in lags.iter().enumerate() {
            if !(1..=47).contains(&lag) {
                return Err("beam_periodicity_lags must be in 1..=47".to_string());
            }
            if lags[..index].contains(&lag) {
                return Err("beam_periodicity_lags must not contain duplicates".to_string());
            }
        }
    }
    if params.beam_periodicity_window.is_some() && !ec_beam_candidate {
        return Err("beam_periodicity_window requires ec_beam_m/ec_beam_n".to_string());
    }
    if params.beam_periodicity_window.is_some() && !allow_exploratory {
        return Err("beam_periodicity_window is exploratory; pass --allow-exploratory".to_string());
    }
    if let Some(window) = params.beam_periodicity_window
        && !(2..=48).contains(&window)
    {
        return Err("beam_periodicity_window is outside the allowed search space".to_string());
    }
    for (name, value) in [
        (
            "beam_pressure_accum_scale",
            params.beam_pressure_accum_scale,
        ),
        ("beam_pressure_rank_scale", params.beam_pressure_rank_scale),
        ("beam_dc_accum_scale", params.beam_dc_accum_scale),
        ("beam_dc_rank_scale", params.beam_dc_rank_scale),
    ] {
        if value.is_some() && !ec_beam_candidate {
            return Err(format!("{name} requires ec_beam_m/ec_beam_n"));
        }
        strict_range(name, value, 0.0, 2.0)?;
    }
    if params.beam_dither_scale.is_some() && !ec_beam_candidate {
        return Err("beam_dither_scale requires ec_beam_m/ec_beam_n".to_string());
    }
    tier_range(
        "beam_dither_scale",
        params.beam_dither_scale,
        0.0,
        0.25,
        0.50,
        allow_exploratory,
    )?;
    tier_range(
        "ec_gated_dither_margin",
        params.ec_gated_dither_margin,
        0.0,
        0.0,
        0.25,
        allow_exploratory,
    )?;
    tier_range(
        "ec_gated_dither_scale",
        params.ec_gated_dither_scale,
        0.0,
        0.0,
        0.50,
        allow_exploratory,
    )?;
    if params.ec_beam_m.is_some() || params.ec_beam_n.is_some() {
        if !allow_exploratory {
            return Err(
                "ec_beam_m/ec_beam_n are exploratory; pass --allow-exploratory".to_string(),
            );
        }
        let m = params
            .ec_beam_m
            .ok_or_else(|| "ec_beam_m is required when ec_beam_n is set".to_string())?;
        let n = params
            .ec_beam_n
            .ok_or_else(|| "ec_beam_n is required when ec_beam_m is set".to_string())?;
        if !(1..=16).contains(&m) {
            return Err("ec_beam_m is outside the allowed search space".to_string());
        }
        if !(1..=48).contains(&n) {
            return Err("ec_beam_n is outside the allowed search space".to_string());
        }
    }
    if candidate_has_ecbeam2_config(params) {
        if !allow_exploratory {
            return Err("EcBeam2 controls are exploratory; pass --allow-exploratory".to_string());
        }
        if let Some(run_mode) = &params.ecbeam2_run_mode {
            parse_ecbeam2_run_mode(run_mode)?;
        }
        if let Some(profile) = &params.ecbeam2_profile {
            parse_ecbeam2_profile(profile)?;
        }
        strict_range(
            "ecbeam2_state_terminal_weight",
            params.ecbeam2_state_terminal_weight,
            0.0,
            1.0e6,
        )?;
        strict_range(
            "ecbeam2_state_deadzone",
            params.ecbeam2_state_deadzone,
            0.0,
            1.0,
        )?;
        strict_range(
            "ecbeam2_state_deadzone_weight",
            params.ecbeam2_state_deadzone_weight,
            0.0,
            4.0,
        )?;
        strict_range(
            "ecbeam2_quantizer_regularizer",
            params.ecbeam2_quantizer_regularizer,
            0.0,
            0.01,
        )?;
        strict_range(
            "ecbeam2_ultrasonic_budget",
            params.ecbeam2_ultrasonic_budget,
            0.0,
            16.0,
        )?;
        strict_range(
            "ecbeam2_signed_error_budget",
            params.ecbeam2_signed_error_budget,
            0.0,
            2.0,
        )?;
    }
    Ok(())
}

fn parse_stage_weight_vec(weights: &[f64]) -> Result<[f64; 7], String> {
    let parsed: [f64; 7] = weights.try_into().map_err(|_| {
        format!(
            "ec2_pressure_stage_weights requires exactly 7 weights, got {}",
            weights.len()
        )
    })?;
    if !parsed.iter().all(|w| w.is_finite()) {
        return Err("ec2_pressure_stage_weights must all be finite".to_string());
    }
    Ok(parsed)
}

fn tier_range(
    name: &str,
    value: Option<f64>,
    core_min: f64,
    core_max: f64,
    exploratory_max: f64,
    allow_exploratory: bool,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_finite() || value < core_min || value > exploratory_max {
        return Err(format!("{name} is outside the allowed search space"));
    }
    if value > core_max && !allow_exploratory {
        return Err(format!("{name} is exploratory; pass --allow-exploratory"));
    }
    Ok(())
}

fn tier_min(
    name: &str,
    value: Option<f64>,
    core_min: f64,
    exploratory_min: f64,
    allow_exploratory: bool,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_finite() || value < exploratory_min {
        return Err(format!("{name} is outside the allowed search space"));
    }
    if value < core_min && !allow_exploratory {
        return Err(format!("{name} is exploratory; pass --allow-exploratory"));
    }
    Ok(())
}

fn tier_inner_range(
    name: &str,
    value: Option<f64>,
    core_min: f64,
    core_max: f64,
    hard_min: f64,
    hard_max: f64,
    allow_exploratory: bool,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_finite() || value < hard_min || value > hard_max {
        return Err(format!("{name} is outside the allowed search space"));
    }
    if !(core_min..=core_max).contains(&value) && !allow_exploratory {
        return Err(format!("{name} is exploratory; pass --allow-exploratory"));
    }
    Ok(())
}

fn strict_range(name: &str, value: Option<f64>, min: f64, max: f64) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(format!("{name} is outside the allowed search space"));
    }
    Ok(())
}

fn parse_seed_value(value: &SeedValue) -> Result<u64, String> {
    match value {
        SeedValue::Number(value) => Ok(*value),
        SeedValue::Text(value) => {
            if let Some(hex) = value.strip_prefix("0x") {
                u64::from_str_radix(hex, 16)
                    .map_err(|_| format!("seed value {value} is not valid hex"))
            } else {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("seed value {value} is not a valid integer"))
            }
        }
    }
}

fn parse_dither_shape(value: &str) -> Result<DitherShape, String> {
    match value {
        "highpass" | "high-pass" | "highpass-tpdf" => Ok(DitherShape::HighPassTpdf),
        "white" | "white-tpdf" => Ok(DitherShape::WhiteTpdf),
        _ => Err(format!(
            "dither_shape must be highpass or white, got {value}"
        )),
    }
}

fn parse_dither_prng(value: &str) -> Result<DitherPrng, String> {
    match value {
        "splitmix64" | "splitmix" => Ok(DitherPrng::SplitMix64),
        "xorshift64" | "xorshift" => Ok(DitherPrng::XorShift64),
        "xoshiro256**" | "xoshiro256starstar" | "xoshiro" => Ok(DitherPrng::Xoshiro256StarStar),
        _ => Err(format!(
            "dither_prng must be splitmix64, xorshift64, or xoshiro256**, got {value}"
        )),
    }
}

fn parse_future_scorer(value: &str) -> Result<EcFutureScorer, String> {
    match value {
        "quantizer-only" => Ok(EcFutureScorer::QuantizerOnly),
        "full" => Ok(EcFutureScorer::Full),
        "full-d25" => Ok(EcFutureScorer::FullDiscount25),
        "full-d10" => Ok(EcFutureScorer::FullDiscount10),
        "quantizer-limit" => Ok(EcFutureScorer::QuantizerLimit),
        "quarter-pressure" => Ok(EcFutureScorer::QuarterPressureNoDcTransition),
        _ => Err(format!("future_scorer has unsupported scorer {value}")),
    }
}

fn parse_ec2_policy(value: &str) -> Result<Ec2LongFilterPolicy, String> {
    Ec2LongFilterPolicy::from_name(value)
        .ok_or_else(|| format!("ec2_policy has unsupported policy {value}"))
}

fn parse_ecbeam2_run_mode(value: &str) -> Result<EcBeam2RunMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(EcBeam2RunMode::Active),
        "shadow-a1" | "shadow_a1" | "shadowa1" => Ok(EcBeam2RunMode::ShadowA1),
        _ => Err(format!(
            "ecbeam2_run_mode must be active or shadow-a1, got {value}"
        )),
    }
}

fn parse_ecbeam2_profile(value: &str) -> Result<EcBeam2ProfileId, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "harness24to32-v1" | "harness24_to_32_v1" | "harness24to32v1" => {
            Ok(EcBeam2ProfileId::Harness24To32V1)
        }
        _ => Err(format!(
            "ecbeam2_profile must be harness24to32-v1, got {value}"
        )),
    }
}

fn parse_selectable_filters_arg(
    args: &[String],
) -> Result<Vec<harness::SelectableDsdFilter>, String> {
    let Some(value) = parse_value_arg(args, "--selectable-filter")? else {
        return Ok(harness::default_selectable_dsd_filters().to_vec());
    };
    let mut filters: Vec<harness::SelectableDsdFilter> = Vec::new();
    for token in value.split(',') {
        let token = token.trim();
        let filter = match token {
            "SplitPhase" | "split-phase" | "split" | "Split128k" | "split128k" => {
                harness::SelectableDsdFilter {
                    name: "SplitPhase",
                    filter: FilterType::Split128k,
                }
            }
            "SplitPhaseE2v3" | "split-phase-e2v3" | "SplitPhase128kE2v3" => {
                harness::SelectableDsdFilter {
                    name: "SplitPhaseE2v3",
                    filter: FilterType::SplitPhase128kE2v3,
                }
            }
            "LinearPhase" | "linear-phase" | "linear" | "SincExtreme32k" | "sinc-extreme32k" => {
                harness::SelectableDsdFilter {
                    name: "LinearPhase",
                    filter: FilterType::SincExtreme32k,
                }
            }
            "MinimumPhase" | "minimum-phase" | "minimum" | "Minimum16k" | "minimum16k" => {
                harness::SelectableDsdFilter {
                    name: "MinimumPhase",
                    filter: FilterType::Minimum16k,
                }
            }
            "MinimumPhase128k1" | "minimum-phase-128k-1" | "MinimumPhase128k" => {
                harness::SelectableDsdFilter {
                    name: "MinimumPhase128k1",
                    filter: FilterType::MinimumPhase128k,
                }
            }
            "MinimumPhase128k2" | "minimum-phase-128k-2" | "MinimumPhase128kV2" => {
                harness::SelectableDsdFilter {
                    name: "MinimumPhase128k2",
                    filter: FilterType::MinimumPhase128kV2,
                }
            }
            "MinimumPhase128k3" | "minimum-phase-128k-3" | "MinimumPhase128kV3" => {
                harness::SelectableDsdFilter {
                    name: "MinimumPhase128k3",
                    filter: FilterType::MinimumPhase128kV3,
                }
            }
            "MinimumPhase128k4" | "minimum-phase-128k-4" | "MinimumPhase128kV4" => {
                harness::SelectableDsdFilter {
                    name: "MinimumPhase128k4",
                    filter: FilterType::MinimumPhase128kV4,
                }
            }
            "IntegratedPhase"
            | "IntegratedPhase1"
            | "integrated-phase-1"
            | "integrated-phase"
            | "integrated"
            | "IntegratedPhase128k"
            | "integratedphase128k"
            | "integrated-phase128k" => harness::SelectableDsdFilter {
                name: "IntegratedPhase1",
                filter: FilterType::IntegratedPhase128k,
            },
            "IntegratedPhase2" | "integrated-phase-2" | "IntegratedPhase128kV2" => {
                harness::SelectableDsdFilter {
                    name: "IntegratedPhase2",
                    filter: FilterType::IntegratedPhase128kV2,
                }
            }
            "IntegratedPhase3" | "integrated-phase-3" | "IntegratedPhase128kV3" => {
                harness::SelectableDsdFilter {
                    name: "IntegratedPhase3",
                    filter: FilterType::IntegratedPhase128kV3,
                }
            }
            "IntegratedPhase4" | "integrated-phase-4" | "IntegratedPhase128kV4" => {
                harness::SelectableDsdFilter {
                    name: "IntegratedPhase4",
                    filter: FilterType::IntegratedPhase128kV4,
                }
            }
            "" => return Err("--selectable-filter must list one or more filters".to_string()),
            _ => {
                return Err(format!(
                    "--selectable-filter contains unsupported filter {token}"
                ));
            }
        };
        if !filters.iter().any(|case| case.name == filter.name) {
            filters.push(filter);
        }
    }
    if filters.is_empty() {
        Err("--selectable-filter must list one or more filters".to_string())
    } else {
        Ok(filters)
    }
}

fn parse_selectable_modulators_arg(args: &[String]) -> Result<Vec<DsdModulator>, String> {
    let Some(value) = parse_value_arg(args, "--selectable-modulator")? else {
        return Ok(vec![DsdModulator::Standard, DsdModulator::EcDepth2]);
    };
    let mut modulators = Vec::new();
    for token in value.split(',') {
        let token = token.trim();
        let modulator = match token {
            "Standard" | "standard" => DsdModulator::Standard,
            "EcDepth1" | "ecdepth1" | "ec-depth1" | "ec1" => DsdModulator::EcDepth1,
            "EcDepth2" | "ecdepth2" | "ec-depth2" | "ec2" => DsdModulator::EcDepth2,
            "EcBeam" | "ecbeam" | "ec-beam" | "ecb" => DsdModulator::EcBeam,
            "EcBeam2" | "ecbeam2" | "ec-beam2" | "ecb2" => DsdModulator::EcBeam2,
            "EcDepth3" | "ecdepth3" | "ec-depth3" | "ec3" => DsdModulator::EcDepth3,
            "EcDepth4" | "ecdepth4" | "ec-depth4" | "ec4" => DsdModulator::EcDepth4,
            "EcDepth8" | "ecdepth8" | "ec-depth8" | "ec8" => DsdModulator::EcDepth8,
            "" => return Err("--selectable-modulator must list one or more modulators".to_string()),
            _ => {
                return Err(format!(
                    "--selectable-modulator contains unsupported modulator {token}"
                ));
            }
        };
        if !modulators.contains(&modulator) {
            modulators.push(modulator);
        }
    }
    if modulators.is_empty() {
        Err("--selectable-modulator must list one or more modulators".to_string())
    } else {
        Ok(modulators)
    }
}

fn parse_selectable_dsd_rates_arg(args: &[String]) -> Result<Vec<DsdRate>, String> {
    if parse_value_arg(args, "--rates")?.is_none() {
        Ok(vec![DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256])
    } else {
        parse_dsd_rates_arg(args, "--rates")
    }
}

fn parse_source_rates_arg(args: &[String], flag: &str) -> Result<Vec<u32>, String> {
    let Some(value) = parse_value_arg(args, flag)? else {
        return Ok(vec![SOURCE_RATE]);
    };
    let mut rates = Vec::new();
    for token in value.split(',') {
        let token = token.trim();
        let rate = token
            .parse::<u32>()
            .map_err(|_| format!("{flag} contains invalid source rate {token}"))?;
        if !matches!(rate, 44_100 | 48_000) {
            return Err(format!("{flag} supports only 44100 and 48000, got {token}"));
        }
        if !rates.contains(&rate) {
            rates.push(rate);
        }
    }
    if rates.is_empty() {
        Err(format!("{flag} must list one or more source rates"))
    } else {
        Ok(rates)
    }
}

fn parse_dsd_rates_arg(args: &[String], flag: &str) -> Result<Vec<DsdRate>, String> {
    let Some(value) = parse_value_arg(args, flag)? else {
        return Ok(vec![DsdRate::Dsd128, DsdRate::Dsd256]);
    };
    let mut rates = Vec::new();
    for token in value.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(format!("{flag} must list one or more of 64, 128, 256"));
        }
        let rate = match token {
            "64" | "dsd64" | "DSD64" => DsdRate::Dsd64,
            "128" | "dsd128" | "DSD128" => DsdRate::Dsd128,
            "256" | "dsd256" | "DSD256" => DsdRate::Dsd256,
            _ => return Err(format!("{flag} contains unsupported DSD rate {token}")),
        };
        if !rates.contains(&rate) {
            rates.push(rate);
        }
    }
    if rates.is_empty() {
        Err(format!("{flag} must list one or more of 64, 128, 256"))
    } else {
        Ok(rates)
    }
}

fn parse_value_arg<'a>(args: &'a [String], flag: &str) -> Result<Option<&'a str>, String> {
    let mut found = None;
    let mut idx = 0;
    while idx < args.len() {
        if args[idx] == flag {
            idx += 1;
            let value = args
                .get(idx)
                .ok_or_else(|| format!("{flag} requires a value"))?;
            found = Some(value.as_str());
        }
        idx += 1;
    }
    Ok(found)
}
