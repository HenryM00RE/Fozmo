# Split Phase DSD Measurements

These software tests cover Split Phase with **7th Order** at −4 dB headroom
and **7th Order Search** at −2 dB. They measure the generated digital stream,
not the analog output of a DAC. Every supported combination completed without
processing errors.

Higher SINAD is better, gain error should be close to zero, and more-negative
noise or spur values are quieter. Each row uses the less favourable result
from the two channels.

## Level test

This checks loud, quiet and extremely quiet tones.

| Rate | Mode | Level dBFS | SINAD dB | Gain error dB | Unexpected spur dBFS | Residual dBFS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| DSD64 | 7th Order | −6 | 131.37 | 0.0000001 | −156.63 | −137.39 |
| DSD64 | 7th Order | −20 | 118.70 | 0.0000002 | −157.80 | −138.71 |
| DSD64 | 7th Order | −60 | 77.98 | 0.0000323 | −157.35 | −137.99 |
| DSD64 | 7th Order | −100 | 38.06 | 0.0001241 | −156.38 | −138.06 |
| DSD64 | 7th Order Search | −6 | 141.54 | 0.0000000 | −165.69 | −147.58 |
| DSD64 | 7th Order Search | −20 | 127.72 | 0.0000000 | −168.01 | −147.73 |
| DSD64 | 7th Order Search | −60 | 88.31 | 0.0000138 | −166.67 | −148.32 |
| DSD64 | 7th Order Search | −100 | 49.24 | 0.0000611 | −168.55 | −149.25 |
| DSD128 | 7th Order | −6 | 175.73 | 0.0000000 | −200.66 | −181.74 |
| DSD128 | 7th Order | −20 | 162.91 | 0.0000000 | −201.45 | −182.94 |
| DSD128 | 7th Order | −60 | 122.54 | 0.0000004 | −201.58 | −183.00 |
| DSD128 | 7th Order | −100 | 82.07 | 0.0000084 | −200.54 | −182.07 |
| DSD128 | 7th Order Search | −6 | 186.43 | 0.0000000 | −210.22 | −192.45 |
| DSD128 | 7th Order Search | −20 | 172.72 | 0.0000000 | −212.64 | −192.74 |
| DSD128 | 7th Order Search | −60 | 132.66 | 0.0000001 | −212.80 | −192.77 |
| DSD128 | 7th Order Search | −100 | 94.18 | 0.0000002 | −214.29 | −194.19 |
| DSD256 | 7th Order | −6 | 181.37 | 0.0000000 | −201.95 | −187.61 |
| DSD256 | 7th Order | −20 | 166.20 | 0.0000000 | −196.47 | −187.14 |
| DSD256 | 7th Order | −60 | 122.13 | 0.0000000 | −212.78 | −191.27 |
| DSD256 | 7th Order | −100 | 94.14 | 0.0000012 | −216.49 | −194.15 |

7th Order Search is available at DSD64 and DSD128. Regular 7th Order also
supports DSD256.

## Silence and tiny signals at DSD64

Both modes stayed quiet with digital silence, tiny DC and a −120 dBFS tone.
The largest full-stream density shift was 0.000001 or less.

| Mode | Signal | Noise dBFS | Unexpected spur dBFS | Maximum DC error |
| --- | --- | ---: | ---: | ---: |
| 7th Order | Silence | −143.74 | −162.84 | — |
| 7th Order | Tiny DC | −137.67 | −156.00 | 1.90e−10 |
| 7th Order | −120 dBFS tone | −139.08 | −157.56 | — |
| 7th Order Search | Silence | −149.09 | −168.06 | — |
| 7th Order Search | Tiny DC | −148.28 | −167.95 | 4.68e−11 |
| 7th Order Search | −120 dBFS tone | −147.45 | −166.62 | — |

## Stress and recovery at DSD128

Both modes handled the same demanding signal and settled again in about 16 ms.

| Mode | SINAD range dB | Expected product dBFS | Other residual dBFS | Unexpected spur dBFS | Recovery ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| 7th Order | 174.78–175.00 | −204.32 | −181.81 | −200.02 | 15.83–16.05 |
| 7th Order Search | 185.36–185.49 | −220.07 | −192.38 | −212.20 | 15.68 |

Neither mode produced meaningful overshoot. During a clean mute, peak/RMS was
−176.27/−185.84 dBFS for 7th Order and −180.82/−189.71 dBFS for 7th Order
Search.

## High-resolution input

This checks gain accuracy at 1, 18, 40 and 70 kHz.

| Rate | Mode | 1 kHz | 18 kHz | 40 kHz | 70 kHz |
| --- | --- | ---: | ---: | ---: | ---: |
| DSD128 | 7th Order | 0.000000673 | 0.000002135 | 0.000160074 | 0.012043037 |
| DSD128 | 7th Order Search | 0.000000090 | 0.000000342 | 0.000001789 | 0.000080826 |
| DSD256 | 7th Order | 0.000000016 | 0.000000021 | 0.000000183 | 0.000021074 |

| Rate | Mode | Frequency range | Residual dBFS | Unexpected spur dBFS |
| --- | --- | --- | ---: | ---: |
| DSD128 | 7th Order | 0–20 kHz | −113.85 | −149.26 |
| DSD128 | 7th Order | 20–80 kHz | −70.00 | −85.72 |
| DSD128 | 7th Order Search | 0–20 kHz | −130.12 | −165.53 |
| DSD128 | 7th Order Search | 20–80 kHz | −80.93 | −95.63 |
| DSD256 | 7th Order | 0–20 kHz | −173.19 | −208.79 |
| DSD256 | 7th Order | 20–80 kHz | −119.81 | −135.15 |

## M4 performance at DSD128

Measured on 22 July 2026 using an Apple M4 Mac mini with 16 GB RAM, macOS
26.5.2, Rust 1.96.0 and an optimized build. The test used two warmups and five
measured passes.

| Test | Result | Health |
| --- | --- | --- |
| Split Phase + 7th Order Search, stereo | 52.907 ms minimum, 53.762 ms average and 55.101 ms maximum for 185.76 ms of source audio, or 3.46× real-time on average | No processing errors |
| 7th Order Search only | 76.69 ns per DSD sample; 43.29% of one core per channel and 86.58% total for stereo | No processing errors |

This short synthetic test does not represent worst-case performance across
every library and system.
See [PCM-to-DSD Verification](dsd-public-quality.md) to run the measurements.
