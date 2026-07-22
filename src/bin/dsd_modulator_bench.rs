use std::hint::black_box;
use std::time::{Duration, Instant};

use fozmo::audio::dsd::delta_sigma::{CrfbModulator, SeventhOrderSearchBenchmarkModulator};
use fozmo::audio::dsd::dsd_coeffs::{
    CRFB7_STANDARD_OSR64, CRFB7_STANDARD_OSR128, CRFB7_STANDARD_OSR256, CRFB7_STANDARD_OSR512,
    CRFB7_STANDARD_OSR1024, ModulatorCoeffs,
};
use fozmo::audio::dsd::dsd_render::DsdRate;

const AUDIO_SECONDS: f64 = 1.0;
const HIGH_RATE_AUDIO_SECONDS: f64 = 0.25;
const WARMUP_PASSES: usize = 2;
const MEASURED_PASSES: usize = 5;

#[derive(Clone, Copy)]
enum Engine {
    Standard(&'static ModulatorCoeffs),
    SeventhOrderSearch,
}

struct BenchCase {
    name: &'static str,
    dsd_rate: DsdRate,
    engine: Engine,
}

struct BenchResult {
    elapsed: Duration,
    samples: usize,
    checksum: u64,
    state_clamps: u64,
    stability_resets: u64,
}

fn main() {
    let case_filter = std::env::var("DSD_MODULATOR_BENCH_FILTER").ok();
    let cases = [
        BenchCase {
            name: "Standard DSD64",
            dsd_rate: DsdRate::Dsd64,
            engine: Engine::Standard(&CRFB7_STANDARD_OSR64),
        },
        BenchCase {
            name: "7th Order Search DSD64",
            dsd_rate: DsdRate::Dsd64,
            engine: Engine::SeventhOrderSearch,
        },
        BenchCase {
            name: "Standard DSD128",
            dsd_rate: DsdRate::Dsd128,
            engine: Engine::Standard(&CRFB7_STANDARD_OSR128),
        },
        BenchCase {
            name: "7th Order Search DSD128",
            dsd_rate: DsdRate::Dsd128,
            engine: Engine::SeventhOrderSearch,
        },
        BenchCase {
            name: "Standard DSD256",
            dsd_rate: DsdRate::Dsd256,
            engine: Engine::Standard(&CRFB7_STANDARD_OSR256),
        },
        BenchCase {
            name: "7th Order Search DSD256",
            dsd_rate: DsdRate::Dsd256,
            engine: Engine::SeventhOrderSearch,
        },
        BenchCase {
            name: "Standard DSD512",
            dsd_rate: DsdRate::Dsd512,
            engine: Engine::Standard(&CRFB7_STANDARD_OSR512),
        },
        BenchCase {
            name: "Standard DSD1024",
            dsd_rate: DsdRate::Dsd1024,
            engine: Engine::Standard(&CRFB7_STANDARD_OSR1024),
        },
    ];

    println!("DSD modulator benchmark");
    println!("  normal input: {AUDIO_SECONDS:.1}s synthetic DSD-rate mono stream");
    println!("  DSD512/1024 input: {HIGH_RATE_AUDIO_SECONDS:.2}s");
    if let Some(filter) = &case_filter {
        println!("  case filter: {filter}");
    }
    println!();
    println!(
        "{:<20} {:>12} {:>14} {:>12} {:>12} {:>11} {:>8} {:>8}",
        "case", "samples", "ns/sample", "1ch core %", "2ch core %", "checksum", "clamps", "resets"
    );

    for case in cases {
        if case_filter
            .as_ref()
            .is_some_and(|filter| !case.name.contains(filter))
        {
            continue;
        }
        let audio_seconds = if matches!(case.dsd_rate, DsdRate::Dsd512 | DsdRate::Dsd1024) {
            HIGH_RATE_AUDIO_SECONDS
        } else {
            AUDIO_SECONDS
        };
        let wire_rate = case.dsd_rate.wire_rate_44k_family();
        let input_peak = match case.engine {
            Engine::Standard(coeffs) => coeffs.input_peak,
            Engine::SeventhOrderSearch => {
                SeventhOrderSearchBenchmarkModulator::input_peak(wire_rate)
                    .expect("7th Order Search benchmark rate should be supported")
            }
        };
        let input = make_input(wire_rate as usize, audio_seconds, input_peak);

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
        let core_percent = result.elapsed.as_secs_f64() / audio_seconds * 100.0;
        println!(
            "{:<20} {:>12} {:>14.2} {:>11.2}% {:>11.2}% {:>11} {:>8} {:>8}",
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

fn run_case(case: &BenchCase, input: &[f64], seed: u64) -> BenchResult {
    let mut bits = Vec::with_capacity(input.len() + 1);
    let start = Instant::now();
    let (state_clamps, stability_resets) = match case.engine {
        Engine::Standard(coeffs) => {
            let mut modulator =
                CrfbModulator::new(coeffs, seed).expect("calibrated standard coefficients");
            modulator.process_into_bits(input, &mut bits);
            modulator.flush_into_bits(&mut bits);
            (modulator.state_clamps(), modulator.stability_resets())
        }
        Engine::SeventhOrderSearch => {
            let mut modulator = SeventhOrderSearchBenchmarkModulator::new_playback(
                seed,
                case.dsd_rate.wire_rate_44k_family(),
            )
            .expect("7th Order Search benchmark configuration");
            modulator.process_into_bits(input, &mut bits);
            modulator.flush_into_bits(&mut bits);
            (modulator.state_clamps(), modulator.stability_resets())
        }
    };
    let elapsed = start.elapsed();
    let checksum = bits.iter().step_by(251).fold(0u64, |acc, bit| {
        acc.wrapping_mul(3).wrapping_add(*bit as u64)
    });
    black_box(&bits);
    BenchResult {
        elapsed,
        samples: input.len(),
        checksum,
        state_clamps,
        stability_resets,
    }
}

fn make_input(wire_rate: usize, audio_seconds: f64, input_peak: f64) -> Vec<f64> {
    let samples = (wire_rate as f64 * audio_seconds).round() as usize;
    (0..samples)
        .map(|sample| {
            let t = sample as f64 / wire_rate as f64;
            input_peak
                * (0.30 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()
                    + 0.08 * (2.0 * std::f64::consts::PI * 7_321.0 * t).sin()
                    + 0.03 * (2.0 * std::f64::consts::PI * 18_101.0 * t).cos())
        })
        .collect()
}
