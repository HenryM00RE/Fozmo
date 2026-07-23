use super::{CrfbModulator, DsdModulator};
use crate::audio::dsd::dsd_coeffs::CRFB7_STANDARD_OSR64;

#[test]
fn public_modulator_ids_only_select_standard_and_seventh_order_search() {
    assert_eq!(DsdModulator::Standard.as_id(), 0);
    assert_eq!(DsdModulator::SeventhOrderSearch.as_id(), 7);
    assert_eq!(
        DsdModulator::SeventhOrderSearch.as_name(),
        "7th-order-search"
    );
    assert_eq!(DsdModulator::from_id(0), DsdModulator::Standard);
    assert_eq!(DsdModulator::from_id(7), DsdModulator::SeventhOrderSearch);
    for retired_id in 1..=6 {
        assert_eq!(DsdModulator::from_id(retired_id), DsdModulator::Standard);
    }
}

#[test]
fn retired_names_are_migration_aliases_for_standard() {
    for retired in [
        "EcDepth1",
        "EcDepth2",
        "EcDepth3",
        "EcDepth4",
        "EcDepth8",
        "EcDepth4Adaptive",
        "EcBeam",
        "7th Order ECB",
    ] {
        assert_eq!(
            DsdModulator::from_name(retired),
            Some(DsdModulator::Standard)
        );
    }
    assert_eq!(
        DsdModulator::from_name("7th-order-search"),
        Some(DsdModulator::SeventhOrderSearch)
    );
    assert_eq!(
        DsdModulator::from_name("EcBeam2"),
        Some(DsdModulator::SeventhOrderSearch)
    );
}

#[test]
fn standard_modulator_is_chunk_invariant_and_has_no_flush_tail() {
    let input: Vec<f64> = (0..4096)
        .map(|index| {
            let phase = index as f64;
            0.21 * (phase * 0.031).sin() + 0.07 * (phase * 0.119).cos()
        })
        .collect();
    let mut whole = CrfbModulator::new(&CRFB7_STANDARD_OSR64, 0x000A_11CE).unwrap();
    let mut chunked = CrfbModulator::new(&CRFB7_STANDARD_OSR64, 0x000A_11CE).unwrap();
    let mut whole_bits = Vec::new();
    let mut chunked_bits = Vec::new();

    whole.process_into_bits(&input, &mut whole_bits);
    for chunk in input.chunks(37) {
        chunked.process_into_bits(chunk, &mut chunked_bits);
    }
    let before_flush = whole_bits.len();
    whole.flush_into_bits(&mut whole_bits);

    assert_eq!(whole_bits, chunked_bits);
    assert_eq!(whole_bits.len(), input.len());
    assert_eq!(whole_bits.len(), before_flush);
    assert_eq!(whole.stability_resets(), 0);
    assert_eq!(whole.state_clamps(), 0);
}

#[test]
fn standard_modulator_contains_nonfinite_input() {
    let mut modulator = CrfbModulator::new(&CRFB7_STANDARD_OSR64, 7).unwrap();
    let mut bits = Vec::new();
    modulator.process_into_bits(&[0.0, f64::NAN, 0.0], &mut bits);

    assert_eq!(bits.len(), 3);
    assert!(modulator.stability_resets() > 0);
}
