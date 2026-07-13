use std::hint::black_box;
use std::time::{Duration, Instant};

use fozmo::audio::dsd::delta_sigma::DsdModulator;
use fozmo::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
use fozmo::audio::dsp::resampler::FilterType;

const SOURCE_RATE: u32 = 44_100;
const SOURCE_FRAMES: usize = 8192;
const WARMUP_PASSES: usize = 2;
const MEASURED_PASSES: usize = 5;

struct Case {
    name: &'static str,
    filter: FilterType,
    dsd_rate: DsdRate,
    modulator: DsdModulator,
}

fn main() -> Result<(), String> {
    let cases = [
        Case {
            name: "Smooth Phase DSD128 Standard",
            filter: FilterType::SmoothPhase128k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::Standard,
        },
        Case {
            name: "Smooth Phase DSD128 EC",
            filter: FilterType::SmoothPhase128k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
        },
        Case {
            name: "Smooth Phase DSD128 Search",
            filter: FilterType::SmoothPhase128k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Smooth Phase DSD256 Standard",
            filter: FilterType::SmoothPhase128k,
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::Standard,
        },
        Case {
            name: "Smooth Phase DSD256 EC",
            filter: FilterType::SmoothPhase128k,
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcDepth2,
        },
        Case {
            name: "Smooth Phase DSD256 Search",
            filter: FilterType::SmoothPhase128k,
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Minimum16k DSD64 EcBeam A1",
            filter: FilterType::Minimum16k,
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Minimum16k DSD64 EcBeam2",
            filter: FilterType::Minimum16k,
            dsd_rate: DsdRate::Dsd64,
            modulator: DsdModulator::EcBeam2,
        },
        Case {
            name: "Minimum16k DSD128 Standard",
            filter: FilterType::Minimum16k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::Standard,
        },
        Case {
            name: "Minimum16k DSD128 EcDepth2",
            filter: FilterType::Minimum16k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
        },
        Case {
            name: "Split128k DSD128 EcDepth2",
            filter: FilterType::Split128k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
        },
        Case {
            name: "Integrated Phase DSD128 EcDepth2",
            filter: FilterType::IntegratedPhase128k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcDepth2,
        },
        Case {
            name: "Integrated Phase 1 DSD128 Search",
            filter: FilterType::IntegratedPhase128k,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Integrated Phase 2 DSD128 Search",
            filter: FilterType::IntegratedPhase128kV2,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Integrated Phase 3 DSD128 Search",
            filter: FilterType::IntegratedPhase128kV3,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Integrated Phase 4 DSD128 Search",
            filter: FilterType::IntegratedPhase128kV4,
            dsd_rate: DsdRate::Dsd128,
            modulator: DsdModulator::EcBeam,
        },
        Case {
            name: "Minimum16k DSD256 EcDepth2",
            filter: FilterType::Minimum16k,
            dsd_rate: DsdRate::Dsd256,
            modulator: DsdModulator::EcDepth2,
        },
    ];

    println!("DSD renderer throughput (stereo)");
    println!(
        "{:<28} {:>10} {:>10} {:>10} {:>8} {:>8}",
        "case", "min ms", "avg ms", "max ms", "resets", "clamps"
    );

    let input = sine_input();
    for case in cases {
        for _ in 0..WARMUP_PASSES {
            black_box(run_case(&case, &input)?);
        }

        let mut timings = Vec::new();
        let mut resets = 0;
        let mut clamps = 0;
        for _ in 0..MEASURED_PASSES {
            let result = run_case(&case, &input)?;
            timings.push(result.elapsed);
            resets = result.stability_resets;
            clamps = result.state_clamps;
        }
        timings.sort();
        let min = timings[0];
        let max = *timings.last().unwrap();
        let avg = timings.iter().map(Duration::as_secs_f64).sum::<f64>() / timings.len() as f64;
        println!(
            "{:<28} {:>10.3} {:>10.3} {:>10.3} {:>8} {:>8}",
            case.name,
            min.as_secs_f64() * 1000.0,
            avg * 1000.0,
            max.as_secs_f64() * 1000.0,
            resets,
            clamps
        );
    }

    Ok(())
}

struct ResultRow {
    elapsed: Duration,
    stability_resets: u64,
    state_clamps: u64,
}

fn run_case(case: &Case, input: &[f64]) -> Result<ResultRow, String> {
    let mut renderer = DsdRenderer::new_with_dsd_modulator(
        case.filter,
        SOURCE_RATE,
        case.dsd_rate,
        case.modulator,
    )?;
    let mut out = Vec::new();
    let start = Instant::now();
    renderer.render(input, input, &mut out);
    renderer.flush_modulators_and_pack(&mut out);
    let elapsed = start.elapsed();
    black_box(out.len());
    Ok(ResultRow {
        elapsed,
        stability_resets: renderer.stability_resets(),
        state_clamps: renderer.state_clamps(),
    })
}

fn sine_input() -> Vec<f64> {
    (0..SOURCE_FRAMES)
        .map(|idx| {
            let t = idx as f64 / SOURCE_RATE as f64;
            0.25 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()
        })
        .collect()
}
