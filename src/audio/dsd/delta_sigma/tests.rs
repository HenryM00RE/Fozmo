#[cfg(feature = "ecbeam2_observer")]
use super::beam_error_profile::profiles_for_wire_rate;
use super::coeff_math::*;
use super::dither::*;
use super::ec_beam::*;
#[cfg(feature = "ecbeam2_observer")]
use super::ecbeam2_observer::{
    ECBEAM2_OBSERVER_MAX_CHILDREN, EcBeam2ObserverConfig, EcBeam2ObserverError,
    EcBeam2ObserverEvent,
};
use super::modulator::*;
use super::stability::*;
use crate::audio::dsd::dsd_coeffs::CALIBRATED;

fn bit_density(bits: &[u8]) -> f64 {
    bits.iter().filter(|&&b| b == 1).count() as f64 / bits.len() as f64
}

fn sine_input(frames: usize, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|i| amp * (2.0 * std::f64::consts::PI * 0.001 * i as f64).sin())
        .collect()
}

fn python_constant(name: &str) -> f64 {
    const GEN_CRFB: &str = include_str!("../../../../tools/gen_crfb.py");
    let prefix = format!("{name} = ");
    GEN_CRFB
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed.strip_prefix(&prefix).map(|value| {
                value
                    .split('#')
                    .next()
                    .expect("split always returns a first segment")
                    .trim()
                    .parse::<f64>()
                    .unwrap_or_else(|err| panic!("failed to parse {name}: {err}"))
            })
        })
        .unwrap_or_else(|| panic!("missing Python constant {name}"))
}

fn assert_python_constant(name: &str, rust_value: f64) {
    let python_value = python_constant(name);
    assert!(
        (python_value - rust_value).abs() < 1e-12,
        "{name} mismatch: python={python_value}, rust={rust_value}",
    );
}

#[test]
fn dither_shapes_are_variance_matched() {
    const N: usize = 400_000;
    let variance = |samples: &dyn Fn(&mut DitherRng, &mut f64) -> f64| -> f64 {
        let mut rng = DitherRng::new(0xD17E_0001, DitherPrng::XorShift64);
        let mut prev = 0.0;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        for _ in 0..N {
            let x = samples(&mut rng, &mut prev);
            sum += x;
            sum_sq += x * x;
        }
        let mean = sum / N as f64;
        sum_sq / N as f64 - mean * mean
    };

    let white = variance(&|rng, _| next_white_tpdf_from(rng));
    for (alpha, gamma) in [(1.0, 0.0), (0.99, 0.0), (0.99, 0.05), (0.98, 0.03)] {
        let norm = high_pass_tpdf_norm(alpha, gamma);
        let hp = variance(&|rng, prev| next_high_pass_tpdf_from(rng, prev, alpha, gamma, norm));
        let ratio = hp / white;
        assert!(
            (ratio - 1.0).abs() < 0.02,
            "hp/white variance ratio {ratio} at alpha={alpha} gamma={gamma}"
        );
    }
}

#[test]
fn dc_bias_decay_for_corner_matches_one_pole_formula() {
    let d64 = dc_bias_decay_for_corner_hz(10.0, 2_822_400);
    assert!((d64 - (1.0 - core::f64::consts::TAU * 10.0 / 2_822_400.0)).abs() < 1e-15);
    // The legacy constant corresponds to a ~225 Hz corner at DSD64 — inside
    // the audio band, which is exactly what the explicit knob exists to fix.
    let legacy_corner = (1.0 - EC_DC_BIAS_DECAY) * 2_822_400.0 / core::f64::consts::TAU;
    assert!(
        (legacy_corner - 224.6).abs() < 1.0,
        "corner {legacy_corner}"
    );
    // Invalid inputs fall back to the legacy decay.
    assert_eq!(
        dc_bias_decay_for_corner_hz(0.0, 2_822_400),
        EC_DC_BIAS_DECAY
    );
    assert_eq!(
        dc_bias_decay_for_corner_hz(f64::NAN, 2_822_400),
        EC_DC_BIAS_DECAY
    );
    assert_eq!(dc_bias_decay_for_corner_hz(10.0, 0), EC_DC_BIAS_DECAY);
}

#[test]
fn dc_bias_decay_default_preserves_bitstream() {
    if !CALIBRATED {
        return;
    }
    let input = sine_input(8192, 0.3);
    let render = |decay: Option<f64>| -> (Vec<u8>, f64) {
        let mut m = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR128, 77)
            .expect("EC constructs");
        if let Some(decay) = decay {
            m.set_dc_bias_decay(decay);
        }
        let bits = run_to_bits(&mut m, &input);
        (bits, m.dc_bias)
    };
    // The instance decay defaults to the legacy constant: bit-identical.
    let (default_bits, _) = render(None);
    let (legacy_bits, _) = render(Some(EC_DC_BIAS_DECAY));
    assert_eq!(default_bits, legacy_bits);
    // An overridden corner must actually reach the tracker.
    let slow = dc_bias_decay_for_corner_hz(10.0, 5_644_800);
    let (_, slow_bias) = render(Some(slow));
    let (_, legacy_bias) = render(Some(EC_DC_BIAS_DECAY));
    assert_ne!(slow_bias, legacy_bias);
    // Invalid overrides are ignored.
    let mut m = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR128, 77)
        .expect("EC constructs");
    m.set_dc_bias_decay(1.5);
    assert_eq!(m.dc_bias_decay(), EC_DC_BIAS_DECAY);
}

#[test]
fn dither_rescale_preserves_shipped_effective_levels() {
    // Effective per-sample dither is `multiplier * norm(alpha, gamma)`. The
    // legacy normalization at the shipped alpha=1, gamma=0 was
    // sqrt(2/2) = 1, so the legacy effective multipliers were 0.375 / 0.30.
    let norm_alpha1 = (1.0f64 / 2.0).sqrt();
    let legacy_norm_alpha1 = (2.0f64 / 2.0).sqrt();
    assert!((EC_DITHER_SCALE_MULTIPLIER * norm_alpha1 - 0.375 * legacy_norm_alpha1).abs() < 1e-12);
    assert!(
        (EC_DSD128_DITHER_SCALE_MULTIPLIER * norm_alpha1 - 0.30 * legacy_norm_alpha1).abs() < 1e-12
    );
    // The invariance holds for every alpha/gamma because old/new norms
    // differ by exactly sqrt(2) independent of the leak settings.
}

#[test]
fn ec2_policy_off_and_trace_preserve_default_bitstream() {
    if !CALIBRATED {
        return;
    }

    let input = sine_input(4096, 0.3);
    let mut reference =
        CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR128, 0x1234)
            .expect("EC modulator");
    reference.set_lookahead_depth(2);
    let mut reference_bits = Vec::new();
    reference.process_into_bits(&input, &mut reference_bits);
    reference.flush_into_bits(&mut reference_bits);

    let mut explicit =
        CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR128, 0x1234)
            .expect("EC modulator");
    explicit.set_lookahead_depth(2);
    explicit.set_ec2_long_filter_policy(Ec2LongFilterPolicy::Off);
    explicit.set_ec2_policy_weights(Ec2PolicyWeights::default());
    explicit.set_ec2_decision_trace_window_bits(Some(1024));
    let mut explicit_bits = Vec::new();
    explicit.process_into_bits(&input, &mut explicit_bits);
    explicit.flush_into_bits(&mut explicit_bits);

    assert_eq!(reference_bits, explicit_bits);
    let trace = explicit.ec2_decision_trace().expect("trace snapshot");
    assert!(trace.summary.total_commits > 0);
}

#[test]
fn ec_candidate_selection_handles_ties_and_nonfinite_scores() {
    let cases = [
        (0.25, 0.5, 1.0, true),
        (0.5, 0.25, -1.0, true),
        (0.25, 0.25, 1.0, true),
        (0.25, f64::NAN, 1.0, true),
        (f64::NAN, 0.25, -1.0, true),
        (f64::INFINITY, 0.25, -1.0, true),
        (0.25, f64::INFINITY, 1.0, true),
        (f64::INFINITY, f64::INFINITY, 1.0, false),
        (f64::NAN, f64::NAN, 1.0, false),
    ];

    for (c_plus, c_minus, expected_v, expected_finite) in cases {
        let (v, finite) = select_ec_candidate(c_plus, c_minus);
        assert_eq!(v, expected_v, "c_plus={c_plus}, c_minus={c_minus}");
        assert_eq!(
            finite, expected_finite,
            "c_plus={c_plus}, c_minus={c_minus}"
        );
    }
}

#[test]
fn denormalized_feedback_matches_mul_then_affine_bit_exactly() {
    let base_norm = [-0.75, -0.125, 0.0, 0.03125, 0.25, 0.5, 0.9375, 0.0];
    let state_limit = [1.0 / 3.0, 1.25, 2.0, 3.5, 8.0, 13.0, 21.0, 0.0];
    let bv = [
        0.0009765625,
        -0.001953125,
        0.00390625,
        -0.0078125,
        0.015625,
        -0.03125,
        0.0625,
        0.0,
    ];
    let f_values = [
        -1.0,
        1.0,
        compensated_feedback(1.0, -1.0, DEFAULT_ISI_PENALTY),
        compensated_feedback(-1.0, 1.0, DEFAULT_ISI_PENALTY),
    ];

    for f in f_values {
        let expected = affine8(&mul8(&base_norm, &state_limit), &bv, f);
        let actual = denormalized_feedback8(&base_norm, &state_limit, &bv, f);
        for i in 0..8 {
            assert_eq!(
                actual[i].to_bits(),
                expected[i].to_bits(),
                "lane {i}, f={f}"
            );
        }
    }
}

/// Process an entire stream (with flush) and return the bits.
fn run_to_bits(m: &mut CrfbModulator, input: &[f64]) -> Vec<u8> {
    let mut bits = Vec::new();
    m.process_into_bits(input, &mut bits);
    m.flush_into_bits(&mut bits);
    bits
}

/// In-place iterative radix-2 Cooley-Tukey FFT. Test-only; n must be a power of 2.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two() && im.len() == n);
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let (w_re, w_im) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cur_re, mut cur_im) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (i + k, i + k + len / 2);
                let t_re = re[b] * cur_re - im[b] * cur_im;
                let t_im = re[b] * cur_im + im[b] * cur_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next_re = cur_re * w_re - cur_im * w_im;
                cur_im = cur_re * w_im + cur_im * w_re;
                cur_re = next_re;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// Windowed power spectrum (4-term Blackman-Harris) of a ±1 bitstream.
fn bit_power_spectrum(bits: &[u8], n: usize) -> Vec<f64> {
    assert!(bits.len() >= n);
    let mut re = Vec::with_capacity(n);
    let mut im = vec![0.0; n];
    let (a0, a1, a2, a3) = (0.35875, 0.48829, 0.14128, 0.01168);
    for (i, &b) in bits[..n].iter().enumerate() {
        let x = if b == 1 { 1.0 } else { -1.0 };
        let t = 2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64;
        let w = a0 - a1 * t.cos() + a2 * (2.0 * t).cos() - a3 * (3.0 * t).cos();
        re.push(x * w);
    }
    fft(&mut re, &mut im);
    (0..n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
}

#[test]
fn dsd_modulator_names_map_to_mode_and_depth() {
    assert_eq!(
        DsdModulator::from_name("Standard"),
        Some(DsdModulator::Standard)
    );
    assert_eq!(
        DsdModulator::from_name("EC depth 2"),
        Some(DsdModulator::EcDepth2)
    );
    assert_eq!(
        DsdModulator::from_name("7th Order ECB"),
        Some(DsdModulator::EcBeam)
    );
    assert_eq!(DsdModulator::EcBeam.as_id(), 2);
    assert_eq!(DsdModulator::from_id(2), DsdModulator::EcBeam);
    assert_eq!(
        DsdModulator::from_name("7th Order ECB2"),
        Some(DsdModulator::EcBeam2)
    );
    assert_eq!(
        DsdModulator::from_name("7th Order Beam"),
        Some(DsdModulator::EcBeam2)
    );
    assert_eq!(DsdModulator::EcBeam2.as_name(), "EcBeam2");
    assert_eq!(DsdModulator::EcBeam2.as_id(), 7);
    assert_eq!(DsdModulator::from_id(7), DsdModulator::EcBeam2);
    for stale_id in 3..=6 {
        assert_eq!(DsdModulator::from_id(stale_id), DsdModulator::EcDepth2);
    }
    for stale_alias in ["EC depth 1", "ec-3", "EcDepth4", "EC depth 8", "ec-4a"] {
        assert_eq!(
            DsdModulator::from_name(stale_alias),
            Some(DsdModulator::EcDepth2)
        );
    }
    assert_eq!(DsdModulator::Standard.mode(), ModulatorMode::Standard);
    assert_eq!(DsdModulator::Standard.lookahead_depth(), 1);
    assert_eq!(DsdModulator::EcDepth2.lookahead_depth(), 2);
    assert_eq!(DsdModulator::EcBeam.mode(), ModulatorMode::Ec);
    assert_eq!(DsdModulator::EcBeam.lookahead_depth(), 2);
    assert_eq!(DsdModulator::EcBeam2.mode(), ModulatorMode::Ec);
    assert_eq!(DsdModulator::EcBeam2.lookahead_depth(), 2);
}

#[test]
fn stabilize_state_clamps_only_offending_integrators() {
    let limit = [1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0];
    let mut inverse = [0.0; 8];
    for (inv, l) in inverse.iter_mut().zip(limit.iter()) {
        *inv = 1.0 / l;
    }
    let mut state = [0.25, -0.5, 1.0, -2.0, 2.5, -3.0, 6.0, 12.0];

    assert_eq!(
        stabilize_state(&mut state, &limit, &inverse),
        StateStability::Ok { clamped: true }
    );
    assert_eq!(state[0], 0.25);
    assert_eq!(state[5], -3.0);
    assert_eq!(state[6], 4.0);
    assert_eq!(state[7], 0.0);
}

#[test]
fn stabilize_state_sum_probe_catches_nonfinite_state() {
    let limit = [1.0; 7];
    let inverse = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0];
    let mut nan_state = [0.0; 8];
    nan_state[3] = f64::NAN;
    assert_eq!(
        stabilize_state(&mut nan_state, &limit, &inverse),
        StateStability::Reset
    );

    let mut inf_state = [0.0; 8];
    inf_state[4] = f64::INFINITY;
    assert_eq!(
        stabilize_state(&mut inf_state, &limit, &inverse),
        StateStability::Reset
    );
}

#[test]
fn xorshift_is_uniformish() {
    let mut rng = XorShift64::new(0xDEAD_BEEF);
    let mut sum = 0.0;
    let n = 10_000;
    for _ in 0..n {
        sum += rng.next_uniform_half();
    }
    // Mean of uniform(-0.5, 0.5) is 0; allow generous slack for 10k samples.
    assert!(
        (sum / n as f64).abs() < 0.02,
        "mean drifted: {}",
        sum / n as f64
    );
}

#[test]
fn tpdf_bounded() {
    let mut rng = XorShift64::new(42);
    for _ in 0..10_000 {
        let v = rng.next_tpdf();
        assert!(v > -1.0 && v < 1.0);
    }
}

#[test]
fn vector_helpers_match_scalar_reference() {
    let base = [0.12, -0.25, 0.37, -0.49, 0.58, -0.61, 0.73, 0.0];
    let col = [0.08, 0.13, -0.21, 0.34, -0.05, 0.07, -0.11, 0.0];
    let f = -0.875;

    let got_affine = affine8(&base, &col, f);
    let mut expected_affine = [0.0f64; 8];
    for i in 0..8 {
        expected_affine[i] = f.mul_add(col[i], base[i]);
    }
    for i in 0..8 {
        assert!(
            (got_affine[i] - expected_affine[i]).abs() <= 1e-15,
            "affine lane {i}: {} != {}",
            got_affine[i],
            expected_affine[i],
        );
    }

    let got_mul = mul8(&base, &col);
    for i in 0..8 {
        assert_eq!(got_mul[i], base[i] * col[i], "mul lane {i}");
    }

    let thr_sq = [0.9, 0.9, 0.05, 0.9, 0.9, 0.9, 0.9, 1.0];
    let (got_s, got_t, got_hot) = score_pair_dots(&base, &col, &thr_sq);
    let mut expected_s = 0.0f64;
    let mut expected_t = 0.0f64;
    let mut expected_hot = false;
    for i in 0..8 {
        let q = base[i] * base[i];
        expected_s += q;
        expected_t = base[i].mul_add(col[i], expected_t);
        expected_hot |= q > thr_sq[i];
    }
    assert!(
        (got_s - expected_s).abs() <= 1e-12,
        "{got_s} != {expected_s}"
    );
    assert!(
        (got_t - expected_t).abs() <= 1e-12,
        "{got_t} != {expected_t}"
    );
    assert_eq!(got_hot, expected_hot);
}

#[test]
fn modulator_construction_matches_calibration_flag() {
    let result = CrfbModulator::new(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 1);
    if CALIBRATED {
        assert!(result.is_ok(), "calibrated coefficients should construct");
    } else {
        assert!(result.is_err(), "placeholder coefficients must refuse");
    }
}

#[test]
fn ec_constants_match_generator() {
    assert_python_constant("DITHER_SCALE", QUANTIZER_DITHER_SCALE);
    assert_python_constant("EC_DITHER_SCALE_MULTIPLIER", EC_DITHER_SCALE_MULTIPLIER);
    assert_python_constant("DEFAULT_ISI_PENALTY", DEFAULT_ISI_PENALTY);
    assert_python_constant("EC_QUANTIZER_ERROR_WEIGHT", EC_QUANTIZER_ERROR_WEIGHT);
    assert_python_constant("EC_STATE_PRESSURE_WEIGHT", EC_STATE_PRESSURE_WEIGHT);
    assert_python_constant("EC_STATE_LIMIT_WEIGHT", EC_STATE_LIMIT_WEIGHT);
    assert_python_constant("EC_TRANSITION_WEIGHT", EC_TRANSITION_WEIGHT);
    assert_python_constant("EC_DC_BIAS_WEIGHT", EC_DC_BIAS_WEIGHT);
    assert_python_constant("EC_DC_BIAS_DECAY", EC_DC_BIAS_DECAY);
    assert_python_constant("EC_LOOKAHEAD_DISCOUNT", EC_LOOKAHEAD_DISCOUNT);
    assert_python_constant("EC_STATE_LIMIT_SOFT_KNEE", EC_STATE_LIMIT_SOFT_KNEE);
}

#[test]
fn ec_default_dither_scale_is_osr_aware() {
    if !CALIBRATED {
        return;
    }
    let dsd128 = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR128, 123)
        .expect("DSD128 EC constructs");
    let dsd256 = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR256, 123)
        .expect("DSD256 EC constructs");
    assert_eq!(
        dsd128.dither_scale(),
        QUANTIZER_DITHER_SCALE * EC_DSD128_DITHER_SCALE_MULTIPLIER
    );
    assert_eq!(
        dsd256.dither_scale(),
        QUANTIZER_DITHER_SCALE * EC_DITHER_SCALE_MULTIPLIER
    );
}

#[test]
fn ec_committed_bit_updates_feedback_bookkeeping_once() {
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 123)
        .expect("EC constructs");
    let bits = run_to_bits(&mut m, &[0.0]);
    assert_eq!(bits.len(), 1);
    let committed = if bits[0] == 1 { 1.0 } else { -1.0 };
    assert_eq!(m.prev_v, committed);
    assert!((m.dc_bias - updated_dc_bias(0.0, committed)).abs() < 1e-15);
}

#[test]
fn ec_bitstream_is_invariant_under_block_chunking() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = sine_input(512, coeffs.input_peak * 0.3);
    for depth in [1usize, 2, 3, 4] {
        let mut whole = CrfbModulator::new_ec(coeffs, 456).expect("EC constructs");
        whole.set_lookahead_depth(depth);
        let whole_bits = run_to_bits(&mut whole, &input);

        let mut chunked = CrfbModulator::new_ec(coeffs, 456).expect("EC constructs");
        chunked.set_lookahead_depth(depth);
        let mut chunked_bits = Vec::new();
        for chunk in input.chunks(7) {
            chunked.process_into_bits(chunk, &mut chunked_bits);
        }
        chunked.flush_into_bits(&mut chunked_bits);

        assert_eq!(whole_bits.len(), input.len());
        assert_eq!(
            whole_bits, chunked_bits,
            "depth-{depth} bitstream changed with block chunking",
        );
    }
}

#[test]
fn gated_dither_disabled_variants_preserve_bitstream() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    // Tiny-DC idle input: the near-tie window the gate keys on is common
    // here, so a live gate would change these bits. That makes this a real
    // guard that the half-configured and invalid settings stay inert.
    let input = vec![coeffs.input_peak * 1.0e-4; 4096 + 8];
    let baseline = run_to_bits(
        &mut CrfbModulator::new_ec(coeffs, 909).expect("EC constructs"),
        &input,
    );

    let mut margin_only = CrfbModulator::new_ec(coeffs, 909).expect("EC constructs");
    margin_only.set_gated_dither(0.05, 0.0); // scale 0 => inert
    assert_eq!(run_to_bits(&mut margin_only, &input), baseline);

    let mut scale_only = CrfbModulator::new_ec(coeffs, 909).expect("EC constructs");
    scale_only.set_gated_dither(0.0, 0.25); // margin 0 => inert
    assert_eq!(run_to_bits(&mut scale_only, &input), baseline);

    let mut invalid = CrfbModulator::new_ec(coeffs, 909).expect("EC constructs");
    invalid.set_gated_dither(f64::NAN, -1.0);
    assert_eq!(invalid.gated_dither_margin(), 0.0);
    assert_eq!(invalid.gated_dither_scale(), 0.0);
    assert_eq!(run_to_bits(&mut invalid, &input), baseline);
}

#[test]
fn gated_dither_fires_on_ties_stays_stable_and_deterministic() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let n = 8192;
    let idle = vec![coeffs.input_peak * 1.0e-4; n + 8];
    let loud = sine_input(n + 8, coeffs.input_peak * 0.5);

    let idle_off = run_to_bits(
        &mut CrfbModulator::new_ec(coeffs, 909).expect("EC constructs"),
        &idle,
    );
    let mut idle_on_mod = CrfbModulator::new_ec(coeffs, 909).expect("EC constructs");
    idle_on_mod.set_gated_dither(0.2, 0.25);
    let idle_on = run_to_bits(&mut idle_on_mod, &idle);
    // The gate must actually fire on idle limit-cycle ties, and it must not
    // destabilize the loop when it does.
    let idle_flips = idle_off
        .iter()
        .zip(&idle_on)
        .filter(|(a, b)| a != b)
        .count();
    assert!(idle_flips > 0, "gated dither never fired on idle input");
    assert_eq!(idle_on_mod.stability_resets(), 0);
    assert_eq!(idle_on_mod.state_clamps(), 0);

    // On a loud tone the argmin is decisive almost everywhere, so far fewer
    // samples are near-ties: the gate concentrates on the idle case.
    let loud_off = run_to_bits(
        &mut CrfbModulator::new_ec(coeffs, 909).expect("EC constructs"),
        &loud,
    );
    let mut loud_on_mod = CrfbModulator::new_ec(coeffs, 909).expect("EC constructs");
    loud_on_mod.set_gated_dither(0.2, 0.25);
    let loud_on = run_to_bits(&mut loud_on_mod, &loud);
    let loud_flips = loud_off
        .iter()
        .zip(&loud_on)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(loud_on_mod.stability_resets(), 0);
    assert!(
        loud_flips < idle_flips,
        "gate did not concentrate on idle ties (idle {idle_flips}, loud {loud_flips})"
    );

    // Determinism: gated ON, whole-stream vs 7-sample chunking must match.
    let mut chunked = CrfbModulator::new_ec(coeffs, 909).expect("EC constructs");
    chunked.set_gated_dither(0.2, 0.25);
    let mut chunked_bits = Vec::new();
    for chunk in idle.chunks(7) {
        chunked.process_into_bits(chunk, &mut chunked_bits);
    }
    chunked.flush_into_bits(&mut chunked_bits);
    assert_eq!(
        chunked_bits, idle_on,
        "gated dither broke block-chunking invariance"
    );
}

#[test]
fn per_stage_pressure_uniform_and_invalid_preserve_bitstream() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = sine_input(4096, coeffs.input_peak * 0.3);
    let baseline = run_to_bits(
        &mut CrfbModulator::new_ec(coeffs, 202).expect("EC constructs"),
        &input,
    );

    // Any positive constant normalizes to 1/7 per stage, i.e. the uniform
    // default: the setter must detect that and leave the fast path engaged.
    for uniform in [[1.0; 7], [2.5; 7]] {
        let mut m = CrfbModulator::new_ec(coeffs, 202).expect("EC constructs");
        m.set_pressure_stage_weights(&uniform);
        assert!(
            !m.pressure_stage_weighted(),
            "uniform weights must not activate per-stage weighting"
        );
        assert_eq!(run_to_bits(&mut m, &input), baseline);
    }

    // Invalid inputs (non-finite, negative, zero-sum) disable the feature.
    let invalid: [[f64; 7]; 3] = [
        [f64::NAN, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        [0.0; 7],
    ];
    for bad in invalid {
        let mut m = CrfbModulator::new_ec(coeffs, 202).expect("EC constructs");
        m.set_pressure_stage_weights(&bad);
        assert!(!m.pressure_stage_weighted());
        assert_eq!(run_to_bits(&mut m, &input), baseline);
    }
}

#[test]
fn per_stage_pressure_skew_changes_output_stays_stable_and_deterministic() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = sine_input(8192, coeffs.input_peak * 0.4);
    let off = run_to_bits(
        &mut CrfbModulator::new_ec(coeffs, 202).expect("EC constructs"),
        &input,
    );

    // Redistribute pressure toward the late integrators.
    let skew = [0.5, 0.5, 1.0, 1.0, 1.5, 2.0, 2.0];
    let mut on = CrfbModulator::new_ec(coeffs, 202).expect("EC constructs");
    on.set_pressure_stage_weights(&skew);
    assert!(on.pressure_stage_weighted());
    let on_bits = run_to_bits(&mut on, &input);
    assert_ne!(
        on_bits, off,
        "skewed per-stage weights did not change the bitstream"
    );
    assert_eq!(on.stability_resets(), 0);
    assert_eq!(on.state_clamps(), 0);

    // Determinism: weighted, whole vs 7-sample chunking must match.
    let mut chunked = CrfbModulator::new_ec(coeffs, 202).expect("EC constructs");
    chunked.set_pressure_stage_weights(&skew);
    let mut chunked_bits = Vec::new();
    for chunk in input.chunks(7) {
        chunked.process_into_bits(chunk, &mut chunked_bits);
    }
    chunked.flush_into_bits(&mut chunked_bits);
    assert_eq!(
        chunked_bits, on_bits,
        "per-stage pressure broke block-chunking invariance"
    );
}

#[test]
fn ec_lookahead_dither_peek_does_not_advance_committed_rng() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let seed = 0xCAFE_F00D;
    let input = sine_input(4096, coeffs.input_peak * 0.35);
    let mut m = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    m.set_lookahead_depth(4);

    let bits = run_to_bits(&mut m, &input);
    assert_eq!(bits.len(), input.len());

    let mut rng = DitherRng::new(seed, DitherPrng::XorShift64);
    let mut prev_tpdf = 0.0;
    for _ in 0..input.len() {
        let _ = next_dither_from(
            &mut rng,
            DitherShape::HighPassTpdf,
            &mut prev_tpdf,
            1.0,
            0.0,
            high_pass_tpdf_norm(1.0, 0.0),
        );
    }
    assert_eq!(m.rng, rng);
    assert_eq!(m.prev_tpdf, prev_tpdf);
}

#[test]
fn dc_input_modulator_stays_stable() {
    // Half of the measured input peak is well inside the calibrated stable range.
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 42)
        .expect("construction succeeds when calibrated");
    let input_amp = 0.5 * crate::audio::dsd::dsd_coeffs::CRFB_OSR128.input_peak;
    let input = vec![input_amp; 10_000];
    let bits = run_to_bits(&mut m, &input);
    assert_eq!(bits.len(), 10_000);
    assert_eq!(
        m.stability_resets(),
        0,
        "modulator went unstable on a benign DC input",
    );
    // Density of 1 bits should track the input level:
    // average bit value ~= (1 + input_amp) / 2. Allow generous slack.
    let density = bit_density(&bits);
    assert!(
        (0.55..0.75).contains(&density),
        "bit density {density} far from expected ~0.66",
    );
}

#[test]
fn silence_input_is_stable() {
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 99)
        .expect("construction succeeds when calibrated");
    let input = vec![0.0; 10_000];
    let bits = run_to_bits(&mut m, &input);
    assert_eq!(bits.len(), 10_000);
    assert_eq!(m.stability_resets(), 0);
    // 0-input should produce approximately 50/50 bit density (modulated by dither).
    let density = bit_density(&bits);
    assert!(
        (0.40..0.60).contains(&density),
        "silence density {density} not near 0.5",
    );
}

#[test]
fn ec_silence_input_is_stable() {
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 99)
        .expect("construction succeeds when calibrated");
    let input = vec![0.0; 10_000];
    let bits = run_to_bits(&mut m, &input);
    assert_eq!(bits.len(), 10_000);
    assert_eq!(m.stability_resets(), 0);
    let density = bit_density(&bits);
    assert!(
        (0.40..0.60).contains(&density),
        "EC silence density {density} not near 0.5",
    );
}

#[test]
fn ec_low_level_and_hot_sines_are_stable() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    for depth in [1usize, 2, 4] {
        for dbfs in [-60.0, -20.0, -6.0, -1.0] {
            let amp = coeffs.input_peak * 10.0f64.powf(dbfs / 20.0);
            let input = sine_input(20_000, amp);
            let mut m =
                CrfbModulator::new_ec(coeffs, 1000 + (dbfs.abs() as u64)).expect("EC constructs");
            m.set_lookahead_depth(depth);
            let bits = run_to_bits(&mut m, &input);
            assert_eq!(bits.len(), input.len());
            assert_eq!(
                m.stability_resets(),
                0,
                "EC depth {depth} reset on {dbfs} dBFS sine",
            );
        }
    }
}

#[test]
fn ec_tiny_dc_and_finite_overload_are_stable() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let tiny_dc = vec![coeffs.input_peak * 1.0e-4; 12_000];
    let mut m = CrfbModulator::new_ec(coeffs, 123).expect("EC constructs");
    let bits = run_to_bits(&mut m, &tiny_dc);
    assert_eq!(bits.len(), tiny_dc.len());
    assert_eq!(m.stability_resets(), 0);

    let finite_overload = vec![0.95; 20_000];
    let bits = run_to_bits(&mut m, &finite_overload);
    assert_eq!(bits.len(), finite_overload.len());
    assert_eq!(
        m.stability_resets(),
        0,
        "finite EC overload should clamp integrators instead of hard-resetting",
    );
}

#[test]
fn ec_depth1_osr256_bitstream_is_invariant_under_block_chunking() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR256;
    let input: Vec<f64> = (0..8192)
        .map(|i| {
            let t = i as f64;
            coeffs.input_peak
                * (0.31 * (2.0 * std::f64::consts::PI * 0.0011 * t).sin()
                    + 0.07 * (2.0 * std::f64::consts::PI * 0.0093 * t).cos()
                    + 0.02 * (2.0 * std::f64::consts::PI * 0.041 * t).sin())
        })
        .collect();

    let mut whole = CrfbModulator::new_ec(coeffs, 0xEC01).expect("EC constructs");
    whole.set_lookahead_depth(1);
    let whole_bits = run_to_bits(&mut whole, &input);
    assert_eq!(whole_bits.len(), input.len());
    assert_eq!(whole.stability_resets(), 0);

    for chunk_size in [1usize, 3, 64, 1025] {
        let mut chunked = CrfbModulator::new_ec(coeffs, 0xEC01).expect("EC constructs");
        chunked.set_lookahead_depth(1);
        let mut chunked_bits = Vec::new();
        for chunk in input.chunks(chunk_size) {
            chunked.process_into_bits(chunk, &mut chunked_bits);
        }
        chunked.flush_into_bits(&mut chunked_bits);
        assert_eq!(
            whole_bits, chunked_bits,
            "depth-1 EC OSR256 bitstream changed with chunk size {chunk_size}",
        );
        assert_eq!(chunked.stability_resets(), 0);
    }
}

#[test]
fn ec_depth1_fast_path_matches_generic_root_with_isi_penalty() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR256;
    let input: Vec<f64> = (0..4096)
        .map(|i| {
            let t = i as f64;
            coeffs.input_peak
                * (0.28 * (2.0 * std::f64::consts::PI * 0.0017 * t).sin()
                    + 0.12 * (2.0 * std::f64::consts::PI * 0.017 * t).cos())
        })
        .collect();

    let mut fast = CrfbModulator::new_ec(coeffs, 0x151).expect("EC constructs");
    fast.set_lookahead_depth(1);
    fast.set_isi_penalty(DEFAULT_ISI_PENALTY);
    let fast_bits = run_to_bits(&mut fast, &input);

    let mut generic = CrfbModulator::new_ec(coeffs, 0x151).expect("EC constructs");
    generic.set_lookahead_depth(1);
    generic.set_isi_penalty(DEFAULT_ISI_PENALTY);
    let mut generic_bits = Vec::with_capacity(input.len());
    let mut carry = None;
    for &u in &input {
        generic.process_ec_buffered_sample::<true, 1>(u, &[], &mut carry, &mut generic_bits);
        assert!(carry.is_none(), "depth-1 generic root should not carry");
    }

    assert_eq!(fast_bits, generic_bits);
    assert_eq!(fast.state, generic.state);
    assert_eq!(fast.prev_v, generic.prev_v);
    assert_eq!(fast.dc_bias, generic.dc_bias);
    assert_eq!(fast.stability_resets(), generic.stability_resets());
    assert_eq!(fast.state_clamps(), generic.state_clamps());
}

#[test]
fn ec_depth1_dither_shape_setter_matches_generic_root() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR256;
    let input: Vec<f64> = (0..4096)
        .map(|i| {
            let t = i as f64;
            coeffs.input_peak
                * (0.21 * (2.0 * std::f64::consts::PI * 0.0023 * t).sin()
                    + 0.09 * (2.0 * std::f64::consts::PI * 0.011 * t).cos())
        })
        .collect();
    let segments = [
        (DitherShape::HighPassTpdf, 0usize, 1237usize),
        (DitherShape::WhiteTpdf, 1237, 2911),
        (DitherShape::HighPassTpdf, 2911, input.len()),
    ];

    let mut fast = CrfbModulator::new_ec(coeffs, 0xD17E).expect("EC constructs");
    fast.set_lookahead_depth(1);
    let mut fast_bits = Vec::with_capacity(input.len());
    for &(shape, start, end) in &segments {
        fast.set_dither_shape(shape);
        fast.process_into_bits(&input[start..end], &mut fast_bits);
    }

    let mut generic = CrfbModulator::new_ec(coeffs, 0xD17E).expect("EC constructs");
    generic.set_lookahead_depth(1);
    let mut generic_bits = Vec::with_capacity(input.len());
    let mut carry = None;
    for &(shape, start, end) in &segments {
        generic.set_dither_shape(shape);
        for &u in &input[start..end] {
            generic.process_ec_buffered_sample::<true, 1>(u, &[], &mut carry, &mut generic_bits);
            assert!(carry.is_none(), "depth-1 generic root should not carry");
        }
    }

    assert_eq!(fast_bits, generic_bits);
    assert_eq!(fast.state, generic.state);
    assert_eq!(fast.prev_v, generic.prev_v);
    assert_eq!(fast.prev_tpdf, generic.prev_tpdf);
    assert_eq!(fast.rng, generic.rng);
    assert_eq!(fast.dc_bias, generic.dc_bias);
    assert_eq!(fast.stability_resets(), generic.stability_resets());
    assert_eq!(fast.state_clamps(), generic.state_clamps());
}

#[test]
fn ec_depth1_osr256_silence_sines_and_finite_overload_are_stable() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR256;
    for input in [
        vec![0.0; 12_000],
        sine_input(16_000, coeffs.input_peak * 10.0f64.powf(-60.0 / 20.0)),
        sine_input(16_000, coeffs.input_peak * 0.5),
        sine_input(16_000, coeffs.input_peak * 10.0f64.powf(-1.0 / 20.0)),
        vec![0.95; 16_000],
    ] {
        let mut m = CrfbModulator::new_ec(coeffs, 0x256).expect("EC constructs");
        m.set_lookahead_depth(1);
        let bits = run_to_bits(&mut m, &input);
        assert_eq!(bits.len(), input.len());
        assert_eq!(
            m.stability_resets(),
            0,
            "depth-1 EC OSR256 reset on stable finite input",
        );
    }
}

#[test]
fn ec_depth1_osr256_nonfinite_input_trips_last_resort_reset() {
    if !CALIBRATED {
        return;
    }
    for u in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut m = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB_OSR256, 0xBAD)
            .expect("EC constructs");
        m.set_lookahead_depth(1);

        let bits = run_to_bits(&mut m, &[u]);
        assert_eq!(bits, vec![1]);
        assert_eq!(m.stability_resets(), 1);

        let mut recovered_bits = Vec::new();
        m.process_into_bits(&[0.0], &mut recovered_bits);
        m.flush_into_bits(&mut recovered_bits);
        assert_eq!(recovered_bits.len(), 1);
        assert_eq!(m.stability_resets(), 1);
    }
}

#[test]
fn ec_nonfinite_input_trips_last_resort_reset() {
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 11)
        .expect("construction succeeds when calibrated");
    let bits = run_to_bits(&mut m, &[f64::NAN]);

    assert_eq!(bits.len(), 1);
    assert_eq!(m.stability_resets(), 1);
}

#[test]
fn overloaded_input_saturates_without_resetting_state() {
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 7)
        .expect("construction succeeds when calibrated");
    let input = vec![0.95; 20_000];
    let bits = run_to_bits(&mut m, &input);

    assert_eq!(bits.len(), input.len());
    assert_eq!(
        m.stability_resets(),
        0,
        "finite overload should clamp integrators instead of hard-resetting",
    );
}

#[test]
fn nonfinite_input_trips_last_resort_reset() {
    if !CALIBRATED {
        return;
    }
    let mut m = CrfbModulator::new(&crate::audio::dsd::dsd_coeffs::CRFB_OSR128, 11)
        .expect("construction succeeds when calibrated");
    let bits = run_to_bits(&mut m, &[f64::NAN]);

    assert_eq!(bits.len(), 1);
    assert_eq!(m.stability_resets(), 1);
}

/// In-band SINAD floor on a −6 dBFS sine. This is the regression gate that fails
/// if in-band noise quietly rises; the threshold is the measured value minus a
/// generous margin, not a target.
#[test]
fn ec_sine_sinad_floor() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let mut m = CrfbModulator::new_ec(coeffs, 2024).expect("EC constructs");
    // −6 dBFS sine on bin 37 of a 64k FFT.
    let sinad_db = measure_sinad_db(&mut m, 1usize << 16, 37, coeffs.input_peak * 0.5);
    // Measured ~151.7 dB at change time; gate well below to absorb seed variance.
    assert!(
        sinad_db > 135.0,
        "in-band SINAD regressed: {sinad_db:.1} dB (expected > 135 dB)",
    );
}

fn measure_sinad_db(m: &mut CrfbModulator, n: usize, signal_bin: usize, amp: f64) -> f64 {
    let osr = m.coeffs.osr as usize;
    let input: Vec<f64> = (0..n + 8)
        .map(|i| amp * (2.0 * std::f64::consts::PI * signal_bin as f64 * i as f64 / n as f64).sin())
        .collect();
    let bits = run_to_bits(m, &input);
    assert_eq!(m.stability_resets(), 0);
    let spectrum = bit_power_spectrum(&bits, n);
    let in_band = n / (2 * osr);
    let guard = 6usize;
    let signal: f64 = spectrum[signal_bin - guard..=signal_bin + guard]
        .iter()
        .sum();
    let noise: f64 = spectrum[1..in_band]
        .iter()
        .enumerate()
        .filter(|(k, _)| (*k + 1).abs_diff(signal_bin) > guard)
        .map(|(_, p)| p)
        .sum();
    10.0 * (signal / noise).log10()
}

/// Diagnostic, not a gate: prints in-band SINAD per lookahead depth.
/// Run with `cargo test --release sinad_by_depth -- --ignored --nocapture`.
#[test]
#[ignore]
fn ec_sinad_by_depth_report() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let n = 1usize << 16;
    for depth in 1..=6 {
        let mut m = CrfbModulator::new_ec(coeffs, 2024).expect("EC constructs");
        m.set_lookahead_depth(depth);
        let sinad = measure_sinad_db(&mut m, n, 37, coeffs.input_peak * 0.5);
        println!("depth {depth}: SINAD = {sinad:.1} dB");
    }
}

#[test]
fn effective_future_scorer_degrades_full_family_above_osr128() {
    let full_family = [
        EcFutureScorer::Full,
        EcFutureScorer::FullDiscount40,
        EcFutureScorer::FullDiscount25,
        EcFutureScorer::FullDiscount10,
        EcFutureScorer::FullDepth3Guard0001,
        EcFutureScorer::FullDepth3Guard001,
        EcFutureScorer::FullDepth3Guard01,
        EcFutureScorer::FullDepth3Guard05,
        EcFutureScorer::FullDepth3Guard10,
    ];
    for scorer in full_family {
        assert_eq!(scorer.effective_for_osr(64), scorer);
        assert_eq!(scorer.effective_for_osr(128), scorer);
        assert_eq!(
            scorer.effective_for_osr(256),
            EcFutureScorer::QuantizerLimit
        );
    }
    for scorer in [
        EcFutureScorer::QuantizerOnly,
        EcFutureScorer::QuantizerLimit,
        EcFutureScorer::QuarterPressureNoDcTransition,
    ] {
        for osr in [64, 128, 256] {
            assert_eq!(scorer.effective_for_osr(osr), scorer);
        }
    }
}

/// The mapping in `effective_for_osr` must match the actual hot-path degrade:
/// at OSR 256, a modulator configured `Full` produces exactly the same
/// bitstream as one configured `QuantizerLimit`.
#[test]
fn effective_future_scorer_matches_hot_path_degrade_at_osr256() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB7_EC_OSR256;
    assert!(
        coeffs.osr > 128,
        "OSR256 table must exceed the degrade edge"
    );
    let n = 1usize << 14;
    let input: Vec<f64> = (0..n + 8)
        .map(|i| {
            coeffs.input_peak
                * 0.5
                * (2.0 * std::f64::consts::PI * 37.0 * i as f64 / n as f64).sin()
        })
        .collect();

    let mut full = CrfbModulator::new_ec(coeffs, 2024).expect("EC constructs");
    full.set_future_scorer(EcFutureScorer::Full);
    let full_bits = run_to_bits(&mut full, &input);

    let mut ql = CrfbModulator::new_ec(coeffs, 2024).expect("EC constructs");
    ql.set_future_scorer(EcFutureScorer::QuantizerLimit);
    let ql_bits = run_to_bits(&mut ql, &input);

    assert_eq!(
        full_bits, ql_bits,
        "Full silently degrades to QuantizerLimit at OSR>128; \
             effective_for_osr must model that"
    );
    assert_eq!(
        EcFutureScorer::Full.effective_for_osr(coeffs.osr),
        EcFutureScorer::QuantizerLimit
    );
}

/// Tiny-DC idle-tone gate: no discrete in-band spur may tower over the local
/// noise floor. Guards the EC path's long-run-length bias against patterning.
#[test]
fn ec_tiny_dc_has_no_dominant_idle_spur() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let n = 1usize << 16;
    let in_band = n / (2 * coeffs.osr as usize);
    let input = vec![coeffs.input_peak * 1.0e-4; n + 8];

    let mut m = CrfbModulator::new_ec(coeffs, 31337).expect("EC constructs");
    let bits = run_to_bits(&mut m, &input);
    assert_eq!(m.stability_resets(), 0);
    let spectrum = bit_power_spectrum(&bits, n);

    // Skip the first bins (DC leakage from the deliberate offset).
    let band = &spectrum[8..in_band];
    let mut sorted: Vec<f64> = band.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2].max(f64::MIN_POSITIVE);
    let peak = sorted[sorted.len() - 1];
    let spur_db = 10.0 * (peak / median).log10();
    // Measured ~0.3 dB at change time (no spur); 20 dB flags real patterning.
    assert!(
        spur_db < 20.0,
        "idle spur {spur_db:.1} dB above the in-band median noise floor",
    );
}

// ---- EcBeam prototype (docs/dev/7th-order-ecm-m-algorithm.md §21.9) ----

/// Program-like multi-tone fixture in the existing OSR256-test idiom.
fn beam_program_input(peak: f64, frames: usize) -> Vec<f64> {
    (0..frames)
        .map(|i| {
            let t = i as f64;
            peak * (0.31 * (2.0 * std::f64::consts::PI * 0.0011 * t).sin()
                + 0.07 * (2.0 * std::f64::consts::PI * 0.0093 * t).cos()
                + 0.02 * (2.0 * std::f64::consts::PI * 0.041 * t).sin())
        })
        .collect()
}

/// §21.9.1 — the anchor: an M=1 beam (any N) must be byte-identical to the
/// shipped `EcDepth1` path, because M=1 selection is greedy every step and
/// the delay only reorders emission, not decisions. Run at a low amplitude
/// (clamps never fire — isolates scoring) and on the finite-overload
/// fixture (exercises the clamp path), under both dither shapes.
#[test]
fn beam_m1_matches_ec_depth1_bitstream() {
    if !CALIBRATED {
        return;
    }
    // Precondition (§21.3): the fixed depth-1 weights the beam scores with
    // must equal the Ec2 policy defaults, so a future retune fails loudly
    // here instead of producing a mystery anchor mismatch.
    let defaults = Ec2PolicyWeights::default();
    assert_eq!(EC_DEPTH1_STATE_PRESSURE_WEIGHT, defaults.pressure_weight);
    assert_eq!(EC_DEPTH1_TRANSITION_WEIGHT, defaults.transition_weight);
    assert_eq!(EC_DC_BIAS_WEIGHT, defaults.dc_weight);
    assert_eq!(EC_QUANTIZER_ERROR_WEIGHT, defaults.quantizer_weight);
    assert_eq!(EC_STATE_LIMIT_WEIGHT, defaults.limit_weight);
    assert_eq!(EC_BEAM_FILTERED_ERROR_WEIGHT, 0.0);
    assert_eq!(EC_BEAM_FILTERED_ERROR_RANK_WEIGHT, 0.0);
    assert_eq!(EC_BEAM_RECONSTRUCTION_ERROR_WEIGHT, 0.0);

    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let low = beam_program_input(coeffs.input_peak * 0.35, 6000);
    let hot = vec![0.95; 6000];
    for (label, input) in [("low", &low), ("hot", &hot)] {
        for shape in [DitherShape::HighPassTpdf, DitherShape::WhiteTpdf] {
            for n in [1usize, 8] {
                let mut reference = CrfbModulator::new_ec(coeffs, 0xBEA1).expect("EC constructs");
                reference.set_lookahead_depth(1);
                reference.set_dither_shape(shape);
                let reference_bits = run_to_bits(&mut reference, input);

                let mut beam = CrfbModulator::new_ec(coeffs, 0xBEA1).expect("EC constructs");
                beam.set_lookahead_depth(1);
                beam.set_dither_shape(shape);
                beam.set_beam_search(1, n);
                assert_eq!(beam.beam_filtered_error_weight(), Some(0.0));
                assert_eq!(beam.beam_filtered_error_rank_weight(), Some(0.0));
                assert_eq!(beam.beam_reconstruction_error_weight(), Some(0.0));
                let beam_bits = run_to_bits(&mut beam, input);

                assert_eq!(
                    beam_bits, reference_bits,
                    "M=1/N={n} beam diverged from EcDepth1 ({label}, {shape:?})",
                );
                assert_eq!(beam.state, reference.state, "{label}/N={n} state");
                assert_eq!(beam.prev_v, reference.prev_v, "{label}/N={n} prev_v");
                assert_eq!(beam.dc_bias, reference.dc_bias, "{label}/N={n} dc_bias");
                assert_eq!(beam.rng, reference.rng, "{label}/N={n} rng position");
                assert_eq!(beam.prev_tpdf, reference.prev_tpdf, "{label}/N={n} tpdf");
                assert_eq!(
                    beam.state_clamps(),
                    reference.state_clamps(),
                    "{label}/N={n} clamps"
                );
                assert_eq!(
                    beam.stability_resets(),
                    reference.stability_resets(),
                    "{label}/N={n} resets"
                );
            }
        }
    }
    // The hot fixture must actually exercise the clamp path for the
    // clamp-sequencing half of the anchor to mean anything.
    let mut probe = CrfbModulator::new_ec(coeffs, 0xBEA1).expect("EC constructs");
    probe.set_lookahead_depth(1);
    let _ = run_to_bits(&mut probe, &hot);
    assert!(
        probe.state_clamps() > 0,
        "overload fixture no longer clamps; the anchor's hot run is inert"
    );
}

#[test]
fn beam_auxiliary_weights_are_rank_only_not_accumulated_metric() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = [coeffs.input_peak * 0.12];

    let mut clean = CrfbModulator::new_ec(coeffs, 0xBEA2).expect("EC constructs");
    clean.set_beam_search(2, 8);
    clean.set_beam_auxiliary_metric_scales(0.0, 1.0, 0.0, 1.0);
    clean.set_ec2_policy_weights(Ec2PolicyWeights {
        pressure_weight: 0.0,
        transition_weight: 0.0,
        dc_weight: 0.0,
        ..Ec2PolicyWeights::default()
    });
    let mut clean_out = Vec::new();
    clean.process_into_bits(&input, &mut clean_out);

    let mut shaped = CrfbModulator::new_ec(coeffs, 0xBEA2).expect("EC constructs");
    shaped.set_beam_search(2, 8);
    shaped.set_beam_auxiliary_metric_scales(0.0, 1.0, 0.0, 1.0);
    shaped.set_ec2_policy_weights(Ec2PolicyWeights {
        pressure_weight: 1000.0,
        transition_weight: 0.0,
        dc_weight: 1000.0,
        ..Ec2PolicyWeights::default()
    });
    shaped.set_beam_alternation_weight(1000.0);
    let mut shaped_out = Vec::new();
    shaped.process_into_bits(&input, &mut shaped_out);

    let clean_beam = clean.beam.as_ref().expect("beam active");
    let shaped_beam = shaped.beam.as_ref().expect("beam active");
    let mut clean_metrics: Vec<(u64, f64)> = clean_beam.parents[clean_beam.parents_bank]
        .iter()
        .take(clean_beam.parents_len)
        .map(|path| (path.bits, path.metric))
        .collect();
    let mut shaped_metrics: Vec<(u64, f64)> = shaped_beam.parents[shaped_beam.parents_bank]
        .iter()
        .take(shaped_beam.parents_len)
        .map(|path| (path.bits, path.metric))
        .collect();
    clean_metrics.sort_by_key(|(bits, _)| *bits);
    shaped_metrics.sort_by_key(|(bits, _)| *bits);
    assert_eq!(clean_metrics, shaped_metrics);
}

#[test]
fn beam_metric_hygiene_accumulates_transition_cost_once() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = [0.0];
    let transition_weight = 123.0;

    let mut m = CrfbModulator::new_ec(coeffs, 0xBEA3).expect("EC constructs");
    m.set_beam_search(2, 8);
    m.set_beam_auxiliary_metric_scales(0.0, 0.0, 0.0, 0.0);
    m.set_ec2_policy_weights(Ec2PolicyWeights {
        quantizer_weight: 0.0,
        pressure_weight: 0.0,
        transition_weight,
        dc_weight: 0.0,
        ..Ec2PolicyWeights::default()
    });

    let mut out = Vec::new();
    m.process_into_bits(&input, &mut out);

    let beam = m.beam.as_ref().expect("beam active");
    let mut metrics: Vec<(u64, f64)> = beam.parents[beam.parents_bank]
        .iter()
        .take(beam.parents_len)
        .map(|path| (path.bits, path.metric))
        .collect();
    metrics.sort_by_key(|(bits, _)| *bits);

    assert_eq!(metrics.len(), 2);
    assert_eq!(metrics[0].0, 0);
    assert_eq!(metrics[1].0, 1);
    assert!((metrics[0].1 - transition_weight).abs() < 1e-12);
    assert!(metrics[1].1.abs() < 1e-12);
}

#[test]
fn beam_soft_limit_penalty_scores_knee_before_hard_overflow() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let m = CrfbModulator::new_ec(coeffs, 0x51A7).expect("EC constructs");
    let mut base_norm = [0.0; 8];

    base_norm[0] = EC_STATE_LIMIT_SOFT_KNEE + 0.5 / EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN;
    let soft_only = m.beam_soft_limit_penalty(&base_norm, 0.0);
    let hard_only = m.beam_hard_limit_overflow_penalty(&base_norm, 0.0);

    assert!((soft_only - 0.25).abs() < 1e-12);
    assert_eq!(hard_only, 0.0);

    base_norm[0] = 1.01;
    let soft_with_overflow = m.beam_soft_limit_penalty(&base_norm, 0.0);
    let hard_with_overflow = m.beam_hard_limit_overflow_penalty(&base_norm, 0.0);

    assert!(hard_with_overflow > 0.0);
    assert!(soft_with_overflow > hard_with_overflow);
}

#[test]
fn beam_metric_mode_controls_rank_only_pruning_terms() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.28, 32);

    let run = |mode: EcBeamMetricMode| {
        let mut m = CrfbModulator::new_ec(coeffs, 0xD1A6).expect("EC constructs");
        m.set_beam_search(4, 8);
        m.set_beam_metric_mode(mode);
        m.set_beam_metric_diagnostics_enabled(true);
        m.set_beam_terminal_weight(1000.0);
        m.set_beam_alternation_rank_weight(1000.0);
        let mut bits = Vec::new();
        m.process_into_bits(&input, &mut bits);
        m.beam_metric_diagnostics().expect("beam active")
    };

    let path_consistent = run(EcBeamMetricMode::PathConsistent);
    assert!(path_consistent.samples > 0);
    assert_eq!(path_consistent.rank_metric_delta_abs_sum, 0.0);
    assert_eq!(path_consistent.top_child_changed_by_rank, 0);
    assert_eq!(path_consistent.survivor_set_changed_by_rank, 0);

    let hybrid = run(EcBeamMetricMode::HybridRankNudged);
    assert!(hybrid.samples > 0);
    assert!(hybrid.rank_metric_delta_abs_sum > 0.0);
}

#[test]
fn beam_policy_weights_are_tunable_without_breaking_default_anchor() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.32, 8192);
    let tuned = Ec2PolicyWeights {
        quantizer_weight: 1.0,
        pressure_weight: 1.25,
        transition_weight: 0.0,
        dc_weight: 0.02,
        ..Ec2PolicyWeights::default()
    };

    let mut default_m4 = CrfbModulator::new_ec(coeffs, 0xBEA4).expect("EC constructs");
    default_m4.set_beam_search(4, 8);
    let default_bits = run_to_bits(&mut default_m4, &input);

    let mut tuned_m4 = CrfbModulator::new_ec(coeffs, 0xBEA4).expect("EC constructs");
    tuned_m4.set_ec2_policy_weights(tuned);
    tuned_m4.set_beam_search(4, 8);
    let tuned_bits = run_to_bits(&mut tuned_m4, &input);

    assert_ne!(
        tuned_bits, default_bits,
        "non-default EC policy weights did not affect M4/N8 beam decisions"
    );
}

fn run_beam_m4n8_plain_pair(
    coeffs: &'static crate::audio::dsd::dsd_coeffs::ModulatorCoeffs,
    seed: u64,
    input: &[f64],
    chunk_size: Option<usize>,
) -> (CrfbModulator, Vec<u8>, CrfbModulator, Vec<u8>) {
    let mut fast = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    fast.set_dither_scale(0.0);
    fast.set_beam_search(4, 8);

    let mut generic = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    generic.set_dither_scale(0.0);
    generic.set_beam_search(4, 8);
    generic.set_beam_force_generic_path(true);

    let mut fast_bits = Vec::new();
    let mut generic_bits = Vec::new();
    if let Some(chunk_size) = chunk_size {
        for chunk in input.chunks(chunk_size) {
            fast.process_into_bits(chunk, &mut fast_bits);
            generic.process_into_bits(chunk, &mut generic_bits);
        }
        fast.flush_into_bits(&mut fast_bits);
        generic.flush_into_bits(&mut generic_bits);
    } else {
        fast_bits = run_to_bits(&mut fast, input);
        generic_bits = run_to_bits(&mut generic, input);
    }

    (fast, fast_bits, generic, generic_bits)
}

fn a1_obg165_ec2_weights() -> Ec2PolicyWeights {
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

fn configure_a1_obg165_beam(modulator: &mut CrfbModulator, wire_rate: u32) {
    modulator.set_dither_scale(0.0);
    modulator.set_future_scorer(EcFutureScorer::QuantizerOnly);
    modulator.set_ec2_long_filter_policy(Ec2LongFilterPolicy::AmbiguityPressure);
    modulator.set_ec2_policy_weights(a1_obg165_ec2_weights());
    modulator.set_pressure_stage_weights(
        &crate::audio::dsd::dsd_render::DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS,
    );
    modulator.set_beam_search(4, 8);
    modulator.set_beam_terminal_weight(0.3);
    modulator.set_beam_alternation_weight(0.0005);
    modulator.set_dc_bias_decay(dc_bias_decay_for_corner_hz(20.0, wire_rate));
}

fn run_beam_m4n8_a1_obg165_pair(
    seed: u64,
    input: &[f64],
    chunk_size: Option<usize>,
) -> (CrfbModulator, Vec<u8>, CrfbModulator, Vec<u8>) {
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let mut fast = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut fast, 2_822_400);

    let mut generic = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut generic, 2_822_400);
    generic.set_beam_force_generic_path(true);

    let mut fast_bits = Vec::new();
    let mut generic_bits = Vec::new();
    if let Some(chunk_size) = chunk_size {
        for chunk in input.chunks(chunk_size) {
            fast.process_into_bits(chunk, &mut fast_bits);
            generic.process_into_bits(chunk, &mut generic_bits);
        }
        fast.flush_into_bits(&mut fast_bits);
        generic.flush_into_bits(&mut generic_bits);
    } else {
        fast_bits = run_to_bits(&mut fast, input);
        generic_bits = run_to_bits(&mut generic, input);
    }

    (fast, fast_bits, generic, generic_bits)
}

fn assert_beam_m4n8_plain_matches_generic(
    fast: &CrfbModulator,
    fast_bits: &[u8],
    generic: &CrfbModulator,
    generic_bits: &[u8],
    label: &str,
) {
    assert_eq!(fast_bits, generic_bits, "{label} bits");
    assert_eq!(fast.state, generic.state, "{label} state");
    assert_eq!(fast.prev_v, generic.prev_v, "{label} prev_v");
    assert_eq!(fast.dc_bias, generic.dc_bias, "{label} dc_bias");
    assert_eq!(
        fast.state_clamps(),
        generic.state_clamps(),
        "{label} clamps"
    );
    assert_eq!(
        fast.stability_resets(),
        generic.stability_resets(),
        "{label} resets"
    );
}

fn assert_beam_m4n8_a1_matches_scalar_oracle(
    fast: &CrfbModulator,
    fast_bits: &[u8],
    generic: &CrfbModulator,
    generic_bits: &[u8],
    label: &str,
) {
    assert_eq!(fast_bits, generic_bits, "{label} bits");
    for stage in 0..8 {
        assert!(
            (fast.state[stage] - generic.state[stage]).abs() <= 1.0e-8,
            "{label} stage {stage}: fast={} scalar={}",
            fast.state[stage],
            generic.state[stage]
        );
    }
    assert_eq!(fast.prev_v, generic.prev_v, "{label} prev_v");
    assert!((fast.dc_bias - generic.dc_bias).abs() <= 1.0e-12);
    assert_eq!(
        fast.state_clamps(),
        generic.state_clamps(),
        "{label} clamps"
    );
    assert_eq!(
        fast.stability_resets(),
        generic.stability_resets(),
        "{label} resets"
    );
}

#[test]
fn beam_m4n8_ranked_a1_obg165_fast_path_matches_generic() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let input = beam_program_input(coeffs.input_peak * 0.32, 4096);
    let (fast, fast_bits, generic, generic_bits) =
        run_beam_m4n8_a1_obg165_pair(0xA10B_1650, &input, None);
    assert!(
        fast.beam
            .as_ref()
            .is_some_and(|beam| fast.beam_m4n8_ranked_eligible(beam))
    );
    assert!(
        !fast
            .beam
            .as_ref()
            .is_some_and(|beam| fast.beam_m4n8_plain_eligible(beam))
    );
    assert_beam_m4n8_a1_matches_scalar_oracle(
        &fast,
        &fast_bits,
        &generic,
        &generic_bits,
        "a1 obg165 ranked",
    );
}

#[test]
fn beam_m4n8_ranked_a1_obg165_fast_path_matches_generic_for_hot_chunked_inputs() {
    if !CALIBRATED {
        return;
    }
    let hot = vec![0.92; 2048];
    let (fast, fast_bits, generic, generic_bits) =
        run_beam_m4n8_a1_obg165_pair(0xA10B_1651, &hot, Some(31));
    assert_beam_m4n8_a1_matches_scalar_oracle(
        &fast,
        &fast_bits,
        &generic,
        &generic_bits,
        "a1 obg165 hot chunked",
    );
}

#[cfg(target_arch = "aarch64")]
#[test]
fn beam_m4n8_a1_simd_is_chunking_invariant() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let mut input = beam_program_input(coeffs.input_peak * 0.42, 8192);
    input[4097] = f64::NAN;

    let mut one_shot = CrfbModulator::new_ec(coeffs, 0xA10C_0001).expect("EC constructs");
    configure_a1_obg165_beam(&mut one_shot, 2_822_400);
    let one_shot_bits = run_to_bits(&mut one_shot, &input);

    let mut chunked = CrfbModulator::new_ec(coeffs, 0xA10C_0001).expect("EC constructs");
    configure_a1_obg165_beam(&mut chunked, 2_822_400);
    let mut chunked_bits = Vec::new();
    let mut offset = 0usize;
    for size in [1usize, 7, 31, 2, 127, 13].into_iter().cycle() {
        if offset == input.len() {
            break;
        }
        let end = (offset + size).min(input.len());
        chunked.process_into_bits(&input[offset..end], &mut chunked_bits);
        offset = end;
    }
    chunked.flush_into_bits(&mut chunked_bits);

    assert_eq!(chunked_bits, one_shot_bits);
    assert_eq!(chunked.state, one_shot.state);
    assert_eq!(chunked.prev_v, one_shot.prev_v);
    assert_eq!(chunked.dc_bias, one_shot.dc_bias);
    assert_eq!(chunked.state_clamps(), one_shot.state_clamps());
    assert_eq!(chunked.stability_resets(), one_shot.stability_resets());
}

#[cfg(target_arch = "aarch64")]
#[test]
fn beam_m4n8_a1_simd_to_scalar_transition_is_deterministic() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let input = beam_program_input(coeffs.input_peak * 0.38, 8192);
    let split = 4093;
    let run = |chunked: bool| {
        let mut modulator = CrfbModulator::new_ec(coeffs, 0xA10C_0003).expect("EC constructs");
        configure_a1_obg165_beam(&mut modulator, 2_822_400);
        let mut bits = Vec::new();
        if chunked {
            for chunk in input[..split].chunks(37) {
                modulator.process_into_bits(chunk, &mut bits);
            }
        } else {
            modulator.process_into_bits(&input[..split], &mut bits);
        }
        modulator.set_beam_alternation_rank_weight(0.0001);
        assert!(
            modulator
                .beam
                .as_ref()
                .is_some_and(|beam| !modulator.beam_m4n8_a1_simd_eligible(beam))
        );
        if chunked {
            for chunk in input[split..].chunks(53) {
                modulator.process_into_bits(chunk, &mut bits);
            }
        } else {
            modulator.process_into_bits(&input[split..], &mut bits);
        }
        modulator.flush_into_bits(&mut bits);
        (bits, modulator)
    };

    let (whole_bits, whole) = run(false);
    let (chunked_bits, chunked) = run(true);
    assert_eq!(whole_bits.len(), input.len());
    assert_eq!(chunked_bits, whole_bits);
    assert_eq!(chunked.state, whole.state);
    assert_eq!(chunked.prev_v, whole.prev_v);
    assert_eq!(chunked.dc_bias, whole.dc_bias);
    assert!(whole.state[..7].iter().all(|lane| lane.is_finite()));
}

#[cfg(target_arch = "aarch64")]
#[test]
#[allow(clippy::needless_range_loop)]
fn beam_m4n8_a1_simd_predictor_matches_scalar_math() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let mut modulator = CrfbModulator::new_ec(coeffs, 0xA10C_0002).expect("EC constructs");
    configure_a1_obg165_beam(&mut modulator, 2_822_400);
    let mut beam = modulator.beam.take().expect("beam active");
    beam.parents_len = 4;
    for parent in 0..4 {
        for stage in 0..7 {
            beam.parents[beam.parents_bank][parent].state[stage] =
                coeffs.state_limit[stage] * (0.07 * (parent + stage + 1) as f64 - 0.31);
        }
    }
    modulator.ensure_m4n8_normalized_state(&mut beam);
    let bank = beam.parents_bank;
    let u = coeffs.input_peak * 0.21;
    let (y, ps, pt, hot) = modulator.predict_m4n8_a1_frontier(&mut beam, bank, u);
    for parent in 0..4 {
        let mut state = [0.0; 8];
        for stage in 0..7 {
            state[stage] = beam.m4n8_norm_state[bank][stage][parent];
        }
        let base = modulator.predict_base_norm::<true>(&state, u);
        let scalar_y = modulator.loop_output_norm::<true>(&state, u);
        let (scalar_ps, scalar_pt) = modulator.weighted_pressure_dots(&base);
        let (_, _, scalar_hot) = score_pair_dots(&base, &modulator.bv_norm, &modulator.knee_thr_sq);
        assert!((y[parent] - scalar_y).abs() <= 1.0e-13);
        assert!((ps[parent] - scalar_ps).abs() <= 1.0e-13);
        assert!((pt[parent] - scalar_pt).abs() <= 1.0e-13);
        assert_eq!(hot[parent], scalar_hot);
        for stage in 0..7 {
            assert!((beam.m4n8_base_norm[stage][parent] - base[stage]).abs() <= 1.0e-13);
        }
    }
    modulator.beam = Some(beam);
}

#[test]
fn beam_m4n8_ranked_a1_obg165_dsd128_zero_dither_uses_fast_path() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128_OBG165;
    let mut modulator = CrfbModulator::new_ec(coeffs, 0xA10B_1280).expect("EC constructs");
    modulator.set_dither_shape(DitherShape::HighPassTpdf);
    modulator.set_dither_prng(DitherPrng::SplitMix64, 0xA10B_1280);
    configure_a1_obg165_beam(&mut modulator, 5_644_800);

    let beam = modulator.beam.as_ref().expect("beam active");
    assert!(
        modulator.beam_m4n8_ranked_eligible(beam),
        "DSD128 A1 OBG1.65 zero-dither config should hit the ranked fast path"
    );
    assert!(modulator.common_side_dither.is_none());
    assert!(!modulator.effective_dither_active());
}

#[test]
fn beam_m4n8_plain_fast_path_matches_generic_for_program_inputs() {
    if !CALIBRATED {
        return;
    }
    for (label, coeffs) in [
        ("osr64", &crate::audio::dsd::dsd_coeffs::CRFB_OSR64),
        ("osr128", &crate::audio::dsd::dsd_coeffs::CRFB_OSR128),
        ("osr256", &crate::audio::dsd::dsd_coeffs::CRFB_OSR256),
    ] {
        let input = beam_program_input(coeffs.input_peak * 0.32, 4096);
        let (fast, fast_bits, generic, generic_bits) =
            run_beam_m4n8_plain_pair(coeffs, 0xFA57, &input, None);
        assert!(
            fast.beam
                .as_ref()
                .is_some_and(|beam| fast.beam_m4n8_plain_eligible(beam))
        );
        assert_beam_m4n8_plain_matches_generic(&fast, &fast_bits, &generic, &generic_bits, label);
    }
}

#[test]
fn beam_m4n8_plain_fast_path_matches_generic_for_hot_chunked_and_nonfinite_inputs() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let hot = vec![0.95; 2048];
    let (fast, fast_bits, generic, generic_bits) =
        run_beam_m4n8_plain_pair(coeffs, 0xA11CE, &hot, Some(37));
    assert_beam_m4n8_plain_matches_generic(
        &fast,
        &fast_bits,
        &generic,
        &generic_bits,
        "hot chunked",
    );

    let mut nonfinite = beam_program_input(coeffs.input_peak * 0.2, 512);
    nonfinite[73] = f64::NAN;
    nonfinite[211] = f64::INFINITY;
    let (fast, fast_bits, generic, generic_bits) =
        run_beam_m4n8_plain_pair(coeffs, 0xBADC0DE, &nonfinite, Some(29));
    assert_beam_m4n8_plain_matches_generic(
        &fast,
        &fast_bits,
        &generic,
        &generic_bits,
        "nonfinite chunked",
    );
}

#[test]
fn beam_m4n8_plain_to_generic_aux_metric_transition_starts_aux_state_at_zero() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.28, 4096);
    let split = 1379;

    let mut fast = CrfbModulator::new_ec(coeffs, 0xA175_7A7E).expect("EC constructs");
    fast.set_dither_scale(0.0);
    fast.set_beam_search(4, 8);
    let mut fast_bits = Vec::new();
    fast.process_into_bits(&input[..split], &mut fast_bits);
    fast.set_beam_reconstruction_error_weight(0.25);
    fast.process_into_bits(&input[split..], &mut fast_bits);
    fast.flush_into_bits(&mut fast_bits);

    let mut generic = CrfbModulator::new_ec(coeffs, 0xA175_7A7E).expect("EC constructs");
    generic.set_dither_scale(0.0);
    generic.set_beam_search(4, 8);
    generic.set_beam_force_generic_path(true);
    let mut generic_bits = Vec::new();
    generic.process_into_bits(&input[..split], &mut generic_bits);
    generic.set_beam_reconstruction_error_weight(0.25);
    generic.process_into_bits(&input[split..], &mut generic_bits);
    generic.flush_into_bits(&mut generic_bits);

    assert_beam_m4n8_plain_matches_generic(
        &fast,
        &fast_bits,
        &generic,
        &generic_bits,
        "plain-to-generic aux transition",
    );
    assert!(
        fast.beam_reconstruction_diagnostics()
            .is_some_and(|diag| diag.samples > 0)
    );
}

#[test]
fn beam_m4n8_plain_eligibility_rejects_non_plain_configs() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let eligible = |mut m: CrfbModulator| {
        let beam = m.beam.take().expect("beam active");
        let eligible = m.beam_m4n8_plain_eligible(&beam);
        m.beam = Some(beam);
        eligible
    };
    let ranked_eligible = |mut m: CrfbModulator| {
        let beam = m.beam.take().expect("beam active");
        let eligible = m.beam_m4n8_ranked_eligible(&beam);
        m.beam = Some(beam);
        eligible
    };
    let base = || {
        let mut m = CrfbModulator::new_ec(coeffs, 0xE116).expect("EC constructs");
        m.set_dither_scale(0.0);
        m.set_beam_search(4, 8);
        m
    };

    assert!(eligible(base()));

    let mut nonzero_dither = base();
    nonzero_dither.set_dither_scale(1.0e-6);
    assert!(!eligible(nonzero_dither));

    let mut zero_common_side = base();
    zero_common_side.set_common_side_dither(0xC011, 0x51DE, 0.35, 1.0);
    assert!(zero_common_side.common_side_dither.is_none());
    assert!(eligible(zero_common_side));

    let mut nonzero_isi = base();
    nonzero_isi.set_isi_penalty(DEFAULT_ISI_PENALTY);
    assert!(!eligible(nonzero_isi));

    let mut aux_metric = base();
    aux_metric.set_beam_reconstruction_error_weight(0.1);
    assert!(!eligible(aux_metric));

    let mut diagnostics = base();
    diagnostics.set_beam_metric_diagnostics_enabled(true);
    assert!(!eligible(diagnostics));

    let mut clamp_policy = base();
    clamp_policy.set_beam_clamp_policy(EcBeamClampPolicy::PenalizeClamp);
    assert!(!eligible(clamp_policy));

    let mut metric_hygiene = base();
    metric_hygiene.set_beam_auxiliary_metric_scales(0.0, 0.0, 0.0, 0.0);
    assert!(!eligible(metric_hygiene));

    let mut wrong_shape = CrfbModulator::new_ec(coeffs, 0xE116).expect("EC constructs");
    wrong_shape.set_dither_scale(0.0);
    wrong_shape.set_beam_search(4, 16);
    assert!(!eligible(wrong_shape));

    let mut terminal_ranked = base();
    terminal_ranked.set_beam_terminal_weight(0.3);
    terminal_ranked.set_beam_alternation_weight(0.0005);
    assert!(!eligible(terminal_ranked));

    let mut terminal_ranked = base();
    terminal_ranked.set_beam_terminal_weight(0.3);
    terminal_ranked.set_beam_alternation_weight(0.0005);
    assert!(ranked_eligible(terminal_ranked));

    let mut path_consistent = base();
    path_consistent.set_beam_terminal_weight(0.3);
    path_consistent.set_beam_metric_mode(EcBeamMetricMode::PathConsistent);
    assert!(!ranked_eligible(path_consistent));
}

#[test]
fn beam_zero_filtered_error_weights_are_inert() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.32, 8192);

    let mut baseline = CrfbModulator::new_ec(coeffs, 0xFEE0).expect("EC constructs");
    baseline.set_beam_search(4, 8);
    let baseline_bits = run_to_bits(&mut baseline, &input);

    let mut explicit_zero = CrfbModulator::new_ec(coeffs, 0xFEE0).expect("EC constructs");
    explicit_zero.set_beam_search(4, 8);
    explicit_zero.set_beam_filtered_error_weight(0.0);
    explicit_zero.set_beam_filtered_error_rank_weight(0.0);
    let explicit_zero_bits = run_to_bits(&mut explicit_zero, &input);

    assert_eq!(explicit_zero_bits, baseline_bits);
    assert_eq!(explicit_zero.state, baseline.state);
    assert_eq!(explicit_zero.prev_v, baseline.prev_v);
    assert_eq!(explicit_zero.dc_bias, baseline.dc_bias);
    assert_eq!(explicit_zero.rng, baseline.rng);
    assert_eq!(explicit_zero.prev_tpdf, baseline.prev_tpdf);
    assert_eq!(explicit_zero.state_clamps(), baseline.state_clamps());
    assert_eq!(
        explicit_zero.stability_resets(),
        baseline.stability_resets()
    );
    assert_eq!(
        explicit_zero.beam_diagnostics(),
        baseline.beam_diagnostics()
    );
}

#[test]
fn beam_zero_reconstruction_error_weight_is_inert() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.32, 8192);

    let mut baseline = CrfbModulator::new_ec(coeffs, 0xBEC0).expect("EC constructs");
    baseline.set_beam_search(4, 8);
    let baseline_bits = run_to_bits(&mut baseline, &input);

    let mut explicit_zero = CrfbModulator::new_ec(coeffs, 0xBEC0).expect("EC constructs");
    explicit_zero.set_beam_search(4, 8);
    explicit_zero.set_beam_reconstruction_error_weight(0.0);
    let explicit_zero_bits = run_to_bits(&mut explicit_zero, &input);

    assert_eq!(explicit_zero_bits, baseline_bits);
    assert_eq!(explicit_zero.state, baseline.state);
    assert_eq!(explicit_zero.prev_v, baseline.prev_v);
    assert_eq!(explicit_zero.dc_bias, baseline.dc_bias);
    assert_eq!(explicit_zero.rng, baseline.rng);
    assert_eq!(explicit_zero.prev_tpdf, baseline.prev_tpdf);
    assert_eq!(explicit_zero.state_clamps(), baseline.state_clamps());
    assert_eq!(
        explicit_zero.stability_resets(),
        baseline.stability_resets()
    );
    assert_eq!(
        explicit_zero.beam_diagnostics(),
        baseline.beam_diagnostics()
    );
}

#[test]
fn beam_nonzero_reconstruction_error_weight_records_proxy_activity() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.32, 512);

    let mut active = CrfbModulator::new_ec(coeffs, 0xBEC1).expect("EC constructs");
    active.set_beam_search(4, 8);
    active.set_beam_reconstruction_error_weight(0.25);
    let bits = run_to_bits(&mut active, &input);
    let diag = active
        .beam_reconstruction_diagnostics()
        .expect("beam active");

    assert_eq!(bits.len(), input.len());
    assert!(diag.samples > 0);
    assert!(diag.filtered_energy_sum > 0.0);
    assert!(diag.weighted_contribution_sum > 0.0);
}

#[test]
fn beam_zero_pressure_deadzone_is_inert() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.32, 8192);

    let mut baseline = CrfbModulator::new_ec(coeffs, 0xBEA1).expect("EC constructs");
    baseline.set_beam_search(4, 8);
    let baseline_bits = run_to_bits(&mut baseline, &input);

    let mut explicit_zero = CrfbModulator::new_ec(coeffs, 0xBEA1).expect("EC constructs");
    explicit_zero.set_beam_search(4, 8);
    explicit_zero.set_beam_pressure_deadzone(0.0);
    let explicit_zero_bits = run_to_bits(&mut explicit_zero, &input);

    assert_eq!(explicit_zero.beam_pressure_deadzone(), Some(0.0));
    assert_eq!(explicit_zero_bits, baseline_bits);
    assert_eq!(explicit_zero.state, baseline.state);
    assert_eq!(explicit_zero.prev_v, baseline.prev_v);
    assert_eq!(explicit_zero.dc_bias, baseline.dc_bias);
    assert_eq!(explicit_zero.rng, baseline.rng);
    assert_eq!(explicit_zero.prev_tpdf, baseline.prev_tpdf);
    assert_eq!(explicit_zero.state_clamps(), baseline.state_clamps());
    assert_eq!(
        explicit_zero.stability_resets(),
        baseline.stability_resets()
    );
    assert_eq!(
        explicit_zero.beam_diagnostics(),
        baseline.beam_diagnostics()
    );
}

#[test]
fn beam_zero_periodicity_weight_is_inert() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.32, 8192);

    let mut baseline = CrfbModulator::new_ec(coeffs, 0xBEA2).expect("EC constructs");
    baseline.set_beam_search(4, 8);
    let baseline_bits = run_to_bits(&mut baseline, &input);

    let mut explicit_zero = CrfbModulator::new_ec(coeffs, 0xBEA2).expect("EC constructs");
    explicit_zero.set_beam_search(4, 8);
    explicit_zero.set_beam_periodicity_weight(0.0);
    explicit_zero.set_beam_periodicity_lags(&[2, 3, 4]);
    explicit_zero.set_beam_periodicity_window(16);
    let explicit_zero_bits = run_to_bits(&mut explicit_zero, &input);

    assert_eq!(explicit_zero.beam_periodicity_weight(), Some(0.0));
    assert_eq!(explicit_zero.beam_periodicity_lags(), Some(vec![2, 3, 4]));
    assert_eq!(explicit_zero.beam_periodicity_lag_count(), Some(3));
    assert_eq!(explicit_zero.beam_periodicity_window(), Some(16));
    assert_eq!(explicit_zero_bits, baseline_bits);
    assert_eq!(explicit_zero.state, baseline.state);
    assert_eq!(explicit_zero.prev_v, baseline.prev_v);
    assert_eq!(explicit_zero.dc_bias, baseline.dc_bias);
    assert_eq!(explicit_zero.rng, baseline.rng);
    assert_eq!(explicit_zero.prev_tpdf, baseline.prev_tpdf);
    assert_eq!(explicit_zero.state_clamps(), baseline.state_clamps());
    assert_eq!(
        explicit_zero.stability_resets(),
        baseline.stability_resets()
    );
    assert_eq!(
        explicit_zero.beam_diagnostics(),
        baseline.beam_diagnostics()
    );
}

#[test]
fn beam_latency_buffering_and_best_state_diagnostics_are_beam_aware() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.3, 4);
    let mut m = CrfbModulator::new_ec(coeffs, 0xBEEF).expect("EC constructs");
    m.set_beam_search(4, 8);
    assert_eq!(m.beam_latency_samples(), Some(7));
    assert_eq!(m.beam_buffered_samples(), Some(0));

    let mut bits = Vec::new();
    m.process_into_bits(&input, &mut bits);
    assert!(bits.is_empty(), "beam should still be buffering");
    assert_eq!(m.beam_buffered_samples(), Some(input.len()));
    assert_eq!(m.state_pressure(), 0.0);
    assert!(m.beam_best_state_pressure().expect("beam active") > 0.0);
    let best_by_stage = m
        .beam_best_state_pressure_by_stage()
        .expect("beam stage pressure");
    assert!(best_by_stage.iter().any(|stage| *stage > 0.0));
    assert_ne!(
        m.diagnostic_loop_output_for_input(input[0]),
        m.beam_best_loop_output_for_input(input[0])
            .expect("beam loop output")
    );

    m.flush_into_bits(&mut bits);
    assert_eq!(bits.len(), input.len());
    assert_eq!(
        m.beam_diagnostics().expect("beam active").emit_count,
        bits.len() as u64
    );
    assert_eq!(m.beam_buffered_samples(), Some(0));
    assert_eq!(
        m.beam_best_state_pressure().expect("beam active"),
        m.state_pressure()
    );
}

#[test]
fn clear_beam_search_flushing_preserves_delayed_bits() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.3, 6);
    let mut m = CrfbModulator::new_ec(coeffs, 0xC1EA).expect("EC constructs");
    m.set_beam_search(4, 8);
    let mut bits = Vec::new();
    m.process_into_bits(&input, &mut bits);
    assert_eq!(bits.len(), 0);
    assert_eq!(m.beam_buffered_samples(), Some(input.len()));
    m.clear_beam_search_flushing(&mut bits);
    assert_eq!(bits.len(), input.len());
    assert_eq!(m.beam_search(), None);
}

#[test]
fn beam_clamp_policies_report_and_distinguish_hard_limit_handling() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = vec![0.95; 4096];

    let run = |policy: EcBeamClampPolicy| {
        let mut m = CrfbModulator::new_ec(coeffs, 0xC1A4).expect("EC constructs");
        m.set_beam_search(4, 8);
        m.set_beam_clamp_policy(policy);
        let bits = run_to_bits(&mut m, &input);
        let diag = m.beam_diagnostics().expect("beam active");
        (bits, diag, m.state_clamps(), m.stability_resets())
    };

    let (legacy_bits, legacy_diag, legacy_clamps, legacy_resets) =
        run(EcBeamClampPolicy::LegacyClampAndContinue);
    let (penalized_bits, penalized_diag, _penalized_clamps, penalized_resets) =
        run(EcBeamClampPolicy::PenalizeClamp);
    let (reject_bits, reject_diag, _reject_clamps, reject_resets) =
        run(EcBeamClampPolicy::RejectHardLimit);

    assert_eq!(legacy_bits.len(), input.len());
    assert_eq!(penalized_bits.len(), input.len());
    assert_eq!(reject_bits.len(), input.len());
    assert_eq!(legacy_diag.emit_count, legacy_bits.len() as u64);
    assert_eq!(penalized_diag.emit_count, penalized_bits.len() as u64);
    assert_eq!(reject_diag.emit_count, reject_bits.len() as u64);
    assert!(legacy_diag.beam_speculative_clamp_total > 0);
    assert!(penalized_diag.beam_speculative_clamp_total > 0);
    assert!(reject_diag.beam_speculative_clamp_total > 0);
    assert_eq!(legacy_diag.beam_rejected_hard_limit_total, 0);
    assert_eq!(penalized_diag.beam_rejected_hard_limit_total, 0);
    assert!(reject_diag.beam_rejected_hard_limit_total > 0);
    assert_eq!(reject_diag.beam_clamp_total, 0);
    assert_eq!(reject_diag.beam_committed_clamp_total, 0);
    assert_eq!(legacy_clamps, legacy_diag.beam_committed_clamp_total);
    assert_eq!(legacy_resets, 0);
    assert_eq!(penalized_resets, 0);
    assert!(
        reject_resets == 0 || reject_diag.beam_all_children_rejected_total > 0,
        "reject policy reset without all-children rejection diagnostic"
    );
}

#[test]
fn beam_alpha_helpers_use_documented_44k_family_wire_rates() {
    for osr in [64, 128, 256] {
        let filtered = beam_filtered_error_alpha(osr);
        let expected_filtered = beam_one_pole_alpha_for_wire_rate(20_000.0, 44_100.0 * osr as f64);
        assert!((filtered - expected_filtered).abs() < f64::EPSILON);

        let reconstruction = beam_reconstruction_error_alpha(osr);
        let expected_reconstruction =
            beam_one_pole_alpha_for_wire_rate(22_000.0, 44_100.0 * osr as f64);
        assert!((reconstruction - expected_reconstruction).abs() < f64::EPSILON);
    }
}

/// §21.9.1 (NaN arm) — the non-finite-input guard must mirror depth-1
/// exactly: one dither draw, hard reset, emit 1, and bit-identical
/// recovery afterwards, with the delayed bits still owed emitted first.
#[test]
fn beam_m1_nonfinite_input_matches_ec_depth1() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let mut input = beam_program_input(coeffs.input_peak * 0.35, 4096);
    input[1500] = f64::NAN;

    let mut reference = CrfbModulator::new_ec(coeffs, 0xBAD2).expect("EC constructs");
    reference.set_lookahead_depth(1);
    let reference_bits = run_to_bits(&mut reference, &input);

    for n in [1usize, 8] {
        let mut beam = CrfbModulator::new_ec(coeffs, 0xBAD2).expect("EC constructs");
        beam.set_lookahead_depth(1);
        beam.set_beam_search(1, n);
        let beam_bits = run_to_bits(&mut beam, &input);
        assert_eq!(beam_bits, reference_bits, "N={n}");
        assert_eq!(beam.stability_resets(), 1, "N={n}");
        assert_eq!(
            beam.beam_diagnostics().expect("beam active").emit_count,
            beam_bits.len() as u64,
            "N={n}"
        );
        assert_eq!(beam.rng, reference.rng, "N={n} rng position");
    }
}

/// §21.9.2 — chunk invariance across beam widths and horizons, flush
/// included. The 8192-sample stream also exercises the low-`n` bit mask
/// well past 64 samples (§21.11).
#[test]
fn beam_bitstream_is_invariant_under_block_chunking() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.3, 8192);
    for (m, n) in [(4usize, 8usize), (8, 16), (16, 32)] {
        let mut whole = CrfbModulator::new_ec(coeffs, 0xB3A7).expect("EC constructs");
        whole.set_beam_search(m, n);
        let whole_bits = run_to_bits(&mut whole, &input);
        assert_eq!(whole_bits.len(), input.len(), "M={m}/N={n} length");
        assert_eq!(whole.stability_resets(), 0, "M={m}/N={n} resets");

        for chunk_size in [7usize, 64, 1025] {
            let mut chunked = CrfbModulator::new_ec(coeffs, 0xB3A7).expect("EC constructs");
            chunked.set_beam_search(m, n);
            let mut chunked_bits = Vec::new();
            for chunk in input.chunks(chunk_size) {
                chunked.process_into_bits(chunk, &mut chunked_bits);
            }
            chunked.flush_into_bits(&mut chunked_bits);
            assert_eq!(
                whole_bits, chunked_bits,
                "M={m}/N={n} bitstream changed with chunk size {chunk_size}",
            );
        }
    }
}

/// §21.9.3 — flush tail: splitting the stream at every position in the
/// last 2N samples produces the identical total bitstream.
#[test]
fn beam_flush_tail_split_invariance() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let (m, n) = (8usize, 16usize);
    let input = beam_program_input(coeffs.input_peak * 0.3, 1024);
    let mut whole = CrfbModulator::new_ec(coeffs, 0xF1A5).expect("EC constructs");
    whole.set_beam_search(m, n);
    let whole_bits = run_to_bits(&mut whole, &input);

    for split in (input.len() - 2 * n)..=input.len() {
        let mut split_mod = CrfbModulator::new_ec(coeffs, 0xF1A5).expect("EC constructs");
        split_mod.set_beam_search(m, n);
        let mut bits = Vec::new();
        split_mod.process_into_bits(&input[..split], &mut bits);
        split_mod.process_into_bits(&input[split..], &mut bits);
        split_mod.flush_into_bits(&mut bits);
        assert_eq!(bits, whole_bits, "split at {split}");
    }
}

/// §21.9.4 — determinism: identically-seeded runs are byte-identical, and
/// the selection key resolves an exact metric tie toward +1 at the
/// earliest divergence (descending packed bits — §21.4).
#[test]
fn beam_is_deterministic_and_breaks_ties_toward_plus() {
    // Comparator unit cases: equal metrics, larger packed bits (the path
    // that chose +1 at the oldest differing position) ranks first.
    assert!(beam_rank_at_or_before(1.0, 0b10, 1.0, 0b01));
    assert!(!beam_rank_at_or_before(1.0, 0b01, 1.0, 0b10));
    // Metric dominates bits.
    assert!(beam_rank_at_or_before(0.5, 0, 1.0, u64::MAX));
    assert!(!beam_rank_at_or_before(1.0, u64::MAX, 0.5, 0));
    // Full key tie: incumbent stays first (insertion-sort stability).
    assert!(beam_rank_at_or_before(1.0, 5, 1.0, 5));
    // Newest-bit tie at the root: +1 child ((bits<<1)|1) precedes the -1
    // child — the select_ec_candidate convention the M=1 anchor needs.
    assert!(beam_rank_at_or_before(2.0, 0b1, 2.0, 0b0));

    // The fixed M4 selector must produce the same ordered prefix as the full
    // stable sort for every supported frontier size, including exact ties.
    let metrics = [3.0, 1.0, 2.0, 1.0, 0.5, 4.0, 0.5, 2.0];
    let bits = [0, 1, 2, 3, 4, 5, 6, 7];
    for children in 1..=8 {
        let mut full = [0u8; 2 * MAX_BEAM_WIDTH];
        sort_beam_children(children, &metrics, &bits, &mut full);
        let mut top4 = [0u8; 4];
        let kept = select_top4_beam_children(children, &metrics, &bits, &mut top4);
        assert_eq!(&top4[..kept], &full[..children.min(4)]);
    }

    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.3, 4096);
    let run = || {
        let mut m = CrfbModulator::new_ec(coeffs, 0xDE7E_2313).expect("EC constructs");
        m.set_beam_search(8, 16);
        (run_to_bits(&mut m, &input), m.beam_diagnostics())
    };
    let (bits_a, diag_a) = run();
    let (bits_b, diag_b) = run();
    assert_eq!(bits_a, bits_b);
    assert_eq!(diag_a, diag_b);
}

/// §21.9.5 — RNG discipline: exactly one dither draw per input sample
/// consumed, unconditional — independent of chunking, pruning, emission,
/// warm-up, flush (draws nothing), and the non-finite reset path.
#[test]
fn beam_rng_draws_match_samples_consumed() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let seed = 0xD1CE_D1CE;
    let replay_rng = |samples: usize| {
        let mut rng = DitherRng::new(seed, DitherPrng::XorShift64);
        let mut prev_tpdf = 0.0;
        for _ in 0..samples {
            let _ = next_dither_from(
                &mut rng,
                DitherShape::HighPassTpdf,
                &mut prev_tpdf,
                1.0,
                0.0,
                high_pass_tpdf_norm(1.0, 0.0),
            );
        }
        (rng, prev_tpdf)
    };

    // Clean chunked run: RNG position and the high-pass TPDF chain both
    // match an independent one-draw-per-sample replay.
    let input = beam_program_input(coeffs.input_peak * 0.3, 4096);
    let mut m = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    m.set_beam_search(8, 16);
    let mut bits = Vec::new();
    for chunk in input.chunks(7) {
        m.process_into_bits(chunk, &mut bits);
    }
    m.flush_into_bits(&mut bits);
    assert_eq!(bits.len(), input.len());
    let (rng, prev_tpdf) = replay_rng(input.len());
    assert_eq!(m.rng, rng, "draws != samples on the clean run");
    assert_eq!(m.prev_tpdf, prev_tpdf);

    // Reset path still draws exactly once for the NaN sample (the TPDF
    // history is zeroed by the hard reset, so only the stream position is
    // comparable here).
    let mut with_nan = input.clone();
    with_nan[999] = f64::NAN;
    let mut m = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    m.set_beam_search(8, 16);
    let mut bits = Vec::new();
    for chunk in with_nan.chunks(7) {
        m.process_into_bits(chunk, &mut bits);
    }
    m.flush_into_bits(&mut bits);
    assert_eq!(bits.len(), with_nan.len());
    assert_eq!(m.stability_resets(), 1);
    let (rng, _) = replay_rng(with_nan.len());
    assert_eq!(m.rng, rng, "draws != samples across the reset path");
}

/// §21.9.6 — stability: zero resets on program material at every beam
/// width; finite overload clamps without resetting; a NaN sample resets
/// exactly once, keeps the output-length contract, recovers, and stays
/// chunk-invariant afterwards.
#[test]
fn beam_program_overload_and_nan_stability() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let program = beam_program_input(coeffs.input_peak * 0.3, 12_000);
    let overload = vec![0.95; 12_000];
    for (m, n) in [(4usize, 8usize), (8, 16), (16, 32)] {
        let mut modulator = CrfbModulator::new_ec(coeffs, 0x57AB).expect("EC constructs");
        modulator.set_beam_search(m, n);
        let bits = run_to_bits(&mut modulator, &program);
        assert_eq!(bits.len(), program.len(), "M={m}/N={n}");
        assert_eq!(modulator.stability_resets(), 0, "M={m}/N={n} program reset");
        assert_eq!(modulator.state_clamps(), 0, "M={m}/N={n} program clamped");

        let mut modulator = CrfbModulator::new_ec(coeffs, 0x57AB).expect("EC constructs");
        modulator.set_beam_search(m, n);
        let bits = run_to_bits(&mut modulator, &overload);
        assert_eq!(bits.len(), overload.len(), "M={m}/N={n}");
        assert_eq!(
            modulator.stability_resets(),
            0,
            "M={m}/N={n} overload must clamp, not reset",
        );
    }

    // NaN mid-stream: exactly one reset, length preserved, and the whole
    // NaN-containing stream stays chunk-invariant.
    let mut with_nan = program[..4096].to_vec();
    with_nan[2000] = f64::NAN;
    let mut whole = CrfbModulator::new_ec(coeffs, 0x57AB).expect("EC constructs");
    whole.set_beam_search(8, 16);
    let whole_bits = run_to_bits(&mut whole, &with_nan);
    assert_eq!(whole_bits.len(), with_nan.len());
    assert_eq!(whole.stability_resets(), 1);
    let mut chunked = CrfbModulator::new_ec(coeffs, 0x57AB).expect("EC constructs");
    chunked.set_beam_search(8, 16);
    let mut chunked_bits = Vec::new();
    for chunk in with_nan.chunks(7) {
        chunked.process_into_bits(chunk, &mut chunked_bits);
    }
    chunked.flush_into_bits(&mut chunked_bits);
    assert_eq!(whole_bits, chunked_bits);
}

/// The §20 Q1/Q2 degeneracy canaries: on program material a real beam must
/// actually switch best paths and flip delayed decisions — all-zero
/// counters would mean the machinery froze (an implementation bug, or an
/// expensive greedy).
#[test]
fn beam_diagnostics_show_real_delayed_commitment() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR128;
    let input = beam_program_input(coeffs.input_peak * 0.3, 16_384);
    let mut m = CrfbModulator::new_ec(coeffs, 0xC0DE).expect("EC constructs");
    m.set_beam_search(8, 16);
    let _ = run_to_bits(&mut m, &input);
    let diag = m.beam_diagnostics().expect("beam active");
    assert!(diag.emit_count > 0);
    assert!(
        diag.path_switches > 0,
        "best survivor never changed: beam is frozen"
    );
    assert!(
        diag.delayed_flips > 0,
        "delayed commitment never changed a decision (degenerate greedy): {diag:?}"
    );
}

#[cfg(feature = "ecbeam2_observer")]
fn production_beam_digest(modulator: &CrfbModulator) -> u64 {
    fn mix(digest: &mut u64, value: u64) {
        *digest ^= value;
        *digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }

    let beam = modulator.beam.as_ref().expect("beam active");
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    for value in [
        beam.m as u64,
        beam.n as u64,
        beam.parents_bank as u64,
        beam.parents_len as u64,
        beam.buffered as u64,
        beam.sample_index,
        beam.ring_index as u64,
        beam.diagnostics.emit_count,
        beam.diagnostics.path_switches,
        beam.diagnostics.delayed_flips,
        beam.diagnostics.pruned_total,
        beam.diagnostics.beam_clamp_total,
        beam.diagnostics.beam_committed_clamp_total,
        beam.diagnostics.min_survivors,
    ] {
        mix(&mut digest, value);
    }
    for parent in &beam.parents[beam.parents_bank][..beam.parents_len] {
        for value in parent.state {
            mix(&mut digest, value.to_bits());
        }
        mix(&mut digest, parent.metric.to_bits());
        mix(&mut digest, parent.prev_v.to_bits());
        mix(&mut digest, parent.dc_bias.to_bits());
        mix(&mut digest, parent.bits);
        mix(&mut digest, parent.clamp_bits);
    }
    digest
}

#[cfg(feature = "ecbeam2_observer")]
fn assert_observer_production_parity(
    disabled: &CrfbModulator,
    disabled_bits: &[u8],
    enabled: &CrfbModulator,
    enabled_bits: &[u8],
) {
    assert_eq!(enabled_bits, disabled_bits);
    assert_eq!(enabled.state, disabled.state);
    assert_eq!(enabled.prev_v, disabled.prev_v);
    assert_eq!(enabled.dc_bias, disabled.dc_bias);
    assert_eq!(enabled.state_clamps(), disabled.state_clamps());
    assert_eq!(enabled.stability_resets(), disabled.stability_resets());
    assert_eq!(enabled.beam_diagnostics(), disabled.beam_diagnostics());
    assert_eq!(
        production_beam_digest(enabled),
        production_beam_digest(disabled)
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[derive(Debug, Default)]
struct ObserverReplayDiagnostics {
    maximum_ultrasonic_ema: f64,
    maximum_signed_error_ema: f64,
    reconstruction_output_energy: f64,
    reconstruction_tail_adjusted_energy: f64,
    remaining_reconstruction_tail: f64,
    maximum_reconstruction_tail: f64,
    maximum_abs_reconstruction_output: f64,
    ultrasonic_energy: f64,
    maximum_ultrasonic_power: f64,
}

#[cfg(feature = "ecbeam2_observer")]
fn replay_observer_committed_stream(
    bits: &[u8],
    input: &[f64],
    wire_rate: u32,
) -> ObserverReplayDiagnostics {
    assert_eq!(bits.len(), input.len());
    let profiles = profiles_for_wire_rate(wire_rate).expect("supported observer wire rate");
    let mut reconstruction_state = [0.0; 6];
    let mut ultrasonic_state = [0.0; 6];
    let beta = (-1.0 / (wire_rate as f64 * 0.010)).exp();
    let one_minus_beta = 1.0 - beta;
    let mut ultrasonic_ema = 0.0;
    let mut signed_error_ema = 0.0;
    let mut replay = ObserverReplayDiagnostics::default();

    for (&bit, &raw_input) in bits.iter().zip(input) {
        let input = if raw_input.is_finite() {
            raw_input
        } else {
            0.0
        };
        let output = if bit == 1 { 1.0 } else { -1.0 };
        let error = output - input;
        let reconstruction_output = profiles.reconstruction.output(&reconstruction_state, error);
        replay.reconstruction_output_energy += reconstruction_output * reconstruction_output;
        replay.maximum_abs_reconstruction_output = replay
            .maximum_abs_reconstruction_output
            .max(reconstruction_output.abs());
        replay.reconstruction_tail_adjusted_energy += profiles
            .reconstruction
            .tail_adjusted_energy_increment(&reconstruction_state, error);
        profiles
            .reconstruction
            .advance(&mut reconstruction_state, error);
        replay.maximum_reconstruction_tail = replay.maximum_reconstruction_tail.max(
            profiles
                .reconstruction
                .remaining_zero_input_energy(&reconstruction_state),
        );
        let ultrasonic_output = profiles.ultrasonic.advance(&mut ultrasonic_state, error);
        let ultrasonic_power = ultrasonic_output * ultrasonic_output;
        replay.ultrasonic_energy += ultrasonic_power;
        replay.maximum_ultrasonic_power = replay.maximum_ultrasonic_power.max(ultrasonic_power);
        ultrasonic_ema = beta.mul_add(ultrasonic_ema, one_minus_beta * ultrasonic_power);
        signed_error_ema = beta.mul_add(signed_error_ema, one_minus_beta * error);
        replay.maximum_ultrasonic_ema = replay.maximum_ultrasonic_ema.max(ultrasonic_ema);
        replay.maximum_signed_error_ema =
            replay.maximum_signed_error_ema.max(signed_error_ema.abs());
    }
    replay.remaining_reconstruction_tail = profiles
        .reconstruction
        .remaining_zero_input_energy(&reconstruction_state);
    replay
}

#[cfg(feature = "ecbeam2_observer")]
fn assert_observer_value_close(actual: f64, expected: f64, label: &str) {
    let error = (actual - expected).abs();
    let tolerance = 2.0e-12 * (1.0 + expected.abs());
    assert!(
        error <= tolerance,
        "{label} mismatch: actual={actual:.17e}, expected={expected:.17e}, error={error:.3e}"
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[test]
fn production_ecbeam_observer_rejects_noncanonical_a1_configuration() {
    if !CALIBRATED {
        return;
    }
    let canonical_coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let config = EcBeam2ObserverConfig::default();
    let expect_not_a1 = |modulator: &mut CrfbModulator, mutation: &str| {
        assert_eq!(
            modulator.enable_ecbeam2_observer(config),
            Err(EcBeam2ObserverError::NotProductionA1),
            "observer accepted noncanonical A1 mutation: {mutation}"
        );
    };

    let mut terminal = CrfbModulator::new_ec(canonical_coeffs, 1).expect("EC constructs");
    configure_a1_obg165_beam(&mut terminal, config.wire_rate);
    terminal.set_beam_terminal_weight(0.31);
    expect_not_a1(&mut terminal, "terminal weight");

    let mut dither = CrfbModulator::new_ec(canonical_coeffs, 2).expect("EC constructs");
    configure_a1_obg165_beam(&mut dither, config.wire_rate);
    dither.set_dither_scale(1.0e-5);
    expect_not_a1(&mut dither, "dither");

    let mut dimensions = CrfbModulator::new_ec(canonical_coeffs, 3).expect("EC constructs");
    configure_a1_obg165_beam(&mut dimensions, config.wire_rate);
    dimensions.set_beam_search(8, 16);
    expect_not_a1(&mut dimensions, "beam dimensions");

    let mut diagnostics = CrfbModulator::new_ec(canonical_coeffs, 4).expect("EC constructs");
    configure_a1_obg165_beam(&mut diagnostics, config.wire_rate);
    diagnostics.set_beam_metric_diagnostics_enabled(true);
    expect_not_a1(&mut diagnostics, "metric diagnostics");

    let mut coefficients =
        CrfbModulator::new_ec(&crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG150, 5)
            .expect("EC constructs");
    configure_a1_obg165_beam(&mut coefficients, config.wire_rate);
    expect_not_a1(&mut coefficients, "CRFB coefficient table");

    let mut wire_family = CrfbModulator::new_ec(canonical_coeffs, 6).expect("EC constructs");
    configure_a1_obg165_beam(&mut wire_family, 3_072_000);
    expect_not_a1(&mut wire_family, "wire-family DC decay");

    let mut already_flushed = CrfbModulator::new_ec(canonical_coeffs, 7).expect("EC constructs");
    configure_a1_obg165_beam(&mut already_flushed, config.wire_rate);
    let mut emitted = Vec::new();
    already_flushed.process_into_bits(&[0.0; 16], &mut emitted);
    already_flushed.flush_into_bits(&mut emitted);
    assert_eq!(emitted.len(), 16);
    assert_eq!(
        already_flushed.enable_ecbeam2_observer(config),
        Err(EcBeam2ObserverError::BeamAlreadyAdvanced),
        "observer attached after a prior render and normal flush"
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[test]
fn production_ecbeam_observer_is_read_only_and_bounded_on_fast_kernel() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let input = beam_program_input(coeffs.input_peak * 0.31, 768);
    let seed = 0xECB2_0B5E;

    let mut disabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut disabled, 2_822_400);
    let mut enabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut enabled, 2_822_400);

    let disabled_eligibility = {
        let beam = disabled.beam.as_ref().expect("beam active");
        (
            disabled.beam_m4n8_plain_eligible(beam),
            disabled.beam_m4n8_ranked_eligible(beam),
            disabled.beam_m4n8_a1_simd_eligible(beam),
        )
    };
    let enabled_before = {
        let beam = enabled.beam.as_ref().expect("beam active");
        (
            enabled.beam_m4n8_plain_eligible(beam),
            enabled.beam_m4n8_ranked_eligible(beam),
            enabled.beam_m4n8_a1_simd_eligible(beam),
        )
    };
    assert_eq!(enabled_before, disabled_eligibility);
    assert!(
        disabled_eligibility.0 || disabled_eligibility.1 || disabled_eligibility.2,
        "A1 fixture did not exercise an optimized production kernel"
    );
    enabled
        .enable_ecbeam2_observer(EcBeam2ObserverConfig {
            wire_rate: 2_822_400,
            capture_events: true,
            event_capacity: 64,
            diagnostic_window: None,
        })
        .expect("observer enables on fresh production beam");
    let enabled_after = {
        let beam = enabled.beam.as_ref().expect("beam active");
        (
            enabled.beam_m4n8_plain_eligible(beam),
            enabled.beam_m4n8_ranked_eligible(beam),
            enabled.beam_m4n8_a1_simd_eligible(beam),
        )
    };
    assert_eq!(enabled_after, disabled_eligibility);

    let mut disabled_bits = Vec::new();
    let mut enabled_bits = Vec::new();
    for chunk in input.chunks(29) {
        disabled.process_into_bits(chunk, &mut disabled_bits);
        enabled.process_into_bits(chunk, &mut enabled_bits);
    }
    let segment = enabled
        .ecbeam2_observer_snapshot()
        .expect("observer snapshot");
    assert_eq!(segment.frontier_events, input.len() as u64);
    assert_eq!(segment.delayed_commits, (input.len() - 7) as u64);
    assert!(segment.best_child_disagreements <= segment.frontier_events);
    assert!(segment.top_m_disagreements <= segment.frontier_events);
    assert!(
        segment.production_maximum_best_fourth_margin
            >= segment.production_minimum_best_fourth_margin
    );
    assert!(
        segment.ecbeam2_maximum_best_fourth_margin >= segment.ecbeam2_minimum_best_fourth_margin
    );
    let identity_error = (segment.committed_reconstruction_tail_adjusted_energy
        - segment.committed_reconstruction_output_energy
        - segment.remaining_reconstruction_tail)
        .abs();
    assert!(
        identity_error <= 1.0e-9 * (1.0 + segment.committed_reconstruction_output_energy.abs()),
        "committed replay violated tail identity: {identity_error}"
    );

    disabled.flush_into_bits(&mut disabled_bits);
    enabled.flush_into_bits(&mut enabled_bits);
    assert_observer_production_parity(&disabled, &disabled_bits, &enabled, &enabled_bits);

    let final_snapshot = enabled
        .ecbeam2_observer_snapshot()
        .expect("observer remains attached");
    assert_eq!(final_snapshot.flush_commits, 7);
    assert_eq!(final_snapshot.resets, 0);
    let final_identity_error = (final_snapshot.committed_reconstruction_tail_adjusted_energy
        - final_snapshot.committed_reconstruction_output_energy
        - final_snapshot.remaining_reconstruction_tail)
        .abs();
    assert!(
        final_identity_error
            <= 1.0e-9 * (1.0 + final_snapshot.committed_reconstruction_output_energy.abs()),
        "flush replay violated tail identity: {final_identity_error}"
    );

    // A normal flush materializes the winner but is not a signal
    // discontinuity. Continuing afterward must preserve observer replay state
    // and production parity across the new frontier.
    let continuation = beam_program_input(coeffs.input_peak * 0.19, 96);
    disabled.process_into_bits(&continuation, &mut disabled_bits);
    enabled.process_into_bits(&continuation, &mut enabled_bits);
    disabled.flush_into_bits(&mut disabled_bits);
    enabled.flush_into_bits(&mut enabled_bits);
    assert_observer_production_parity(&disabled, &disabled_bits, &enabled, &enabled_bits);
    let continued_snapshot = enabled
        .ecbeam2_observer_snapshot()
        .expect("observer remains attached after continued rendering");
    assert_eq!(continued_snapshot.resets, 0);
    let continued_identity_error = (continued_snapshot
        .committed_reconstruction_tail_adjusted_energy
        - continued_snapshot.committed_reconstruction_output_energy
        - continued_snapshot.remaining_reconstruction_tail)
        .abs();
    assert!(
        continued_identity_error
            <= 1.0e-9
                * (1.0
                    + continued_snapshot
                        .committed_reconstruction_output_energy
                        .abs()),
        "continued replay violated tail identity: {continued_identity_error}"
    );
    assert!(final_snapshot.dropped_events > 0);
    let events = enabled.drain_ecbeam2_observer_events();
    assert!(
        !events.is_empty(),
        "explicit event capture produced no events"
    );
    assert!(events.len() <= 64, "event buffering is not bounded");
    let frontier = events.iter().find_map(|event| match event {
        EcBeam2ObserverEvent::Frontier(frontier) => Some(frontier),
        _ => None,
    });
    let frontier = frontier.expect("bounded tail retains frontier events");
    assert!(frontier.child_count <= ECBEAM2_OBSERVER_MAX_CHILDREN);
    assert_eq!(frontier.selected_count, 4);
    assert!(frontier.post_prune_count >= 1);
    assert!(
        frontier.children[..frontier.child_count]
            .iter()
            .all(|child| child.ultrasonic_power >= 0.0)
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[test]
fn production_ecbeam_observer_is_read_only_on_generic_kernel() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let input = beam_program_input(coeffs.input_peak * 0.27, 512);
    let seed = 0xECB2_6E4E;
    let mut disabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut disabled, 3_072_000);
    disabled.set_beam_force_generic_path(true);
    let mut enabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut enabled, 3_072_000);
    enabled.set_beam_force_generic_path(true);
    enabled
        .enable_ecbeam2_observer(EcBeam2ObserverConfig {
            wire_rate: 3_072_000,
            ..EcBeam2ObserverConfig::default()
        })
        .expect("observer enables");
    assert!(
        !enabled
            .beam
            .as_ref()
            .is_some_and(|beam| enabled.beam_m4n8_ranked_eligible(beam))
    );

    let mut disabled_bits = Vec::new();
    let mut enabled_bits = Vec::new();
    disabled.process_into_bits(&input, &mut disabled_bits);
    enabled.process_into_bits(&input, &mut enabled_bits);
    disabled.flush_into_bits(&mut disabled_bits);
    enabled.flush_into_bits(&mut enabled_bits);
    assert_observer_production_parity(&disabled, &disabled_bits, &enabled, &enabled_bits);
    let snapshot = enabled
        .ecbeam2_observer_snapshot()
        .expect("observer snapshot");
    assert_eq!(snapshot.frontier_events, input.len() as u64);
    assert_eq!(
        snapshot.delayed_commits + snapshot.flush_commits,
        input.len() as u64
    );
    assert_eq!(snapshot.dropped_events, 0);
    assert!(
        enabled.drain_ecbeam2_observer_events().is_empty(),
        "snapshot-only default unexpectedly retained public frontier events"
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[test]
fn production_ecbeam_observer_explicit_reset_starts_a_coherent_measurement_epoch() {
    if !CALIBRATED {
        return;
    }
    const WIRE_RATE: u32 = 2_822_400;
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let input = beam_program_input(coeffs.input_peak * 0.17, 96);
    let seed = 0xECB2_E0C4;
    let mut disabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut disabled, WIRE_RATE);
    let mut enabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut enabled, WIRE_RATE);
    enabled
        .enable_ecbeam2_observer(EcBeam2ObserverConfig {
            wire_rate: WIRE_RATE,
            ..EcBeam2ObserverConfig::default()
        })
        .expect("observer enables");

    let mut disabled_bits = Vec::new();
    let mut enabled_bits = Vec::new();
    disabled.process_into_bits(&input, &mut disabled_bits);
    enabled.process_into_bits(&input, &mut enabled_bits);
    disabled.flush_into_bits(&mut disabled_bits);
    enabled.flush_into_bits(&mut enabled_bits);
    assert_observer_production_parity(&disabled, &disabled_bits, &enabled, &enabled_bits);

    disabled.reset();
    enabled.reset();
    let reset = enabled
        .ecbeam2_observer_snapshot()
        .expect("observer remains attached after explicit reset");
    assert_eq!(reset.epoch, 1);
    assert_eq!(reset.resets, 1);
    assert_eq!(reset.next_sequence, 0);
    assert_eq!(reset.frontier_events, 0);
    assert_eq!(reset.delayed_commits, 0);
    assert_eq!(reset.flush_commits, 0);
    assert_eq!(reset.recovery_commits, 0);
    assert_eq!(reset.committed_positive_bits, 0);
    assert_eq!(reset.best_child_disagreements, 0);
    assert_eq!(reset.top_m_disagreements, 0);
    assert_eq!(reset.committed_reconstruction_output_energy, 0.0);
    assert_eq!(reset.committed_reconstruction_tail_adjusted_energy, 0.0);
    assert_eq!(reset.remaining_reconstruction_tail, 0.0);
    assert_eq!(reset.committed_ultrasonic_energy, 0.0);

    disabled_bits.clear();
    enabled_bits.clear();
    disabled.process_into_bits(&input, &mut disabled_bits);
    enabled.process_into_bits(&input, &mut enabled_bits);
    disabled.flush_into_bits(&mut disabled_bits);
    enabled.flush_into_bits(&mut enabled_bits);
    assert_observer_production_parity(&disabled, &disabled_bits, &enabled, &enabled_bits);
    let rerun = enabled.ecbeam2_observer_snapshot().expect("observer rerun");
    assert_eq!(rerun.epoch, 1);
    assert_eq!(rerun.frontier_events, input.len() as u64);
    assert_eq!(
        rerun.delayed_commits + rerun.flush_commits,
        input.len() as u64
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[test]
fn production_ecbeam_observer_event_capture_includes_the_delayed_window_fringe() {
    if !CALIBRATED {
        return;
    }
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let mut modulator = CrfbModulator::new_ec(coeffs, 0xECB2_E7E7).expect("EC constructs");
    configure_a1_obg165_beam(&mut modulator, 2_822_400);
    modulator
        .enable_ecbeam2_observer(EcBeam2ObserverConfig {
            wire_rate: 2_822_400,
            capture_events: true,
            event_capacity: 128,
            diagnostic_window: Some(super::ec_beam2::EcBeam2DiagnosticWindow {
                start_sequence: 10,
                end_sequence: 20,
            }),
        })
        .expect("observer enables");
    let input = beam_program_input(coeffs.input_peak * 0.13, 32);
    let mut bits = Vec::new();
    modulator.process_into_bits(&input, &mut bits);
    modulator.flush_into_bits(&mut bits);

    let mut captured_commits = std::collections::BTreeSet::new();
    for event in modulator.drain_ecbeam2_observer_events() {
        let commit = match event {
            EcBeam2ObserverEvent::Frontier(frontier) => frontier.commit,
            EcBeam2ObserverEvent::Commit(commit) => Some(commit),
            EcBeam2ObserverEvent::Reset(_) => None,
        };
        if let Some(commit) = commit
            && (10..20).contains(&commit.input_sequence)
        {
            captured_commits.insert(commit.input_sequence);
        }
    }
    assert_eq!(
        captured_commits,
        (10u64..20).collect::<std::collections::BTreeSet<_>>()
    );
}

#[cfg(feature = "ecbeam2_observer")]
#[test]
fn production_ecbeam_observer_recovery_preserves_physical_replay_state() {
    if !CALIBRATED {
        return;
    }
    const WIRE_RATE: u32 = 2_822_400;
    let coeffs = &crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
    let mut input = beam_program_input(coeffs.input_peak * 0.23, 384);
    input[97] = f64::NAN;
    input[231] = f64::INFINITY;
    let seed = 0xECB2_5AFE;

    let mut disabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut disabled, WIRE_RATE);
    let mut enabled = CrfbModulator::new_ec(coeffs, seed).expect("EC constructs");
    configure_a1_obg165_beam(&mut enabled, WIRE_RATE);
    enabled
        .enable_ecbeam2_observer(EcBeam2ObserverConfig {
            wire_rate: WIRE_RATE,
            ..EcBeam2ObserverConfig::default()
        })
        .expect("observer enables on canonical A1");

    let mut disabled_bits = Vec::new();
    let mut enabled_bits = Vec::new();
    for chunk in input.chunks(17) {
        disabled.process_into_bits(chunk, &mut disabled_bits);
        enabled.process_into_bits(chunk, &mut enabled_bits);
    }
    disabled.flush_into_bits(&mut disabled_bits);
    enabled.flush_into_bits(&mut enabled_bits);
    assert_observer_production_parity(&disabled, &disabled_bits, &enabled, &enabled_bits);
    assert_eq!(enabled_bits.len(), input.len());
    assert_eq!(enabled.stability_resets(), 2);

    let snapshot = enabled
        .ecbeam2_observer_snapshot()
        .expect("observer remains attached across recovery");
    assert_eq!(snapshot.recovery_commits, 2);
    assert_eq!(snapshot.invalid_input_substitutions, 2);
    assert_eq!(snapshot.resets, 2);
    assert_eq!(snapshot.frontier_events, (input.len() - 2) as u64);
    assert_eq!(
        snapshot.delayed_commits + snapshot.flush_commits + snapshot.recovery_commits,
        input.len() as u64
    );
    assert_eq!(
        snapshot.committed_positive_bits,
        enabled_bits
            .iter()
            .map(|&bit| u64::from(bit == 1))
            .sum::<u64>()
    );
    assert!(enabled.drain_ecbeam2_observer_events().is_empty());

    let replay = replay_observer_committed_stream(&enabled_bits, &input, WIRE_RATE);
    assert_observer_value_close(
        snapshot.maximum_committed_ultrasonic_ema,
        replay.maximum_ultrasonic_ema,
        "maximum committed ultrasonic EMA",
    );
    assert_observer_value_close(
        snapshot.maximum_committed_signed_error_ema,
        replay.maximum_signed_error_ema,
        "maximum committed signed-error EMA",
    );
    assert_observer_value_close(
        snapshot.committed_reconstruction_output_energy,
        replay.reconstruction_output_energy,
        "committed reconstruction output energy",
    );
    assert_observer_value_close(
        snapshot.committed_reconstruction_tail_adjusted_energy,
        replay.reconstruction_tail_adjusted_energy,
        "committed tail-adjusted energy",
    );
    assert_observer_value_close(
        snapshot.remaining_reconstruction_tail,
        replay.remaining_reconstruction_tail,
        "remaining committed reconstruction tail",
    );
    assert_observer_value_close(
        snapshot.maximum_reconstruction_tail,
        replay.maximum_reconstruction_tail,
        "maximum committed reconstruction tail",
    );
    assert_observer_value_close(
        snapshot.maximum_abs_reconstruction_output,
        replay.maximum_abs_reconstruction_output,
        "maximum committed causal reconstruction output",
    );
    assert_observer_value_close(
        snapshot.committed_ultrasonic_energy,
        replay.ultrasonic_energy,
        "committed ultrasonic energy",
    );
    assert_observer_value_close(
        snapshot.maximum_ultrasonic_power,
        replay.maximum_ultrasonic_power,
        "maximum committed ultrasonic power",
    );
    assert_eq!(snapshot.desynchronizations, 0);
    assert_observer_value_close(
        snapshot.committed_reconstruction_tail_adjusted_energy,
        snapshot.committed_reconstruction_output_energy + snapshot.remaining_reconstruction_tail,
        "recovery-spanning Lyapunov telescope",
    );
}
