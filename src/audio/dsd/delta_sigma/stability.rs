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
