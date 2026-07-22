//! Seventh-order PCM-to-DSD modulation.
//!
//! Playback has two implementations: the standard hard-sign CRFB quantizer and
//! the isolated 7th Order Search engine. They share coefficient validation,
//! normalized CRFB matrices, and state-stability handling; 7th Order Search
//! owns all of its candidate search, delayed commitment, diagnostics, and SIMD
//! kernels.

pub(crate) mod beam_error_profile;
mod coeff_math;
mod dither;
mod modulator;
mod seventh_order_search;
mod stability;

#[cfg(test)]
mod tests;

pub use modulator::*;
pub use seventh_order_search::{
    SeventhOrderSearchBenchmarkModulator, SeventhOrderSearchDiagnosticWindow,
    SeventhOrderSearchDiagnostics, SeventhOrderSearchExactOracleReport,
    SeventhOrderSearchExperimentConfig, SeventhOrderSearchObjectiveComponents,
    SeventhOrderSearchOracleComparison, SeventhOrderSearchOracleSeed, SeventhOrderSearchProfileId,
    SeventhOrderSearchScaleDistribution, prepare_seventh_order_search_oracle_seed,
    run_seventh_order_search_exact_oracle, run_seventh_order_search_exact_oracle_from_seed,
};
pub(crate) use seventh_order_search::{
    SeventhOrderSearchModulator, seventh_order_search_dsd64_production_coefficients,
    seventh_order_search_dsd128_production_coefficients,
    seventh_order_search_dsd256_production_coefficients, seventh_order_search_production_config,
};
