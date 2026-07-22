// The benchmark binary imports the production resampler module directly, including helpers it does not call.
#[allow(dead_code)]
#[path = "../audio/dsp/resampler.rs"]
mod resampler;

use std::hint::black_box;
use std::time::{Duration, Instant};

use resampler::{FilterType, SincResampler};

const SOURCE_RATE: u32 = 44_100;
const TARGET_RATE: u32 = 176_400;
const CHUNK_FRAMES: usize = 1024;
const INPUT_SECONDS: usize = 20;
const WARMUP_PASSES: usize = 2;
const MEASURED_PASSES: usize = 6;

struct BenchCase {
    name: &'static str,
    filter: FilterType,
}

struct BenchResult {
    frames_out: usize,
    elapsed: Duration,
    latency_ms: f64,
    memory_bytes: usize,
    checksum: f64,
}

fn main() {
    let cases = [
        BenchCase {
            name: "LinearPhase128k",
            filter: FilterType::LinearPhase128k,
        },
        BenchCase {
            name: "Minimum16k",
            filter: FilterType::Minimum16k,
        },
        BenchCase {
            name: "MinimumPhaseCompact128k",
            filter: FilterType::MinimumPhaseCompact128k,
        },
        BenchCase {
            name: "SplitPhase128kE3",
            filter: FilterType::SplitPhase128kE3,
        },
    ];

    let (left, right) = make_input(SOURCE_RATE as usize * INPUT_SECONDS);

    println!("Resampler benchmark");
    println!("  source: {SOURCE_RATE} Hz");
    println!("  target: {TARGET_RATE} Hz");
    println!("  chunk:  {CHUNK_FRAMES} source frames");
    println!("  input:  {INPUT_SECONDS} seconds");
    println!();
    println!(
        "{:<12} {:>12} {:>14} {:>12} {:>10} {:>10} {:>13}",
        "filter", "out frames", "ns/out frame", "1-core %", "lat ms", "mem KB", "checksum"
    );

    for case in cases {
        for _ in 0..WARMUP_PASSES {
            black_box(run_case(case.filter, &left, &right));
        }

        let mut best: Option<BenchResult> = None;
        for _ in 0..MEASURED_PASSES {
            let result = run_case(case.filter, &left, &right);
            if best
                .as_ref()
                .is_none_or(|best_result| result.elapsed < best_result.elapsed)
            {
                best = Some(result);
            }
        }

        let result = best.expect("measured passes should produce a result");
        let ns_per_frame = result.elapsed.as_nanos() as f64 / result.frames_out as f64;
        let audio_seconds = result.frames_out as f64 / TARGET_RATE as f64;
        let one_core_percent = result.elapsed.as_secs_f64() / audio_seconds * 100.0;

        println!(
            "{:<12} {:>12} {:>14.1} {:>11.2}% {:>10.1} {:>10.1} {:>13.6}",
            case.name,
            result.frames_out,
            ns_per_frame,
            one_core_percent,
            result.latency_ms,
            result.memory_bytes as f64 / 1024.0,
            result.checksum
        );
    }
}

fn make_input(frames: usize) -> (Vec<f64>, Vec<f64>) {
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);

    for i in 0..frames {
        let t = i as f64 / SOURCE_RATE as f64;
        let l = 0.49 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()
            + 0.23 * (2.0 * std::f64::consts::PI * 7_321.0 * t).sin()
            + 0.06 * (2.0 * std::f64::consts::PI * 18_101.0 * t).sin();
        let r = 0.44 * (2.0 * std::f64::consts::PI * 1_103.0 * t).cos()
            + 0.21 * (2.0 * std::f64::consts::PI * 6_607.0 * t).sin()
            + 0.05 * (2.0 * std::f64::consts::PI * 17_219.0 * t).cos();

        left.push(l);
        right.push(r);
    }

    (left, right)
}

fn run_case(filter: FilterType, left: &[f64], right: &[f64]) -> BenchResult {
    black_box((
        filter.as_id(),
        FilterType::from_id(filter.as_id()),
        FilterType::from_name(filter.as_name()),
    ));

    let mut resampler = SincResampler::new(filter, SOURCE_RATE, TARGET_RATE);
    black_box((resampler.source_rate(), resampler.target_rate()));
    let latency_ms = resampler.latency_ms();
    let memory_bytes = resampler.estimated_memory_bytes();

    let mut output = Vec::with_capacity(CHUNK_FRAMES * 8);
    let mut frames_out = 0;
    let mut checksum = 0.0;

    let start = Instant::now();
    for chunk_start in (0..left.len()).step_by(CHUNK_FRAMES) {
        let chunk_end = (chunk_start + CHUNK_FRAMES).min(left.len());
        resampler.input(
            &left[chunk_start..chunk_end],
            &right[chunk_start..chunk_end],
        );

        output.clear();
        let frames = resampler.process(&mut output);
        frames_out += frames;

        checksum += output.iter().step_by(257).copied().sum::<f64>();
        black_box(&output);
    }
    let elapsed = start.elapsed();
    resampler.reset();

    BenchResult {
        frames_out,
        elapsed,
        latency_ms,
        memory_bytes,
        checksum: black_box(checksum),
    }
}
