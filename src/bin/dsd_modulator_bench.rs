use std::hint::black_box;
use std::time::{Duration, Instant};

use fozmo::audio::dsd::delta_sigma::{
    CrfbModulator, DsdModulator, Ec2LongFilterPolicy, Ec2PolicyWeights, EcBeam2BenchmarkModulator,
    dc_bias_decay_for_corner_hz,
};
use fozmo::audio::dsd::dsd_coeffs::{
    CRFB_OSR64_OBG165, CRFB_OSR128_OBG165, CRFB_OSR256_OBG165, ModulatorCoeffs,
};
use fozmo::audio::dsd::dsd_render::{DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS, DsdRate};

const AUDIO_SECONDS: f64 = 1.0;
const WARMUP_PASSES: usize = 2;
const MEASURED_PASSES: usize = 5;

struct BenchCase {
    name: &'static str,
    dsd_rate: DsdRate,
    modulator: DsdModulator,
    ec2_policy: Option<Ec2PolicyWeights>,
    /// EcBeam prototype `(m, n)` (docs/dev/7th-order-ecm-m-algorithm.md §21.10):
    /// activates the delayed-commitment M-algorithm search instead of the
    /// lookahead tree. Beam rows run default policy weights (the beam ignores
    /// the taper/ambiguity policies by design).
    beam: Option<(usize, usize)>,
    beam_dither_scale: Option<f64>,
    coeffs_override: Option<&'static ModulatorCoeffs>,
    a1_beam: bool,
}

struct BenchResult {
    elapsed: Duration,
    samples: usize,
    checksum: u64,
    state_clamps: u64,
    stability_resets: u64,
}

struct EcBeam2BenchCase {
    name: &'static str,
    dsd_rate: DsdRate,
}

fn main() {
    let case_filter = std::env::var("DSD_MODULATOR_BENCH_FILTER").ok();
    let cases = [
        BenchCase {
            name: "Standard DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::Standard,
            ec2_policy: None,
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "EC2 prod DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(production_ec2_weights(1.5)),
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "EC2 P075 DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(production_ec2_weights(0.75)),
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Standard DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::Standard,
            ec2_policy: None,
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "EC2 prod DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(production_ec2_weights(1.5)),
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "EC2 P075 DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(production_ec2_weights(0.75)),
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Standard DSD256",
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::Standard,
            ec2_policy: None,
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "EC2 prod DSD256",
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(production_ec2_weights(1.5)),
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "EC2 P075 DSD256",
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(production_ec2_weights(0.75)),
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        // EcDepth8: the "is the beam cheaper than brute depth" reference row
        // (docs/dev/7th-order-ecm-m-algorithm.md §14).
        BenchCase {
            name: "EC8 DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth8,
            ec2_policy: None,
            beam: None,
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        // EcBeam V1 fast-path rows: M4/N8, sparse CRFB, zero dither.
        BenchCase {
            name: "Beam M4N8 plain DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((4, 8)),
            beam_dither_scale: Some(0.0),
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Beam A1 OBG165 DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(a1_ec2_weights()),
            beam: Some((4, 8)),
            beam_dither_scale: Some(0.0),
            coeffs_override: Some(&CRFB_OSR64_OBG165),
            a1_beam: true,
        },
        BenchCase {
            name: "Beam A1 OBG165 DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(a1_ec2_weights()),
            beam: Some((4, 8)),
            beam_dither_scale: Some(0.0),
            coeffs_override: Some(&CRFB_OSR128_OBG165),
            a1_beam: true,
        },
        BenchCase {
            name: "Beam A1 OBG165 DSD256",
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcDepth2,
            ec2_policy: Some(a1_ec2_weights()),
            beam: Some((4, 8)),
            beam_dither_scale: Some(0.0),
            coeffs_override: Some(&CRFB_OSR256_OBG165),
            a1_beam: true,
        },
        BenchCase {
            name: "Beam M4N8 plain DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((4, 8)),
            beam_dither_scale: Some(0.0),
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Beam M4N8 plain DSD256",
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((4, 8)),
            beam_dither_scale: Some(0.0),
            coeffs_override: None,
            a1_beam: false,
        },
        // EcBeam ladder rungs L1 (DSD64) and L2 (DSD128) — §22.4. N = 16,
        // sparse production tables, default weights.
        BenchCase {
            name: "Beam M4 DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((4, 16)),
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Beam M8 DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((8, 16)),
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Beam M16 DSD64",
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((16, 16)),
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Beam M4 DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((4, 16)),
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
        BenchCase {
            name: "Beam M8 DSD128",
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth1,
            ec2_policy: None,
            beam: Some((8, 16)),
            beam_dither_scale: None,
            coeffs_override: None,
            a1_beam: false,
        },
    ];

    println!("DSD modulator benchmark");
    println!("  input: {AUDIO_SECONDS:.1}s synthetic DSD-rate mono stream");
    if let Some(filter) = &case_filter {
        println!("  case filter: {filter}");
    }
    println!();
    println!(
        "{:<18} {:>12} {:>14} {:>12} {:>12} {:>11} {:>8} {:>8}",
        "case", "samples", "ns/sample", "1ch core %", "2ch core %", "checksum", "clamps", "resets"
    );

    for case in cases {
        if case_filter
            .as_ref()
            .is_some_and(|filter| !case.name.contains(filter))
        {
            continue;
        }
        let coeffs = case
            .coeffs_override
            .unwrap_or_else(|| case.dsd_rate.coeffs_for_mode(case.modulator.mode()));
        let wire_rate = case.dsd_rate.wire_rate_44k_family();
        let input = make_input(wire_rate as usize, coeffs.input_peak);
        for pass in 0..WARMUP_PASSES {
            black_box(run_case(&case, &input, 0xC0DE + pass as u64));
        }

        let mut best: Option<BenchResult> = None;
        for pass in 0..MEASURED_PASSES {
            let result = run_case(&case, &input, 0xBEEF + pass as u64);
            if best
                .as_ref()
                .is_none_or(|best_result| result.elapsed < best_result.elapsed)
            {
                best = Some(result);
            }
        }

        let result = best.expect("measured passes should produce a result");
        let ns_per_sample = result.elapsed.as_nanos() as f64 / result.samples as f64;
        let core_percent = result.elapsed.as_secs_f64() / AUDIO_SECONDS * 100.0;
        println!(
            "{:<18} {:>12} {:>14.2} {:>11.2}% {:>11.2}% {:>11} {:>8} {:>8}",
            case.name,
            result.samples,
            ns_per_sample,
            core_percent,
            core_percent * 2.0,
            result.checksum,
            result.state_clamps,
            result.stability_resets
        );
    }

    let ecbeam2_cases = [
        EcBeam2BenchCase {
            name: "EcBeam2 playback DSD64",
            dsd_rate: DsdRate::Dsd64,
        },
        EcBeam2BenchCase {
            name: "EcBeam2 playback DSD128",
            dsd_rate: DsdRate::Dsd128,
        },
        EcBeam2BenchCase {
            name: "EcBeam2 playback DSD256",
            dsd_rate: DsdRate::Dsd256,
        },
    ];
    for case in ecbeam2_cases {
        if case_filter
            .as_ref()
            .is_some_and(|filter| !case.name.contains(filter))
        {
            continue;
        }
        let wire_rate = case.dsd_rate.wire_rate_44k_family();
        let input_peak = EcBeam2BenchmarkModulator::input_peak(wire_rate)
            .expect("qualified EcBeam2 playback wire rate");
        let input = make_input(wire_rate as usize, input_peak);
        for pass in 0..WARMUP_PASSES {
            black_box(run_ecbeam2_case(&case, &input, 0xC0DE + pass as u64));
        }

        let mut best: Option<BenchResult> = None;
        for pass in 0..MEASURED_PASSES {
            let result = run_ecbeam2_case(&case, &input, 0xBEEF + pass as u64);
            if best
                .as_ref()
                .is_none_or(|best_result| result.elapsed < best_result.elapsed)
            {
                best = Some(result);
            }
        }

        let result = best.expect("measured passes should produce a result");
        let ns_per_sample = result.elapsed.as_nanos() as f64 / result.samples as f64;
        let core_percent = result.elapsed.as_secs_f64() / AUDIO_SECONDS * 100.0;
        println!(
            "{:<18} {:>12} {:>14.2} {:>11.2}% {:>11.2}% {:>11} {:>8} {:>8}",
            case.name,
            result.samples,
            ns_per_sample,
            core_percent,
            core_percent * 2.0,
            result.checksum,
            result.state_clamps,
            result.stability_resets
        );
    }
}

fn run_ecbeam2_case(case: &EcBeam2BenchCase, input: &[f64], seed: u64) -> BenchResult {
    let mut modulator =
        EcBeam2BenchmarkModulator::new_playback(seed, case.dsd_rate.wire_rate_44k_family())
            .expect("qualified EcBeam2 playback configuration");
    let mut bits = Vec::with_capacity(input.len());
    let start = Instant::now();
    modulator.process_into_bits(input, &mut bits);
    modulator.flush_into_bits(&mut bits);
    let elapsed = start.elapsed();
    let checksum = bits.iter().step_by(251).fold(0u64, |acc, bit| {
        acc.wrapping_mul(3).wrapping_add(*bit as u64)
    });
    black_box(&bits);
    BenchResult {
        elapsed,
        samples: input.len(),
        checksum,
        state_clamps: modulator.state_clamps(),
        stability_resets: modulator.stability_resets(),
    }
}

fn run_case(case: &BenchCase, input: &[f64], seed: u64) -> BenchResult {
    let coeffs = case
        .coeffs_override
        .unwrap_or_else(|| case.dsd_rate.coeffs_for_mode(case.modulator.mode()));
    let mut modulator = CrfbModulator::new_with_mode(coeffs, seed, case.modulator.mode())
        .expect("calibrated coefficients");
    modulator.set_lookahead_depth(case.modulator.lookahead_depth());
    if let Some(weights) = case.ec2_policy {
        modulator.set_ec2_long_filter_policy(Ec2LongFilterPolicy::AmbiguityPressure);
        modulator.set_ec2_policy_weights(weights);
    }
    if let Some(scale) = case.beam_dither_scale {
        modulator.set_dither_scale(scale);
    }
    if let Some((m, n)) = case.beam {
        modulator.set_beam_search(m, n);
        if case.a1_beam {
            modulator.set_pressure_stage_weights(&DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS);
            modulator.set_beam_terminal_weight(0.3);
            modulator.set_beam_alternation_weight(0.0005);
            modulator.set_dc_bias_decay(dc_bias_decay_for_corner_hz(
                20.0,
                case.dsd_rate.wire_rate_44k_family(),
            ));
        }
        // The dense fallback is a different cost regime and would silently
        // void the beam ladder numbers (§22.6).
        assert!(
            modulator.crfb_sparse(),
            "beam bench requires the sparse CRFB path"
        );
    }
    let mut bits = Vec::with_capacity(input.len());
    let start = Instant::now();
    modulator.process_into_bits(input, &mut bits);
    modulator.flush_into_bits(&mut bits);
    let elapsed = start.elapsed();
    let checksum = bits.iter().step_by(251).fold(0u64, |acc, bit| {
        acc.wrapping_mul(3).wrapping_add(*bit as u64)
    });
    black_box(&bits);
    BenchResult {
        elapsed,
        samples: input.len(),
        checksum,
        state_clamps: modulator.state_clamps(),
        stability_resets: modulator.stability_resets(),
    }
}

fn a1_ec2_weights() -> Ec2PolicyWeights {
    Ec2PolicyWeights {
        quantizer_weight: 0.8,
        pressure_weight: 2.75,
        limit_weight: 80.0,
        transition_weight: 0.002,
        dc_weight: 0.04,
        lookahead_discount: 0.8,
        ambiguity_margin: 0.0,
        pressure_taper_start: 0.60,
        pressure_taper_strength: 0.0,
    }
}

fn production_ec2_weights(pressure_weight: f64) -> Ec2PolicyWeights {
    Ec2PolicyWeights {
        quantizer_weight: 1.0,
        pressure_weight,
        limit_weight: 80.0,
        transition_weight: 0.0,
        dc_weight: 0.04,
        lookahead_discount: 0.8,
        ambiguity_margin: 0.005,
        pressure_taper_start: 0.45,
        pressure_taper_strength: 2.0,
    }
}

fn make_input(wire_rate: usize, input_peak: f64) -> Vec<f64> {
    let samples = (wire_rate as f64 * AUDIO_SECONDS).round() as usize;
    let mut input = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f64 / wire_rate as f64;
        input.push(
            input_peak
                * (0.30 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()
                    + 0.08 * (2.0 * std::f64::consts::PI * 7_321.0 * t).sin()
                    + 0.03 * (2.0 * std::f64::consts::PI * 18_101.0 * t).cos()),
        );
    }
    input
}
