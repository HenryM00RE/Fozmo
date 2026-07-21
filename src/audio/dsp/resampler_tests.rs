use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

fn compensated_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0_f64;
    let mut compensation = 0.0_f64;
    for value in values {
        let adjusted = value - compensation;
        let next = sum + adjusted;
        compensation = (next - sum) - adjusted;
        sum = next;
    }
    sum
}

fn step_overshoot(impulse: &[f64]) -> f64 {
    let mut step = 0.0_f64;
    let mut peak = f64::NEG_INFINITY;
    for &sample in impulse {
        step += sample;
        peak = peak.max(step);
    }
    (peak - 1.0).max(0.0)
}

fn dominant_amplitude(impulse: &[f64]) -> f64 {
    impulse.iter().copied().map(f64::abs).fold(0.0, f64::max)
}

fn impulse_energy_time_ms(impulse: &[f64], fraction: f64) -> f64 {
    let total = impulse.iter().map(|sample| sample * sample).sum::<f64>();
    let mut cumulative = 0.0;
    let index = impulse
        .iter()
        .position(|sample| {
            cumulative += sample * sample;
            cumulative >= total * fraction
        })
        .unwrap();
    index as f64 / 88_200.0 * 1_000.0
}

fn tail_energy_db_after_ms(impulse: &[f64], milliseconds: f64) -> f64 {
    let total = impulse.iter().map(|sample| sample * sample).sum::<f64>();
    let start = (88_200.0 * milliseconds / 1_000.0).round() as usize;
    let tail = impulse[start..]
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>();
    10.0 * (tail / total).log10()
}

#[test]
fn minimum_phase_compact_has_stable_identity_and_spec() {
    let filter = FilterType::MinimumPhaseCompact128k;
    assert_eq!(filter.as_id(), 30);
    assert_eq!(FilterType::from_id(30), Some(filter));
    assert_eq!(filter.as_name(), "MinimumPhaseCompact128k");
    assert_eq!(
        FilterType::from_name("MinimumPhaseCompact128k"),
        Some(filter)
    );
    assert_eq!(FilterType::from_name("MinimumPhase128kV5"), Some(filter));
    assert_eq!(
        serde_json::from_str::<FilterType>("\"MinimumPhase128kV5\"").unwrap(),
        filter
    );
    assert_eq!(filter.character_beta(), None);
    assert_eq!(filter.cleanup_beta(), MINIMUM_COMPACT_CLEANUP_BETA);

    let plan = build_integer_stage_plan(44_100, 88_200, filter, 100.0).unwrap();
    assert!(matches!(
        plan.stages.first(),
        Some(StageSpec::Character2x {
            taps_total: MINIMUM_COMPACT_BRANCH_TAPS,
            beta: MINIMUM_COMPACT_CLEANUP_BETA,
            engine: EngineKind::PartitionedFft {
                partition_frames: 4096
            },
            phase_mode: PhaseMode::MinimumPhaseCompact128k(MinimumCompactProfile::Original),
            ..
        })
    ));
}

#[test]
fn minimum_phase_compact_v2_is_the_plain_128k_minimum_phase_filter() {
    let filter = FilterType::MinimumPhaseCompact128kV2;
    assert_eq!(filter.as_id(), 31);
    assert_eq!(FilterType::from_id(31), Some(filter));
    assert_eq!(filter.as_name(), "MinimumPhaseCompact128kV2");
    assert_eq!(FilterType::from_name(filter.as_name()), Some(filter));
    assert_eq!(
        serde_json::from_str::<FilterType>(&serde_json::to_string(&filter).unwrap()).unwrap(),
        filter
    );
    assert_eq!(filter.minimum_compact_profile(), None);
    assert_eq!(filter.cutoff(), MINIMUM16K_PRODUCTION_CUTOFF);
    assert_eq!(filter.beta(), MINIMUM16K_PRODUCTION_BETA);
    assert_eq!(filter.character_beta(), Some(MINIMUM16K_PRODUCTION_BETA));
    assert_eq!(filter.cleanup_beta(), MINIMUM_COMPACT_CLEANUP_BETA);
    let plan = build_integer_stage_plan(44_100, 88_200, filter, 100.0).unwrap();
    assert!(matches!(
        plan.stages.first(),
        Some(StageSpec::Character2x {
            taps_total: MINIMUM_COMPACT_BRANCH_TAPS,
            engine: EngineKind::PartitionedFft {
                partition_frames: 4096
            },
            phase_mode: PhaseMode::MinimumPhase128k(MinimumPhase128kProfile::One),
            ..
        })
    ));
}

#[test]
fn smooth_phase_has_stable_identity_and_fir_only_stage_spec() {
    let filter = FilterType::SmoothPhase128k;
    assert_eq!(filter.as_id(), 32);
    assert_eq!(FilterType::from_id(32), Some(filter));
    assert_eq!(filter.as_name(), "SmoothPhase128k");
    assert_eq!(FilterType::from_name(filter.as_name()), Some(filter));
    assert_eq!(
        serde_json::from_str::<FilterType>(&serde_json::to_string(&filter).unwrap()).unwrap(),
        filter
    );
    assert_eq!(
        filter.minimum_compact_profile(),
        Some(MinimumCompactProfile::Smooth)
    );
    assert_eq!(filter.character_beta(), None);
    assert_eq!(filter.cleanup_beta(), MINIMUM_COMPACT_CLEANUP_BETA);

    let plan = build_integer_stage_plan(44_100, 88_200, filter, 100.0).unwrap();
    assert!(matches!(
        plan.stages.first(),
        Some(StageSpec::Character2x {
            taps_total: MINIMUM_COMPACT_BRANCH_TAPS,
            engine: EngineKind::PartitionedFft {
                partition_frames: 4096
            },
            phase_mode: PhaseMode::MinimumPhaseCompact128k(MinimumCompactProfile::Smooth),
            ..
        })
    ));

    // Filter selection contributes only the FIR stage above; there is no EQ
    // configuration or render-stage processor in the resulting stage plan.
    assert_eq!(plan.stages.len(), 1);
}

#[test]
fn minimum_phase_128k_profiles_share_fixed_cleanup_beta() {
    for filter in [
        FilterType::MinimumPhase128k,
        FilterType::MinimumPhase128kV2,
        FilterType::MinimumPhase128kV3,
        FilterType::MinimumPhase128kV4,
        FilterType::MinimumPhaseCompact128k,
        FilterType::MinimumPhaseCompact128kV2,
        FilterType::SmoothPhase128k,
    ] {
        let cleanup = cleanup_stage_spec(1, filter);
        assert!(matches!(cleanup, StageSpec::CleanupHalfband2x { beta, .. }
            if (beta - MINIMUM_COMPACT_CLEANUP_BETA).abs() < f64::EPSILON));
    }
}

#[test]
fn smootherstep7_is_bounded_monotonic_and_has_exact_endpoints() {
    assert_eq!(smootherstep7(-1.0), 0.0);
    assert_eq!(smootherstep7(0.0), 0.0);
    assert_eq!(smootherstep7(1.0), 1.0);
    assert_eq!(smootherstep7(2.0), 1.0);

    let mut previous = 0.0;
    for index in 0..=10_000 {
        let value = smootherstep7(index as f64 / 10_000.0);
        assert!((0.0..=1.0).contains(&value));
        assert!(value >= previous, "smootherstep7 reversed at {index}");
        previous = value;
    }
}

#[test]
fn plan_d_direct_magnitude_matches_the_production_specification() {
    let params = minimum_compact_params(MinimumCompactProfile::Original);
    assert_eq!(params.transition, MinimumCompactTransition::Smootherstep7);
    assert!(params.treble_taper.is_none());
    assert_eq!(params.stop_gain, MINIMUM_COMPACT_PRODUCTION_STOP_GAIN);
    assert_eq!(params.nyquist_gain, MINIMUM_COMPACT_PRODUCTION_STOP_GAIN);
    assert!((20.0 * params.stop_gain.log10() + 144.0).abs() <= 1e-12);

    for hz in [0.0, 14_500.0, 18_500.0, 20_000.0, 20_200.0] {
        assert_eq!(compact_minimum_magnitude(hz / 88_200.0, params), 1.0);
    }
    for normalized_frequency in [params.stop_edge_2x, 0.3, 0.4, 0.5, 0.75, 1.0] {
        assert_eq!(
            compact_minimum_magnitude(normalized_frequency, params),
            MINIMUM_COMPACT_PRODUCTION_STOP_GAIN
        );
    }

    let mut previous = 1.0;
    for index in 0..=10_000 {
        let frequency = params.pass_edge_2x
            + (params.stop_edge_2x - params.pass_edge_2x) * index as f64 / 10_000.0;
        let gain = compact_minimum_magnitude(frequency, params);
        assert!(gain <= previous, "Plan D transition reversed at {index}");
        assert!((params.stop_gain..=1.0).contains(&gain));
        previous = gain;
    }
}

#[test]
fn smooth_phase_direct_target_retains_its_planck_taper() {
    let params = minimum_compact_params(MinimumCompactProfile::Smooth);
    assert_eq!(params.transition, MinimumCompactTransition::Planck);
    for (hz, expected_db) in [(14_500.0, 0.0), (16_500.0, -0.275), (18_500.0, -0.55)] {
        let gain = compact_minimum_magnitude(hz / 88_200.0, params);
        let actual_db = 20.0 * gain.log10();
        assert!(
            (actual_db - expected_db).abs() <= 1e-12,
            "Smooth Phase direct gain at {hz} Hz was {actual_db} dB"
        );
    }
}

#[test]
fn minimum_phase_compact_meets_response_and_time_concentration_gates() {
    let impulse = minimum_phase_compact_impulse(MinimumCompactProfile::Original);
    assert_eq!(impulse.len(), MINIMUM_COMPACT_IMPULSE_SAMPLES);
    assert!(impulse.iter().all(|sample| sample.is_finite()));
    assert!((impulse.iter().sum::<f64>() - 1.0).abs() < 1e-10);

    let passband = [20.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 20_000.0]
        .map(|hz| coefficient_magnitude_db(&impulse, hz / 88_200.0));
    let passband_span = passband.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - passband.iter().copied().fold(f64::INFINITY, f64::min);
    let gain_20k = coefficient_magnitude_db(&impulse, 20_000.0 / 88_200.0);
    let gain_20_2k = coefficient_magnitude_db(&impulse, 20_200.0 / 88_200.0);
    let gain_20_6215k = coefficient_magnitude_db(&impulse, 20_621.5 / 88_200.0);
    let gain_21k = coefficient_magnitude_db(&impulse, 21_000.0 / 88_200.0);
    let gain_22_05k = coefficient_magnitude_db(&impulse, 22_050.0 / 88_200.0);
    assert!(
        passband_span <= 0.005,
        "passband span was {passband_span} dB"
    );
    assert!(gain_20k >= -0.01, "20 kHz gain was {gain_20k} dB");
    assert!(gain_20_2k >= -0.005, "20.2 kHz gain was {gain_20_2k} dB");
    assert!(
        (-0.7..=-0.2).contains(&gain_20_6215k),
        "20.6215 kHz gain was {gain_20_6215k} dB"
    );
    assert!(
        (-5.0..=-3.0).contains(&gain_21k),
        "21 kHz gain was {gain_21k} dB"
    );
    assert!(gain_22_05k <= -140.0, "22.05 kHz gain was {gain_22_05k} dB");

    let total_energy = impulse.iter().map(|x| x * x).sum::<f64>();
    let energy_time_ms = |fraction: f64| {
        let mut cumulative = 0.0;
        let index = impulse
            .iter()
            .position(|sample| {
                cumulative += sample * sample;
                cumulative >= total_energy * fraction
            })
            .unwrap();
        index as f64 / 88_200.0 * 1_000.0
    };
    assert!(energy_time_ms(0.999) <= 2.0);
    assert!(energy_time_ms(0.9999) <= 3.0);
    let after_5ms = impulse[(88_200 * 5 / 1_000)..]
        .iter()
        .map(|x| x * x)
        .sum::<f64>();
    let tail_db = 10.0 * (after_5ms / total_energy).log10();
    assert!(tail_db <= -65.0, "tail energy after 5 ms was {tail_db} dB");

    let even_sum = impulse.iter().step_by(2).sum::<f64>();
    let odd_sum = impulse.iter().skip(1).step_by(2).sum::<f64>();
    let nyquist_gain = (even_sum - odd_sum).abs();
    assert!(
        (nyquist_gain - MINIMUM_COMPACT_PRODUCTION_STOP_GAIN).abs() <= 1e-9,
        "reconstructed Nyquist gain was {nyquist_gain}"
    );

    // Reconstruct the coefficients exactly as the streaming 2x stage sees
    // them after reversal, branch padding, and independent normalization.
    let (mut phase0, mut phase1, _, _) = build_character_polyphase_pair(
        MINIMUM_COMPACT_BRANCH_TAPS / 2,
        MINIMUM_COMPACT_CLEANUP_BETA,
        0.5,
        PhaseMode::MinimumPhaseCompact128k(MinimumCompactProfile::Original),
    );
    assert_eq!(phase0.len(), MINIMUM_COMPACT_BRANCH_TAPS);
    assert_eq!(phase1.len(), MINIMUM_COMPACT_BRANCH_TAPS);
    phase0.reverse();
    phase1.reverse();
    let mut streaming_impulse = Vec::with_capacity(phase0.len() * 2);
    for (even, odd) in phase0.into_iter().zip(phase1) {
        streaming_impulse.push(even * 0.5);
        streaming_impulse.push(odd * 0.5);
    }
    let streaming_20_6215k = coefficient_magnitude_db(&streaming_impulse, 20_621.5 / 88_200.0);
    let streaming_21k = coefficient_magnitude_db(&streaming_impulse, 21_000.0 / 88_200.0);
    let streaming_22_05k = coefficient_magnitude_db(&streaming_impulse, 22_050.0 / 88_200.0);
    assert!((-0.7..=-0.2).contains(&streaming_20_6215k));
    assert!((-5.0..=-3.0).contains(&streaming_21k));
    assert!(streaming_22_05k <= -140.0);
}

#[test]
fn plan_d_exact_rational_impulse_and_rows_preserve_the_specification() {
    for (source_rate, target_rate, phase_den, pass_edge_hz, stop_edge_hz) in [
        (44_100, 48_000, 160, 20_200.0, 22_050.0),
        (48_000, 44_100, 147, 20_200.0, 22_050.0),
    ] {
        let half_width = PolyphaseResampler::fractional_half_width(
            FilterType::MinimumPhaseCompact128k,
            source_rate,
            target_rate,
            phase_den,
        );
        let impulse = minimum_phase_compact_rational_impulse(
            MinimumCompactProfile::Original,
            half_width,
            phase_den,
            source_rate,
            target_rate,
        );
        let num_taps = 2 * half_width + 1;
        assert_eq!(impulse.len(), num_taps * phase_den);
        assert!(impulse.iter().all(|sample| sample.is_finite()));
        assert!((impulse.iter().sum::<f64>() - 1.0).abs() <= 1e-10);

        let fine_rate = source_rate as f64 * phase_den as f64;
        let pass_db = coefficient_magnitude_db(&impulse, pass_edge_hz / fine_rate);
        let stop_db = coefficient_magnitude_db(&impulse, stop_edge_hz / fine_rate);
        assert!(
            pass_db >= -0.005,
            "{source_rate}->{target_rate} pass edge was {pass_db} dB"
        );
        assert!(
            stop_db <= -137.0,
            "{source_rate}->{target_rate} stop edge was {stop_db} dB"
        );

        let table = deinterleave_rational_impulse_into_rows(&impulse, half_width, phase_den);
        assert_eq!(table.len(), num_taps * phase_den);
        assert!(table.iter().all(|coefficient| coefficient.is_finite()));
        for phase in 0..phase_den {
            let row = &table[phase * num_taps..(phase + 1) * num_taps];
            assert!((row.iter().sum::<f64>() - 1.0).abs() <= 1e-12);
            let row_pass_db = coefficient_magnitude_db(row, pass_edge_hz / source_rate as f64);
            assert!(
                row_pass_db >= -0.01,
                "{source_rate}->{target_rate} phase {phase} pass edge was {row_pass_db} dB"
            );
            for stopband_step in 0..=16 {
                let stopband_hz = stop_edge_hz
                    + (source_rate as f64 * 0.5 - stop_edge_hz) * stopband_step as f64 / 16.0;
                let row_stop_db = coefficient_magnitude_db(row, stopband_hz / source_rate as f64);
                assert!(
                    row_stop_db <= -137.0,
                    "{source_rate}->{target_rate} phase {phase} at {stopband_hz} Hz was {row_stop_db} dB"
                );
            }
        }
    }
}

#[test]
fn minimum_phase_compact_balanced_legacy_profile_remains_stable() {
    let balanced = minimum_phase_compact_impulse(MinimumCompactProfile::Balanced);
    assert_eq!(balanced.len(), MINIMUM_COMPACT_IMPULSE_SAMPLES);
    assert!(balanced.iter().all(|sample| sample.is_finite()));
    assert!((balanced.iter().sum::<f64>() - 1.0).abs() < 1e-10);

    let stop_db = coefficient_magnitude_db(&balanced, 22_050.0 / 88_200.0);
    assert!(stop_db <= -150.0, "Balanced stop gain was {stop_db} dB");
    let balanced_dominant = dominant_amplitude(&balanced);
    let balanced_overshoot = step_overshoot(&balanced);
    assert!(balanced_dominant <= 0.300);
    assert!(balanced_overshoot <= 0.228);
    assert!(impulse_energy_time_ms(&balanced, 0.999) <= 2.0);
    assert!(impulse_energy_time_ms(&balanced, 0.9999) <= 2.5);
    assert!(tail_energy_db_after_ms(&balanced, 5.0) <= -68.0);
    let even_sum = balanced.iter().step_by(2).sum::<f64>();
    let odd_sum = balanced.iter().skip(1).step_by(2).sum::<f64>();
    assert!((even_sum - odd_sum).abs() <= 1e-12);
}

#[test]
fn smooth_phase_meets_response_and_time_concentration_gates() {
    let balanced = minimum_phase_compact_impulse(MinimumCompactProfile::Balanced);
    let smooth = minimum_phase_compact_impulse(MinimumCompactProfile::Smooth);
    assert_eq!(smooth.len(), MINIMUM_COMPACT_IMPULSE_SAMPLES);
    assert!(smooth.iter().all(|sample| sample.is_finite()));
    assert!((smooth.iter().sum::<f64>() - 1.0).abs() < 1e-10);

    for hz in [10_000.0, 12_000.0, 14_000.0, 14_500.0] {
        let delta = coefficient_magnitude_db(&smooth, hz / 88_200.0)
            - coefficient_magnitude_db(&balanced, hz / 88_200.0);
        assert!(delta.abs() <= 0.005, "{hz} Hz changed by {delta} dB");
    }
    for (hz, bounds) in [
        (15_000.0, (-0.01, 0.0)),
        (16_000.0, (-0.17, -0.12)),
        (16_500.0, (-0.31, -0.24)),
        (17_000.0, (-0.45, -0.37)),
        (18_000.0, (-0.59, -0.51)),
        (18_500.0, (-0.59, -0.51)),
        (20_000.0, (-0.59, -0.51)),
    ] {
        let delta = coefficient_magnitude_db(&smooth, hz / 88_200.0)
            - coefficient_magnitude_db(&balanced, hz / 88_200.0);
        assert!(
            (bounds.0..=bounds.1).contains(&delta),
            "{hz} Hz changed by {delta} dB"
        );
    }

    let stop_db = coefficient_magnitude_db(&smooth, 22_050.0 / 88_200.0);
    assert!(stop_db <= -150.0, "Smooth Phase stop gain was {stop_db} dB");
    assert!(impulse_energy_time_ms(&smooth, 0.999) <= 2.0);
    assert!(impulse_energy_time_ms(&smooth, 0.9999) <= 2.5);
    assert!(tail_energy_db_after_ms(&smooth, 5.0) <= -68.0);
    assert!(step_overshoot(&smooth) <= step_overshoot(&balanced));
    let even_sum = smooth.iter().step_by(2).sum::<f64>();
    let odd_sum = smooth.iter().skip(1).step_by(2).sum::<f64>();
    assert!((even_sum - odd_sum).abs() <= 1e-12);
}

#[test]
fn minimum_phase128k_profiles_have_stable_identity_and_stage_specs() {
    let profiles = [
        (
            FilterType::MinimumPhase128k,
            MinimumPhase128kProfile::One,
            26,
            "MinimumPhase128k",
            MINIMUM128K_PROFILE_1_BETA,
        ),
        (
            FilterType::MinimumPhase128kV2,
            MinimumPhase128kProfile::Two,
            27,
            "MinimumPhase128kV2",
            MINIMUM128K_PROFILE_2_BETA,
        ),
        (
            FilterType::MinimumPhase128kV3,
            MinimumPhase128kProfile::Three,
            28,
            "MinimumPhase128kV3",
            MINIMUM128K_PROFILE_3_BETA,
        ),
        (
            FilterType::MinimumPhase128kV4,
            MinimumPhase128kProfile::Four,
            29,
            "MinimumPhase128kV4",
            MINIMUM128K_PROFILE_4_BETA,
        ),
    ];
    for (filter, profile, id, name, beta) in profiles {
        assert_eq!(filter.as_id(), id);
        assert_eq!(FilterType::from_id(id), Some(filter));
        assert_eq!(filter.as_name(), name);
        assert_eq!(FilterType::from_name(name), Some(filter));
        assert!((filter.cutoff() - MINIMUM16K_PRODUCTION_CUTOFF).abs() < 1e-12);
        assert!((filter.beta() - beta).abs() < 1e-12);
        assert_eq!(
            serde_json::from_str::<FilterType>(&serde_json::to_string(&filter).unwrap()).unwrap(),
            filter
        );
        let plan = build_integer_stage_plan(44_100, 88_200, filter, 100.0).unwrap();
        assert!(matches!(plan.stages.first(), Some(StageSpec::Character2x {
            taps_total: MINIMUM128K_TAPS_TOTAL,
            engine: EngineKind::PartitionedFft { partition_frames: 4096 },
            phase_mode: PhaseMode::MinimumPhase128k(actual), ..
        }) if *actual == profile));
    }
}

#[test]
fn minimum_phase128k_profiles_are_pure_front_loaded_and_preserve_audible_magnitude() {
    let profiles = [
        MinimumPhase128kProfile::One,
        MinimumPhase128kProfile::Two,
        MinimumPhase128kProfile::Three,
        MinimumPhase128kProfile::Four,
    ];
    let mut knee_db = Vec::new();
    for profile in profiles {
        let proto =
            build_full_rate_2x_prototype(4_095, profile.beta(), MINIMUM16K_PRODUCTION_CUTOFF);
        let minimum = minimum_phase_impulse_with_params(&proto, minimum128k_phase_params());
        assert!(minimum.iter().all(|sample| sample.is_finite()));
        assert!((minimum.iter().sum::<f64>() - 1.0).abs() < 1e-10);
        assert!(dominant_impulse_index(&minimum) < minimum.len() / 16);
        for frequency in [0.0, 0.10, 0.20, 0.225] {
            let proto_db = full_rate_magnitude_db(&proto, frequency);
            let minimum_db = full_rate_magnitude_db(&minimum, frequency);
            assert!(
                (minimum_db - proto_db).abs() < 0.03,
                "{profile:?} magnitude mismatch at {frequency}: prototype={proto_db} minimum={minimum_db}"
            );
        }
        knee_db.push(full_rate_magnitude_db(&minimum, 0.2336));
    }
    assert!(
        knee_db.windows(2).all(|pair| pair[1] <= pair[0] + 1e-6),
        "the smoothing progression must broaden monotonically: {knee_db:?}"
    );
}

#[test]
fn minimum_phase128k_production_noise_rejection_is_clean() {
    let mut knee_db = Vec::new();
    for profile in [
        MinimumPhase128kProfile::One,
        MinimumPhase128kProfile::Two,
        MinimumPhase128kProfile::Three,
        MinimumPhase128kProfile::Four,
    ] {
        let proto = build_full_rate_2x_prototype(
            MINIMUM128K_TAPS_TOTAL / 2,
            profile.beta(),
            MINIMUM16K_PRODUCTION_CUTOFF,
        );
        let minimum = minimum_phase_impulse_with_params(&proto, minimum128k_phase_params());
        let gain_18k = coefficient_magnitude_db(&minimum, 18_000.0 / 88_200.0);
        let gain_20k = coefficient_magnitude_db(&minimum, 20_000.0 / 88_200.0);
        let gain_20_6k = coefficient_magnitude_db(&minimum, 20_621.5 / 88_200.0);
        let worst_stopband = [0.240, 0.250, 0.300, 0.400]
            .map(|frequency| coefficient_magnitude_db(&minimum, frequency))
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            gain_18k.abs() < 0.01,
            "{profile:?} 18 kHz gain was {gain_18k} dB"
        );
        assert!(
            gain_20k.abs() < 0.03,
            "{profile:?} 20 kHz gain was {gain_20k} dB"
        );
        assert!(
            worst_stopband < -120.0,
            "{profile:?} worst stopband noise rejection was {worst_stopband} dB"
        );
        knee_db.push(gain_20_6k);
        println!(
            "Minimum Phase 128k {profile:?}: 18k={gain_18k:.4} dB 20k={gain_20k:.4} dB 20.6215k={gain_20_6k:.3} dB stop={worst_stopband:.2} dB"
        );
    }
    assert!(
        knee_db.windows(2).all(|pair| pair[1] <= pair[0] + 1e-6),
        "production reconstruction knees must broaden monotonically: {knee_db:?}"
    );
}

#[test]
fn integrated128k_profiles_share_magnitude_tuning_and_use_candidate_transitions() {
    let profiles = [
        (
            FilterType::IntegratedPhase128k,
            IntegratedPhaseProfile::One,
            4_000.0,
            15_500.0,
        ),
        (
            FilterType::IntegratedPhase128kV2,
            IntegratedPhaseProfile::Two,
            3_750.0,
            15_250.0,
        ),
        (
            FilterType::IntegratedPhase128kV3,
            IntegratedPhaseProfile::Three,
            3_500.0,
            14_750.0,
        ),
        (
            FilterType::IntegratedPhase128kV4,
            IntegratedPhaseProfile::Four,
            3_250.0,
            14_250.0,
        ),
    ];
    for (filter, profile, f_lo, f_hi) in profiles {
        let params = integrated128k_phase_params(profile);
        assert!((filter.cutoff() - 0.468_750).abs() < 1e-12);
        assert!((filter.beta() - 22.400).abs() < 1e-12);
        assert!((params.transition_f_lo - f_lo / 88_200.0).abs() < 1e-12);
        assert!((params.transition_f_hi - f_hi / 88_200.0).abs() < 1e-12);
        assert!((params.causality_shift_scale - 1.02).abs() < 1e-12);
        assert!((params.tail_fade_fraction - 0.0048).abs() < 1e-12);
        assert!((params.phase_floor_rel - 1.0e-7).abs() < 1e-15);
    }
}

#[test]
fn integrated128k_id_name_serde_and_stage_spec_are_stable() {
    let profiles = [
        (
            FilterType::IntegratedPhase128k,
            IntegratedPhaseProfile::One,
            22,
            "IntegratedPhase128k",
        ),
        (
            FilterType::IntegratedPhase128kV2,
            IntegratedPhaseProfile::Two,
            23,
            "IntegratedPhase128kV2",
        ),
        (
            FilterType::IntegratedPhase128kV3,
            IntegratedPhaseProfile::Three,
            24,
            "IntegratedPhase128kV3",
        ),
        (
            FilterType::IntegratedPhase128kV4,
            IntegratedPhaseProfile::Four,
            25,
            "IntegratedPhase128kV4",
        ),
    ];
    assert_eq!(DEFAULT_FILTER_TYPE, FilterType::SplitPhase128kE2v3);
    assert_eq!(
        FilterType::from_name("IntegratedPhase"),
        Some(FilterType::IntegratedPhase128k)
    );
    for (filter, profile, id, name) in profiles {
        assert_eq!(filter.as_id(), id);
        assert_eq!(FilterType::from_id(id), Some(filter));
        assert_eq!(filter.as_name(), name);
        assert_eq!(FilterType::from_name(name), Some(filter));
        assert_eq!(
            serde_json::from_str::<FilterType>(&serde_json::to_string(&filter).unwrap()).unwrap(),
            filter
        );
        let plan = build_integer_stage_plan(44_100, 88_200, filter, 100.0).unwrap();
        assert!(plan.high_latency);
        assert!(matches!(plan.stages.first(), Some(StageSpec::Character2x {
            taps_total: 131_071,
            engine: EngineKind::PartitionedFft { partition_frames: 4096 },
            phase_mode: PhaseMode::IntegratedPhase128k(actual), ..
        }) if *actual == profile));
    }
}

#[test]
fn integrated_phase_standard_dsd_stage_plans_preserve_phase_mode() {
    for source_rate in [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
        let family_base = if source_rate.is_multiple_of(44_100) {
            2_822_400
        } else {
            3_072_000
        };
        for target_rate in [family_base, family_base * 2, family_base * 4] {
            if target_rate <= source_rate || !target_rate.is_multiple_of(source_rate) {
                continue;
            }
            let ratio = target_rate / source_rate;
            if !ratio.is_power_of_two() || ratio > 256 {
                continue;
            }
            for (filter, profile) in [
                (FilterType::IntegratedPhase128k, IntegratedPhaseProfile::One),
                (
                    FilterType::IntegratedPhase128kV2,
                    IntegratedPhaseProfile::Two,
                ),
                (
                    FilterType::IntegratedPhase128kV3,
                    IntegratedPhaseProfile::Three,
                ),
                (
                    FilterType::IntegratedPhase128kV4,
                    IntegratedPhaseProfile::Four,
                ),
            ] {
                let plan =
                    build_integer_stage_plan(source_rate, target_rate, filter, 100.0).unwrap();
                assert!(matches!(plan.stages.first(), Some(StageSpec::Character2x {
                    phase_mode: PhaseMode::IntegratedPhase128k(actual), ..
                }) if *actual == profile));
            }
        }
    }
}

#[test]
fn integrated_phase_weight_is_exact_monotonic_and_flat_at_endpoints() {
    let params = integrated128k_phase_params(IntegratedPhaseProfile::One);
    assert_eq!(integrated_phase_weight(params.transition_f_lo, params), 0.0);
    assert_eq!(integrated_phase_weight(params.transition_f_hi, params), 1.0);
    let values: Vec<f64> = (0..=1000)
        .map(|i| {
            let f = params.transition_f_lo
                + (params.transition_f_hi - params.transition_f_lo) * i as f64 / 1000.0;
            integrated_phase_weight(f, params)
        })
        .collect();
    assert!(values.iter().all(|value| value.is_finite()));
    assert!(values.windows(2).all(|pair| pair[1] + 1e-15 >= pair[0]));
    assert!(values[1] < 1e-8);
    assert!(1.0 - values[999] < 1e-8);
}

#[test]
fn integrated_phase_profiles_progress_monotonically_toward_minimum_phase() {
    let params = [
        integrated128k_phase_params(IntegratedPhaseProfile::One),
        integrated128k_phase_params(IntegratedPhaseProfile::Two),
        integrated128k_phase_params(IntegratedPhaseProfile::Three),
        integrated128k_phase_params(IntegratedPhaseProfile::Four),
    ];
    for hz in [4_000.0, 5_000.0, 6_000.0, 8_000.0, 10_000.0, 12_000.0] {
        let normalized = hz / 88_200.0;
        let weights = params.map(|profile| integrated_phase_weight(normalized, profile));
        assert!(
            weights.windows(2).all(|pair| pair[0] <= pair[1] + 1e-15),
            "{hz} Hz weights: {weights:?}"
        );
    }
}

#[test]
fn integrated_phase_increments_are_constant_rejoin_and_match_minimum() {
    let fft_len = 4096;
    let bins = fft_len / 2 + 1;
    let minimum: Vec<f64> = (0..bins)
        .map(|k| -0.007 * k as f64 - 2.0e-7 * (k as f64).powi(2))
        .collect();
    let params = integrated128k_phase_params(IntegratedPhaseProfile::One);
    let target = integrated_phase_from_unwrapped_minimum(&minimum, fft_len, params);
    assert!(target.iter().all(|value| value.is_finite()));

    let lo_end = (params.transition_f_lo * fft_len as f64 - 0.5).floor() as usize;
    let low_increments: Vec<f64> = (1..=lo_end).map(|k| target[k] - target[k - 1]).collect();
    let spread = low_increments
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        - low_increments.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(spread < 1e-12, "low-band increment spread was {spread}");

    let hi_start = (params.transition_f_hi * fft_len as f64 + 0.5).ceil() as usize;
    assert!((target[hi_start] - minimum[hi_start]).abs() < 1e-9);
    for k in hi_start..bins {
        let target_increment = target[k] - target[k - 1];
        let minimum_increment = minimum[k] - minimum[k - 1];
        assert!((target_increment - minimum_increment).abs() < 1e-12);
    }
}

#[test]
fn integrated_phase_runtime_reports_profile_preservation() {
    for filter in [
        FilterType::IntegratedPhase128k,
        FilterType::IntegratedPhase128kV2,
        FilterType::IntegratedPhase128kV3,
        FilterType::IntegratedPhase128kV4,
    ] {
        let exact = SincResampler::new(filter, 192_000, 176_400);
        assert_eq!(
            exact.runtime_info().path_kind,
            ResamplerPathKind::ExactRational
        );
        assert_eq!(
            (
                exact.runtime_info().ratio_num,
                exact.runtime_info().ratio_den
            ),
            (160, 147)
        );
        assert!(exact.runtime_info().phase_profile_preserved);
    }

    let fallback = SincResampler::new(FilterType::IntegratedPhase128k, 44_100, 50_000);
    assert_eq!(
        fallback.runtime_info().path_kind,
        ResamplerPathKind::CappedFractional
    );
    assert!(!fallback.runtime_info().phase_profile_preserved);

    let high_denominator = SincResampler::new(FilterType::IntegratedPhase128k, 44_100, 96_000);
    assert_eq!(
        high_denominator.runtime_info().path_kind,
        ResamplerPathKind::ExactRational
    );
    assert_eq!(high_denominator.runtime_info().ratio_den, 320);
    assert!(!high_denominator.runtime_info().phase_profile_preserved);
}

#[test]
fn integrated_phase_small_impulse_is_finite_normalized_and_front_loaded() {
    let proto = build_full_rate_2x_prototype(64, 14.0, 0.46);
    for profile in [
        IntegratedPhaseProfile::One,
        IntegratedPhaseProfile::Two,
        IntegratedPhaseProfile::Three,
        IntegratedPhaseProfile::Four,
    ] {
        let impulse =
            integrated_phase_impulse_with_params(&proto, integrated128k_phase_params(profile));
        assert!(impulse.iter().all(|sample| sample.is_finite()));
        assert!((impulse.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(dominant_impulse_index(&impulse) < impulse.len() / 4);
    }
    assert_eq!(nearest_even_delay_samples(13.1) % 2, 0);
}

#[test]
fn integrated_phase_profiles_preserve_the_same_prototype_magnitude_response() {
    let proto = build_full_rate_2x_prototype(4_095, 22.400, 0.468_750);
    let profiles = [
        IntegratedPhaseProfile::One,
        IntegratedPhaseProfile::Two,
        IntegratedPhaseProfile::Three,
        IntegratedPhaseProfile::Four,
    ];
    let impulses = profiles.map(|profile| {
        integrated_phase_impulse_with_params(&proto, integrated128k_phase_params(profile))
    });
    for frequency in [0.0, 0.025, 0.05, 0.10, 0.15, 0.20, 0.225] {
        let prototype_db = full_rate_magnitude_db(&proto, frequency);
        let profile_db: [f64; 4] =
            std::array::from_fn(|index| full_rate_magnitude_db(&impulses[index], frequency));
        assert!(
            profile_db.iter().all(|db| (db - prototype_db).abs() < 0.03),
            "prototype mismatch at f={frequency}: prototype={prototype_db}, profiles={profile_db:?}"
        );
        let min = profile_db.iter().copied().fold(f64::INFINITY, f64::min);
        let max = profile_db.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max - min < 0.01,
            "profile magnitude spread at f={frequency}: {profile_db:?}"
        );
    }
}

#[test]
fn split128k_defaults_match_dsd128_ec2_production_profile() {
    let params = split128k_phase_params();

    assert!((FilterType::Split128k.cutoff() - 0.465333).abs() < 1e-12);
    assert!((FilterType::Split128k.beta() - 23.12088).abs() < 1e-12);
    assert!((params.split_f_lo - 3_000.0 / 88_200.0).abs() < 1e-12);
    assert!((params.split_f_hi - 14_000.0 / 88_200.0).abs() < 1e-12);
    assert!((params.low_blend_floor - 0.038155).abs() < 1e-12);
    assert!((params.causality_shift_scale - 1.040606).abs() < 1e-12);
    assert!((params.tail_fade_fraction - 0.005621).abs() < 1e-12);
}

#[test]
fn minimum16k_defaults_match_dsd128_ec2_tuned_profile() {
    let params = minimum16k_phase_params();

    assert!((FilterType::Minimum16k.cutoff() - 0.467621).abs() < 1e-12);
    assert!((FilterType::Minimum16k.beta() - 20.47325).abs() < 1e-12);
    assert!((params.tail_fade_fraction - 0.007617).abs() < 1e-12);
    assert!((params.mag_floor_rel - 1.13771845358e-12).abs() < 1e-18);
}

fn make_signal(frames: usize) -> (Vec<f64>, Vec<f64>) {
    let left: Vec<f64> = (0..frames)
        .map(|i| (2.0 * std::f64::consts::PI * 0.037 * i as f64).sin())
        .collect();
    let right: Vec<f64> = (0..frames)
        .map(|i| (2.0 * std::f64::consts::PI * 0.071 * i as f64).cos())
        .collect();
    (left, right)
}

fn make_pop_sensitive_signal(frames: usize, sample_rate: u32) -> (Vec<f64>, Vec<f64>) {
    let rate = sample_rate as f64;
    let f2 = (rate * 0.11).min(12_500.0);
    let f3 = (rate * 0.19).min(18_000.0);
    let denom = frames.saturating_sub(1).max(1) as f64;
    let mut seed = 0x1234_5678_u64;

    let mut next_noise = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let bits = ((seed >> 33) as u32) as f64 / u32::MAX as f64;
        bits * 2.0 - 1.0
    };

    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = i as f64 / rate;
        let window = (PI * i as f64 / denom).sin().powi(2);
        let slow = 0.72 + 0.18 * (2.0 * PI * 0.7 * t).sin();
        let noise = next_noise() * 0.015;
        left.push(
            window
                * slow
                * (0.46 * (2.0 * PI * 997.0 * t).sin()
                    + 0.18 * (2.0 * PI * f2 * t).sin()
                    + 0.08 * (2.0 * PI * f3 * t).cos()
                    + noise),
        );
        right.push(
            window
                * slow
                * (0.42 * (2.0 * PI * 1231.0 * t).cos()
                    + 0.16 * (2.0 * PI * (f2 * 0.73) * t).sin()
                    + 0.07 * (2.0 * PI * (f3 * 0.61) * t).cos()
                    - noise),
        );
    }
    (left, right)
}

fn max_adjacent_frame_jump(interleaved: &[f64], channels: usize) -> (f64, usize) {
    let mut worst = 0.0_f64;
    let mut worst_frame = 0usize;
    for (frame_idx, (a, b)) in interleaved
        .chunks_exact(channels)
        .zip(interleaved.chunks_exact(channels).skip(1))
        .enumerate()
    {
        let jump = a
            .iter()
            .zip(b)
            .map(|(x, y)| (y - x).abs())
            .fold(0.0, f64::max);
        if jump > worst {
            worst = jump;
            worst_frame = frame_idx + 1;
        }
    }
    (worst, worst_frame)
}

fn count_interior_near_zero_blocks(
    interleaved: &[f64],
    channels: usize,
    block_frames: usize,
) -> Option<usize> {
    let frames: Vec<&[f64]> = interleaved.chunks_exact(channels).collect();
    let first_active = frames
        .iter()
        .position(|frame| frame.iter().any(|sample| sample.abs() >= 1e-10))?;
    let last_active = frames
        .iter()
        .rposition(|frame| frame.iter().any(|sample| sample.abs() >= 1e-10))?;
    let start = first_active.saturating_add(block_frames);
    let end = last_active.saturating_sub(block_frames);
    if start >= end || end.saturating_sub(start) < block_frames {
        return None;
    }

    (start..=end.saturating_sub(block_frames)).find(|&frame| {
        interleaved[frame * channels..(frame + block_frames) * channels]
            .iter()
            .all(|sample| sample.abs() < 1e-12)
    })
}

fn count_isolated_single_frame_spikes(
    interleaved: &[f64],
    channels: usize,
    spike_limit: f64,
) -> Option<usize> {
    let frames: Vec<&[f64]> = interleaved.chunks_exact(channels).collect();
    if frames.len() < 3 {
        return None;
    }
    (1..frames.len() - 1).find(|&idx| {
        let before = frames[idx - 1];
        let current = frames[idx];
        let after = frames[idx + 1];
        let neighbor_jump = before
            .iter()
            .zip(after)
            .map(|(a, b)| (b - a).abs())
            .fold(0.0, f64::max);
        let current_jump = before
            .iter()
            .zip(current)
            .chain(current.iter().zip(after))
            .map(|(a, b)| (b - a).abs())
            .fold(0.0, f64::max);
        current_jump > spike_limit && neighbor_jump < spike_limit * 0.25
    })
}

fn assert_no_pop(label: &str, interleaved: &[f64], channels: usize, jump_limit: f64) {
    assert!(
        channels > 0 && interleaved.len().is_multiple_of(channels),
        "{label}: invalid channel layout"
    );
    assert!(
        interleaved.iter().all(|sample| sample.is_finite()),
        "{label}: non-finite sample"
    );

    let peak = interleaved
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f64, f64::max);
    if peak > 1.05 {
        let path = dump_failing_wav_segment(label, interleaved, channels, 0);
        panic!("{label}: peak {peak} exceeds 1.05, debug wav={path:?}");
    }

    let (jump, jump_frame) = max_adjacent_frame_jump(interleaved, channels);
    if jump > jump_limit {
        let path = dump_failing_wav_segment(label, interleaved, channels, jump_frame);
        panic!("{label}: adjacent frame jump {jump} exceeds {jump_limit}, debug wav={path:?}");
    }

    if let Some(frame) = count_interior_near_zero_blocks(interleaved, channels, 64) {
        let path = dump_failing_wav_segment(label, interleaved, channels, frame);
        panic!("{label}: unexpected interior zero block near frame {frame}, debug wav={path:?}");
    }

    if let Some(frame) = count_isolated_single_frame_spikes(interleaved, channels, jump_limit) {
        let path = dump_failing_wav_segment(label, interleaved, channels, frame);
        panic!("{label}: isolated single-frame spike near frame {frame}, debug wav={path:?}");
    }
}

fn dump_failing_wav_segment(
    label: &str,
    interleaved: &[f64],
    channels: usize,
    center_frame: usize,
) -> Option<PathBuf> {
    let frames = interleaved.len() / channels;
    if frames == 0 {
        return None;
    }
    let half_window = 4800usize;
    let start_frame = center_frame.saturating_sub(half_window);
    let end_frame = (center_frame + half_window).min(frames);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/audio-pop-failures");
    std::fs::create_dir_all(&dir).ok()?;
    let safe_label: String = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe_label}-{center_frame}.wav"));
    let mut file = std::fs::File::create(&path).ok()?;
    write_wav_i16(
        &mut file,
        &interleaved[start_frame * channels..end_frame * channels],
        channels as u16,
        48_000,
    )
    .ok()?;
    Some(path)
}

fn write_wav_i16(
    writer: &mut dyn Write,
    samples: &[f64],
    channels: u16,
    sample_rate: u32,
) -> std::io::Result<()> {
    let data_bytes = samples.len() as u32 * 2;
    writer.write_all(b"RIFF")?;
    writer.write_all(&(36 + data_bytes).to_le_bytes())?;
    writer.write_all(b"WAVEfmt ")?;
    writer.write_all(&16u32.to_le_bytes())?;
    writer.write_all(&1u16.to_le_bytes())?;
    writer.write_all(&channels.to_le_bytes())?;
    writer.write_all(&sample_rate.to_le_bytes())?;
    writer.write_all(&(sample_rate * channels as u32 * 2).to_le_bytes())?;
    writer.write_all(&(channels * 2).to_le_bytes())?;
    writer.write_all(&16u16.to_le_bytes())?;
    writer.write_all(b"data")?;
    writer.write_all(&data_bytes.to_le_bytes())?;
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f64).round() as i16;
        writer.write_all(&pcm.to_le_bytes())?;
    }
    Ok(())
}

#[test]
fn filter_ids_are_backward_compatible() {
    assert_eq!(FilterType::SincExtreme32k.as_id(), 6);
    assert_eq!(FilterType::LinearPhase128k.as_id(), 33);
    assert_eq!(FilterType::Minimum16k.as_id(), 15);
    assert_eq!(FilterType::Split128k.as_id(), 21);
    assert_eq!(FilterType::Split128kV2.as_id(), 34);
    assert_eq!(FilterType::SplitPhase128kV3.as_id(), 35);
    assert_eq!(FilterType::SplitPhase128kV4.as_id(), 36);
    assert_eq!(FilterType::SplitPhase128kE2v3.as_id(), 37);
    assert_eq!(FilterType::SplitPhase128kE3.as_id(), 38);
    assert_eq!(FilterType::from_id(0), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(2), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(11), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(1), None);
    assert_eq!(FilterType::from_id(3), None);
    assert_eq!(FilterType::from_id(4), None);
    assert_eq!(FilterType::from_id(5), None);
    assert_eq!(FilterType::from_id(7), None);
    assert_eq!(FilterType::from_id(8), None);
    assert_eq!(FilterType::from_id(9), None);
    assert_eq!(FilterType::from_id(10), None);
    assert_eq!(FilterType::from_id(12), None);
    assert_eq!(FilterType::from_id(13), None);
    assert_eq!(FilterType::from_id(14), None);
    assert_eq!(FilterType::from_id(15), Some(FilterType::Minimum16k));
    assert_eq!(FilterType::from_id(16), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(17), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(18), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(19), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(20), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(21), Some(FilterType::Split128k));
    assert_eq!(FilterType::from_id(33), Some(FilterType::LinearPhase128k));
    assert_eq!(FilterType::from_id(34), Some(FilterType::Split128kV2));
    assert_eq!(FilterType::from_id(35), Some(FilterType::SplitPhase128kV3));
    assert_eq!(FilterType::from_id(36), Some(FilterType::SplitPhase128kV4));
    assert_eq!(
        FilterType::from_id(37),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(FilterType::from_id(38), Some(FilterType::SplitPhase128kE3));
    assert_eq!(FilterType::SincExtreme32k.as_name(), "SincExtreme32k");
    assert_eq!(FilterType::LinearPhase128k.as_name(), "LinearPhase128k");
    assert_eq!(FilterType::Minimum16k.as_name(), "Minimum16k");
    assert_eq!(FilterType::Split128k.as_name(), "Split128k");
    assert_eq!(FilterType::Split128kV2.as_name(), "Split128kV2");
    assert_eq!(FilterType::SplitPhase128kV3.as_name(), "SplitPhase128kV3");
    assert_eq!(FilterType::SplitPhase128kV4.as_name(), "SplitPhase128kV4");
    assert_eq!(
        FilterType::SplitPhase128kE2v3.as_name(),
        "SplitPhase128kE2v3"
    );
    assert_eq!(FilterType::SplitPhase128kE3.as_name(), "SplitPhase128kE3");
    assert_eq!(
        FilterType::from_name("Split128k"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("Split128kV2"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("SplitPhase128kV3"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("SplitPhase128kV4"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("SplitPhase128kE2v3"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("SplitPhase128kV5E2v3"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("SplitPhase128kE3"),
        Some(FilterType::SplitPhase128kE3)
    );
    assert_eq!(
        FilterType::from_name("SplitPhaseB"),
        Some(FilterType::SplitPhase128kE3)
    );
    assert_eq!(
        serde_json::from_str::<FilterType>("\"split-phase-b\"").unwrap(),
        FilterType::SplitPhase128kE3
    );
    assert_eq!(
        serde_json::from_str::<FilterType>("\"SplitPhase128kV3\"").unwrap(),
        FilterType::SplitPhase128kV3
    );
    assert_eq!(
        serde_json::to_string(&FilterType::SplitPhase128kV3).unwrap(),
        "\"SplitPhase128kV3\""
    );
    assert_eq!(
        serde_json::from_str::<FilterType>("\"SplitPhase128kV4\"").unwrap(),
        FilterType::SplitPhase128kV4
    );
    assert_eq!(
        serde_json::to_string(&FilterType::SplitPhase128kV4).unwrap(),
        "\"SplitPhase128kV4\""
    );
    assert_eq!(
        serde_json::from_str::<FilterType>("\"SplitPhase128kE2v3\"").unwrap(),
        FilterType::SplitPhase128kE2v3
    );
    assert_eq!(
        FilterType::from_name("Minimum16k"),
        Some(FilterType::Minimum16k)
    );
    assert_eq!(
        FilterType::from_name("Mixed16k"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("Linear"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("LinearPhase128k"),
        Some(FilterType::LinearPhase128k)
    );
    assert_eq!(
        FilterType::from_name("SincMedium"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("Perfect"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("Split16k"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        FilterType::from_name("Split16kDsd128"),
        Some(FilterType::SplitPhase128kE2v3)
    );
    assert_eq!(
        serde_json::from_str::<FilterType>("\"Perfect\"").unwrap(),
        FilterType::Split128k
    );
    assert_eq!(
        serde_json::from_str::<FilterType>("\"Mixed16k\"").unwrap(),
        FilterType::Split128k
    );
    assert_eq!(FilterType::from_name("SincFast"), None);
    assert_eq!(FilterType::from_name("SincExtreme"), None);
    assert_eq!(FilterType::from_name("Minimum32k"), None);
    assert_eq!(FilterType::from_name("SincExtreme128k"), None);
    assert_eq!(FilterType::from_name("Apodizing"), None);
    assert_eq!(FilterType::from_name("SincLongRealtime"), None);
    assert_eq!(FilterType::from_name("ApodizingLong"), None);
    assert_eq!(FilterType::from_name("SincExperimental512k"), None);
    assert_eq!(FilterType::from_name("MinimumExtreme128k"), None);
    assert_eq!(FilterType::from_name("MinimumExp512k"), None);
    assert_eq!(FilterType::from_name("MinimumExp1m"), None);
}

#[test]
fn frozen_filter_assets_decode_little_endian_f64() {
    let values = [0.5_f64, -0.25, f64::MIN_POSITIVE];
    let bytes = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let decoded = decode_f64le_asset(&bytes, values.len(), "test");
    assert_eq!(&*decoded, &values);
}

#[test]
fn planner_rejects_non_integer_ratios() {
    assert_eq!(
        build_integer_stage_plan(44_100, 48_000, FilterType::SincExtreme32k, 100.0).unwrap_err(),
        StagePlanError::NonIntegerRatio
    );
}

#[test]
fn planner_builds_power_of_two_cascade() {
    let plan =
        build_integer_stage_plan(44_100, 352_800, FilterType::SincExtreme32k, 100.0).unwrap();
    assert_eq!(plan.stages.len(), 3);
    assert!(matches!(plan.stages[0], StageSpec::Character2x { .. }));
    assert!(matches!(
        plan.stages[1],
        StageSpec::CleanupHalfband2x { .. }
    ));
    assert!(matches!(
        plan.stages[2],
        StageSpec::CleanupHalfband2x { .. }
    ));
    assert!(plan.high_latency);
    assert!(plan.latency_ms > 100.0, "latency was {}", plan.latency_ms);
}

#[test]
fn planner_supports_dsd_ratios() {
    // DSD128 from 44.1 kHz: ratio 128 → 7 stages.
    let plan =
        build_integer_stage_plan(44_100, 44_100 * 128, FilterType::SincExtreme32k, 1000.0).unwrap();
    assert_eq!(plan.stages.len(), 7);
    // DSD256: ratio 256 → 8 stages.
    let plan =
        build_integer_stage_plan(44_100, 44_100 * 256, FilterType::SincExtreme32k, 1000.0).unwrap();
    assert_eq!(plan.stages.len(), 8);
    // Late cleanup stages must stay short enough to keep the SIMD direct path engaged.
    for stage in plan.stages.iter().skip(2) {
        let taps = match stage {
            StageSpec::Character2x { taps_total, .. }
            | StageSpec::CleanupHalfband2x { taps_total, .. } => *taps_total,
        };
        assert!(
            taps <= MAX_SIMD_DIRECT_TAPS,
            "late stage has {taps} taps, must be ≤ {MAX_SIMD_DIRECT_TAPS}",
        );
    }
}

#[test]
fn planner_rejects_ratio_above_256() {
    assert!(matches!(
        build_integer_stage_plan(44_100, 44_100 * 512, FilterType::SincExtreme32k, 10_000.0),
        Err(StagePlanError::UnsupportedIntegerRatio(512))
    ));
}

#[test]
fn planner_uses_fft_for_retained_character_stages_and_simd_for_cleanups() {
    let plan =
        build_integer_stage_plan(44_100, 176_400, FilterType::SincExtreme32k, 100.0).unwrap();
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            engine: EngineKind::PartitionedFft { .. },
            ..
        }
    ));
    assert!(matches!(
        plan.stages[1],
        StageSpec::CleanupHalfband2x {
            engine: EngineKind::DirectSimd,
            ..
        }
    ));
}

#[test]
fn planner_marks_32k_as_high_latency() {
    let plan =
        build_integer_stage_plan(44_100, 352_800, FilterType::SincExtreme32k, 100.0).unwrap();
    assert!(plan.high_latency);
    assert!(plan.latency_ms > 400.0, "latency was {}", plan.latency_ms);
}

#[test]
fn minimum_phase_plan_reports_front_loaded_latency() {
    // Minimum16k's min-phase character stage is front-loaded: only the FFT
    // partition and the (linear) cleanup stages should remain in the
    // reported latency.
    let linear =
        build_integer_stage_plan(44_100, 352_800, FilterType::SincExtreme32k, 100.0).unwrap();
    let minimum = build_integer_stage_plan(44_100, 352_800, FilterType::Minimum16k, 100.0).unwrap();
    assert!(
        minimum.latency_ms < linear.latency_ms * 0.25,
        "min-phase latency {} ms should be far below linear-phase {} ms",
        minimum.latency_ms,
        linear.latency_ms
    );
}

#[test]
fn fractional_downsample_attenuates_above_target_nyquist() {
    // 192 kHz -> 176.4 kHz with SincMedium. A 91.5 kHz tone sits above the
    // 88.2 kHz target Nyquist but inside SincMedium's unscaled 0.48
    // passband (92.16 kHz at the source rate) — without ratio-scaled
    // cutoff it sails through and aliases down to ~84.9 kHz.
    let mut resampler = PolyphaseResampler::new(FilterType::SincExtreme32k, 192_000, 176_400);
    let freq = 91_500.0 / 192_000.0;
    let input: Vec<f64> = (0..16_384)
        .map(|i| (2.0 * PI * freq * i as f64).sin())
        .collect();
    resampler.input(&input, &input);
    let mut output = Vec::new();
    resampler.process(&mut output);
    assert!(output.len() > 4096, "expected output, got {}", output.len());

    // Skip the filter's settling region, then measure peak amplitude.
    let settled = &output[2048..];
    let peak = settled.iter().fold(0.0_f64, |acc, &s| acc.max(s.abs()));
    assert!(
        peak < 0.01,
        "above-target-Nyquist tone leaked through at peak {peak} (should be < -40 dB)"
    );
}

#[test]
fn fractional_downsample_stretches_kernel_support() {
    let resampler = PolyphaseResampler::new(FilterType::SincExtreme32k, 192_000, 44_100);
    assert_eq!(resampler.phase_count, POLYPHASE_PHASES);
    assert_eq!(
        resampler.half_width,
        max_polyphase_half_width(POLYPHASE_PHASES)
    );
    assert!(
        resampler.coefficients.len() * size_of::<f64>() <= MAX_POLYPHASE_COEFFICIENT_TABLE_BYTES
    );
}

#[test]
fn same_family_downsample_uses_reverse_stage_order() {
    let resampler = SincResampler::new(FilterType::Split128k, 192_000, 48_000);
    let ResamplerPath::Downsample(chain) = &resampler.path else {
        panic!("expected downsample chain");
    };

    assert_eq!(chain.source_rate, 192_000);
    assert_eq!(chain.target_rate, 48_000);
    assert_eq!(chain.steps.len(), 2);
    assert!(chain.high_latency);

    let DownsampleStep::Decimate2(first) = &chain.steps[0];
    assert_eq!(first.filter_type, FilterType::Split128k);
    assert_eq!(first.input_rate, 192_000);
    assert_eq!(first.output_rate, 96_000);
    assert!(matches!(first.spec, StageSpec::CleanupHalfband2x { .. }));

    let DownsampleStep::Decimate2(second) = &chain.steps[1];
    assert_eq!(second.input_rate, 96_000);
    assert_eq!(second.output_rate, 48_000);
    assert!(matches!(
        second.spec,
        StageSpec::Character2x {
            phase_mode: PhaseMode::SplitPhase128k,
            ..
        }
    ));
}

#[test]
fn standard_cross_family_ratios_use_exact_rational_path() {
    let cases = [
        (44_100, 48_000, 147, 160),
        (48_000, 44_100, 160, 147),
        (88_200, 96_000, 147, 160),
        (96_000, 88_200, 160, 147),
        (192_000, 176_400, 160, 147),
        (96_000, 44_100, 320, 147),
    ];

    for (source_rate, target_rate, ratio_num, ratio_den) in cases {
        let resampler = SincResampler::new(FilterType::SincExtreme32k, source_rate, target_rate);
        let ResamplerPath::Rational(rational) = &resampler.path else {
            panic!("expected exact rational for {source_rate}->{target_rate}");
        };
        assert_eq!(
            (rational.step_num, rational.phase_den),
            (ratio_num, ratio_den)
        );
        let info = resampler.runtime_info();
        assert_eq!(info.path_kind, ResamplerPathKind::ExactRational);
        assert!(!info.uses_capped_fallback);
        assert_eq!(
            (info.ratio_num, info.ratio_den),
            (ratio_num as u32, ratio_den as u32)
        );
    }
}

#[test]
fn plan_d_exact_rational_runtime_is_preserved_finite_and_drains_to_nominal_length() {
    let source_rate = 88_200;
    let target_rate = 96_000;
    let (left, right) = make_signal(4_096);
    let mut resampler = SincResampler::new(
        FilterType::MinimumPhaseCompact128k,
        source_rate,
        target_rate,
    );
    let info = resampler.runtime_info();
    assert_eq!(info.path_kind, ResamplerPathKind::ExactRational);
    assert!(info.phase_profile_preserved);
    assert!(!info.uses_capped_fallback);
    assert_eq!((info.ratio_num, info.ratio_den), (147, 160));

    let mut output = Vec::new();
    for start in (0..left.len()).step_by(257) {
        let end = (start + 257).min(left.len());
        resampler.input(&left[start..end], &right[start..end]);
        resampler.process(&mut output);
    }
    resampler.drain_eof(&mut output);
    assert_eq!(
        output.len(),
        nominal_output_samples(left.len(), source_rate, target_rate)
    );
    assert!(output.iter().all(|sample| sample.is_finite()));
}

#[test]
fn long_filter_downsample_constructs_for_same_family_rates() {
    for filter in [
        FilterType::Split128k,
        FilterType::Minimum16k,
        FilterType::IntegratedPhase128k,
        FilterType::IntegratedPhase128kV2,
        FilterType::IntegratedPhase128kV3,
        FilterType::IntegratedPhase128kV4,
    ] {
        for (source_rate, target_rate) in [(96_000, 48_000), (192_000, 48_000)] {
            let resampler = SincResampler::new(filter, source_rate, target_rate);
            assert!(
                matches!(resampler.path, ResamplerPath::Downsample(_)),
                "{filter:?} {source_rate}->{target_rate} should use DownsampleChain"
            );
            assert_eq!(resampler.filter_type(), filter);
            assert_eq!(resampler.source_rate(), source_rate);
            assert_eq!(resampler.target_rate(), target_rate);
            assert!(resampler.is_high_latency());
            assert!(resampler.estimated_memory_bytes() > 0);
        }
    }
}

#[test]
fn long_filter_downsample_preserves_dc_unity_gain() {
    let input = vec![1.0; 96_000];
    for filter in [
        FilterType::Split128k,
        FilterType::Minimum16k,
        FilterType::IntegratedPhase128k,
    ] {
        let output = run_resampler_left(filter, 96_000, 48_000, &input, &input, 1024, 96_000);
        assert!(
            output.len() > 32_768,
            "expected settled output for {filter:?}"
        );
        let settled = &output[16_384..32_768];
        for (idx, sample) in settled.iter().enumerate() {
            assert!(
                (*sample - 1.0).abs() < 1e-4,
                "{filter:?} DC sample {idx} drifted: {sample}"
            );
        }
    }
}

#[test]
fn split16k_cross_family_downsample_rejects_above_target_nyquist() {
    let source_rate = 96_000u32;
    let target_rate = 44_100u32;
    let freq_hz = 23_000.0;
    let frames = 96_000usize;
    let input: Vec<f64> = (0..frames)
        .map(|i| (2.0 * PI * freq_hz * i as f64 / source_rate as f64).sin())
        .collect();
    let output = run_resampler_left(
        FilterType::Split128k,
        source_rate,
        target_rate,
        &input,
        &input,
        1024,
        96_000,
    );
    assert!(output.len() > 16_384, "expected settled output");

    let settled_start = 16_384;
    let settled_end = 32_768.min(output.len());
    let settled = &output[settled_start..settled_end];
    let peak = settled.iter().fold(0.0_f64, |acc, &s| acc.max(s.abs()));
    assert!(
        peak < 0.01,
        "23 kHz tone leaked through 96k->44.1k Split16k downsample at peak {peak}"
    );
}

#[test]
fn split16k_cross_family_downsample_stays_front_loaded() {
    let response = run_impulse_response(FilterType::Split128k, 96_000, 44_100);
    assert!(!response.is_empty());
    let (first_idx, peak_idx, pre_peak_samples) = impulse_peak_metrics(&response);
    let limit_samples = (44_100usize * 25) / 10_000;
    assert!(
        pre_peak_samples <= limit_samples,
        "96k->44.1k Split16k downsample pre-ringing peak window was {pre_peak_samples} samples ({:.3} ms), first={first_idx}, peak={peak_idx}, limit={limit_samples}",
        pre_peak_samples as f64 / 44_100.0 * 1000.0
    );
}

#[test]
fn split16k_downsample_is_consistent_across_blocks_and_reset() {
    let frames = 24_000usize;
    let (left, right) = make_signal(frames);
    let one_block = run_resampler_left(
        FilterType::Split128k,
        96_000,
        48_000,
        &left,
        &right,
        frames,
        96_000,
    );
    let chunked = run_resampler_left(
        FilterType::Split128k,
        96_000,
        48_000,
        &left,
        &right,
        257,
        96_000,
    );

    assert_eq!(one_block.len(), chunked.len());
    for idx in 0..one_block.len() {
        assert!(
            (one_block[idx] - chunked[idx]).abs() < 2e-5,
            "block split changed sample {idx}: one_block={}, chunked={}",
            one_block[idx],
            chunked[idx]
        );
    }

    let mut resampler = SincResampler::new(FilterType::Split128k, 96_000, 48_000);
    let mut first = Vec::new();
    resampler.input(&left, &right);
    resampler.process(&mut first);
    resampler.reset();
    let mut second = Vec::new();
    resampler.input(&left, &right);
    resampler.process(&mut second);
    assert_eq!(first.len(), second.len());
    for idx in 0..first.len() {
        assert!(
            (first[idx] - second[idx]).abs() < 2e-5,
            "reset changed sample {idx}: first={}, second={}",
            first[idx],
            second[idx]
        );
    }
}

#[test]
fn split16k_is_chunk_boundary_invariant_for_pop_sensitive_rates() {
    assert_long_filter_chunk_case(
        FilterType::Split128k,
        "Split16k",
        44_100,
        176_400,
        &[4095, 4096, 4097],
        1024,
        8192,
    );
}

#[test]
fn integrated_phase_is_chunk_boundary_invariant_for_pop_sensitive_rates() {
    for (filter, label) in [
        (FilterType::IntegratedPhase128k, "IntegratedPhase128k"),
        (FilterType::IntegratedPhase128kV4, "IntegratedPhase128kV4"),
    ] {
        assert_long_filter_chunk_case(
            filter,
            label,
            44_100,
            176_400,
            &[4095, 4096, 4097],
            1024,
            8192,
        );
    }
}

#[test]
fn frozen_split_phase_filters_are_chunk_boundary_invariant_and_reset_eof_stable() {
    for (filter, label) in [
        (FilterType::SplitPhase128kV3, "SplitPhase128kV3"),
        (FilterType::SplitPhase128kV4, "SplitPhase128kV4"),
        (FilterType::SplitPhase128kE2v3, "SplitPhase128kE2v3"),
    ] {
        assert_long_filter_chunk_case(
            filter,
            label,
            44_100,
            176_400,
            &[1, 2, 3, 127, 255, 256, 257, 4095, 4096, 4097],
            1024,
            8192,
        );

        let (left, right) = make_signal(2_048);
        let mut resampler = SincResampler::new(filter, 44_100, 88_200);
        let mut first = Vec::new();
        resampler.input(&left, &right);
        resampler.process(&mut first);
        resampler.drain_eof(&mut first);
        assert_eq!(
            first.len(),
            nominal_output_samples(left.len(), 44_100, 88_200)
        );
        assert!(first.iter().all(|sample| sample.is_finite()));

        resampler.reset();
        let mut second = Vec::new();
        resampler.input(&left, &right);
        resampler.process(&mut second);
        resampler.drain_eof(&mut second);
        assert_eq!(first.len(), second.len());
        assert!(
            first
                .iter()
                .zip(&second)
                .all(|(left, right)| left.to_bits() == right.to_bits())
        );
        let mut already_drained = Vec::new();
        assert_eq!(resampler.drain_eof(&mut already_drained), 0);
        assert!(already_drained.is_empty());
    }
}

#[test]
fn minimum_phase128k_endpoints_are_chunk_boundary_invariant() {
    for (filter, label) in [
        (FilterType::MinimumPhase128k, "MinimumPhase128k1"),
        (FilterType::MinimumPhase128kV4, "MinimumPhase128k4"),
        (
            FilterType::MinimumPhaseCompact128k,
            "MinimumPhaseCompact128k",
        ),
        (
            FilterType::MinimumPhaseCompact128kV2,
            "MinimumPhaseCompact128kV2",
        ),
        (FilterType::SmoothPhase128k, "SmoothPhase128k"),
    ] {
        assert_long_filter_chunk_case(
            filter,
            label,
            44_100,
            176_400,
            &[4095, 4096, 4097],
            1024,
            8192,
        );
    }
}

#[test]
#[ignore = "broader pop-sensitive Split16k sweep for artifact investigations"]
fn split16k_exhaustive_chunk_boundary_sweep_for_pop_sensitive_rates() {
    let chunk_sizes = [1usize, 255, 256, 257, 4095, 4096, 4097];
    for (source_rate, target_rate) in [
        (44_100, 176_400),
        (44_100, 352_800),
        (48_000, 192_000),
        (48_000, 384_000),
        (96_000, 44_100),
        (192_000, 44_100),
    ] {
        assert_long_filter_chunk_case(
            FilterType::Split128k,
            "Split16k",
            source_rate,
            target_rate,
            &chunk_sizes,
            8192,
            16_384,
        );
    }
}

#[test]
fn split16k_two_x_stage_matches_slow_branch_reference_for_offset_impulses() {
    let taps_total = 513usize;
    let half_width = taps_total / 2;
    let spec = StageSpec::Character2x {
        taps_total,
        cutoff: FilterType::Split128k.cutoff(),
        beta: FilterType::Split128k.beta(),
        engine: EngineKind::DirectSimd,
        phase_mode: PhaseMode::SplitPhase128k,
        coefficient_source: CharacterCoefficientSource::Procedural,
    };
    let (phase0, phase1, prepad0, prepad1) = build_character_polyphase_pair(
        half_width,
        FilterType::Split128k.beta(),
        FilterType::Split128k.cutoff(),
        PhaseMode::SplitPhase128k,
    );

    for impulse_offset in [0usize, 1, 7, 64, 257, 513] {
        let mut input = vec![0.0_f64; impulse_offset + taps_total * 3];
        input[impulse_offset] = 1.0;

        let mut stage = TwoXStage::new(&spec);
        let mut actual_l = Vec::new();
        let mut actual_r = Vec::new();
        for start in (0..input.len()).step_by(73) {
            let end = (start + 73).min(input.len());
            stage.input(&input[start..end], &input[start..end]);
            stage.process(&mut actual_l, &mut actual_r);
        }

        let phase0_ref =
            direct_fir_reference(&input, &phase0, prepad0.expect("Split16k phase0 prepad"));
        let phase1_ref =
            direct_fir_reference(&input, &phase1, prepad1.expect("Split16k phase1 prepad"));
        let frames = phase0_ref.len().min(phase1_ref.len());
        let mut reference = Vec::with_capacity(frames * 2);
        for idx in 0..frames {
            reference.push(phase0_ref[idx]);
            reference.push(phase1_ref[idx]);
        }

        assert_eq!(
            actual_l.len(),
            reference.len(),
            "offset {impulse_offset} output length mismatch"
        );
        for (idx, (expected, actual)) in reference.iter().zip(&actual_l).enumerate() {
            assert!(
                (expected - actual).abs() < 1e-12,
                "offset {impulse_offset} sample={idx}: expected={expected} actual={actual}"
            );
        }
        assert_eq!(actual_l, actual_r, "right channel should mirror left");
    }
}

#[test]
fn integrated_phase_two_x_stage_matches_slow_branch_reference_for_offset_impulses() {
    let taps_total = 511usize;
    let half_width = taps_total / 2;
    for (filter, profile) in [
        (FilterType::IntegratedPhase128k, IntegratedPhaseProfile::One),
        (
            FilterType::IntegratedPhase128kV2,
            IntegratedPhaseProfile::Two,
        ),
        (
            FilterType::IntegratedPhase128kV3,
            IntegratedPhaseProfile::Three,
        ),
        (
            FilterType::IntegratedPhase128kV4,
            IntegratedPhaseProfile::Four,
        ),
    ] {
        let phase_mode = PhaseMode::IntegratedPhase128k(profile);
        let spec = StageSpec::Character2x {
            taps_total,
            cutoff: filter.cutoff(),
            beta: filter.beta(),
            engine: EngineKind::DirectSimd,
            phase_mode,
            coefficient_source: CharacterCoefficientSource::Procedural,
        };
        let (phase0, phase1, prepad0, prepad1) =
            build_character_polyphase_pair(half_width, filter.beta(), filter.cutoff(), phase_mode);

        for impulse_offset in [0usize, 1, 7, 64, 257, 511] {
            let mut input = vec![0.0_f64; impulse_offset + taps_total * 3];
            input[impulse_offset] = 1.0;
            let mut stage = TwoXStage::new(&spec);
            let mut actual_l = Vec::new();
            let mut actual_r = Vec::new();
            for start in (0..input.len()).step_by(73) {
                let end = (start + 73).min(input.len());
                stage.input(&input[start..end], &input[start..end]);
                stage.process(&mut actual_l, &mut actual_r);
            }

            let phase0_ref = direct_fir_reference(
                &input,
                &phase0,
                prepad0.expect("Integrated Phase phase0 prepad"),
            );
            let phase1_ref = direct_fir_reference(
                &input,
                &phase1,
                prepad1.expect("Integrated Phase phase1 prepad"),
            );
            let frames = phase0_ref.len().min(phase1_ref.len());
            let mut reference = Vec::with_capacity(frames * 2);
            for idx in 0..frames {
                reference.push(phase0_ref[idx]);
                reference.push(phase1_ref[idx]);
            }

            assert_eq!(actual_l.len(), reference.len());
            for (idx, (expected, actual)) in reference.iter().zip(&actual_l).enumerate() {
                assert!(
                    (expected - actual).abs() < 1e-12,
                    "{filter:?} offset {impulse_offset} sample={idx}: expected={expected} actual={actual}"
                );
            }
            assert_eq!(actual_l, actual_r);
        }
    }
}

#[test]
fn bragging_rights_filters_are_high_latency_fft_modes() {
    let cases = [
        (
            FilterType::SincExtreme32k,
            32_769,
            2048,
            0.454,
            19.5,
            PhaseMode::Linear,
        ),
        (
            FilterType::Minimum16k,
            16_385,
            4096,
            0.467621,
            20.47325,
            PhaseMode::Minimum,
        ),
        (
            FilterType::Split128k,
            131_073,
            4096,
            0.465333,
            23.12088,
            PhaseMode::SplitPhase128k,
        ),
    ];

    for (filter, taps_total, partition_frames, cutoff, beta, phase_mode) in cases {
        let plan = build_integer_stage_plan(44_100, 176_400, filter, 100.0).unwrap();
        assert!(plan.high_latency, "{filter:?} should be high-latency");
        assert!(matches!(
            plan.stages[0],
            StageSpec::Character2x {
                taps_total: actual_taps,
                engine: EngineKind::PartitionedFft {
                    partition_frames: actual_partition
                },
                cutoff: actual_cutoff,
                beta: actual_beta,
                phase_mode: actual_phase_mode,
                ..
            } if actual_taps == taps_total
                && actual_partition == partition_frames
                && actual_cutoff == cutoff
                && actual_beta == beta
                && actual_phase_mode == phase_mode
        ));
    }
}

#[test]
fn long_modes_do_not_allocate_polyphase_sized_tables() {
    let resampler = SincResampler::new(FilterType::SincExtreme32k, 44_100, 352_800);
    assert!(resampler.is_high_latency());
    assert!(
        resampler.estimated_memory_bytes() < MAX_COEFFICIENT_TABLE_BYTES,
        "estimated memory was {}",
        resampler.estimated_memory_bytes()
    );
}

#[test]
fn direct_and_fft_fir_match_for_small_filters() {
    let coeffs = build_phase_coefficients(16, 0.5, FilterType::SincExtreme32k.beta(), 0.48);
    let (left, right) = make_signal(512);
    let mut direct = DirectFirEngine::with_prepad(coeffs.clone(), false, None);
    let mut fft = BlockFftFirEngine::with_prepad(coeffs, 16, None);
    let mut direct_l = Vec::new();
    let mut direct_r = Vec::new();
    let mut fft_l = Vec::new();
    let mut fft_r = Vec::new();

    direct.process_stereo(&left, &right, &mut direct_l, &mut direct_r);
    for start in (0..left.len()).step_by(37) {
        let end = (start + 37).min(left.len());
        fft.process_stereo(
            &left[start..end],
            &right[start..end],
            &mut fft_l,
            &mut fft_r,
        );
    }

    assert!(!fft_l.is_empty());
    for idx in 0..fft_l.len() {
        assert!(
            (direct_l[idx] - fft_l[idx]).abs() < 2e-5,
            "left mismatch at {idx}: direct={} fft={}",
            direct_l[idx],
            fft_l[idx]
        );
        assert!(
            (direct_r[idx] - fft_r[idx]).abs() < 2e-5,
            "right mismatch at {idx}: direct={} fft={}",
            direct_r[idx],
            fft_r[idx]
        );
    }
}

#[test]
fn simd_fir_matches_scalar_reference() {
    let coeffs = build_phase_coefficients(
        64,
        0.5,
        FilterType::SincExtreme32k.beta(),
        FilterType::SincExtreme32k.cutoff(),
    );
    let (left, right) = make_signal(1024);
    let mut scalar = DirectFirEngine::with_prepad(coeffs.clone(), false, None);
    let mut simd = DirectFirEngine::with_prepad(coeffs, true, None);
    let mut scalar_l = Vec::new();
    let mut scalar_r = Vec::new();
    let mut simd_l = Vec::new();
    let mut simd_r = Vec::new();

    scalar.process_stereo(&left, &right, &mut scalar_l, &mut scalar_r);
    simd.process_stereo(&left, &right, &mut simd_l, &mut simd_r);

    assert_eq!(scalar_l.len(), simd_l.len());
    for idx in 0..scalar_l.len() {
        assert!(
            (scalar_l[idx] - simd_l[idx]).abs() < 2e-6,
            "left mismatch at {idx}: scalar={} simd={}",
            scalar_l[idx],
            simd_l[idx]
        );
        assert!(
            (scalar_r[idx] - simd_r[idx]).abs() < 2e-6,
            "right mismatch at {idx}: scalar={} simd={}",
            scalar_r[idx],
            simd_r[idx]
        );
    }
}

#[test]
fn simd_convolution_handles_tail_taps() {
    let samples: Vec<f64> = (0..37)
        .map(|idx| (idx as f64 * 0.13).sin() * 0.75)
        .collect();
    let coeffs: Vec<f64> = (0..37)
        .map(|idx| (idx as f64 * 0.07).cos() * 0.125)
        .collect();

    let scalar = DirectFirEngine::convolve_scalar(&samples, &coeffs);
    let simd = convolve_simd_or_scalar(&samples, &coeffs);

    assert!(
        (scalar - simd).abs() < 1e-12,
        "tail convolution mismatch: scalar={scalar} simd={simd}"
    );
}

#[test]
fn character_stage_does_not_accumulate_even_delay_history() {
    let spec = first_stage_spec(FilterType::SincExtreme32k);
    let mut stage = TwoXStage::new(&spec);
    let (left, right) = make_signal(2048);

    for start in (0..left.len()).step_by(128) {
        let end = (start + 128).min(left.len());
        stage.input(&left[start..end], &right[start..end]);
        let mut out_l = Vec::new();
        let mut out_r = Vec::new();
        stage.process(&mut out_l, &mut out_r);
    }

    assert_eq!(stage.even_delay_l.len(), 0);
    assert_eq!(stage.even_delay_r.len(), 0);
}

#[test]
fn fractional_resampler_preserves_dc() {
    let mut resampler = SincResampler::new(FilterType::SincExtreme32k, 44_100, 48_000);
    let input = vec![1.0; 4096];
    let mut output = Vec::new();
    resampler.input(&input, &input);
    let frames = resampler.process(&mut output);

    assert!(frames > 0);
    for sample in output.iter().skip(256).take(1024) {
        assert!((*sample - 1.0).abs() < 1e-3, "DC sample drifted: {sample}");
    }
}

#[test]
fn fractional_resampler_is_consistent_across_blocks() {
    let (left, right) = make_signal(4096);
    let mut one_block = SincResampler::new(FilterType::SincExtreme32k, 44_100, 48_000);
    let mut one_block_output = Vec::new();
    one_block.input(&left, &right);
    one_block.process(&mut one_block_output);

    let mut chunked = SincResampler::new(FilterType::SincExtreme32k, 44_100, 48_000);
    let mut chunked_output = Vec::new();
    for start in (0..left.len()).step_by(113) {
        let end = (start + 113).min(left.len());
        chunked.input(&left[start..end], &right[start..end]);
        chunked.process(&mut chunked_output);
    }

    assert_eq!(one_block_output.len(), chunked_output.len());
    for idx in 0..one_block_output.len() {
        assert!(
            (one_block_output[idx] - chunked_output[idx]).abs() < 1e-5,
            "block split changed sample {idx}: one_block={}, chunked={}",
            one_block_output[idx],
            chunked_output[idx]
        );
    }
}

#[test]
fn exact_rational_and_split_phase_v4_are_consistent_across_chunk_sizes() {
    let (left, right) = make_signal(8192);
    for filter in [FilterType::SincExtreme32k, FilterType::SplitPhase128kV4] {
        let reference =
            run_resampler_interleaved(filter, 44_100, 48_000, &left, &right, left.len(), 4096);
        assert!(!reference.is_empty());

        for chunk_frames in [1usize, 2, 3, 127, 255, 256, 257, 997, 4095, 4096, 4097] {
            let chunked = run_resampler_interleaved(
                filter,
                44_100,
                48_000,
                &left,
                &right,
                chunk_frames,
                4096,
            );
            assert_eq!(
                reference.len(),
                chunked.len(),
                "filter={filter:?} chunk={chunk_frames} changed output length"
            );
            for (idx, (expected, actual)) in reference.iter().zip(&chunked).enumerate() {
                assert!(
                    (expected - actual).abs() < 1e-10,
                    "filter={filter:?} chunk={chunk_frames} sample={idx}: expected={expected} actual={actual}"
                );
            }
        }
    }
}

#[test]
fn drain_eof_matches_manual_zero_flush_for_exact_rational_and_split_phase_v4() {
    let source_rate = 44_100;
    let target_rate = 48_000;
    let frames = 4096usize;
    let (left, right) = make_signal(frames);
    for filter in [FilterType::SincExtreme32k, FilterType::SplitPhase128kV4] {
        let actual = run_resampler_interleaved_with_eof_drain(
            filter,
            source_rate,
            target_rate,
            &left,
            &right,
            997,
        );
        let mut expected =
            run_resampler_interleaved(filter, source_rate, target_rate, &left, &right, 997, 4096);
        expected.truncate(nominal_output_samples(frames, source_rate, target_rate));

        assert_eq!(actual.len(), expected.len());
        for (idx, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
            let err = (expected - actual).abs();
            assert!(
                err < 1e-9,
                "filter={filter:?} exact rational EOF drain sample {idx} mismatch: expected={expected} actual={actual} err={err}"
            );
        }
    }
}

#[test]
fn drain_eof_matches_manual_zero_flush_for_split16k_integer_cascade() {
    let source_rate = 44_100;
    let target_rate = 176_400;
    let frames = 2048usize;
    let (left, right) = make_pop_sensitive_signal(frames, source_rate);
    let actual = run_resampler_interleaved_with_eof_drain(
        FilterType::Split128k,
        source_rate,
        target_rate,
        &left,
        &right,
        257,
    );
    let mut expected = run_resampler_interleaved(
        FilterType::Split128k,
        source_rate,
        target_rate,
        &left,
        &right,
        257,
        8192,
    );
    expected.truncate(nominal_output_samples(frames, source_rate, target_rate));

    assert_eq!(actual.len(), expected.len());
    assert_no_pop("Split16k EOF drain", &actual, 2, 0.75);
    for (idx, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
        let err = (expected - actual).abs();
        assert!(
            err < 5e-5,
            "Split16k EOF drain sample {idx} mismatch: expected={expected} actual={actual} err={err}"
        );
    }
}

#[test]
fn drain_eof_is_idempotent_and_reset_restores_reuse() {
    let source_rate = 44_100;
    let target_rate = 48_000;
    let frames = 1500usize;
    let (left, right) = make_signal(frames);
    let mut resampler = SincResampler::new(FilterType::SincExtreme32k, source_rate, target_rate);
    let mut output = Vec::new();
    let mut tail = Vec::new();
    resampler.input(&left, &right);
    resampler.process(&mut output);
    let first_tail_frames = resampler.drain_eof(&mut tail);
    assert!(first_tail_frames > 0);
    assert_eq!(
        output.len() + tail.len(),
        nominal_output_samples(frames, source_rate, target_rate)
    );

    let mut second_tail = Vec::new();
    assert_eq!(resampler.drain_eof(&mut second_tail), 0);
    assert!(second_tail.is_empty());

    resampler.reset();
    output.clear();
    tail.clear();
    resampler.input(&left, &right);
    resampler.process(&mut output);
    let reused_tail_frames = resampler.drain_eof(&mut tail);
    assert_eq!(first_tail_frames, reused_tail_frames);
    assert_eq!(
        output.len() + tail.len(),
        nominal_output_samples(frames, source_rate, target_rate)
    );
}

#[test]
fn minimum_phase_compact_reset_and_eof_are_bit_stable() {
    let source_rate = 44_100;
    let target_rate = 88_200;
    let (left, right) = make_signal(2_048);
    let mut resampler = SincResampler::new(
        FilterType::MinimumPhaseCompact128k,
        source_rate,
        target_rate,
    );

    let mut first = Vec::new();
    resampler.input(&left, &right);
    resampler.process(&mut first);
    resampler.drain_eof(&mut first);

    resampler.reset();
    let mut second = Vec::new();
    resampler.input(&left, &right);
    resampler.process(&mut second);
    resampler.drain_eof(&mut second);

    assert_eq!(first, second);
    assert_eq!(
        first.len(),
        nominal_output_samples(left.len(), source_rate, target_rate)
    );
    let mut already_drained = Vec::new();
    assert_eq!(resampler.drain_eof(&mut already_drained), 0);
    assert!(already_drained.is_empty());
}

#[test]
fn exact_rational_and_split_phase_v4_long_duration_frame_count_drift_stays_bounded() {
    let source_rate = 44_100u32;
    let target_rate = 48_000u32;
    let input_frames = source_rate as usize * 60;
    let chunk_frames = 997usize;
    for filter in [FilterType::SincExtreme32k, FilterType::SplitPhase128kV4] {
        let mut resampler = SincResampler::new(filter, source_rate, target_rate);
        let ResamplerPath::Rational(rational) = &resampler.path else {
            panic!("expected exact rational path");
        };
        let (half_width, step_num, phase_den) =
            (rational.half_width, rational.step_num, rational.phase_den);

        let zeros = vec![0.0; chunk_frames];
        let mut block = Vec::new();
        let mut written = 0usize;
        let mut remaining = input_frames;
        while remaining > 0 {
            let frames = remaining.min(chunk_frames);
            resampler.input(&zeros[..frames], &zeros[..frames]);
            block.clear();
            written += resampler.process(&mut block);
            remaining -= frames;
        }

        let expected = expected_rational_output_frames_without_eof_tail(
            input_frames,
            half_width,
            step_num,
            phase_den,
        );
        let drift = written.abs_diff(expected);
        assert!(
            drift <= 1,
            "filter={filter:?} expected {expected} output frames over 60s, wrote {written}, drift={drift}"
        );
    }
}

#[test]
fn capped_fractional_fallback_is_bounded_and_marked() {
    let resampler = SincResampler::new(FilterType::Split128k, 44_100, 50_000);
    let info = resampler.runtime_info();

    assert_eq!(info.path_kind, ResamplerPathKind::CappedFractional);
    assert!(info.uses_capped_fallback);
    assert!(
        info.estimated_memory_bytes <= MAX_POLYPHASE_COEFFICIENT_TABLE_BYTES,
        "capped fallback memory was {}",
        info.estimated_memory_bytes
    );
}

#[test]
fn exact_160_147_bridge_is_consistent_across_blocks() {
    let (left, right) = make_signal(8192);
    let mut one_block = SincResampler::new_exact_160_147_without_capped_polyphase_warning(
        FilterType::SincExtreme32k,
        192_000,
        176_400,
    );
    let mut one_block_output = Vec::new();
    one_block.input(&left, &right);
    one_block.process(&mut one_block_output);

    let mut chunked = SincResampler::new_exact_160_147_without_capped_polyphase_warning(
        FilterType::SincExtreme32k,
        192_000,
        176_400,
    );
    let mut chunked_output = Vec::new();
    for start in (0..left.len()).step_by(197) {
        let end = (start + 197).min(left.len());
        chunked.input(&left[start..end], &right[start..end]);
        chunked.process(&mut chunked_output);
    }

    assert_eq!(one_block_output.len(), chunked_output.len());
    for idx in 0..one_block_output.len() {
        assert!(
            (one_block_output[idx] - chunked_output[idx]).abs() < 1e-10,
            "block split changed sample {idx}: one_block={}, chunked={}",
            one_block_output[idx],
            chunked_output[idx]
        );
    }
}

#[test]
fn exact_160_147_bridge_keeps_split_phase_table_small() {
    let resampler = SincResampler::new_exact_160_147_without_capped_polyphase_warning(
        FilterType::Split128k,
        192_000,
        176_400,
    );

    assert!(
        resampler.estimated_memory_bytes() < 4 * 1024 * 1024,
        "exact 160/147 bridge should use the compact 147-phase table"
    );
}

#[test]
fn integer_cascade_returns_frame_count_not_sample_count() {
    let mut resampler = SincResampler::new(FilterType::SincExtreme32k, 44_100, 176_400);
    let (left, right) = make_signal(2048);
    let mut output = Vec::new();
    resampler.input(&left, &right);
    let frames = resampler.process(&mut output);
    assert_eq!(output.len(), frames * 2);
}

#[test]
fn full_cascade_fft_first_stage_matches_direct_first_stage() {
    let (left, right) = make_signal(8192);
    let fft_plan = build_integer_stage_plan(44_100, 176_400, FilterType::Split128k, 100.0).unwrap();
    let mut direct_plan = fft_plan.clone();
    if let StageSpec::Character2x { engine, .. } = &mut direct_plan.stages[0] {
        *engine = EngineKind::DirectSimd;
    }

    let mut fft_cascade = IntegerCascade::new(fft_plan);
    let mut direct_cascade = IntegerCascade::new(direct_plan);
    let mut fft_out = Vec::new();
    let mut direct_out = Vec::new();

    for start in (0..left.len()).step_by(157) {
        let end = (start + 157).min(left.len());
        fft_cascade.input(&left[start..end], &right[start..end]);
        fft_cascade.process(&mut fft_out);
        direct_cascade.input(&left[start..end], &right[start..end]);
        direct_cascade.process(&mut direct_out);
    }

    assert!(!fft_out.is_empty());
    assert!(direct_out.len() >= fft_out.len());
    for idx in 0..fft_out.len() {
        assert!(
            (direct_out[idx] - fft_out[idx]).abs() < 4e-5,
            "cascade mismatch at {idx}: direct={} fft={}",
            direct_out[idx],
            fft_out[idx]
        );
    }
}

fn coefficient_magnitude_db(coeffs: &[f64], frequency: f64) -> f64 {
    let mut re = 0.0;
    let mut im = 0.0;
    for (idx, c) in coeffs.iter().enumerate() {
        let phase = -2.0 * PI * frequency * idx as f64;
        re += c * phase.cos();
        im += c * phase.sin();
    }
    let mag = (re * re + im * im).sqrt().max(1e-18);
    20.0 * mag.log10()
}

fn run_impulse_response(filter: FilterType, source_rate: u32, target_rate: u32) -> Vec<f64> {
    let frames = 32_768usize;
    let mut left = vec![0.0; frames];
    let right = vec![0.0; frames];
    left[0] = 1.0;
    let mut resampler = SincResampler::new(filter, source_rate, target_rate);
    let mut interleaved = Vec::new();
    for start in (0..frames).step_by(1024) {
        let end = (start + 1024).min(frames);
        resampler.input(&left[start..end], &right[start..end]);
        resampler.process(&mut interleaved);
    }
    let flush = vec![0.0; frames];
    for start in (0..frames).step_by(1024) {
        let end = (start + 1024).min(frames);
        resampler.input(&flush[start..end], &flush[start..end]);
        resampler.process(&mut interleaved);
    }

    interleaved.chunks_exact(2).map(|frame| frame[0]).collect()
}

fn run_resampler_left(
    filter: FilterType,
    source_rate: u32,
    target_rate: u32,
    left: &[f64],
    right: &[f64],
    chunk_frames: usize,
    flush_frames: usize,
) -> Vec<f64> {
    let mut resampler = SincResampler::new(filter, source_rate, target_rate);
    let mut interleaved = Vec::new();
    let mut block = Vec::new();

    for start in (0..left.len()).step_by(chunk_frames) {
        let end = (start + chunk_frames).min(left.len());
        resampler.input(&left[start..end], &right[start..end]);
        block.clear();
        resampler.process(&mut block);
        interleaved.extend_from_slice(&block);
    }

    if flush_frames > 0 {
        let zeros = vec![0.0; flush_frames];
        for start in (0..zeros.len()).step_by(chunk_frames) {
            let end = (start + chunk_frames).min(zeros.len());
            resampler.input(&zeros[start..end], &zeros[start..end]);
            block.clear();
            resampler.process(&mut block);
            interleaved.extend_from_slice(&block);
        }
    }

    interleaved.chunks_exact(2).map(|frame| frame[0]).collect()
}

fn run_resampler_interleaved(
    filter: FilterType,
    source_rate: u32,
    target_rate: u32,
    left: &[f64],
    right: &[f64],
    chunk_frames: usize,
    flush_frames: usize,
) -> Vec<f64> {
    let mut resampler = SincResampler::new(filter, source_rate, target_rate);
    let mut interleaved = Vec::new();
    let mut block = Vec::new();

    for start in (0..left.len()).step_by(chunk_frames) {
        let end = (start + chunk_frames).min(left.len());
        resampler.input(&left[start..end], &right[start..end]);
        block.clear();
        resampler.process(&mut block);
        interleaved.extend_from_slice(&block);
    }

    if flush_frames > 0 {
        let zeros = vec![0.0; flush_frames];
        let flush_chunk_frames = chunk_frames.max(1024);
        for start in (0..zeros.len()).step_by(flush_chunk_frames) {
            let end = (start + flush_chunk_frames).min(zeros.len());
            resampler.input(&zeros[start..end], &zeros[start..end]);
            block.clear();
            resampler.process(&mut block);
            interleaved.extend_from_slice(&block);
        }
    }

    interleaved
}

fn run_resampler_interleaved_with_eof_drain(
    filter: FilterType,
    source_rate: u32,
    target_rate: u32,
    left: &[f64],
    right: &[f64],
    chunk_frames: usize,
) -> Vec<f64> {
    let mut resampler = SincResampler::new(filter, source_rate, target_rate);
    let mut interleaved = Vec::new();
    let mut block = Vec::new();

    for start in (0..left.len()).step_by(chunk_frames) {
        let end = (start + chunk_frames).min(left.len());
        resampler.input(&left[start..end], &right[start..end]);
        block.clear();
        resampler.process(&mut block);
        interleaved.extend_from_slice(&block);
    }

    block.clear();
    resampler.drain_eof(&mut block);
    interleaved.extend_from_slice(&block);
    interleaved
}

fn nominal_output_samples(input_frames: usize, source_rate: u32, target_rate: u32) -> usize {
    ceil_mul_div_usize(input_frames, target_rate as usize, source_rate as usize) * 2
}

fn expected_rational_output_frames_without_eof_tail(
    input_frames: usize,
    half_width: usize,
    step_num: usize,
    phase_den: usize,
) -> usize {
    let input_len = half_width + input_frames;
    let mut current_time_num = half_width * phase_den;
    let mut frames = 0usize;
    while current_time_num / phase_den + half_width < input_len {
        frames += 1;
        current_time_num += step_num;
    }
    frames
}

fn direct_fir_reference(input: &[f64], coeffs: &[f64], prepad: usize) -> Vec<f64> {
    let taps = coeffs.len();
    let mut buffer = vec![0.0_f64; prepad];
    buffer.extend_from_slice(input);
    if buffer.len() <= taps.saturating_sub(1) {
        return Vec::new();
    }

    let frames = buffer.len() - (taps - 1);
    let mut output = Vec::with_capacity(frames);
    for start in 0..frames {
        output.push(
            buffer[start..start + taps]
                .iter()
                .zip(coeffs)
                .map(|(sample, coeff)| sample * coeff)
                .sum(),
        );
    }
    output
}

fn assert_long_filter_chunk_case(
    filter: FilterType,
    label: &str,
    source_rate: u32,
    target_rate: u32,
    chunk_sizes: &[usize],
    frames: usize,
    flush_frames: usize,
) {
    let (left, right) = make_pop_sensitive_signal(frames, source_rate);
    let reference = run_resampler_interleaved(
        filter,
        source_rate,
        target_rate,
        &left,
        &right,
        frames,
        flush_frames,
    );
    assert!(
        !reference.is_empty(),
        "{label} {source_rate}->{target_rate} reference produced no output"
    );
    assert_no_pop(
        &format!("{label} {source_rate}->{target_rate} reference"),
        &reference,
        2,
        0.75,
    );

    for &chunk in chunk_sizes {
        let chunked = run_resampler_interleaved(
            filter,
            source_rate,
            target_rate,
            &left,
            &right,
            chunk,
            flush_frames,
        );
        assert_eq!(
            reference.len(),
            chunked.len(),
            "{label} {source_rate}->{target_rate} chunk={chunk} changed output length"
        );
        for (idx, (expected, actual)) in reference.iter().zip(&chunked).enumerate() {
            let err = (expected - actual).abs();
            assert!(
                err < 5e-5,
                "{label} {source_rate}->{target_rate} chunk={chunk} sample={idx} err={err} expected={expected} actual={actual}"
            );
        }
        assert_no_pop(
            &format!("{label} {source_rate}->{target_rate} chunk={chunk}"),
            &chunked,
            2,
            0.75,
        );
    }
}

fn impulse_peak_metrics(response: &[f64]) -> (usize, usize, usize) {
    let (peak_idx, peak) = response
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .map(|(idx, sample)| (idx, sample.abs()))
        .unwrap();
    let threshold = peak * 1e-6;
    let first_significant = response
        .iter()
        .position(|sample| sample.abs() >= threshold)
        .unwrap_or(peak_idx);
    (first_significant, peak_idx, peak_idx - first_significant)
}

fn full_rate_magnitude_db(samples: &[f64], frequency: f64) -> f64 {
    let spectrum = real_spectrum(samples, (samples.len() * 16).next_power_of_two());
    let bin = (frequency * (spectrum.len() - 1) as f64 * 2.0).round() as usize;
    let bin = bin.min(spectrum.len().saturating_sub(1));
    20.0 * spectrum[bin].norm().max(1e-18).log10()
}

/// Group delay (in samples at the impulse's own rate) sampled across
/// [freq_lo, freq_hi] (normalized, 0..0.5), from the unwrapped phase.
fn group_delay_samples(impulse: &[f64], freq_lo: f64, freq_hi: f64) -> Vec<f64> {
    let fft_len = (impulse.len() * 16).next_power_of_two();
    let spectrum = real_spectrum(impulse, fft_len);
    let unwrapped = unwrap_spectrum_phase_with_floor(&spectrum, 0.0);
    let lo_bin = ((freq_lo * fft_len as f64).round() as usize).max(1);
    let hi_bin = ((freq_hi * fft_len as f64).round() as usize).min(unwrapped.len() - 2);
    (lo_bin..hi_bin)
        .map(|bin| -(unwrapped[bin + 1] - unwrapped[bin]) * fft_len as f64 / (2.0 * PI))
        .collect()
}

#[test]
fn minimum_phase_preserves_magnitude_response() {
    // Small Kaiser sinc — compare magnitudes at a handful of frequencies.
    let half_width = 64usize;
    let beta = 11.0;
    let cutoff = 0.45;
    let linear = build_phase_coefficients(half_width, 0.0, beta, cutoff);
    let mp = minimum_phase_impulse(&linear);

    for &freq in &[0.0_f64, 0.05, 0.15, 0.30, 0.42] {
        let lin_db = coefficient_magnitude_db(&linear, freq);
        let mp_db = coefficient_magnitude_db(&mp, freq);
        assert!(
            (lin_db - mp_db).abs() < 0.5,
            "magnitude mismatch at f={freq}: linear={lin_db} dB, min-phase={mp_db} dB"
        );
    }
}

#[test]
fn minimum_phase_impulse_is_front_loaded() {
    let half_width = 64usize;
    let linear = build_phase_coefficients(half_width, 0.0, 11.0, 0.45);
    let mp = minimum_phase_impulse(&linear);

    let total_energy: f64 = mp.iter().map(|x| x * x).sum();
    let quarter = mp.len() / 4;
    let head_energy: f64 = mp[..quarter].iter().map(|x| x * x).sum();
    // Linear-phase symmetric kernel has ~half its energy in the first
    // half (~25% in the first quarter is typical). A real min-phase
    // impulse should pack the majority of the energy into the front.
    assert!(
        head_energy / total_energy > 0.6,
        "min-phase head energy ratio {} should exceed 0.6 (front-loaded)",
        head_energy / total_energy
    );
}

#[test]
fn minimum_phase_polyphase_pair_preserves_dc() {
    let (phase0, phase1) = build_minimum_phase_polyphase_pair(32, 11.0, 0.45);
    let dc0: f64 = phase0.iter().sum();
    let dc1: f64 = phase1.iter().sum();
    assert!((dc0 - 1.0).abs() < 1e-9, "phase0 DC gain {}", dc0);
    assert!((dc1 - 1.0).abs() < 1e-9, "phase1 DC gain {}", dc1);
}

#[test]
fn fractional_phase_rows_use_fixed_window_support() {
    let coeffs = build_phase_coefficients(64, 0.5, 11.0, 0.45);
    assert_eq!(
        coeffs[0], 0.0,
        "half-sample phases should not widen the Kaiser window"
    );
}

#[test]
fn phase_converted_impulses_fade_to_zero_at_truncation() {
    let proto = build_full_rate_2x_prototype(64, 19.5, 0.454);
    let minimum = minimum_phase_impulse(&proto);
    assert!(
        minimum.last().unwrap().abs() < 1e-18,
        "minimum-phase tail should fade to zero"
    );
}

/// Regression: the FIR engines convolve oldest-sample × coeffs[0],
/// so a causal min-phase impulse must be stored REVERSED in coeffs
/// for the engine to behave as a min-phase filter (otherwise it
/// becomes max-phase: pre-ringing piled before every transient,
/// audibly smooth-but-lifeless). Drive an impulse through the engine
/// and confirm the response peaks early, not late.
#[test]
fn minimum_phase_polyphase_runs_as_min_phase_not_max_phase() {
    let (phase0, _phase1) = build_minimum_phase_polyphase_pair(32, 11.0, 0.45);
    let taps = phase0.len();
    let mut engine = DirectFirEngine::with_prepad(phase0, false, None);

    // Send an impulse followed by enough zeros to flush the kernel.
    let mut input = vec![0.0_f64; taps * 2];
    input[0] = 1.0;
    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    engine.process_stereo(&input, &input, &mut out_l, &mut out_r);

    assert!(!out_l.is_empty(), "engine produced no output");
    let total_energy: f64 = out_l.iter().map(|x| x * x).sum();
    assert!(total_energy > 1e-12, "engine output was silent");

    // Locate the peak. A min-phase impulse should peak well within the
    // first half of the response; a max-phase mirror would peak in the
    // last quarter.
    let (peak_idx, _) = out_l
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap();
    assert!(
        peak_idx < out_l.len() / 2,
        "peak at index {} of {} — coeffs likely stored time-reversed (max-phase behavior)",
        peak_idx,
        out_l.len()
    );
}

#[test]
fn planner_marks_minimum16k_as_high_latency() {
    let plan = build_integer_stage_plan(44_100, 352_800, FilterType::Minimum16k, 100.0).unwrap();
    assert!(plan.high_latency);
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            phase_mode: PhaseMode::Minimum,
            ..
        }
    ));
}

#[test]
fn planner_configures_linear128k_as_long_partitioned_linear_phase() {
    let plan =
        build_integer_stage_plan(44_100, 44_100 * 32, FilterType::LinearPhase128k, 100.0).unwrap();
    assert!(plan.high_latency);
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            taps_total: 131_073,
            cutoff: 0.465333,
            beta: 23.12088,
            engine: EngineKind::PartitionedFft {
                partition_frames: 4096
            },
            phase_mode: PhaseMode::Linear,
            coefficient_source: CharacterCoefficientSource::Procedural,
        }
    ));

    let cleanup: Vec<(usize, f64)> = plan
        .stages
        .iter()
        .skip(1)
        .map(|stage| match stage {
            StageSpec::CleanupHalfband2x {
                taps_total, cutoff, ..
            } => (*taps_total, *cutoff),
            _ => panic!("expected cleanup stage"),
        })
        .collect();
    assert_eq!(cleanup, vec![(255, 0.5), (127, 0.5), (63, 0.5), (31, 0.5)]);
}

#[test]
fn linear128k_resampler_constructs_with_symmetric_character_kernel() {
    let resampler = SincResampler::new(FilterType::LinearPhase128k, 44_100, 352_800);
    assert!(resampler.is_high_latency());
    assert!(resampler.estimated_memory_bytes() > 0);

    let half_width = LINEAR128K_TAPS_TOTAL / 2;
    let coefficients = build_phase_coefficients(
        half_width,
        0.0,
        FilterType::LinearPhase128k.beta(),
        FilterType::LinearPhase128k.cutoff(),
    );
    assert_eq!(coefficients.len(), LINEAR128K_TAPS_TOTAL);
    for index in 0..coefficients.len() / 2 {
        assert_eq!(
            coefficients[index],
            coefficients[coefficients.len() - 1 - index]
        );
    }
}

#[test]
fn planner_configures_minimum16k_as_apodizing_min_phase_fft_mode() {
    let plan =
        build_integer_stage_plan(44_100, 44_100 * 32, FilterType::Minimum16k, 100.0).unwrap();
    assert!(plan.high_latency);
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            taps_total: 16_385,
            cutoff: 0.467621,
            beta: 20.47325,
            engine: EngineKind::PartitionedFft {
                partition_frames: 4096
            },
            phase_mode: PhaseMode::Minimum,
            coefficient_source: CharacterCoefficientSource::Procedural,
        }
    ));

    let cleanup: Vec<(usize, f64)> = plan
        .stages
        .iter()
        .skip(1)
        .map(|stage| match stage {
            StageSpec::CleanupHalfband2x {
                taps_total, cutoff, ..
            } => (*taps_total, *cutoff),
            _ => panic!("expected cleanup stage"),
        })
        .collect();
    assert_eq!(cleanup, vec![(255, 0.5), (127, 0.5), (63, 0.5), (31, 0.5)]);
}

#[test]
fn minimum16k_resampler_constructs_for_integer_ratio() {
    let resampler = SincResampler::new(FilterType::Minimum16k, 44_100, 352_800);
    assert!(resampler.is_high_latency());
    assert!(
        resampler.estimated_memory_bytes() < MAX_COEFFICIENT_TABLE_BYTES,
        "estimated memory was {}",
        resampler.estimated_memory_bytes()
    );
}

#[test]
fn split_phase_blend_weight_splits_bands_in_log_frequency() {
    assert_eq!(split_phase_blend_weight(0.0), 0.0);
    assert_eq!(split_phase_blend_weight(SPLIT_PHASE_BLEND_F_LO), 0.0);
    assert_eq!(split_phase_blend_weight(SPLIT_PHASE_BLEND_F_HI), 1.0);
    assert_eq!(split_phase_blend_weight(0.5), 1.0);

    let mut previous = 0.0;
    for step in 0..=200 {
        let freq = SPLIT_PHASE_BLEND_F_LO
            + (SPLIT_PHASE_BLEND_F_HI - SPLIT_PHASE_BLEND_F_LO) * step as f64 / 200.0;
        let weight = split_phase_blend_weight(freq);
        assert!(
            weight >= previous,
            "blend weight must be monotone: w({freq}) = {weight} < {previous}"
        );
        previous = weight;
    }

    // Smootherstep in LOG frequency: half weight at the geometric mean
    // of the split points, not the arithmetic mean.
    let log_mid = (SPLIT_PHASE_BLEND_F_LO.ln() + SPLIT_PHASE_BLEND_F_HI.ln()) / 2.0;
    assert!((split_phase_blend_weight(log_mid.exp()) - 0.5).abs() < 1e-12);

    let near_lo = SPLIT_PHASE_BLEND_F_LO * (1.0 + 1e-4);
    let near_hi = SPLIT_PHASE_BLEND_F_HI * (1.0 - 1e-4);
    assert!(
        split_phase_blend_weight(near_lo) < 1e-9,
        "smootherstep should leave the low split with a near-zero slope"
    );
    assert!(
        1.0 - split_phase_blend_weight(near_hi) < 1e-9,
        "smootherstep should enter the high split with a near-zero slope"
    );
}

#[test]
fn planner_configures_split128k_as_long_frequency_split_fft_mode() {
    let plan = build_integer_stage_plan(44_100, 44_100 * 32, FilterType::Split128k, 100.0).unwrap();
    assert!(plan.high_latency);
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            taps_total: 131_073,
            cutoff: 0.465333,
            beta: 23.12088,
            engine: EngineKind::PartitionedFft {
                partition_frames: 4096
            },
            phase_mode: PhaseMode::SplitPhase128k,
            coefficient_source: CharacterCoefficientSource::Procedural,
        }
    ));

    let cleanup: Vec<(usize, f64)> = plan
        .stages
        .iter()
        .skip(1)
        .map(|stage| match stage {
            StageSpec::CleanupHalfband2x {
                taps_total, cutoff, ..
            } => (*taps_total, *cutoff),
            _ => panic!("expected cleanup stage"),
        })
        .collect();
    assert_eq!(cleanup, vec![(255, 0.5), (127, 0.5), (63, 0.5), (31, 0.5)]);

    let dsd_plan =
        build_integer_stage_plan(44_100, 44_100 * 256, FilterType::Split128k, 1000.0).unwrap();
    assert_eq!(dsd_plan.stages.len(), 8);
    assert!(dsd_plan.high_latency);
    for stage in dsd_plan.stages.iter().skip(1) {
        assert!(stage.taps_total() <= MAX_SIMD_DIRECT_TAPS);
    }
}

#[test]
fn planner_routes_split_phase_v3_to_frozen_character_and_cleanup_sources() {
    for exponent in 1..=8 {
        let ratio = 1u32 << exponent;
        let plan = build_integer_stage_plan(
            44_100,
            44_100 * ratio,
            FilterType::SplitPhase128kV3,
            1_000.0,
        )
        .unwrap();
        assert!(plan.high_latency);
        assert!(matches!(
            plan.stages[0],
            StageSpec::Character2x {
                taps_total: 131_073,
                cutoff: 0.0,
                beta: 0.0,
                engine: EngineKind::PartitionedFft {
                    partition_frames: 4096
                },
                phase_mode: PhaseMode::FrozenSplitPhase(FrozenFilterVersion::V3),
                coefficient_source: CharacterCoefficientSource::Frozen(FrozenFilterVersion::V3),
            }
        ));
        assert_eq!(
            plan.stages[0].latency_frames_at_stage_rate(),
            SPLIT_PHASE_V3_FULL_RATE_ORIGIN / 2 + 4096
        );
        for (stage_index, stage) in plan.stages.iter().enumerate().skip(1) {
            assert!(matches!(
                stage,
                StageSpec::CleanupHalfband2x {
                    coefficient_source: CleanupCoefficientSource::Frozen {
                        version: FrozenFilterVersion::V3,
                        stage_index: asset_stage
                    },
                    ..
                } if *asset_stage == stage_index as u8
            ));
        }
    }
}

#[test]
fn planner_routes_split_phase_v4_to_its_frozen_bundle() {
    let plan = build_integer_stage_plan(44_100, 11_289_600, FilterType::SplitPhase128kV4, 1_000.0)
        .expect("Split Phase V4 256x plan");
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            phase_mode: PhaseMode::FrozenSplitPhase(FrozenFilterVersion::V4),
            coefficient_source: CharacterCoefficientSource::Frozen(FrozenFilterVersion::V4),
            ..
        }
    ));
    for (stage_index, stage) in plan.stages.iter().enumerate().skip(1) {
        assert!(matches!(
            stage,
            StageSpec::CleanupHalfband2x {
                coefficient_source: CleanupCoefficientSource::Frozen {
                    version: FrozenFilterVersion::V4,
                    stage_index: asset_stage,
                },
                ..
            } if *asset_stage == stage_index as u8
        ));
    }
}

#[test]
fn planner_routes_split_phase_e2v3_to_its_frozen_bundle() {
    let plan =
        build_integer_stage_plan(44_100, 11_289_600, FilterType::SplitPhase128kE2v3, 1_000.0)
            .expect("Split Phase E2v3 256x plan");
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            phase_mode: PhaseMode::FrozenSplitPhase(FrozenFilterVersion::E2v3),
            coefficient_source: CharacterCoefficientSource::Frozen(FrozenFilterVersion::E2v3),
            ..
        }
    ));
    for (stage_index, stage) in plan.stages.iter().enumerate().skip(1) {
        assert!(matches!(
            stage,
            StageSpec::CleanupHalfband2x {
                coefficient_source: CleanupCoefficientSource::Frozen {
                    version: FrozenFilterVersion::E2v3,
                    stage_index: asset_stage,
                },
                ..
            } if *asset_stage == stage_index as u8
        ));
    }
}

#[test]
fn planner_extends_split_phase_e2v3_to_512x_with_terminal_cleanup() {
    let plan =
        build_integer_stage_plan(44_100, 22_579_200, FilterType::SplitPhase128kE2v3, 1_000.0)
            .expect("Split Phase E2v3 512x plan");
    assert_eq!(plan.stages.len(), 9);
    assert!(matches!(
        plan.stages[8],
        StageSpec::CleanupHalfband2x {
            coefficient_source: CleanupCoefficientSource::Frozen {
                version: FrozenFilterVersion::E2v3,
                stage_index: 7,
            },
            ..
        }
    ));
}

#[test]
fn planner_routes_split_phase_b_to_its_frozen_bundle() {
    let plan = build_integer_stage_plan(44_100, 11_289_600, FilterType::SplitPhase128kE3, 1_000.0)
        .expect("Split Phase B 256x plan");
    assert!(matches!(
        plan.stages[0],
        StageSpec::Character2x {
            phase_mode: PhaseMode::FrozenSplitPhase(FrozenFilterVersion::E3),
            coefficient_source: CharacterCoefficientSource::Frozen(FrozenFilterVersion::E3),
            ..
        }
    ));
    for (stage_index, stage) in plan.stages.iter().enumerate().skip(1) {
        assert!(matches!(
            stage,
            StageSpec::CleanupHalfband2x {
                coefficient_source: CleanupCoefficientSource::Frozen {
                    version: FrozenFilterVersion::E3,
                    stage_index: asset_stage,
                },
                ..
            } if *asset_stage == stage_index as u8
        ));
    }
}

#[test]
fn split_phase_v4_generated_constants_match_embedded_assets() {
    let assets = split_phase_v4_assets();
    assert_eq!(
        assets.character.len(),
        SPLIT_PHASE_V4_CHARACTER_COEFFICIENTS
    );
    assert_eq!(
        assets.alignment.full_rate_origin,
        SPLIT_PHASE_V4_FULL_RATE_ORIGIN
    );
    assert_eq!(assets.alignment.phase0_prepad, SPLIT_PHASE_V4_PHASE0_PREPAD);
    assert_eq!(assets.alignment.phase1_prepad, SPLIT_PHASE_V4_PHASE1_PREPAD);
    assert_eq!(
        assets.alignment.decimation_prepad,
        SPLIT_PHASE_V4_DECIMATION_PREPAD
    );
    let bytes = assets
        .character
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        SPLIT_PHASE_V4_CHARACTER_SHA256
    );
    for (index, cleanup) in assets.cleanups.iter().enumerate() {
        assert_eq!(cleanup.len(), SPLIT_PHASE_V4_CLEANUP_COEFFICIENTS[index]);
        let bytes = cleanup
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            SPLIT_PHASE_V4_CLEANUP_SHA256[index]
        );
    }
    for (values, expected) in [
        (
            &assets.rational_tables.phase_147_160,
            SPLIT_PHASE_V4_RATIONAL_147_160_SHA256,
        ),
        (
            &assets.rational_tables.phase_160_147,
            SPLIT_PHASE_V4_RATIONAL_160_147_SHA256,
        ),
    ] {
        let bytes = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
    }
}

#[test]
fn split_phase_e2v3_generated_constants_match_embedded_assets() {
    let assets = split_phase_e2v3_assets();
    assert_eq!(
        assets.character.len(),
        SPLIT_PHASE_E2V3_CHARACTER_COEFFICIENTS
    );
    assert_eq!(
        assets.alignment.full_rate_origin,
        SPLIT_PHASE_E2V3_FULL_RATE_ORIGIN
    );
    assert_eq!(
        assets.alignment.phase0_prepad,
        SPLIT_PHASE_E2V3_PHASE0_PREPAD
    );
    assert_eq!(
        assets.alignment.phase1_prepad,
        SPLIT_PHASE_E2V3_PHASE1_PREPAD
    );
    assert_eq!(
        assets.alignment.decimation_prepad,
        SPLIT_PHASE_E2V3_DECIMATION_PREPAD
    );
    let bytes = assets
        .character
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        SPLIT_PHASE_E2V3_CHARACTER_SHA256
    );
    for (index, cleanup) in assets.cleanups.iter().enumerate() {
        assert_eq!(cleanup.len(), SPLIT_PHASE_E2V3_CLEANUP_COEFFICIENTS[index]);
        let bytes = cleanup
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            SPLIT_PHASE_E2V3_CLEANUP_SHA256[index]
        );
    }
    for (values, expected) in [
        (
            &assets.rational_tables.phase_147_160,
            SPLIT_PHASE_E2V3_RATIONAL_147_160_SHA256,
        ),
        (
            &assets.rational_tables.phase_160_147,
            SPLIT_PHASE_E2V3_RATIONAL_160_147_SHA256,
        ),
    ] {
        let bytes = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
    }
}

#[test]
fn split_phase_e3_generated_constants_match_embedded_assets() {
    let assets = split_phase_e3_assets();
    assert_eq!(
        assets.character.len(),
        SPLIT_PHASE_E3_CHARACTER_COEFFICIENTS
    );
    assert_eq!(
        assets.alignment.full_rate_origin,
        SPLIT_PHASE_E3_FULL_RATE_ORIGIN
    );
    assert_eq!(assets.alignment.phase0_prepad, SPLIT_PHASE_E3_PHASE0_PREPAD);
    assert_eq!(assets.alignment.phase1_prepad, SPLIT_PHASE_E3_PHASE1_PREPAD);
    assert_eq!(
        assets.alignment.decimation_prepad,
        SPLIT_PHASE_E3_DECIMATION_PREPAD
    );
    let bytes = assets
        .character
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        SPLIT_PHASE_E3_CHARACTER_SHA256
    );
    for (index, cleanup) in assets.cleanups.iter().enumerate() {
        assert_eq!(cleanup.len(), SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[index]);
        let bytes = cleanup
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            SPLIT_PHASE_E3_CLEANUP_SHA256[index]
        );
    }
    for (values, expected) in [
        (
            &assets.rational_tables.phase_147_160,
            SPLIT_PHASE_E3_RATIONAL_147_160_SHA256,
        ),
        (
            &assets.rational_tables.phase_160_147,
            SPLIT_PHASE_E3_RATIONAL_160_147_SHA256,
        ),
    ] {
        let bytes = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
    }
}

#[test]
fn split_phase_b_manifest_matches_the_promoted_asset() {
    let asset_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/filters/split_phase_e3");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(asset_dir.join("manifest.json")).expect("Split Phase B manifest"),
    )
    .expect("valid Split Phase B manifest JSON");
    assert_eq!(manifest["runtime_name"], "SplitPhase128kE3");
    assert_eq!(manifest["display_name"], "Split Phase B");
    assert_eq!(manifest["experimental"], false);
    assert_eq!(manifest["production_promoted"], true);
    assert_eq!(manifest["accepted_full_pipeline"], true);
    assert_eq!(
        manifest["files"]["character"]["sha256"],
        SPLIT_PHASE_E3_CHARACTER_SHA256
    );

    let certification: serde_json::Value = serde_json::from_slice(
        &fs::read(asset_dir.join("certification.json")).expect("Split Phase B certification"),
    )
    .expect("valid Split Phase B certification JSON");
    assert_eq!(
        certification["promotion"]["production_asset_integrated"],
        true
    );
    assert_eq!(certification["promotion"]["ui_exposed"], true);
    assert_eq!(
        certification["candidate"]["character_sha256"],
        SPLIT_PHASE_E3_CHARACTER_SHA256
    );
}

#[test]
fn split_phase_v4_runtime_cost_and_memory_class_match_v3() {
    let mut maximum_memory_ratio = 0.0_f64;
    for (source_rate, target_rate) in [
        (44_100, 88_200),
        (44_100, 11_289_600),
        (88_200, 44_100),
        (11_289_600, 44_100),
        (44_100, 48_000),
        (48_000, 44_100),
    ] {
        let c = SincResampler::new(FilterType::SplitPhase128kV3, source_rate, target_rate)
            .runtime_info();
        let d = SincResampler::new(FilterType::SplitPhase128kV4, source_rate, target_rate)
            .runtime_info();
        assert_eq!(c.path_kind, d.path_kind);
        let ratio = d.estimated_memory_bytes as f64 / c.estimated_memory_bytes.max(1) as f64;
        maximum_memory_ratio = maximum_memory_ratio.max(ratio);
        assert!(
            ratio <= 1.0,
            "{source_rate}->{target_rate} memory ratio {ratio}"
        );
    }
    // V3 and V4 use the same generic frozen engine, coefficient counts,
    // partition sizes, and rational dimensions. Their exact operation-count
    // ratio is therefore one; coefficient values do not alter the hot path.
    println!(
        "SPLIT_PHASE_V4_COST operation_count_ratio=1.000000000000 memory_ratio={maximum_memory_ratio:.12}"
    );
}

#[test]
fn split_phase_v3_assets_match_manifest_hashes_and_exact_contracts() {
    let asset_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/filters/split_phase_v3");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(asset_dir.join("manifest.json")).expect("Split Phase V3 manifest"),
    )
    .expect("valid Split Phase V3 manifest JSON");
    assert_eq!(manifest["identity"], "SplitPhase128kV3");
    assert_eq!(
        manifest["alignment"]["full_rate_origin"].as_u64(),
        Some(SPLIT_PHASE_V3_FULL_RATE_ORIGIN as u64)
    );
    assert_eq!(
        manifest["alignment"]["phase0_prepad"].as_u64(),
        Some(SPLIT_PHASE_V3_PHASE0_PREPAD as u64)
    );
    assert_eq!(
        manifest["alignment"]["phase1_prepad"].as_u64(),
        Some(SPLIT_PHASE_V3_PHASE1_PREPAD as u64)
    );
    assert_eq!(
        manifest["alignment"]["decimation_prepad"].as_u64(),
        Some(SPLIT_PHASE_V3_DECIMATION_PREPAD as u64)
    );

    let files = &manifest["files"];
    let mut entries = vec![&files["character"], &files["rational_147_160"]];
    entries.push(&files["rational_160_147"]);
    entries.extend(
        files["cleanups"]
            .as_array()
            .expect("cleanup manifest entries"),
    );
    for entry in entries {
        let file = entry["file"].as_str().expect("asset filename");
        let bytes = fs::read(asset_dir.join(file)).expect("frozen coefficient asset");
        assert_eq!(
            bytes.len() as u64,
            entry["byte_length"].as_u64().expect("asset byte length"),
            "{file} byte length"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            entry["sha256"].as_str().expect("asset SHA-256"),
            "{file} SHA-256"
        );
    }

    let assets = split_phase_v3_assets();
    assert_eq!(
        assets.character.len(),
        SPLIT_PHASE_V3_CHARACTER_COEFFICIENTS
    );
    assert!((compensated_sum(assets.character.iter().step_by(2).copied()) - 0.5).abs() <= 2e-15);
    assert!(
        (compensated_sum(assets.character.iter().skip(1).step_by(2).copied()) - 0.5).abs() <= 2e-15
    );
    for cleanup in &assets.cleanups {
        let center = cleanup.len() / 2;
        assert!(
            cleanup
                .iter()
                .zip(cleanup.iter().rev())
                .all(|(left, right)| left.to_bits() == right.to_bits())
        );
        assert_eq!(cleanup[center].to_bits(), 0.5_f64.to_bits());
        for (index, coefficient) in cleanup.iter().enumerate().step_by(2) {
            if index != center {
                assert_eq!(coefficient.to_bits(), 0.0_f64.to_bits());
            }
        }
        assert!((compensated_sum(cleanup.iter().copied()) - 1.0).abs() <= 2e-15);
        assert!((compensated_sum(cleanup.iter().skip(1).step_by(2).copied()) - 0.5).abs() <= 2e-15);
    }
    for left in 3..assets.cleanups.len() {
        for right in left + 1..assets.cleanups.len() {
            assert_ne!(
                assets.cleanups[left].as_ref(),
                assets.cleanups[right].as_ref()
            );
        }
    }
}

#[test]
fn split_phase_v3_runtime_mapping_uses_frozen_coefficients_without_normalization() {
    let assets = split_phase_v3_assets();
    let (phase0, phase1, prepad0, prepad1) =
        frozen_character_polyphase_pair(FrozenFilterVersion::V3);
    assert_eq!(prepad0, Some(SPLIT_PHASE_V3_PHASE0_PREPAD));
    assert_eq!(prepad1, Some(SPLIT_PHASE_V3_PHASE1_PREPAD));
    assert_eq!(phase0.len(), 131_073);
    assert_eq!(phase1.len(), 131_073);
    assert!((compensated_sum(phase0.iter().copied()) - 1.0).abs() <= 4e-15);
    assert!((compensated_sum(phase1.iter().copied()) - 1.0).abs() <= 4e-15);
    assert_eq!(phase1[0].to_bits(), 0.0_f64.to_bits());
    for index in 0..131_072 {
        assert_eq!(
            phase0[131_072 - index].to_bits(),
            (2.0 * assets.character[2 * index]).to_bits()
        );
        assert_eq!(
            phase1[131_072 - index].to_bits(),
            (2.0 * assets.character[2 * index + 1]).to_bits()
        );
    }
    assert_eq!(
        phase0[0].to_bits(),
        (2.0 * assets.character[262_144]).to_bits()
    );

    for stage_index in 1..=7u8 {
        let branch = frozen_cleanup_odd_branch(FrozenFilterVersion::V3, stage_index);
        let canonical = &assets.cleanups[usize::from(stage_index) - 1];
        assert_eq!(branch.len(), canonical.len() / 2 + 1);
        assert_eq!(branch[0].to_bits(), 0.0_f64.to_bits());
        assert!((compensated_sum(branch.iter().copied()) - 1.0).abs() <= 4e-15);
        for odd_index in 0..canonical.len() / 2 {
            assert_eq!(
                branch[branch.len() - 1 - odd_index].to_bits(),
                (2.0 * canonical[2 * odd_index + 1]).to_bits()
            );
        }
    }

    let character_spec =
        build_integer_stage_plan(44_100, 88_200, FilterType::SplitPhase128kV3, 1_000.0)
            .expect("Split Phase V3 2x plan")
            .stages
            .remove(0);
    let (decimation, prepad) = build_decimation_coefficients(&character_spec);
    assert_eq!(prepad, SPLIT_PHASE_V3_DECIMATION_PREPAD);
    assert!(
        decimation
            .iter()
            .zip(assets.character.iter().rev())
            .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
    );
}

#[test]
fn split_phase_v3_rational_tables_are_selected_only_for_frozen_ratios() {
    let assets = split_phase_v3_assets();
    for (source_rate, target_rate, expected) in [
        (44_100, 48_000, &assets.rational_tables.phase_147_160),
        (48_000, 44_100, &assets.rational_tables.phase_160_147),
    ] {
        let resampler = SincResampler::new(FilterType::SplitPhase128kV3, source_rate, target_rate);
        let ResamplerPath::Rational(rational) = &resampler.path else {
            panic!("expected frozen exact-rational path");
        };
        assert!(Arc::ptr_eq(&rational.coefficients, expected));
        assert!(resampler.runtime_info().phase_profile_preserved);
        let row_taps = 2 * rational.half_width + 1;
        for row in rational.coefficients.chunks_exact(row_taps) {
            assert!((compensated_sum(row.iter().copied()) - 1.0).abs() <= 2e-15);
        }
    }

    let unsupported = SincResampler::new(FilterType::SplitPhase128kV3, 96_000, 44_100);
    let ResamplerPath::Rational(rational) = &unsupported.path else {
        panic!("expected generic exact-rational fallback");
    };
    assert_eq!((rational.step_num, rational.phase_den), (320, 147));
    assert!(!unsupported.runtime_info().phase_profile_preserved);
    assert!(!Arc::ptr_eq(
        &rational.coefficients,
        &assets.rational_tables.phase_160_147
    ));
}

#[test]
fn split_phase_v4_rational_tables_are_selected_only_for_frozen_ratios() {
    let assets = split_phase_v4_assets();
    for (source_rate, target_rate, expected) in [
        (44_100, 48_000, &assets.rational_tables.phase_147_160),
        (48_000, 44_100, &assets.rational_tables.phase_160_147),
    ] {
        let resampler = SincResampler::new(FilterType::SplitPhase128kV4, source_rate, target_rate);
        let ResamplerPath::Rational(rational) = &resampler.path else {
            panic!("expected frozen exact-rational path");
        };
        assert!(Arc::ptr_eq(&rational.coefficients, expected));
        assert!(resampler.runtime_info().phase_profile_preserved);
        let row_taps = 2 * rational.half_width + 1;
        for row in rational.coefficients.chunks_exact(row_taps) {
            assert!((compensated_sum(row.iter().copied()) - 1.0).abs() <= 2e-15);
        }
    }

    let unsupported = SincResampler::new(FilterType::SplitPhase128kV4, 96_000, 44_100);
    let ResamplerPath::Rational(rational) = &unsupported.path else {
        panic!("expected generic exact-rational fallback");
    };
    assert!(!unsupported.runtime_info().phase_profile_preserved);
    assert!(!Arc::ptr_eq(
        &rational.coefficients,
        &assets.rational_tables.phase_160_147
    ));
}

#[test]
fn split_phase_v3_partitioned_runtime_rejects_the_2x_image_below_140_db() {
    const FRAMES: usize = 147_456;
    const ANALYSIS_FRAMES: usize = 16_384;
    let input_omega = PI / 2.0;
    let left = (0..FRAMES)
        .map(|index| (input_omega * index as f64).cos())
        .collect::<Vec<_>>();
    let right = (0..FRAMES)
        .map(|index| (input_omega * index as f64).sin())
        .collect::<Vec<_>>();
    let mut resampler = SincResampler::new(FilterType::SplitPhase128kV3, 44_100, 88_200);
    let mut output = Vec::new();
    for start in (0..FRAMES).step_by(4_093) {
        let end = (start + 4_093).min(FRAMES);
        resampler.input(&left[start..end], &right[start..end]);
        resampler.process(&mut output);
    }

    let output_frames = output.len() / 2;
    assert!(output_frames >= ANALYSIS_FRAMES);
    let start = output_frames - ANALYSIS_FRAMES;
    let desired_omega = input_omega / 2.0;
    let image_omega = desired_omega + PI;
    let mut desired = Complex64::new(0.0, 0.0);
    let mut image = Complex64::new(0.0, 0.0);
    for (local_index, frame) in output
        .chunks_exact(2)
        .skip(start)
        .take(ANALYSIS_FRAMES)
        .enumerate()
    {
        let sample = Complex64::new(frame[0], frame[1]);
        desired += sample * Complex64::from_polar(1.0, -desired_omega * local_index as f64);
        image += sample * Complex64::from_polar(1.0, -image_omega * local_index as f64);
    }
    let desired_amplitude = desired.norm() / ANALYSIS_FRAMES as f64;
    let image_amplitude = image.norm() / ANALYSIS_FRAMES as f64;
    let image_db = 20.0 * (image_amplitude / desired_amplitude).max(1.0e-300).log10();
    assert!(
        desired_amplitude > 0.99,
        "desired amplitude {desired_amplitude}"
    );
    assert!(image_db <= -140.0, "runtime image peak was {image_db} dB");
}

#[test]
fn frozen_split_phase_partitioned_runtime_rejects_2x_image_and_alias_below_145_db() {
    const INTERPOLATION_FRAMES: usize = 147_456;
    const DECIMATION_FRAMES: usize = 327_680;
    const ANALYSIS_FRAMES: usize = 16_384;

    fn run_tone(
        filter: FilterType,
        source_rate: u32,
        target_rate: u32,
        frames: usize,
        omega: f64,
    ) -> Vec<f64> {
        let left = (0..frames)
            .map(|index| (omega * index as f64).cos())
            .collect::<Vec<_>>();
        let right = (0..frames)
            .map(|index| (omega * index as f64).sin())
            .collect::<Vec<_>>();
        let mut resampler = SincResampler::new(filter, source_rate, target_rate);
        let mut output = Vec::new();
        for start in (0..frames).step_by(4_093) {
            let end = (start + 4_093).min(frames);
            resampler.input(&left[start..end], &right[start..end]);
            resampler.process(&mut output);
        }
        output
    }

    fn tone_amplitude(output: &[f64], omega: f64) -> f64 {
        let output_frames = output.len() / 2;
        assert!(output_frames >= ANALYSIS_FRAMES);
        let start = output_frames - ANALYSIS_FRAMES;
        let mut accumulator = Complex64::new(0.0, 0.0);
        for (local_index, frame) in output
            .chunks_exact(2)
            .skip(start)
            .take(ANALYSIS_FRAMES)
            .enumerate()
        {
            let sample = Complex64::new(frame[0], frame[1]);
            accumulator += sample * Complex64::from_polar(1.0, -omega * local_index as f64);
        }
        accumulator.norm() / ANALYSIS_FRAMES as f64
    }

    for (filter, label) in [
        (FilterType::SplitPhase128kV4, "SPLIT_PHASE_V4_RUNTIME"),
        (FilterType::SplitPhase128kE2v3, "SPLIT_PHASE_E2V3_RUNTIME"),
        (FilterType::SplitPhase128kE3, "SPLIT_PHASE_E3_RUNTIME"),
    ] {
        let input_omega = PI / 2.0;
        let interpolated = run_tone(filter, 44_100, 88_200, INTERPOLATION_FRAMES, input_omega);
        let desired_omega = input_omega / 2.0;
        let desired = tone_amplitude(&interpolated, desired_omega);
        let image = tone_amplitude(&interpolated, desired_omega + PI);
        let image_db = 20.0 * (image / desired).max(1.0e-300).log10();
        assert!(
            desired > 0.99,
            "{label} desired interpolation amplitude {desired}"
        );
        assert!(
            image_db <= -145.0,
            "{label} runtime image peak was {image_db} dB"
        );

        // -pi/4 and 3pi/4 both map to -pi/2 after decimation. The latter is
        // the independently exercised reverse alias branch.
        let desired_input = run_tone(filter, 88_200, 44_100, DECIMATION_FRAMES, -PI / 4.0);
        let alias_input = run_tone(filter, 88_200, 44_100, DECIMATION_FRAMES, 3.0 * PI / 4.0);
        let output_omega = -PI / 2.0;
        let desired = tone_amplitude(&desired_input, output_omega);
        let alias_amplitude = tone_amplitude(&alias_input, output_omega);
        let alias_db = 20.0 * (alias_amplitude / desired).max(1.0e-300).log10();
        assert!(
            desired > 0.99,
            "{label} desired decimation amplitude {desired}"
        );
        assert!(
            alias_db <= -145.0,
            "{label} runtime alias peak was {alias_db} dB"
        );
        println!("{label} image_db={image_db:.12} alias_db={alias_db:.12}");
    }
}

#[test]
fn frozen_split_phase_rational_runtime_rejects_image_and_reverse_alias_below_145_db() {
    fn run_tone(
        filter: FilterType,
        source_rate: u32,
        target_rate: u32,
        frames: usize,
        frequency_hz: f64,
    ) -> Vec<f64> {
        let omega = 2.0 * PI * frequency_hz / source_rate as f64;
        let left = (0..frames)
            .map(|index| (omega * index as f64).cos())
            .collect::<Vec<_>>();
        let right = (0..frames)
            .map(|index| (omega * index as f64).sin())
            .collect::<Vec<_>>();
        let mut resampler = SincResampler::new(filter, source_rate, target_rate);
        let mut output = Vec::new();
        for start in (0..frames).step_by(997) {
            let end = (start + 997).min(frames);
            resampler.input(&left[start..end], &right[start..end]);
            resampler.process(&mut output);
        }
        output
    }

    fn tone_amplitude(output: &[f64], target_rate: u32, frequency_hz: f64) -> f64 {
        let analysis_frames = target_rate as usize;
        let output_frames = output.len() / 2;
        assert!(output_frames >= analysis_frames);
        let omega = 2.0 * PI * frequency_hz / target_rate as f64;
        let mut accumulator = Complex64::new(0.0, 0.0);
        for (local_index, frame) in output
            .chunks_exact(2)
            .skip(output_frames - analysis_frames)
            .take(analysis_frames)
            .enumerate()
        {
            accumulator += Complex64::new(frame[0], frame[1])
                * Complex64::from_polar(1.0, -omega * local_index as f64);
        }
        accumulator.norm() / analysis_frames as f64
    }

    for (filter, label) in [
        (
            FilterType::SplitPhase128kV4,
            "SPLIT_PHASE_V4_RATIONAL_RUNTIME",
        ),
        (
            FilterType::SplitPhase128kE2v3,
            "SPLIT_PHASE_E2V3_RATIONAL_RUNTIME",
        ),
        (
            FilterType::SplitPhase128kE3,
            "SPLIT_PHASE_E3_RATIONAL_RUNTIME",
        ),
    ] {
        let upsampled = run_tone(filter, 44_100, 48_000, 88_200, 20_000.0);
        let desired_up = tone_amplitude(&upsampled, 48_000, 20_000.0);
        // The first 44.1 kHz periodic image at -24.1 kHz wraps to +23.9 kHz
        // in the 48 kHz output's discrete-frequency interval.
        let image_up = tone_amplitude(&upsampled, 48_000, 23_900.0);
        let image_db = 20.0 * (image_up / desired_up).max(1.0e-300).log10();
        assert!(
            desired_up > 0.99,
            "{label} rational desired amplitude {desired_up}"
        );
        assert!(
            image_db <= -145.0,
            "{label} rational runtime image was {image_db} dB"
        );

        // +23.9 kHz and -20.2 kHz differ by the 44.1 kHz output rate, so
        // they exercise the desired and reverse-alias terms independently.
        let desired_down_output = run_tone(filter, 48_000, 44_100, 96_000, -20_200.0);
        let alias_down_output = run_tone(filter, 48_000, 44_100, 96_000, 23_900.0);
        let desired_down = tone_amplitude(&desired_down_output, 44_100, -20_200.0);
        let alias_down = tone_amplitude(&alias_down_output, 44_100, -20_200.0);
        let alias_db = 20.0 * (alias_down / desired_down).max(1.0e-300).log10();
        assert!(
            desired_down > 0.01,
            "{label} rational desired transition amplitude {desired_down}"
        );
        assert!(
            alias_db <= -145.0,
            "{label} rational runtime alias was {alias_db} dB"
        );
        println!("{label} image_db={image_db:.12} alias_db={alias_db:.12}");
    }
}

#[test]
fn split_phase_polyphase_pair_preserves_branch_dc() {
    let (phase0, phase1, prepad0, prepad1) =
        build_character_polyphase_pair(256, 23.12088, 0.465333, PhaseMode::SplitPhase128k);
    let dc0: f64 = phase0.iter().sum();
    let dc1: f64 = phase1.iter().sum();
    assert!((dc0 - 1.0).abs() < 1e-9, "phase0 DC gain {}", dc0);
    assert!((dc1 - 1.0).abs() < 1e-9, "phase1 DC gain {}", dc1);
    assert!(prepad0.is_some());
    assert!(prepad1.is_some());
}

#[test]
fn split_phase_v2_polyphase_pair_preserves_branch_dc() {
    let (phase0, phase1, prepad0, prepad1) =
        build_character_polyphase_pair(256, 23.12088, 0.465333, PhaseMode::SplitPhase128kV2);
    let dc0: f64 = phase0.iter().sum();
    let dc1: f64 = phase1.iter().sum();
    assert!((dc0 - 1.0).abs() < 1e-9, "phase0 DC gain {dc0}");
    assert!((dc1 - 1.0).abs() < 1e-9, "phase1 DC gain {dc1}");
    assert!(prepad0.is_some());
    assert!(prepad1.is_some());
}

#[test]
fn split_phase_v2_integrates_group_delay_and_closes_to_minimum_phase() {
    let fft_len = 4096usize;
    let params = SplitPhaseParams {
        low_blend_floor: SPLIT128K_PRODUCTION_BLEND_FLOOR,
        ..SplitPhaseParams::default()
    };
    let minimum_phase = (0..=fft_len / 2)
        .map(|k| {
            let x = k as f64;
            -0.004 * x - 1.5e-6 * x * x + 2.0e-10 * x * x * x
        })
        .collect::<Vec<_>>();
    let target = split_phase_v2_from_unwrapped_minimum(&minimum_phase, fft_len, params);
    let lo_bin =
        ((params.split_f_lo * fft_len as f64).round() as usize).clamp(1, minimum_phase.len() - 2);
    let join_bin =
        ((params.split_f_hi * fft_len as f64 + 0.5).ceil() as usize).min(minimum_phase.len() - 1);

    let reference_increment = minimum_phase[lo_bin] / lo_bin as f64;
    for k in 0..=lo_bin {
        let expected = (1.0 - params.low_blend_floor) * reference_increment * k as f64
            + params.low_blend_floor * minimum_phase[k];
        assert!(
            (target[k] - expected).abs() < 1.0e-14,
            "low-frequency identity changed at bin {k}"
        );
    }

    for k in join_bin..minimum_phase.len() {
        assert_eq!(
            target[k], minimum_phase[k],
            "target did not close at bin {k}"
        );
    }

    let mut inferred_correction: Option<f64> = None;
    for k in (lo_bin + 1)..join_bin {
        let freq_mid = (k as f64 - 0.5) / fft_len as f64;
        let weight = split_phase_v2_blend_weight(freq_mid, params);
        let closure_shape = split_phase_v2_closure_bump(freq_mid, params);
        if closure_shape < 1.0e-5 {
            continue;
        }
        let target_increment = target[k] - target[k - 1];
        let minimum_increment = minimum_phase[k] - minimum_phase[k - 1];
        let base_increment = (1.0 - weight) * reference_increment + weight * minimum_increment;
        let correction = (target_increment - base_increment) / closure_shape;
        if let Some(expected) = inferred_correction {
            assert!(
                (correction - expected).abs() < 1.0e-9,
                "phase increments do not share one smooth closure correction: {correction} vs {expected}"
            );
        } else {
            inferred_correction = Some(correction);
        }
    }

    assert_eq!(
        split_phase_v2_blend_weight(params.split_f_lo, params),
        params.low_blend_floor
    );
    assert_eq!(split_phase_v2_blend_weight(params.split_f_hi, params), 1.0);
    assert_eq!(split_phase_v2_closure_bump(params.split_f_lo, params), 0.0);
    assert_eq!(split_phase_v2_closure_bump(params.split_f_hi, params), 0.0);
}

#[test]
fn split_phase_impulse_fades_to_zero_at_truncation() {
    let proto = build_full_rate_2x_prototype(64, 23.12088, 0.465333);
    let split = split_phase_impulse_with_params(&proto, split128k_phase_params());
    assert!(
        split.last().unwrap().abs() < 1e-18,
        "split-phase tail should fade to zero"
    );
}

#[test]
fn split_phase_v2_impulse_fades_to_zero_at_truncation() {
    let proto = build_full_rate_2x_prototype(64, 23.12088, 0.465333);
    let split = split_phase_v2_impulse_with_params(&proto, split128k_phase_params());
    assert!(split.iter().all(|sample| sample.is_finite()));
    assert!(
        split.last().unwrap().abs() < 1e-18,
        "Split Phase V2 tail should fade to zero"
    );
}

#[test]
fn split128k_kernel_keeps_split_phase_shape() {
    let half_width = 8_192usize; // 16,385-tap family -> 32,769-tap 2x prototype
    let beta = 23.12088;
    let cutoff = 0.465333;
    let proto = build_full_rate_2x_prototype(half_width, beta, cutoff);
    let split = split_phase_impulse_with_params(&proto, split128k_phase_params());
    let minimum = minimum_phase_impulse(&proto);

    // Split128k keeps the split-phase recipe: same passband magnitude as
    // its own linear prototype, without the retired shorter split paths.
    for &freq in &[0.0_f64, 0.025, 0.05, 0.10, 0.15, 0.20, 0.2268] {
        let lin_db = full_rate_magnitude_db(&proto, freq);
        let split_db = full_rate_magnitude_db(&split, freq);
        assert!(
            (lin_db - split_db).abs() < 0.03,
            "Split128k magnitude mismatch at f={freq}: linear={lin_db} dB, split={split_db} dB"
        );
    }
    let mut worst_stop_db = f64::NEG_INFINITY;
    for &freq in &[0.235_f64, 0.25, 0.27, 0.30, 0.35, 0.45] {
        let stop_db = full_rate_magnitude_db(&split, freq);
        worst_stop_db = worst_stop_db.max(stop_db);
        assert!(
            stop_db < -185.0,
            "Split128k stopband at f={freq} must stay below -185 dB, got {stop_db} dB"
        );
    }
    println!("Split128k worst measured stopband: {worst_stop_db:.2} dB");

    let gd = group_delay_samples(&split, 100.0 / 88_200.0, SPLIT_PHASE_BLEND_F_LO);
    assert!(!gd.is_empty());
    let gd_mean = gd.iter().sum::<f64>() / gd.len() as f64;
    let gd_dev = gd
        .iter()
        .fold(0.0_f64, |acc, &g| acc.max((g - gd_mean).abs()));
    assert!(
        gd_dev < 4.0,
        "Split128k midband group delay must remain flat, got deviation {gd_dev} around mean {gd_mean}"
    );
    let transition_gd = group_delay_samples(&split, SPLIT_PHASE_BLEND_F_LO, SPLIT_PHASE_BLEND_F_HI);
    assert!(!transition_gd.is_empty());
    let max_transition_step = transition_gd
        .windows(2)
        .fold(0.0_f64, |acc, pair| acc.max((pair[1] - pair[0]).abs()));
    assert!(
        max_transition_step < 4.0,
        "Split128k transition-band group delay should change smoothly, got adjacent step {max_transition_step}"
    );

    let split_peak = dominant_impulse_index(&split);
    let minimum_peak = dominant_impulse_index(&minimum);
    let gd_min = group_delay_samples(&minimum, 100.0 / 88_200.0, SPLIT_PHASE_BLEND_F_LO);
    let gd_min_mean = gd_min.iter().sum::<f64>() / gd_min.len() as f64;
    let split_lf_lag = gd_mean - split_peak as f64;
    let minimum_lf_lag = gd_min_mean - minimum_peak as f64;
    assert!(
        (split_lf_lag - minimum_lf_lag).abs() < 32.0,
        "Split128k peak-relative LF lag {split_lf_lag} should track the min-phase reference {minimum_lf_lag}"
    );
    assert!(
        gd_mean < half_width as f64 / 4.0,
        "Split128k LF group delay {gd_mean} must stay far below the symmetric half-length {}",
        2 * half_width
    );
    assert!(
        split_peak < proto.len() / 8,
        "Split128k impulse peak at {split_peak} of {} is not front-loaded",
        proto.len()
    );
}

#[test]
fn split128k_v2_kernel_preserves_magnitude_and_has_a_smooth_transition() {
    let half_width = 8_192usize;
    let proto = build_full_rate_2x_prototype(half_width, 23.12088, 0.465333);
    let split = split_phase_v2_impulse_with_params(&proto, split128k_phase_params());

    for &freq in &[0.0_f64, 0.025, 0.05, 0.10, 0.15, 0.20, 0.2268] {
        let linear_db = full_rate_magnitude_db(&proto, freq);
        let split_db = full_rate_magnitude_db(&split, freq);
        assert!(
            (linear_db - split_db).abs() < 0.03,
            "Split Phase V2 magnitude mismatch at f={freq}: linear={linear_db} dB, split={split_db} dB"
        );
    }
    for &freq in &[0.235_f64, 0.25, 0.27, 0.30, 0.35, 0.45] {
        let stop_db = full_rate_magnitude_db(&split, freq);
        assert!(
            stop_db < -185.0,
            "Split Phase V2 stopband at f={freq} must stay below -185 dB, got {stop_db} dB"
        );
    }

    let transition = group_delay_samples(&split, SPLIT_PHASE_BLEND_F_LO, SPLIT_PHASE_BLEND_F_HI);
    let max_adjacent_step = transition.windows(2).fold(0.0_f64, |maximum, pair| {
        maximum.max((pair[1] - pair[0]).abs())
    });
    assert!(
        max_adjacent_step < 4.0,
        "Split Phase V2 transition group delay has an adjacent step of {max_adjacent_step} samples"
    );
    assert!(
        dominant_impulse_index(&split) < proto.len() / 8,
        "Split Phase V2 impulse is not front-loaded"
    );
}

#[test]
fn split16k_impulse_peak_preringing_stays_under_two_point_five_ms() {
    let cases = [
        (44_100u32, 48_000u32),
        (44_100u32, 352_800u32),
        (48_000u32, 384_000u32),
    ];

    for (source_rate, target_rate) in cases {
        let response = run_impulse_response(FilterType::Split128k, source_rate, target_rate);
        assert!(
            !response.is_empty(),
            "empty impulse response for {source_rate}->{target_rate}"
        );
        let (first_idx, peak_idx, pre_peak_samples) = impulse_peak_metrics(&response);
        let limit_samples = (target_rate as usize * 25) / 10_000;
        assert!(
            pre_peak_samples <= limit_samples,
            "{source_rate}->{target_rate} Split16k pre-ringing peak window was {pre_peak_samples} samples ({:.3} ms), first={first_idx}, peak={peak_idx}, limit={limit_samples}",
            pre_peak_samples as f64 / target_rate as f64 * 1000.0
        );

        let minimum_response =
            run_impulse_response(FilterType::Minimum16k, source_rate, target_rate);
        let (_, minimum_peak_idx, _) = impulse_peak_metrics(&minimum_response);
        // The captured response starts at the first emitted output frame,
        // so this checks front-loaded impulse shape rather than absolute
        // buffering latency. The larger Split16k partition is covered by
        // the planner test above.
        let peak_skew_ms =
            (peak_idx as f64 - minimum_peak_idx as f64).abs() / target_rate as f64 * 1000.0;
        assert!(
            peak_skew_ms < 10.0,
            "{source_rate}->{target_rate} Split16k captured peak position should stay near Minimum16k's: split={peak_idx}, minimum={minimum_peak_idx}, skew={peak_skew_ms:.3} ms"
        );
    }
}

#[test]
fn split16k_resampler_constructs_for_dsd256_integer_ratios() {
    for source_rate in [44_100u32, 48_000u32] {
        let resampler = SincResampler::new(FilterType::Split128k, source_rate, source_rate * 256);
        assert!(resampler.is_high_latency());
        assert!(
            resampler.estimated_memory_bytes() < 32 * 1024 * 1024,
            "estimated memory for {source_rate}->DSD256 was {}",
            resampler.estimated_memory_bytes()
        );
    }
}
