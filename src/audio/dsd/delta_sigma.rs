//! 7th-order delta-sigma modulator for PCM → 1-bit DSD conversion.
//!
//! Implements the Schreier-toolbox ABCD state-space form directly:
//!   y = C·x + d1·u
//!   v = sign(y)              (v ∈ {-1, +1}) for the standard path
//!   x' = A·x + B·[u; v]
//!
//! The EC path keeps the same 7th-order CRFB loop but replaces the hard sign
//! decision with a configurable-depth, analog-aware `+1`/`-1` candidate search
//! (branch-and-bound over the bit trellis). Integrator states use
//! coefficient-table-specific limits; finite overload clamps each integrator
//! independently, while non-finite state math triggers a full safety reset.
//!
//! The EC search exploits the affine structure of the loop: with
//! `n1 = base1 + f1·bv`, every depth-2 quantity is affine in the feedback `f1`
//! (`A·n1 = A·base1 + f1·(A·bv)`, `c·n1 = c·base1 + f1·(c·bv)`), so `A·bv` and
//! `c·bv` are precomputed once and each candidate expansion costs 7 mul-adds
//! plus a scalar instead of a full matvec + dot. This holds at every depth.
//!
//! The CRFB realization is sparse: each `A` row has at most 3 nonzeros (the
//! resonator-pair band structure) and only `c[6]` is nonzero. The hot path is
//! monomorphized over that pattern — verified against the table at
//! construction — cutting the per-node matvec from 56 FMAs to 19 and the loop
//! output dot from 8 to 1. Tables that don't match fall back to dense math.

// Experimental modulator variants and diagnostics are compiled for harnesses before runtime use.
#![allow(dead_code)]

pub(crate) mod beam_error_profile;
mod coeff_math;
mod diagnostics;
mod dither;
mod ec_beam;
mod ec_beam2;
mod ec_depth1;
mod ec_search;
#[cfg(feature = "ecbeam2_observer")]
mod ecbeam2_observer;
mod modulator;
mod stability;

#[cfg(test)]
mod tests;

pub use coeff_math::{
    compensated_feedback, dc_bias_decay_for_corner_hz, ec_candidate_score, updated_dc_bias,
};
pub use diagnostics::{
    AdaptiveDecisionTraceSnapshot, AdaptiveDecisionWindow, BeamDiagnostics, BeamMetricDiagnostics,
    BeamPeriodicityDiagnostics, BeamReconstructionDiagnostics, Ec2DecisionTraceSnapshot,
    Ec2DecisionTraceSummary, Ec2DecisionWindow,
};
pub(crate) use ec_beam2::EcBeam2Modulator;
pub use ec_beam2::{
    EcBeam2DiagnosticWindow, EcBeam2Diagnostics, EcBeam2ExactOracleReport, EcBeam2ExperimentConfig,
    EcBeam2ObjectiveComponents, EcBeam2OracleComparison, EcBeam2OracleSeed, EcBeam2ProfileId,
    EcBeam2RunMode, EcBeam2ScaleDistribution, prepare_ecbeam2_oracle_seed,
    run_ecbeam2_exact_oracle, run_ecbeam2_exact_oracle_from_seed,
};
#[cfg(feature = "ecbeam2_observer")]
pub use ecbeam2_observer::{
    ECBEAM2_OBSERVER_MAX_CHILDREN, ECBEAM2_OBSERVER_MAX_PARENTS, EcBeam2FrontierEvent,
    EcBeam2ObservedChild, EcBeam2ObservedCommit, EcBeam2ObservedCommitKind, EcBeam2ObservedMapping,
    EcBeam2ObservedParent, EcBeam2ObserverConfig, EcBeam2ObserverError, EcBeam2ObserverEvent,
    EcBeam2ObserverResetEvent, EcBeam2ObserverSnapshot,
};
pub use modulator::*;
