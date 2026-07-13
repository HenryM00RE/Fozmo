use std::f64::consts::PI;

use fozmo::audio::dsd::delta_sigma::DsdModulator;
use fozmo::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
use fozmo::audio::dsd::native_dsd::NativeDsdOrder;
use fozmo::audio::dsp::resampler::FilterType;

const SOURCE_RATE: u32 = 44_100;
const FRAMES: usize = 4096;

#[test]
fn production_modulators_render_native_dsd() {
    for modulator in [DsdModulator::Standard, DsdModulator::EcDepth2] {
        let (left, right, resets) = render_native_bits(modulator);
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
        assert_eq!(
            resets,
            0,
            "{} reported stability resets",
            modulator.as_name()
        );
    }
}

#[test]
fn stale_persisted_modulator_aliases_normalize_to_ec2() {
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
        assert_eq!(DsdModulator::from_name(name), Some(DsdModulator::EcDepth2));
    }
}

fn render_native_bits(modulator: DsdModulator) -> (Vec<u8>, Vec<u8>, u64) {
    let mut renderer = DsdRenderer::new_with_dsd_modulator(
        FilterType::Minimum16k,
        SOURCE_RATE,
        DsdRate::Dsd128,
        modulator,
    )
    .expect("DSD renderer should initialize");
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let input = sine_input();
    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    renderer.upsample(&input, &input);
    renderer.modulate_and_pack_native(1.0, &mut out_l, &mut out_r);
    renderer.flush_modulators_and_pack_native(&mut out_l, &mut out_r);
    (out_l, out_r, renderer.stability_resets())
}

fn sine_input() -> Vec<f64> {
    (0..FRAMES)
        .map(|idx| {
            let t = idx as f64 / SOURCE_RATE as f64;
            0.25 * (2.0 * PI * 997.0 * t).sin()
        })
        .collect()
}
