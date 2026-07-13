#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StateStability {
    Ok { clamped: bool },
    Reset,
}

/// Limits are validated at construction, so the hot path only needs the cheap
/// sum-probe for non-finite state plus the per-integrator clamp.
pub(super) fn stabilize_state(
    state: &mut [f64; 8],
    limit: &[f64; 7],
    inverse_limit: &[f64; 8],
) -> StateStability {
    let mut probe = 0.0f64;
    for s in &state[..7] {
        probe += s;
    }
    if !probe.is_finite() {
        return StateStability::Reset;
    }

    let mut clamped = false;
    for i in 0..7 {
        if state[i].abs() * inverse_limit[i] > 1.0 {
            state[i] = state[i].clamp(-limit[i], limit[i]);
            clamped = true;
        }
    }
    state[7] = 0.0;
    StateStability::Ok { clamped }
}

/// Normalized-space equivalent used by the fixed EcBeam kernel. The state
/// limits are exactly +/-1 here, avoiding a raw-space round trip for every
/// surviving child.
#[inline(always)]
pub(super) fn stabilize_normalized_state(state: &mut [f64; 8]) -> StateStability {
    let mut probe = 0.0f64;
    for &lane in &state[..7] {
        probe += lane;
    }
    if !probe.is_finite() {
        return StateStability::Reset;
    }

    let mut clamped = false;
    for lane in &mut state[..7] {
        if lane.abs() > 1.0 {
            *lane = lane.clamp(-1.0, 1.0);
            clamped = true;
        }
    }
    state[7] = 0.0;
    StateStability::Ok { clamped }
}
