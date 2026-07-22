//! Seventh-order PCM-to-DSD modulation.
//!
//! Playback has two implementations: the standard hard-sign CRFB quantizer and
//! the isolated EcBeam2 search engine. They share coefficient validation,
//! normalized CRFB matrices, and state-stability handling; EcBeam2 owns all of
//! its candidate search, delayed commitment, diagnostics, and SIMD kernels.

pub(crate) mod beam_error_profile;
mod coeff_math;
mod dither;
mod ec_beam2;
mod modulator;
mod stability;

#[cfg(test)]
mod tests;

pub use ec_beam2::{
    EcBeam2BenchmarkModulator, EcBeam2DiagnosticWindow, EcBeam2Diagnostics,
    EcBeam2ExactOracleReport, EcBeam2ExperimentConfig, EcBeam2ObjectiveComponents,
    EcBeam2OracleComparison, EcBeam2OracleSeed, EcBeam2ProfileId, EcBeam2ScaleDistribution,
    prepare_ecbeam2_oracle_seed, run_ecbeam2_exact_oracle, run_ecbeam2_exact_oracle_from_seed,
};
pub(crate) use ec_beam2::{
    EcBeam2Modulator, ecbeam2_dsd64_production_coefficients,
    ecbeam2_dsd128_production_coefficients, ecbeam2_dsd256_production_coefficients,
    ecbeam2_production_config,
};
pub use modulator::*;
