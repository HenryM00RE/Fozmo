from __future__ import annotations

import gc
import json
import math
from pathlib import Path
from typing import Any

import jax
import jax.numpy as jnp
import jaxopt
import numpy as np
import optax

jax.config.update("jax_enable_x64", True)


def _compensated_sum(values: np.ndarray) -> float:
    return math.fsum(float(value) for value in values)


def project_polyphase_sums(coefficients: np.ndarray) -> np.ndarray:
    result = np.asarray(coefficients, dtype=np.float64).copy()
    even_index = int(2 * np.argmax(np.abs(result[::2])))
    odd_index = int(2 * np.argmax(np.abs(result[1::2])) + 1)
    for _ in range(4):
        result[even_index] += 0.5 - _compensated_sum(result[::2])
        result[odd_index] += 0.5 - _compensated_sum(result[1::2])
    return result


def project_cleanup_constraints(coefficients: np.ndarray) -> np.ndarray:
    result = np.asarray(coefficients, dtype=np.float64).copy()
    result = 0.5 * (result + result[::-1])
    center = result.size // 2
    if center % 2 != 0:
        raise ValueError("cleanup centre must lie in the even polyphase branch")
    result[::2] = 0.0
    result[center] = 0.5
    odd = result[1::2]
    left_odd = int(np.argmax(np.abs(odd[: odd.size // 2])))
    left = 2 * left_odd + 1
    right = result.size - 1 - left
    for _ in range(4):
        correction = 0.5 - _compensated_sum(result[1::2])
        result[left] += 0.5 * correction
        result[right] += 0.5 * correction
    return result


def _candidate_from_periodic(
    periodic_impulse: np.ndarray, support: int, delay: int
) -> np.ndarray:
    indices = (np.arange(support, dtype=np.int64) - delay) % periodic_impulse.size
    return np.asarray(periodic_impulse[indices], dtype=np.float64)


def initialize_finite_support(
    magnitude: np.ndarray,
    residual_phase: np.ndarray,
    fft_len: int,
    support: int,
    v2_delay: int,
    edge_samples: int,
    work_dir: Path,
    resume: bool = True,
) -> tuple[np.ndarray, int, dict[str, Any]]:
    coefficient_path = work_dir / "character_initial.npy"
    report_path = work_dir / "support_search.json"
    if resume and coefficient_path.exists() and report_path.exists():
        report = json.loads(report_path.read_text())
        return np.load(coefficient_path), int(report["selected_delay"]), report

    target_spectrum = np.asarray(magnitude) * np.exp(1j * np.asarray(residual_phase))
    periodic_impulse = np.fft.irfft(target_spectrum, n=fft_len)
    total_energy = float(np.dot(periodic_impulse, periodic_impulse))

    def score(delay: int) -> tuple[tuple[float, float, float], dict[str, float]]:
        candidate = _candidate_from_periodic(periodic_impulse, support, delay)
        retained = float(np.dot(candidate, candidate))
        peak = int(np.argmax(np.abs(candidate)))
        pre_peak = float(np.dot(candidate[:peak], candidate[:peak]))
        edge = float(
            np.dot(candidate[:edge_samples], candidate[:edge_samples])
            + np.dot(candidate[-edge_samples:], candidate[-edge_samples:])
        )
        omitted = max(total_energy - retained, 0.0)
        relative = lambda value: 10.0 * math.log10(max(value / total_energy, 1.0e-300))
        metrics = {
            "omitted_energy_db": relative(omitted),
            "pre_peak_energy_db": relative(pre_peak),
            "edge_energy_db": relative(edge),
            "peak_index": peak,
        }
        return (omitted, pre_peak, edge), metrics

    coarse_delays = range(v2_delay - 2048, v2_delay + 2049, 64)
    coarse = [(delay, *score(delay)) for delay in coarse_delays if delay >= 0 and delay % 2 == 0]
    coarse.sort(key=lambda item: item[1])
    coarse_best = coarse[0][0]
    fine_delays = range(coarse_best - 64, coarse_best + 65, 2)
    fine = [(delay, *score(delay)) for delay in fine_delays if delay >= 0 and delay % 2 == 0]
    fine.sort(key=lambda item: item[1])
    selected_delay, _, selected_metrics = fine[0]
    coefficients = project_polyphase_sums(
        _candidate_from_periodic(periodic_impulse, support, selected_delay)
    )
    report = {
        "selected_delay": selected_delay,
        "selected_metrics": selected_metrics,
        "coarse_candidates": [
            {"delay": delay, **metrics} for delay, _, metrics in coarse
        ],
        "fine_candidates": [{"delay": delay, **metrics} for delay, _, metrics in fine],
        "canonical_even_sum": _compensated_sum(coefficients[::2]),
        "canonical_odd_sum": _compensated_sum(coefficients[1::2]),
    }
    np.save(coefficient_path, coefficients)
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return coefficients, selected_delay, report


def optimize_character(
    initial: np.ndarray,
    magnitude: np.ndarray,
    residual_phase: np.ndarray,
    design_fft_len: int,
    working_fft_len: int,
    delay: int,
    sample_rate_hz: float,
    pass_edge_hz: float,
    stop_edge_hz: float,
    p_continuation: list[int],
    adam_steps_per_p: int,
    lbfgs_steps: int,
    cleanup_filters: list[np.ndarray],
    work_dir: Path,
    resume: bool = True,
) -> tuple[np.ndarray, dict[str, Any]]:
    final_path = work_dir / "character_optimized.npy"
    report_path = work_dir / "character_optimization.json"
    if resume and final_path.exists() and report_path.exists():
        return np.load(final_path), json.loads(report_path.read_text())
    if design_fft_len % working_fft_len != 0:
        raise ValueError("design FFT must be an integer multiple of the working FFT")

    stride = design_fft_len // working_fft_len
    working_magnitude = np.asarray(magnitude)[::stride]
    working_phase = np.asarray(residual_phase)[::stride]
    omega = np.linspace(0.0, np.pi, working_fft_len // 2 + 1)
    frequency = omega * sample_rate_hz / (2.0 * np.pi)
    target = working_magnitude * np.exp(1j * (working_phase - omega * delay))
    pass_mask = frequency <= pass_edge_hz
    transition_mask = (frequency >= pass_edge_hz) & (frequency <= stop_edge_hz)
    stop_mask = frequency >= stop_edge_hz
    pass_indices = jnp.asarray(np.flatnonzero(pass_mask), dtype=jnp.int32)
    transition_indices = jnp.asarray(np.flatnonzero(transition_mask), dtype=jnp.int32)
    stop_indices = jnp.asarray(np.flatnonzero(stop_mask), dtype=jnp.int32)
    target_jax = jnp.asarray(target)
    cleanup_products = [np.ones_like(omega)]
    accumulated = np.ones_like(omega)
    interpolation_axis = np.arange(omega.size, dtype=np.float64)
    for stage, cleanup in enumerate(cleanup_filters, start=1):
        cleanup_magnitude = np.abs(np.fft.rfft(cleanup, n=working_fft_len))
        stage_magnitude = np.interp(
            interpolation_axis / (2**stage), interpolation_axis, cleanup_magnitude
        )
        accumulated = accumulated * stage_magnitude
        cleanup_products.append(accumulated.copy())
    if len(cleanup_products) != 8:
        raise ValueError("Split Phase V3 requires seven distinct cleanup stages")
    cleanup_products_jax = tuple(jnp.asarray(value) for value in cleanup_products)
    edge_samples = 2048
    even_fix = int(2 * np.argmax(np.abs(initial[::2])))
    odd_fix = int(2 * np.argmax(np.abs(initial[1::2])) + 1)

    @jax.jit
    def project(value: jax.Array) -> jax.Array:
        value = value.at[even_fix].add(0.5 - jnp.sum(value[::2]))
        value = value.at[odd_fix].add(0.5 - jnp.sum(value[1::2]))
        return value

    def make_loss(p: int):
        p_value = float(p)

        @jax.jit
        def loss(unprojected: jax.Array) -> jax.Array:
            coefficients = project(unprojected)
            response = jnp.fft.rfft(coefficients, n=working_fft_len)
            normalized_errors = []
            for cleanup_product in cleanup_products_jax:
                composite = response * cleanup_product
                normalized_errors.append(
                    jnp.abs(composite[pass_indices] - target_jax[pass_indices])
                    / 2.0e-5
                )
                normalized_errors.append(
                    jnp.abs(jnp.abs(composite[pass_indices]) - 1.0) / 1.15e-5
                )
                normalized_errors.append(
                    jnp.abs(composite[stop_indices]) / 3.162277660168379e-8
                )
            normalized = jnp.concatenate(normalized_errors)
            smooth_max = jnp.exp(
                jax.scipy.special.logsumexp(
                    p_value * jnp.log(jnp.maximum(normalized, 1.0e-30))
                )
                / p_value
            )
            transition_magnitude = jnp.abs(response[transition_indices])
            upward = jax.nn.relu(jnp.diff(transition_magnitude) - 5.75e-5)
            edge = jnp.concatenate(
                (coefficients[:edge_samples], coefficients[-edge_samples:])
            )
            return (
                smooth_max
                + 1.0e4 * jnp.mean(upward**2)
                + 1.0e20 * jnp.sum(edge**2) / jnp.sum(coefficients**2)
            )

        return loss

    parameters = jnp.asarray(initial, dtype=jnp.float64)
    candidate_parameters: list[tuple[str, jax.Array]] = [("finite_support_initial", parameters)]
    history: list[dict[str, Any]] = []
    for p in p_continuation:
        checkpoint = work_dir / f"character_p{p}.npy"
        if resume and checkpoint.exists():
            parameters = jnp.asarray(np.load(checkpoint), dtype=jnp.float64)
            candidate_parameters.append((f"p{p}_checkpoint", parameters))
            history.append({"p": p, "resumed": True})
            continue
        loss = make_loss(p)
        optimizer = optax.chain(
            optax.clip_by_global_norm(1.0),
            optax.adam(learning_rate=1.0e-11),
        )
        state = optimizer.init(parameters)
        value_and_gradient = jax.jit(jax.value_and_grad(loss))
        first_value = None
        final_value = None
        for _ in range(adam_steps_per_p):
            value, gradient = value_and_gradient(parameters)
            updates, state = optimizer.update(gradient, state, parameters)
            parameters = optax.apply_updates(parameters, updates)
            if first_value is None:
                first_value = float(value)
            final_value = float(value)
        parameters = project(parameters)
        np.save(checkpoint, np.asarray(parameters))
        candidate_parameters.append((f"p{p}_adam", parameters))
        history.append(
            {
                "p": p,
                "resumed": False,
                "adam_steps": adam_steps_per_p,
                "initial_loss": first_value,
                "final_loss": final_value,
            }
        )

    final_loss = make_loss(p_continuation[-1])
    candidate_losses = [
        (label, float(final_loss(value)), value)
        for label, value in candidate_parameters
    ]
    selected_label, selected_loss, parameters = min(
        candidate_losses, key=lambda item: item[1]
    )
    lbfgs = jaxopt.LBFGS(
        fun=final_loss,
        maxiter=lbfgs_steps,
        tol=1.0e-10,
        history_size=10,
        jit=True,
    )
    result = lbfgs.run(parameters)
    lbfgs_parameters = project(result.params)
    lbfgs_loss = float(final_loss(lbfgs_parameters))
    lbfgs_accepted = bool(np.isfinite(lbfgs_loss) and lbfgs_loss <= selected_loss)
    if lbfgs_accepted:
        parameters = lbfgs_parameters
        selected_label = "lbfgs"
        selected_loss = lbfgs_loss
    else:
        parameters = project(parameters)
    # The support-edge coefficients live around 1e-12 while the main impulse
    # is O(1). A global coefficient parameterization leaves this objective
    # badly scaled even in float64. Polish the two edge blocks in normalized
    # coordinates so L-BFGS can reduce edge energy without applying a window
    # or discarding any coefficient.
    edge_values = jnp.concatenate(
        (parameters[:edge_samples], parameters[-edge_samples:])
    )
    edge_scale = max(float(jnp.max(jnp.abs(edge_values))), 1.0e-18)

    @jax.jit
    def edge_loss(normalized_edges: jax.Array) -> jax.Array:
        candidate = parameters.at[:edge_samples].set(
            normalized_edges[:edge_samples] * edge_scale
        )
        candidate = candidate.at[-edge_samples:].set(
            normalized_edges[edge_samples:] * edge_scale
        )
        return final_loss(candidate)

    edge_solver = jaxopt.LBFGS(
        fun=edge_loss,
        maxiter=200,
        tol=1.0e-12,
        history_size=12,
        jit=True,
    )
    edge_result = edge_solver.run(edge_values / edge_scale)
    edge_candidate = parameters.at[:edge_samples].set(
        edge_result.params[:edge_samples] * edge_scale
    )
    edge_candidate = edge_candidate.at[-edge_samples:].set(
        edge_result.params[edge_samples:] * edge_scale
    )
    edge_candidate = project(edge_candidate)
    edge_polish_loss = float(final_loss(edge_candidate))
    edge_polish_accepted = bool(
        np.isfinite(edge_polish_loss) and edge_polish_loss <= selected_loss
    )
    if edge_polish_accepted:
        parameters = edge_candidate
        selected_label = "scaled_edge_lbfgs"
        selected_loss = edge_polish_loss
    coefficients = project_polyphase_sums(np.asarray(parameters, dtype=np.float64))
    response = np.fft.rfft(coefficients, n=working_fft_len)
    pass_error = 0.0
    stop_peak = 0.0
    ratio_metrics = []
    for exponent, cleanup_product in enumerate(cleanup_products):
        composite = response * cleanup_product
        ratio_pass_error = float(
            np.max(np.abs(composite[pass_mask] - target[pass_mask]))
        )
        ratio_stop_peak = float(np.max(np.abs(composite[stop_mask])))
        pass_error = max(pass_error, ratio_pass_error)
        stop_peak = max(stop_peak, ratio_stop_peak)
        ratio_metrics.append(
            {
                "ratio": 1 << (exponent + 1),
                "maximum_passband_complex_error": ratio_pass_error,
                "stopband_peak_db": 20.0
                * math.log10(max(ratio_stop_peak, 1.0e-300)),
            }
        )
    total_energy = float(np.dot(coefficients, coefficients))
    edge_energy = float(
        np.dot(coefficients[:edge_samples], coefficients[:edge_samples])
        + np.dot(coefficients[-edge_samples:], coefficients[-edge_samples:])
    )
    report = {
        "working_fft_len": working_fft_len,
        "continuation": history,
        "lbfgs_iterations": int(result.state.iter_num),
        "lbfgs_error": float(result.state.error),
        "lbfgs_loss": lbfgs_loss,
        "lbfgs_accepted": lbfgs_accepted,
        "selected_candidate": selected_label,
        "selected_final_loss": selected_loss,
        "edge_polish_scale": edge_scale,
        "edge_polish_iterations": int(edge_result.state.iter_num),
        "edge_polish_error": float(edge_result.state.error),
        "edge_polish_loss": edge_polish_loss,
        "edge_polish_accepted": edge_polish_accepted,
        "candidate_final_losses": {
            label: value for label, value, _ in candidate_losses
        },
        "maximum_passband_complex_error": pass_error,
        "stopband_peak_db": 20.0 * math.log10(max(stop_peak, 1.0e-300)),
        "edge_energy_db": 10.0 * math.log10(max(edge_energy / total_energy, 1.0e-300)),
        "canonical_even_sum": _compensated_sum(coefficients[::2]),
        "canonical_odd_sum": _compensated_sum(coefficients[1::2]),
        "ratios": ratio_metrics,
    }
    np.save(final_path, coefficients)
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return coefficients, report


def polish_cleanup_filters(
    character: np.ndarray,
    cleanup_filters: list[np.ndarray],
    magnitude: np.ndarray,
    residual_phase: np.ndarray,
    design_fft_len: int,
    bulk_delay: int,
    sample_rate_hz: float,
    pass_edge_hz: float,
    stop_edge_hz: float,
    cycles: int,
    work_dir: Path,
    resume: bool = True,
) -> tuple[list[np.ndarray], dict[str, Any]]:
    final_path = work_dir / "cleanup_polished.npz"
    report_path = work_dir / "cleanup_polish.json"
    if resume and final_path.exists() and report_path.exists():
        saved = np.load(final_path)
        return (
            [np.asarray(saved[f"stage_{index}"], dtype=np.float64) for index in range(1, 8)],
            json.loads(report_path.read_text()),
        )
    # The character support is longer than 2^18. A shorter FFT silently
    # truncates np.fft input and makes the block objective unrelated to the
    # exported system.
    fft_len = max(65536, 1 << int(math.ceil(math.log2(character.size))))
    stride = design_fft_len // fft_len
    working_magnitude = np.asarray(magnitude)[::stride]
    working_phase = np.asarray(residual_phase)[::stride]
    omega = np.linspace(0.0, np.pi, fft_len // 2 + 1)
    frequency = omega * sample_rate_hz / (2.0 * np.pi)
    pass_indices = jnp.asarray(np.flatnonzero(frequency <= pass_edge_hz), dtype=jnp.int32)
    stop_indices = jnp.asarray(np.flatnonzero(frequency >= stop_edge_hz), dtype=jnp.int32)
    target = jnp.asarray(
        working_magnitude * np.exp(1j * (working_phase - omega * bulk_delay))
    )
    character_response = jnp.asarray(np.fft.rfft(character, n=fft_len))
    omega_jax = jnp.asarray(omega)
    current = [np.asarray(value, dtype=np.float64).copy() for value in cleanup_filters]
    start_cycle = 0
    if resume:
        for completed_cycle in range(cycles, 0, -1):
            cycle_path = work_dir / f"cleanup_cycle_{completed_cycle}.npz"
            if cycle_path.exists():
                saved = np.load(cycle_path)
                current = [
                    np.asarray(saved[f"stage_{index}"], dtype=np.float64)
                    for index in range(1, 8)
                ]
                start_cycle = completed_cycle
                break
    transition_half_widths = (0.035, 0.060, 0.090, 0.175, 0.180, 0.185, 0.190)
    normalized_frequency = omega / np.pi
    cleanup_pass_indices = tuple(
        jnp.asarray(
            np.flatnonzero(normalized_frequency <= 0.5 - half_width),
            dtype=jnp.int32,
        )
        for half_width in transition_half_widths
    )
    cleanup_stop_indices = tuple(
        jnp.asarray(
            np.flatnonzero(normalized_frequency >= 0.5 + half_width),
            dtype=jnp.int32,
        )
        for half_width in transition_half_widths
    )

    def project_cleanup(value: jax.Array) -> jax.Array:
        value = 0.5 * (value + value[::-1])
        center = value.size // 2
        value = value.at[::2].set(0.0)
        value = value.at[center].set(0.5)
        odd_sum = jnp.sum(value[1::2])
        left = center - 1
        right = center + 1
        correction = 0.5 - odd_sum
        value = value.at[left].add(0.5 * correction)
        value = value.at[right].add(0.5 * correction)
        return value

    def system_loss(block: jax.Array, block_index: int, frozen: tuple[jax.Array, ...]):
        filters = list(frozen)
        filters[block_index] = project_cleanup(block)
        accumulated = jnp.ones_like(omega_jax)
        normalized_errors = []
        # Ratio 2x has no cleanup.
        normalized_errors.append(
            jnp.abs(character_response[pass_indices] - target[pass_indices]) / 2.0e-5
        )
        normalized_errors.append(
            jnp.abs(jnp.abs(character_response[pass_indices]) - 1.0) / 1.15e-5
        )
        normalized_errors.append(
            jnp.abs(character_response[stop_indices]) / 3.162277660168379e-8
        )
        for stage, cleanup in enumerate(filters, start=1):
            response = jnp.fft.rfft(cleanup, n=fft_len)
            center = cleanup.size // 2
            zero_phase = jnp.real(response * jnp.exp(1j * omega_jax * center))
            normalized_errors.append(
                jnp.abs(zero_phase[cleanup_pass_indices[stage - 1]] - 1.0) / 2.0e-8
            )
            normalized_errors.append(
                jnp.abs(zero_phase[cleanup_stop_indices[stage - 1]])
                / 3.162277660168379e-8
            )
            stage_zero_phase = jnp.interp(
                omega_jax / (2**stage), omega_jax, zero_phase
            )
            accumulated = accumulated * stage_zero_phase
            composite = character_response * accumulated
            normalized_errors.append(
                jnp.abs(composite[pass_indices] - target[pass_indices]) / 2.0e-5
            )
            normalized_errors.append(
                jnp.abs(jnp.abs(composite[pass_indices]) - 1.0) / 1.15e-5
            )
            normalized_errors.append(
                jnp.abs(composite[stop_indices]) / 3.162277660168379e-8
            )
        normalized = jnp.concatenate(normalized_errors)
        p = 64.0
        return jnp.exp(
            jax.scipy.special.logsumexp(
                p * jnp.log(jnp.maximum(normalized, 1.0e-30))
            )
            / p
        )

    history: list[dict[str, Any]] = []
    for cycle in range(start_cycle, cycles):
        cycle_start = current.copy()
        for block_index in range(7):
            frozen = tuple(jnp.asarray(value) for value in current)
            loss = lambda block, index=block_index, fixed=frozen: system_loss(
                block, index, fixed
            )
            solver = jaxopt.LBFGS(
                fun=loss,
                maxiter=30,
                tol=1.0e-9,
                history_size=8,
                jit=True,
            )
            initial = jnp.asarray(current[block_index])
            before = float(loss(initial))
            result = solver.run(initial)
            candidate = project_cleanup_constraints(
                np.asarray(project_cleanup(result.params), dtype=np.float64)
            )
            after = float(loss(jnp.asarray(candidate)))
            accepted = after <= before
            if accepted:
                current[block_index] = candidate
            history.append(
                {
                    "cycle": cycle,
                    "stage": block_index + 1,
                    "before": before,
                    "after": after,
                    "accepted": accepted,
                    "iterations": int(result.state.iter_num),
                }
            )
        np.savez_compressed(
            work_dir / f"cleanup_cycle_{cycle + 1}.npz",
            **{f"stage_{index}": value for index, value in enumerate(current, start=1)},
        )
        # Each block captures a new frozen system response. Release those JIT
        # executables between cycles so an overnight resume cannot accumulate
        # several copies of the 524288-point working graph and be OOM-killed.
        jax.clear_caches()
        gc.collect()
        maximum_change = max(
            float(np.max(np.abs(after - before)))
            for before, after in zip(cycle_start, current)
        )
        if maximum_change < 1.0e-12:
            break
    current = [project_cleanup_constraints(value) for value in current]
    report = {
        "cycles_requested": cycles,
        "resumed_from_cycle": start_cycle,
        "working_fft_len": fft_len,
        "history": history,
    }
    np.savez_compressed(
        final_path,
        **{f"stage_{index}": value for index, value in enumerate(current, start=1)},
    )
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return current, report
