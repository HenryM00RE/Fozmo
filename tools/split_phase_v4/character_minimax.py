from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import numpy as np
from scipy.sparse.linalg import LinearOperator, cg


def project_character_equalities(coefficients: np.ndarray) -> np.ndarray:
    result = np.asarray(coefficients, dtype=np.float64).copy()
    for parity in (0, 1):
        view = result[parity::2]
        index = int(np.argmax(np.abs(view)))
        for _ in range(4):
            view[index] += 0.5 - math.fsum(float(value) for value in view)
    return result


def _project_zero_character_equalities(coefficients: np.ndarray) -> np.ndarray:
    """Orthogonally project a correction onto the two parity-sum nullspaces."""
    result = np.asarray(coefficients, dtype=np.float64).copy()
    for parity in (0, 1):
        view = result[parity::2]
        view -= np.sum(view, dtype=np.float64) / view.size
    return result


def _dense_score(coefficients: np.ndarray, target: np.ndarray, fft_len: int) -> tuple[float, ...]:
    response = np.fft.rfft(coefficients, n=fft_len)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    passband = frequency <= 20_000.0
    stopband = frequency >= 22_050.0
    complex_error = float(np.max(np.abs(response[passband] - target[passband])))
    stop_peak = float(np.max(np.abs(response[stopband])))
    peak = int(np.argmax(np.abs(coefficients)))
    total = max(float(np.dot(coefficients, coefficients)), 1.0e-300)
    pre = float(np.dot(coefficients[:peak], coefficients[:peak])) / total
    edge = float(np.dot(coefficients[:2048], coefficients[:2048]) + np.dot(coefficients[-2048:], coefficients[-2048:])) / total
    hard_complex = max(complex_error / 8.0e-9 - 1.0, 0.0)
    hard_stop = max(stop_peak / 1.0e-8 - 1.0, 0.0)
    return (
        max(hard_complex, hard_stop),
        hard_complex + hard_stop,
        hard_complex,
        hard_stop,
        complex_error,
        pre,
        edge,
        stop_peak,
    )


def matrix_free_lawson(
    initial: np.ndarray,
    target: np.ndarray,
    fft_len: int,
    iterations: int = 40,
    trust_radius: float = 5.0e-4,
) -> tuple[np.ndarray, dict[str, Any]]:
    if target.size != fft_len // 2 + 1:
        raise ValueError("target spectrum and FFT length disagree")
    incumbent = project_character_equalities(initial)
    incumbent_score = _dense_score(incumbent, target, fft_len)
    history = []
    frequency = np.linspace(0.0, 44_100.0, target.size)
    design = frequency <= 20_000.0
    stopband = frequency >= 22_050.0
    design_or_stop = design | stopband
    for iteration in range(iterations):
        response = np.fft.rfft(incumbent, n=fft_len)
        desired = target.copy()
        desired[stopband] = 0.0
        residual = desired - response
        active_error = np.abs(residual)
        normalized_error = np.zeros(target.size, dtype=np.float64)
        normalized_error[design] = active_error[design] / 8.0e-9
        normalized_error[stopband] = active_error[stopband] / 1.0e-8
        weight = np.zeros(target.size, dtype=np.float64)
        weight[design_or_stop] = np.maximum(normalized_error[design_or_stop], 0.05) ** 4
        weight /= max(float(np.max(weight[design_or_stop])), 1.0e-300)

        # Solve the Lawson weighted least-squares subproblem rather than taking a
        # single truncated-IFFT gradient step.  The latter stalls when passband
        # and stopband peaks need to move in opposite directions.  rfft/irfft
        # provide both products, and the correction remains in the exact
        # parity-sum nullspace throughout CG.
        def adjoint(spectrum: np.ndarray) -> np.ndarray:
            return np.fft.irfft(spectrum, n=fft_len)[: incumbent.size] * fft_len

        right_hand_side = _project_zero_character_equalities(adjoint(weight * residual))
        regularization = 1.0e-7 * float(np.mean(weight[design_or_stop])) * fft_len

        def normal_product(vector: np.ndarray) -> np.ndarray:
            projected = _project_zero_character_equalities(vector)
            product = adjoint(weight * np.fft.rfft(projected, n=fft_len))
            return _project_zero_character_equalities(product) + regularization * projected

        operator = LinearOperator(
            (incumbent.size, incumbent.size),
            matvec=normal_product,
            dtype=np.float64,
        )
        correction, cg_info = cg(
            operator,
            right_hand_side,
            rtol=1.0e-4,
            atol=1.0e-14,
            maxiter=80,
        )
        correction_norm = float(np.linalg.norm(correction))
        if correction_norm > trust_radius:
            correction *= trust_radius / correction_norm
        accepted = False
        candidate_score = incumbent_score
        step = 1.0
        for _ in range(12):
            candidate = project_character_equalities(incumbent + step * correction)
            score = _dense_score(candidate, target, fft_len)
            if score < incumbent_score:
                incumbent = candidate
                incumbent_score = score
                candidate_score = score
                accepted = True
                break
            step *= 0.5
        history.append({
            "iteration": iteration + 1,
            "accepted": accepted,
            "step": step,
            "score": list(candidate_score),
            "active_maximum_error": float(np.max(active_error[design_or_stop])),
            "maximum_normalized_error": float(np.max(normalized_error[design_or_stop])),
            "lawson_weight_minimum": float(np.min(weight[design_or_stop])),
            "weighted_least_squares_cg_info": int(cg_info),
            "weighted_least_squares_cg_max_iterations": 80,
        })
        if not accepted:
            trust_radius *= 0.5
        if trust_radius < 1.0e-12:
            break
    report = {
        "method": "matrix-free constrained Lawson IRLS with CG-solved FFT normal products",
        "post_normalization": False,
        "best_feasible_incumbent_retained": True,
        "iterations": history,
        "final_score": list(incumbent_score),
        "canonical_even_sum": math.fsum(float(value) for value in incumbent[::2]),
        "canonical_odd_sum": math.fsum(float(value) for value in incumbent[1::2]),
    }
    return incumbent, report


def save(coefficients: np.ndarray, report: dict[str, Any], work_dir: Path) -> None:
    work_dir.mkdir(parents=True, exist_ok=True)
    np.save(work_dir / "character_optimized.npy", coefficients)
    (work_dir / "character_minimax.json").write_text(json.dumps(report, indent=2) + "\n")
