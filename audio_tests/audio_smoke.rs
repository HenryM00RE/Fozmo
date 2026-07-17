use std::f64::consts::PI;

use fozmo::audio::dsd::delta_sigma::DsdModulator;
use fozmo::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
use fozmo::audio::dsd::native_dsd::NativeDsdOrder;
use fozmo::audio::dsp::resampler::FilterType;

const SOURCE_RATE: u32 = 44_100;
const FRAMES: usize = 1024;

#[test]
fn production_modulators_render_native_dsd() {
    for modulator in [
        DsdModulator::Standard,
        DsdModulator::EcDepth2,
        DsdModulator::EcBeam,
        DsdModulator::EcBeam2,
    ] {
        let filter = if modulator == DsdModulator::EcBeam2 {
            FilterType::Split128k
        } else {
            FilterType::SincExtreme32k
        };
        let (left, right) = render_native_bits(modulator, filter, DsdRate::Dsd128);
        assert!(
            !left.is_empty(),
            "{} produced no left-channel DSD",
            modulator.as_name()
        );
        assert_eq!(
            left.len(),
            right.len(),
            "{} channel byte counts differ",
            modulator.as_name()
        );
    }
}

#[test]
fn ecbeam2_renders_every_supported_filter_and_rate() {
    for filter in [
        FilterType::Minimum16k,
        FilterType::Split128k,
        FilterType::SmoothPhase128k,
    ] {
        for rate in [DsdRate::Dsd64, DsdRate::Dsd128] {
            let (left, right) = render_native_bits(DsdModulator::EcBeam2, filter, rate);
            assert!(!left.is_empty(), "{filter:?} {rate:?} produced no DSD");
            assert_eq!(
                left.len(),
                right.len(),
                "{filter:?} {rate:?} channel lengths"
            );
        }
    }
}

#[test]
fn stale_persisted_modulator_aliases_normalize_to_standard() {
    for name in [
        "EcDepth1",
        "ec-1",
        "EcDepth3",
        "ec-3",
        "EcDepth4",
        "ec-4",
        "EcDepth8",
        "ec-8",
        "EcDepth4Adaptive",
        "ec-4a",
    ] {
        assert_eq!(DsdModulator::from_name(name), Some(DsdModulator::Standard));
    }
}

fn render_native_bits(
    modulator: DsdModulator,
    filter: FilterType,
    rate: DsdRate,
) -> (Vec<u8>, Vec<u8>) {
    let mut renderer = DsdRenderer::new_with_dsd_modulator(filter, SOURCE_RATE, rate, modulator)
        .expect("DSD renderer should initialize");
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let input = sine_input();
    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    renderer.upsample(&input, &input);
    let headroom_db = if matches!(modulator, DsdModulator::EcBeam | DsdModulator::EcBeam2) {
        -2.0
    } else {
        -4.0
    };
    let input_gain = 10.0f64.powf(headroom_db / 20.0);
    renderer.modulate_and_pack_native(input_gain, &mut out_l, &mut out_r);
    renderer.drain_resampler_eof();
    renderer.modulate_and_pack_native(input_gain, &mut out_l, &mut out_r);
    renderer.flush_modulators_and_pack_native(&mut out_l, &mut out_r);
    let expected_bytes = FRAMES * (rate.wire_rate_44k_family() as usize) / SOURCE_RATE as usize / 8;
    assert_eq!(out_l.len(), expected_bytes, "pre-idle left output length");
    assert_eq!(out_r.len(), expected_bytes, "pre-idle right output length");
    renderer.flush_native_with_idle(&mut out_l, &mut out_r);
    assert_eq!(out_l.len(), expected_bytes, "idle padded the left output");
    assert_eq!(out_r.len(), expected_bytes, "idle padded the right output");

    assert_eq!(renderer.stability_resets(), 0, "stability resets");
    assert_eq!(renderer.state_clamps(), 0, "state clamps");
    let limiter = renderer.limiter_telemetry();
    assert_eq!(limiter.limited_events, 0, "limiter events");
    assert_eq!(limiter.limited_samples, 0, "limited samples");
    let truncation = renderer.truncation_telemetry();
    assert_eq!(truncation.events, 0, "truncation events");
    assert_eq!(truncation.discarded_left_bits, 0, "discarded left bits");
    assert_eq!(truncation.discarded_right_bits, 0, "discarded right bits");
    for diagnostics in renderer.beam_diagnostics().into_iter().flatten() {
        assert_eq!(
            diagnostics.beam_committed_clamp_total, 0,
            "EcBeam committed clamps"
        );
        assert_eq!(
            diagnostics.beam_all_children_rejected_total, 0,
            "EcBeam all-children-rejected events"
        );
    }
    (out_l, out_r)
}

fn sine_input() -> Vec<f64> {
    (0..FRAMES)
        .map(|idx| {
            let t = idx as f64 / SOURCE_RATE as f64;
            0.25 * (2.0 * PI * 997.0 * t).sin()
        })
        .collect()
}
