use super::*;

const ACTIVE_FILTERS: [FilterType; 4] = [
    FilterType::LinearPhase128k,
    FilterType::Minimum16k,
    FilterType::MinimumPhaseCompact128k,
    FilterType::SplitPhase128kE3,
];

#[test]
fn active_filters_round_trip_through_ids_names_and_serde() {
    for filter in ACTIVE_FILTERS {
        assert_eq!(FilterType::from_id(filter.as_id()), Some(filter));
        assert_eq!(FilterType::from_name(filter.as_name()), Some(filter));
        let encoded = serde_json::to_string(&filter).unwrap();
        assert_eq!(
            serde_json::from_str::<FilterType>(&encoded).unwrap(),
            filter
        );
    }

    let mut ids = ACTIVE_FILTERS.map(FilterType::as_id).to_vec();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), ACTIVE_FILTERS.len());
}

#[test]
fn retired_filter_names_migrate_to_supported_filters() {
    assert_eq!(
        FilterType::from_name("SincExtreme32k"),
        Some(FilterType::LinearPhase128k)
    );
    for name in [
        "MinimumPhase128k",
        "MinimumPhase128kV2",
        "MinimumPhase128kV3",
        "MinimumPhase128kV4",
        "MinimumPhase128kV5",
        "MinimumPhaseCompact128kV2",
    ] {
        assert_eq!(
            FilterType::from_name(name),
            Some(FilterType::MinimumPhaseCompact128k)
        );
    }
    for name in [
        "Split128k",
        "Split128kV2",
        "SplitPhase128kV3",
        "SplitPhase128kV4",
        "SplitPhase128kE2v3",
        "IntegratedPhase128k",
        "SmoothPhase128k",
    ] {
        assert_eq!(
            FilterType::from_name(name),
            Some(FilterType::SplitPhase128kE3)
        );
    }
}

#[test]
fn retired_filter_ids_migrate_to_supported_filters() {
    assert_eq!(FilterType::from_id(6), Some(FilterType::LinearPhase128k));
    for id in 26..=31 {
        assert_eq!(
            FilterType::from_id(id),
            Some(FilterType::MinimumPhaseCompact128k)
        );
    }
    for id in [0, 2, 11, 16, 20, 25, 32, 34, 35, 36, 37] {
        assert_eq!(FilterType::from_id(id), Some(FilterType::SplitPhase128kE3));
    }
    assert_eq!(FilterType::from_id(u32::MAX), None);
}

#[test]
fn default_filter_is_the_visible_split_phase_filter() {
    assert_eq!(DEFAULT_FILTER_TYPE, FilterType::SplitPhase128kE3);
    assert_eq!(DEFAULT_FILTER_NAME, "SplitPhase128kE3");
}

#[test]
fn active_filters_build_expected_character_stages() {
    let cases = [
        (
            FilterType::LinearPhase128k,
            LINEAR128K_TAPS_TOTAL,
            PhaseMode::Linear,
            CharacterCoefficientSource::Procedural,
        ),
        (
            FilterType::Minimum16k,
            16_385,
            PhaseMode::Minimum,
            CharacterCoefficientSource::Procedural,
        ),
        (
            FilterType::MinimumPhaseCompact128k,
            MINIMUM_COMPACT_BRANCH_TAPS,
            PhaseMode::MinimumPhaseCompact128k,
            CharacterCoefficientSource::Procedural,
        ),
        (
            FilterType::SplitPhase128kE3,
            131_073,
            PhaseMode::FrozenSplitPhase,
            CharacterCoefficientSource::Frozen,
        ),
    ];

    for (filter, expected_taps, expected_phase, expected_source) in cases {
        let plan = build_integer_stage_plan(44_100, 88_200, filter, 100.0).unwrap();
        assert!(plan.high_latency);
        assert!(matches!(
            plan.stages.as_slice(),
            [StageSpec::Character2x {
                taps_total,
                engine: EngineKind::PartitionedFft {
                    partition_frames: 4096
                },
                phase_mode,
                coefficient_source,
                ..
            }] if *taps_total == expected_taps
                && *phase_mode == expected_phase
                && *coefficient_source == expected_source
        ));
    }
}

#[test]
fn only_split_phase_e3_uses_frozen_cleanup_coefficients() {
    for filter in ACTIVE_FILTERS {
        let cleanup = cleanup_stage_spec(1, filter);
        let expected = if filter == FilterType::SplitPhase128kE3 {
            CleanupCoefficientSource::Frozen { stage_index: 1 }
        } else {
            CleanupCoefficientSource::Procedural
        };
        assert!(matches!(
            cleanup,
            StageSpec::CleanupHalfband2x {
                coefficient_source,
                ..
            } if coefficient_source == expected
        ));
    }
}

#[test]
fn split_phase_e3_asset_contract_matches_generated_metadata() {
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
    for (actual, expected) in assets
        .cleanups
        .iter()
        .map(|coefficients| coefficients.len())
        .zip(SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS)
    {
        assert_eq!(actual, expected);
    }
}

#[test]
fn compact_transition_is_bounded_and_monotonic() {
    assert_eq!(smootherstep7(-1.0), 0.0);
    assert_eq!(smootherstep7(0.0), 0.0);
    assert_eq!(smootherstep7(1.0), 1.0);
    assert_eq!(smootherstep7(2.0), 1.0);

    let mut previous = 0.0;
    for step in 0..=100 {
        let value = smootherstep7(step as f64 / 100.0);
        assert!((0.0..=1.0).contains(&value));
        assert!(value >= previous);
        previous = value;
    }
}

#[test]
fn invalid_integer_ratios_are_rejected() {
    assert!(matches!(
        build_integer_stage_plan(44_100, 44_100, DEFAULT_FILTER_TYPE, 100.0),
        Err(StagePlanError::NonIntegerRatio)
    ));
    assert!(matches!(
        build_integer_stage_plan(0, 88_200, DEFAULT_FILTER_TYPE, 100.0),
        Err(StagePlanError::InvalidRate)
    ));
}
