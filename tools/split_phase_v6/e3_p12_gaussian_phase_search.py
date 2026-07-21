from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p10_packet_contract import (
    PACKET_ABSOLUTE_CEILINGS_DB,
    PACKET_GATES_DB,
    measure_packet_set,
    packet_gate_deltas,
    packet_gate_failures,
)
from .e3_p11_structural_search import (
    _frequency_metrics,
    _timing_with_runtime_step,
)
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    OUTPUT_RATE_HZ,
    _cascade_character_and_cleanup,
    _read_f64le,
)
from .evaluate_e3_packets import PACKET_CYCLES, PACKET_FREQUENCIES_HZ


IDENTITY = "SplitPhase128kE3-P12-packet-aware-gaussian-phase-search"
SOURCE_RATE_HZ = 44_100.0
SEARCH_FFT_LENGTH = 1 << 15
SEARCH_SUPPORT = 8_192
GAUSSIAN_CENTERS_HZ = np.arange(14_000.0, 22_000.1, 500.0)
GAUSSIAN_WIDTHS_HZ = (500.0, 1_000.0, 2_000.0)
DEFAULT_GAUSSIAN_SPECIFICATIONS = tuple(
    (float(width_hz), float(center_hz))
    for width_hz in GAUSSIAN_WIDTHS_HZ
    for center_hz in GAUSSIAN_CENTERS_HZ
)

# The first packet-aware seed already meets all frozen impulse guards, but has
# four small P10 packet-window failures.  It is retained so that the repair is
# reproducible rather than starting from an undocumented optimizer state.
NARROW_SEED_CONTROLS = np.asarray(
    (
        0.0304350177163,
        -0.0890703792519,
        -0.102834743637,
        -0.168279017616,
        -0.0945579266957,
        -0.0343369240122,
        0.047221415084,
        0.197271463839,
        -0.66490467863,
        -0.189239957066,
        -0.231304247258,
        0.0661830573995,
        -0.0637152637519,
        0.000230665566705,
        -0.241319538644,
        0.13263999714,
        1.79091648455,
        -0.460523501807,
        -0.0885073860921,
        -0.0572047849589,
        -0.150362140977,
        -0.127141992713,
        -0.0971790748242,
        -0.0811889488319,
        -0.129733105364,
        -0.192689404506,
        -0.265682401483,
        -0.349592125609,
        -0.399454745795,
        -0.388565442291,
        -0.179273349864,
        0.0476684086146,
        0.660713442415,
        1.45433524524,
        -1.10205639381,
        0.251277024641,
        0.72696757301,
        0.716668376955,
        0.486418637675,
        0.221907615688,
        -0.0396502648226,
        -0.17966275638,
        -0.240990824941,
        -0.267250195541,
        -0.277333777486,
        -0.271152700602,
        -0.226570786759,
        -0.103808682769,
        0.0383640837576,
        0.309886335861,
        0.560055086116,
    ),
    dtype=np.float64,
)

# First exact P10-clean repair.  This is an immutable search anchor, not a
# production asset.  Subsequent objective profiles are always allowed to fall
# back to it if they cannot retain every hard gate.
PACKET_CLEAN_CONTROLS = np.asarray(
    (
        0.045427912503231,
        -0.0884139471636249,
        -0.095828966577592,
        -0.171308545231841,
        -0.106609137726778,
        -0.0256920281198057,
        0.0473122165916748,
        0.226482479612266,
        -0.658091468799519,
        -0.197754568206717,
        -0.227727721799021,
        0.0602205374809884,
        -0.0620209903029192,
        -0.00341173072445948,
        -0.233560524701446,
        0.122206117632892,
        1.77430478931373,
        -0.460071104600701,
        -0.0876377787421425,
        -0.0572602434309003,
        -0.159356638737665,
        -0.136705613682023,
        -0.103391846397014,
        -0.0773499562923326,
        -0.123368792999408,
        -0.189393288833469,
        -0.264184670621028,
        -0.348537410554975,
        -0.4049248957883,
        -0.38945802494378,
        -0.180915690294677,
        0.0453279815151269,
        0.657048377845027,
        1.44658792613296,
        -1.09664436229752,
        0.257211405075632,
        0.734349695190324,
        0.722500503539687,
        0.487379817998219,
        0.221818820218364,
        -0.0395861583425043,
        -0.179224445800678,
        -0.240287928774013,
        -0.266547983169175,
        -0.277078026041881,
        -0.272264726423947,
        -0.234211905486488,
        -0.111078966147609,
        0.0322572119848441,
        0.304744019385978,
        0.555328593448769,
    ),
    dtype=np.float64,
)

HARD_TIMING_GATES = {
    "pre_energy_db_total": -4.85,
    "maximum_pre_lobe_db_peak": -22.5,
    "main_lobe_width_us": 62.5,
    "step_overshoot_percent": 12.8,
    "step_undershoot_percent": 9.5,
    "decay_120_ms": 7.0,
}

SEARCH_STATIC_GATES = {
    "pre_energy_db_total": -4.85,
    "maximum_pre_lobe_db_peak": -22.5,
    "post_energy_db_total": -2.65,
    "maximum_post_lobe_db_peak": -10.65,
    "main_lobe_width_us": 62.5,
    "step_overshoot_percent": 12.8,
    "step_undershoot_percent": 9.5,
    "tail_energy_db_at_4_ms": -120.0,
}

SEARCH_STATIC_SCALES = {
    "pre_energy_db_total": 0.03,
    "maximum_pre_lobe_db_peak": 0.08,
    "post_energy_db_total": 0.03,
    "maximum_post_lobe_db_peak": 0.05,
    "main_lobe_width_us": 0.03,
    "step_overshoot_percent": 0.04,
    "step_undershoot_percent": 0.04,
    "tail_energy_db_at_4_ms": 0.50,
}

OBJECTIVE_PROFILES = {
    "balanced": {
        "post_energy_db_total": 1.0,
        "maximum_post_lobe_db_peak": 1.0,
        "main_lobe_width_us": 0.5,
        "step_undershoot_percent": 0.75,
    },
    "post": {
        "post_energy_db_total": 1.5,
        "maximum_post_lobe_db_peak": 2.0,
        "main_lobe_width_us": 0.15,
        "step_undershoot_percent": 0.35,
    },
    "localization": {
        "post_energy_db_total": 0.5,
        "maximum_post_lobe_db_peak": 0.75,
        "main_lobe_width_us": 1.5,
        "step_undershoot_percent": 1.0,
    },
    "post_energy": {
        "post_energy_db_total": 3.0,
        "maximum_post_lobe_db_peak": 0.5,
        "main_lobe_width_us": 0.2,
        "step_undershoot_percent": 0.2,
    },
    "post_lobe": {
        "post_energy_db_total": 0.3,
        "maximum_post_lobe_db_peak": 3.0,
        "main_lobe_width_us": 0.2,
        "step_undershoot_percent": 0.2,
    },
    "undershoot": {
        "post_energy_db_total": 0.5,
        "maximum_post_lobe_db_peak": 0.5,
        "main_lobe_width_us": 0.25,
        "step_undershoot_percent": 3.0,
    },
    "multiresolution": {
        "post_energy_db_total": 1.5,
        "maximum_post_lobe_db_peak": 2.0,
        "main_lobe_width_us": 0.6,
        "step_undershoot_percent": 0.8,
    },
    "replacement": {
        "post_energy_db_total": 4.0,
        "maximum_post_lobe_db_peak": 4.0,
        "main_lobe_width_us": 0.3,
        "step_undershoot_percent": 0.3,
    },
}

OBJECTIVE_SCALES = {
    "post_energy_db_total": 0.20,
    "maximum_post_lobe_db_peak": 0.40,
    "main_lobe_width_us": 0.50,
    "step_undershoot_percent": 0.20,
}


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _write_json_lf(path: Path, value: Any) -> None:
    path.write_bytes((json.dumps(value, indent=2) + "\n").encode("utf-8"))


def multiresolution_phase_specifications() -> tuple[tuple[float, float], ...]:
    return tuple(
        (float(width_hz), float(center_hz))
        for width_hz, step_hz in (
            (250.0, 250.0),
            (500.0, 250.0),
            (1_000.0, 500.0),
            (2_000.0, 500.0),
            (4_000.0, 500.0),
        )
        for center_hz in np.arange(12_000.0, 22_000.1, step_hz)
    )


def gaussian_phase_basis(
    frequency_hz: np.ndarray,
    specifications: tuple[tuple[float, float], ...] = DEFAULT_GAUSSIAN_SPECIFICATIONS,
) -> np.ndarray:
    """Return smooth endpoint-closed phase directions in deterministic order."""
    frequency = np.asarray(frequency_hz, dtype=np.float64)
    if frequency.ndim != 1 or frequency.size < 2:
        raise ValueError("frequency grid must be one dimensional")
    nyquist = float(frequency[-1])
    if nyquist <= 0.0:
        raise ValueError("frequency grid must end above zero")
    rows = []
    for width_hz, center_hz in specifications:
        if width_hz <= 0.0:
            raise ValueError("Gaussian widths must be positive")
        row = np.exp(-0.5 * ((frequency - center_hz) / width_hz) ** 2)
        row -= row[0] + (row[-1] - row[0]) * frequency / nyquist
        rows.append(row)
    return np.asarray(rows, dtype=np.float64)


def realize_character(
    baseline: np.ndarray,
    controls: np.ndarray,
    *,
    fft_length: int = FFT_LENGTH,
    support: int | None = None,
    transition_shift_hz: float = 0.0,
    basis_specifications: tuple[
        tuple[float, float], ...
    ] = DEFAULT_GAUSSIAN_SPECIFICATIONS,
) -> tuple[np.ndarray, dict[str, float]]:
    baseline = np.asarray(baseline, dtype=np.float64)
    controls = np.asarray(controls, dtype=np.float64)
    if support is None:
        support = int(baseline.size)
    expected = len(basis_specifications)
    if controls.shape != (expected,):
        raise ValueError(f"expected {expected} Gaussian controls")
    if fft_length < max(baseline.size, support):
        raise ValueError("FFT length is shorter than the requested realization")

    frequency_hz = np.fft.rfftfreq(fft_length, 1.0 / CHARACTER_RATE_HZ)
    phase_delta = controls @ gaussian_phase_basis(
        frequency_hz, basis_specifications
    )
    baseline_spectrum = np.fft.rfft(baseline, fft_length)
    if transition_shift_hz:
        magnitude_db = 20.0 * np.log10(
            np.maximum(np.abs(baseline_spectrum), 1.0e-300)
        )
        shifted_magnitude_db = np.interp(
            frequency_hz - transition_shift_hz,
            frequency_hz,
            magnitude_db,
            left=float(magnitude_db[0]),
            right=float(magnitude_db[-1]),
        )
        baseline_spectrum = np.power(10.0, shifted_magnitude_db / 20.0) * np.exp(
            1j * np.angle(baseline_spectrum)
        )
    target = baseline_spectrum * np.exp(1j * phase_delta)
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, fft_length)
    character = periodic[:support].copy()
    baseline_sum = float(math.fsum(float(value) for value in baseline))
    character *= baseline_sum / float(
        math.fsum(float(value) for value in character)
    )
    total = max(float(np.dot(periodic, periodic)), 1.0e-300)
    omitted = float(np.dot(periodic[support:], periodic[support:])) / total
    return character, {
        "fft_length": int(fft_length),
        "support": int(support),
        "maximum_absolute_phase_delta_rad": float(np.max(np.abs(phase_delta))),
        "transition_shift_hz": float(transition_shift_hz),
        "basis_direction_count": len(basis_specifications),
        "omitted_periodic_energy_ratio": omitted,
        "character_sum": float(math.fsum(float(value) for value in character)),
    }


def _timing_failures(timing: dict[str, Any]) -> list[str]:
    failures = []
    for metric, limit in HARD_TIMING_GATES.items():
        value = timing[metric]
        if value is not None and float(value) > limit + 1.0e-12:
            failures.append(f"timing/{metric}")
    return failures


def restarted_carrier_measurement(
    response: np.ndarray, baseline_response: np.ndarray
) -> dict[str, Any]:
    from .e3_p6_restarted_carrier_search import RestartedCarrierProbe, _reference_probe

    probe = RestartedCarrierProbe()
    baseline_envelope, baseline_rms, _ = _reference_probe(
        probe, baseline_response
    )
    _, interval_rms, interval_excess = _reference_probe(
        probe, response, baseline_envelope
    )
    return {
        "interval_rms_dbfs": interval_rms,
        "interval_rms_delta_db_vs_e2v3": [
            value - reference
            for value, reference in zip(interval_rms, baseline_rms, strict=True)
        ],
        "positive_excess_power_linear_seconds_vs_e2v3": interval_excess,
        "total_positive_excess_power_linear_seconds_vs_e2v3": float(
            math.fsum(interval_excess)
        ),
    }


def evaluate_exact(
    root: Path,
    controls: np.ndarray,
    *,
    transition_shift_hz: float = 0.0,
    basis_specifications: tuple[
        tuple[float, float], ...
    ] = DEFAULT_GAUSSIAN_SPECIFICATIONS,
) -> tuple[dict[str, Any], np.ndarray]:
    asset_dir = root / "assets/filters/split_phase_e2v3"
    baseline = _read_f64le(asset_dir / "character_full_rate.f64le")
    cleanup = _read_f64le(asset_dir / "cleanup_stage_1.f64le")
    character, realization = realize_character(
        baseline,
        controls,
        transition_shift_hz=transition_shift_hz,
        basis_specifications=basis_specifications,
    )
    response = _cascade_character_and_cleanup(character, cleanup)
    baseline_response = _cascade_character_and_cleanup(baseline, cleanup)
    reference_packets = measure_packet_set(baseline_response)
    packets = measure_packet_set(response)
    timing = _timing_with_runtime_step(response)
    packet_failures = packet_gate_failures(packets, reference_packets)
    timing_failures = _timing_failures(timing)
    payload = np.asarray(character, dtype="<f8").tobytes()
    return (
        {
            "character_sha256": _sha256_bytes(payload),
            "controls": np.asarray(controls, dtype=np.float64).tolist(),
            "realization": realization,
            "timing": timing,
            "timing_failures": timing_failures,
            "packets": packets,
            "packet_gate_delta_db_vs_e2v3": packet_gate_deltas(
                packets, reference_packets
            ),
            "packet_failures": packet_failures,
            "restarted_carrier": restarted_carrier_measurement(
                response, baseline_response
            ),
            "frequency": _frequency_metrics(response),
            "passes_exact_static_packet_frequency_gates": not (
                timing_failures or packet_failures
            ),
        },
        character,
    )


def _exact_objective(record: dict[str, Any], profile: dict[str, float]) -> float:
    timing = record["timing"]
    return float(
        math.fsum(
            profile[metric] * timing[metric] / OBJECTIVE_SCALES[metric]
            for metric in profile
        )
    )


def _packet_targets(reference: dict[str, dict[str, float]]) -> list[float]:
    return [
        max(
            reference[frequency][metric] + relative_tolerance,
            PACKET_ABSOLUTE_CEILINGS_DB.get(metric, -300.0),
        )
        for frequency in reference
        for metric, relative_tolerance in PACKET_GATES_DB.items()
    ]


def optimize(
    root: Path,
    output_dir: Path,
    *,
    iterations: int = 1_200,
    profile_names: tuple[str, ...] = tuple(OBJECTIVE_PROFILES),
    transition_shift_hz: float = 0.0,
    initial_controls: np.ndarray | None = None,
    basis_specifications: tuple[
        tuple[float, float], ...
    ] = DEFAULT_GAUSSIAN_SPECIFICATIONS,
    search_static_gates: dict[str, float] = SEARCH_STATIC_GATES,
    candidate_prefix: str = "p12",
    report_filename: str = "e3_p12_gaussian_phase_search.json",
    identity: str = IDENTITY,
    learning_rate: float = 0.0015,
    regularization: float = 1.0e-4,
    learning_rate_milestones: tuple[float, ...] = (0.5, 0.75),
    training_packet_cells: tuple[tuple[float, float], ...] | None = None,
    restart_excess_target_power_seconds: float | None = None,
) -> dict[str, Any]:
    """Run deterministic differentiable screening, followed by exact checks.

    PyTorch is imported lazily because exact realization and regression tests do
    not require it.  Only exact million-point realizations can enter the report's
    qualified set; compact-model results are screening data.
    """
    try:
        import torch
    except ImportError as error:  # pragma: no cover - environment dependent
        raise RuntimeError("P12 optimization requires PyTorch") from error

    unknown = sorted(set(profile_names) - set(OBJECTIVE_PROFILES))
    if unknown:
        raise ValueError(f"unknown objective profiles: {', '.join(unknown)}")
    if not profile_names:
        raise ValueError("at least one objective profile is required")
    if iterations <= 0:
        raise ValueError("iterations must be positive")
    if learning_rate <= 0.0 or regularization < 0.0:
        raise ValueError("invalid optimizer hyperparameters")
    if any(not 0.0 < value < 1.0 for value in learning_rate_milestones):
        raise ValueError("learning-rate milestones must be inside (0, 1)")
    if (
        restart_excess_target_power_seconds is not None
        and restart_excess_target_power_seconds <= 0.0
    ):
        raise ValueError("restart excess target must be positive")
    if initial_controls is None:
        if basis_specifications == DEFAULT_GAUSSIAN_SPECIFICATIONS:
            initial_controls = PACKET_CLEAN_CONTROLS
        else:
            initial_controls = np.zeros(len(basis_specifications), dtype=np.float64)
    initial_controls = np.asarray(initial_controls, dtype=np.float64)
    if initial_controls.shape != (len(basis_specifications),):
        raise ValueError("initial controls do not match the Gaussian basis")
    if tuple(search_static_gates) != tuple(SEARCH_STATIC_GATES):
        raise ValueError("search static gate names or ordering changed")

    torch.use_deterministic_algorithms(True)
    torch.set_default_dtype(torch.float64)
    torch.manual_seed(0)
    asset_dir = root / "assets/filters/split_phase_e2v3"
    baseline = _read_f64le(asset_dir / "character_full_rate.f64le")
    cleanup_np = _read_f64le(asset_dir / "cleanup_stage_1.f64le")
    baseline_response = _cascade_character_and_cleanup(baseline, cleanup_np)
    reference_packets = measure_packet_set(baseline_response)
    if training_packet_cells is None:
        training_packet_cells = tuple(
            (float(frequency), float(PACKET_CYCLES))
            for frequency in PACKET_FREQUENCIES_HZ
        )
    if not training_packet_cells:
        raise ValueError("at least one packet cell is required")

    frequency_hz = np.fft.rfftfreq(
        SEARCH_FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ
    )
    basis = torch.as_tensor(
        gaussian_phase_basis(frequency_hz, basis_specifications)
    )
    raw_baseline_spectrum = np.fft.rfft(
        np.asarray(baseline[:SEARCH_SUPPORT], dtype=np.float64),
        SEARCH_FFT_LENGTH,
    )
    if transition_shift_hz:
        raw_magnitude_db = 20.0 * np.log10(
            np.maximum(np.abs(raw_baseline_spectrum), 1.0e-300)
        )
        shifted_magnitude_db = np.interp(
            frequency_hz - transition_shift_hz,
            frequency_hz,
            raw_magnitude_db,
            left=float(raw_magnitude_db[0]),
            right=float(raw_magnitude_db[-1]),
        )
        raw_baseline_spectrum = np.power(
            10.0, shifted_magnitude_db / 20.0
        ) * np.exp(1j * np.angle(raw_baseline_spectrum))
    baseline_spectrum = torch.as_tensor(raw_baseline_spectrum)
    cleanup = torch.as_tensor(cleanup_np)
    baseline_sum = float(math.fsum(float(value) for value in baseline))
    packet_target_values = []
    packet_sample_counts = []
    packet_i_spectra = []
    packet_q_spectra = []
    for frequency, cycles in training_packet_cells:
        reference = measure_packet_set(
            baseline_response, (frequency,), cycles
        )
        packet_target_values.extend(_packet_targets(reference))
        samples = max(round(cycles / frequency * SOURCE_RATE_HZ), 3)
        index = np.arange(samples, dtype=np.float64)
        window = 0.5 - 0.5 * np.cos(2.0 * np.pi * index / (samples - 1))
        phase = 2.0 * np.pi * frequency * index / SOURCE_RATE_HZ
        spectra = []
        for source in (window * np.cos(phase), window * np.sin(phase)):
            upsampled = np.zeros(SEARCH_FFT_LENGTH, dtype=np.float64)
            upsampled[: (samples - 1) * 4 + 1 : 4] = source
            spectra.append(torch.as_tensor(np.fft.rfft(upsampled)))
        packet_sample_counts.append(samples)
        packet_i_spectra.append(spectra[0])
        packet_q_spectra.append(spectra[1])
    packet_targets = torch.as_tensor(packet_target_values)
    packet_i_spectra_tensor = torch.stack(packet_i_spectra)
    packet_q_spectra_tensor = torch.stack(packet_q_spectra)

    restart_source_spectrum = None
    restart_reference_envelope = None
    restart_output_start = 0
    restart_output_count = 0
    restart_window_samples = 0
    if restart_excess_target_power_seconds is not None:
        from .e3_p6_restarted_carrier_search import (
            FULL_RATE_ORIGIN,
            SOURCE_PHASES_RAD,
            TOLERANCE_RMS,
            RestartedCarrierProbe,
        )

        probe = RestartedCarrierProbe()
        mute_frames = 2_048
        source_index = np.arange(-mute_frames, 0, dtype=np.float64)
        missing = np.zeros(mute_frames, dtype=np.float64)
        for frequency_hz, phase_rad in zip(
            probe.frequencies_hz, SOURCE_PHASES_RAD, strict=True
        ):
            missing -= probe.carrier_amplitude * np.sin(
                2.0 * np.pi * frequency_hz * source_index / SOURCE_RATE_HZ
                + phase_rad
            )
        upsampled_missing = np.zeros(SEARCH_FFT_LENGTH, dtype=np.float64)
        upsampled_missing[: (mute_frames - 1) * 4 + 1 : 4] = missing
        restart_source_spectrum = torch.as_tensor(
            np.fft.rfft(upsampled_missing)
        )
        restart_reference_envelope = torch.as_tensor(
            probe.envelope(probe.residual(baseline_response))
        )
        restart_output_start = mute_frames * 4 + FULL_RATE_ORIGIN
        restart_output_count = probe.residual_samples
        restart_window_samples = probe.window_samples
        restart_tolerance_power = TOLERANCE_RMS**2

    def response(controls):
        phase_delta = controls @ basis
        character = torch.fft.irfft(
            baseline_spectrum * torch.exp(1j * phase_delta),
            n=SEARCH_FFT_LENGTH,
        )[:SEARCH_SUPPORT]
        character = character * (baseline_sum / torch.sum(character))
        upsampled = torch.zeros(2 * SEARCH_SUPPORT - 1)
        upsampled[::2] = 2.0 * character
        output_length = upsampled.numel() + cleanup.numel() - 1
        return torch.fft.irfft(
            torch.fft.rfft(upsampled, n=SEARCH_FFT_LENGTH)
            * torch.fft.rfft(2.0 * cleanup, n=SEARCH_FFT_LENGTH),
            n=SEARCH_FFT_LENGTH,
        )[:output_length]

    with torch.no_grad():
        seed_response = response(torch.as_tensor(initial_controls))
        peak_index = int(torch.argmax(torch.abs(seed_response)))
        sign = torch.sign(seed_response[peak_index])
        left_index = peak_index
        while (
            left_index > 0
            and seed_response[left_index - 1] != 0.0
            and torch.sign(seed_response[left_index - 1]) == sign
        ):
            left_index -= 1
        right_index = peak_index
        while (
            right_index + 1 < seed_response.numel()
            and seed_response[right_index + 1] != 0.0
            and torch.sign(seed_response[right_index + 1]) == sign
        ):
            right_index += 1

    static_names = tuple(search_static_gates)
    static_targets = torch.as_tensor(
        [search_static_gates[name] for name in static_names]
    )
    static_scales = torch.as_tensor(
        [SEARCH_STATIC_SCALES[name] for name in static_names]
    )

    def differentiable_metrics(controls):
        value = response(controls)
        peak_squared = value[peak_index] ** 2
        total = torch.sum(value**2)
        pre_energy = 10.0 * torch.log10(torch.sum(value[:peak_index] ** 2) / total)
        post_energy = 10.0 * torch.log10(
            torch.sum(value[peak_index + 1 :] ** 2) / total
        )
        pre_lobe = 10.0 * torch.log10(
            torch.max(value[:left_index] ** 2) / peak_squared
        )
        post_lobe = 10.0 * torch.log10(
            torch.max(value[right_index + 1 :] ** 2) / peak_squared
        )
        left = left_index - 1 + torch.abs(value[left_index - 1]) / (
            torch.abs(value[left_index - 1]) + torch.abs(value[left_index])
        )
        right = right_index + torch.abs(value[right_index]) / (
            torch.abs(value[right_index]) + torch.abs(value[right_index + 1])
        )
        width = (right - left) / OUTPUT_RATE_HZ * 1_000_000.0
        branches = [torch.cumsum(value[phase::4], dim=0) for phase in range(4)]
        settled = torch.stack([branch[-1] for branch in branches]).mean()
        maximum = torch.stack([branch.max() for branch in branches]).max()
        minimum = torch.stack([branch.min() for branch in branches]).min()
        overshoot = torch.relu(maximum / settled - 1.0) * 100.0
        undershoot = torch.relu(-minimum / settled) * 100.0
        tail_start = peak_index + math.ceil(0.004 * OUTPUT_RATE_HZ)
        tail_4ms = 10.0 * torch.log10(torch.sum(value[tail_start:] ** 2) / total)
        static = torch.stack(
            (
                pre_energy,
                pre_lobe,
                post_energy,
                post_lobe,
                width,
                overshoot,
                undershoot,
                tail_4ms,
            )
        )

        response_spectrum = torch.fft.rfft(value, n=SEARCH_FFT_LENGTH)
        output_i_all = torch.fft.irfft(
            packet_i_spectra_tensor * response_spectrum[None, :],
            n=SEARCH_FFT_LENGTH,
        )
        output_q_all = torch.fft.irfft(
            packet_q_spectra_tensor * response_spectrum[None, :],
            n=SEARCH_FFT_LENGTH,
        )
        packet_values = []
        for cell_index, samples in enumerate(packet_sample_counts):
            output_length = (samples - 1) * 4 + 1 + value.numel() - 1
            output_i = output_i_all[cell_index, :output_length]
            output_q = output_q_all[cell_index, :output_length]
            envelope_squared = output_i**2 + output_q**2
            energy = torch.sum(envelope_squared)
            peak = torch.max(envelope_squared)
            values = [
                10.0
                * torch.log10(torch.max(envelope_squared[:peak_index]) / peak)
            ]
            for near_ms, far_ms in ((0.0, 0.5), (0.5, 2.0), (2.0, 8.0)):
                start = max(
                    peak_index - round(far_ms / 1000.0 * OUTPUT_RATE_HZ), 0
                )
                end = max(
                    peak_index - round(near_ms / 1000.0 * OUTPUT_RATE_HZ), 0
                )
                values.append(
                    10.0
                    * torch.log10(torch.sum(envelope_squared[start:end]) / energy)
                )
            packet_values.extend(values)
        restart_excess = None
        if restart_source_spectrum is not None:
            restart_convolution = torch.fft.irfft(
                restart_source_spectrum * response_spectrum,
                n=SEARCH_FFT_LENGTH,
            )
            restart_residual = restart_convolution[
                restart_output_start : restart_output_start + restart_output_count
            ]
            restart_envelope = torch.nn.functional.avg_pool1d(
                restart_residual.square()[None, None, :],
                kernel_size=restart_window_samples,
                stride=1,
            )[0, 0]
            restart_excess = torch.relu(
                restart_envelope
                - restart_reference_envelope
                - restart_tolerance_power
            ).sum() / OUTPUT_RATE_HZ
        return static, torch.stack(packet_values), restart_excess

    shift_suffix = (
        "" if transition_shift_hz == 0.0 else f"-shift{transition_shift_hz:+g}hz"
    )
    profile_records = []
    milestone_iterations = {
        round(iterations * fraction) for fraction in learning_rate_milestones
    }
    for profile_name in profile_names:
        profile = OBJECTIVE_PROFILES[profile_name]
        controls = torch.as_tensor(initial_controls.copy()).requires_grad_(True)
        optimizer = torch.optim.Adam((controls,), lr=learning_rate)
        best_controls = controls.detach().clone()
        best_score = math.inf
        feasible_iterations = 0
        closest_controls = controls.detach().clone()
        closest_violation = math.inf
        for iteration in range(iterations + 1):
            optimizer.zero_grad()
            static, packet_values, restart_excess = differentiable_metrics(controls)
            static_margins = static - static_targets
            packet_margins = packet_values - packet_targets
            # Soft barriers retain useful gradients just inside a boundary.  A
            # small cushion prevents compact/exact rounding from deciding gates.
            static_barrier = torch.nn.functional.softplus(
                4.0 * (static_margins + 0.01) / static_scales
            ) / 4.0
            packet_barrier = torch.nn.functional.softplus(
                4.0 * (packet_margins + 0.02) / 0.05
            ) / 4.0
            loss = 40.0 * (
                torch.sum(static_barrier**2) + torch.sum(packet_barrier**2)
            )
            if restart_excess is not None:
                restart_margin = (
                    restart_excess / restart_excess_target_power_seconds - 1.0
                )
                restart_barrier = torch.nn.functional.softplus(
                    3.0 * (restart_margin + 0.02) / 0.10
                ) / 3.0
                loss = loss + 80.0 * restart_barrier**2
            for metric, weight in profile.items():
                index = static_names.index(metric)
                loss = loss + weight * static[index] / OBJECTIVE_SCALES[metric]
            loss = loss + regularization * torch.sum(
                (controls - torch.as_tensor(initial_controls)) ** 2
            )
            loss.backward()
            torch.nn.utils.clip_grad_norm_((controls,), 100.0)
            optimizer.step()
            if iteration in milestone_iterations:
                for group in optimizer.param_groups:
                    group["lr"] *= 0.35

            with torch.no_grad():
                static, packet_values, restart_excess = differentiable_metrics(
                    controls
                )
                feasible = bool(
                    torch.max(static - static_targets) <= 0.0
                    and torch.max(packet_values - packet_targets) <= 0.0
                    and (
                        restart_excess is None
                        or restart_excess
                        <= restart_excess_target_power_seconds
                    )
                )
                normalized_violations = [
                    float(torch.max((static - static_targets) / static_scales)),
                    float(torch.max((packet_values - packet_targets) / 0.05)),
                ]
                if restart_excess is not None:
                    normalized_violations.append(
                        float(
                            restart_excess
                            / restart_excess_target_power_seconds
                            - 1.0
                        )
                    )
                violation = max(normalized_violations)
                if violation < closest_violation:
                    closest_violation = violation
                    closest_controls = controls.detach().clone()
                if feasible:
                    feasible_iterations += 1
                    score = float(
                        math.fsum(
                            profile[metric]
                            * float(static[static_names.index(metric)])
                            / OBJECTIVE_SCALES[metric]
                            for metric in profile
                        )
                    )
                    if score < best_score:
                        best_score = score
                        best_controls = controls.detach().clone()

        if feasible_iterations == 0:
            best_controls = closest_controls

        exact, character = evaluate_exact(
            root,
            best_controls.numpy(),
            transition_shift_hz=transition_shift_hz,
            basis_specifications=basis_specifications,
        )
        exact["identifier"] = f"{candidate_prefix}-{profile_name}{shift_suffix}"
        exact["profile"] = profile
        exact["compact_feasible_iterations"] = feasible_iterations
        exact["compact_best_score"] = best_score
        exact["compact_closest_normalized_violation"] = closest_violation
        exact["exact_objective"] = _exact_objective(exact, profile)
        exact_restart = exact["restarted_carrier"][
            "total_positive_excess_power_linear_seconds_vs_e2v3"
        ]
        exact["passes_exact_restart_excess_gate"] = bool(
            restart_excess_target_power_seconds is None
            or exact_restart <= restart_excess_target_power_seconds
        )
        output_dir.mkdir(parents=True, exist_ok=True)
        payload = np.asarray(character, dtype="<f8").tobytes()
        (
            output_dir
            / f"{candidate_prefix}-{profile_name}{shift_suffix}.character.f64le"
        ).write_bytes(
            payload
        )
        profile_records.append(exact)

    anchor, anchor_character = evaluate_exact(
        root,
        initial_controls,
        transition_shift_hz=transition_shift_hz,
        basis_specifications=basis_specifications,
    )
    anchor["identifier"] = f"{candidate_prefix}-search-anchor{shift_suffix}"
    output_dir.mkdir(parents=True, exist_ok=True)
    (
        output_dir
        / f"{candidate_prefix}-search-anchor{shift_suffix}.character.f64le"
    ).write_bytes(
        np.asarray(anchor_character, dtype="<f8").tobytes()
    )
    qualified = [
        record
        for record in profile_records
        if record["passes_exact_static_packet_frequency_gates"]
        and record["passes_exact_restart_excess_gate"]
    ]
    report = {
        "schema_version": 1,
        "identity": identity,
        "production_promoted": False,
        "hypothesis": (
            "A smooth redundant Gaussian phase basis can redistribute E2v3's "
            "upper-band group delay while preserving its exact magnitude. "
            "Putting the P10 onset windows and native polyphase step directly "
            "in the optimizer can retain musical-transient safety while moving "
            "the post-ringing frontier."
        ),
        "contract": {
            "search_fft_length": SEARCH_FFT_LENGTH,
            "search_support": SEARCH_SUPPORT,
            "exact_fft_length": FFT_LENGTH,
            "exact_support": int(anchor_character.size),
            "basis_specifications": [list(item) for item in basis_specifications],
            "hard_timing_gates": HARD_TIMING_GATES,
            "search_static_gates": search_static_gates,
            "packet_relative_gates_db": PACKET_GATES_DB,
            "packet_absolute_ceilings_db": PACKET_ABSOLUTE_CEILINGS_DB,
            "iterations_per_profile": iterations,
            "profiles": list(profile_names),
            "transition_shift_hz": float(transition_shift_hz),
            "initial_controls": initial_controls.tolist(),
            "learning_rate": learning_rate,
            "regularization": regularization,
            "learning_rate_milestones": list(learning_rate_milestones),
            "training_packet_cells": [
                {"frequency_hz": frequency, "cycles": cycles}
                for frequency, cycles in training_packet_cells
            ],
            "restart_excess_target_power_seconds": (
                restart_excess_target_power_seconds
            ),
            "deterministic_torch_algorithms": True,
        },
        "packet_clean_anchor": anchor,
        "profiles": profile_records,
        "exact_qualified_count": len(qualified),
        "best_qualified_by_profile": {
            profile_name: min(
                (
                    record
                    for record in qualified
                    if record["profile"] == OBJECTIVE_PROFILES[profile_name]
                ),
                key=lambda record: record["exact_objective"],
                default=None,
            )["identifier"]
            if any(
                record["profile"] == OBJECTIVE_PROFILES[profile_name]
                for record in qualified
            )
            else None
            for profile_name in profile_names
        },
    }
    _write_json_lf(output_dir / report_filename, report)
    return report


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run the E3 P12 packet-aware Gaussian phase search"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p12/gaussian-phase",
    )
    parser.add_argument("--iterations", type=int, default=1_200)
    parser.add_argument("--transition-shift-hz", type=float, default=0.0)
    parser.add_argument(
        "--profiles",
        nargs="+",
        choices=tuple(OBJECTIVE_PROFILES),
        default=list(OBJECTIVE_PROFILES),
    )
    arguments = parser.parse_args()
    report = optimize(
        arguments.root.resolve(),
        arguments.output_dir.resolve(),
        iterations=arguments.iterations,
        profile_names=tuple(arguments.profiles),
        transition_shift_hz=arguments.transition_shift_hz,
    )
    print(
        json.dumps(
            {
                "output": str(
                    arguments.output_dir / "e3_p12_gaussian_phase_search.json"
                ),
                "exact_qualified_count": report["exact_qualified_count"],
                "best_qualified_by_profile": report["best_qualified_by_profile"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
