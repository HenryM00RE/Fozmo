from __future__ import annotations

import numpy as np


def response_and_group_delay(coefficients: np.ndarray, omega: np.ndarray, block: int = 256) -> tuple[np.ndarray, np.ndarray]:
    values = np.asarray(coefficients, dtype=np.float64)
    frequencies = np.asarray(omega, dtype=np.float64)
    response = np.empty(frequencies.size, dtype=np.complex128)
    delay = np.empty(frequencies.size, dtype=np.float64)
    index = np.arange(values.size, dtype=np.float64)
    weighted = index * values
    for start in range(0, frequencies.size, block):
        stop = min(start + block, frequencies.size)
        operator = np.exp(-1j * frequencies[start:stop, None] * index[None, :])
        h = operator @ values
        # H'(omega)=-j sum(n*q[n]*exp(-j*omega*n)); -Im(H'/H)=Re(sum(nqz)/H).
        response[start:stop] = h
        delay[start:stop] = np.real((operator @ weighted) / h)
    return response, delay


def physical_log_derivatives(frequency_hz: np.ndarray, delay_samples: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    coordinate = np.log(np.asarray(frequency_hz, dtype=np.float64))
    slope = np.gradient(delay_samples, coordinate, edge_order=2)
    curvature = np.gradient(slope, coordinate, edge_order=2)
    return slope, curvature
