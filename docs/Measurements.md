# Split Phase DSD Measurements

These are reproducible digital measurements of Fozmo's promoted **Split Phase**
filter (`SplitPhase128kE2v3`) with the two modulators exposed in the UI:

- **7th Order** (`Standard`), measured at its tuned −4 dB headroom;
- **7th Order Search** (`EcBeam2`), measured at its tuned −2 dB headroom.

The normal DSD64 and DSD128 cases use generated 44.1 kHz PCM. The focused
hi-res case uses generated 176.4 kHz PCM with carriers through 70 kHz and
renders it to DSD128. All measurements went through the production renderer,
normal EOF flush, native one-bit packing, and the fixed public-bench
reconstruction decoder.

The full promoted 28-cell Split Phase matrix passed with zero structural
failures. The additional eight-cell DSD128 Standard/Search run, including the
two hi-res rate-comparison cells, also completed with zero structural failures.
The tables below report the measurements directly; the presentation score is
intentionally omitted.

## Coherent level sweep

These figures are the more conservative result from the two channels at each
level: lower SINAD, largest absolute gain error, and the least-negative residual
or unexpected spur. A gain error closer to zero is better; more-negative noise
and spur values are lower.

| Rate | Modulator | Effective level dBFS | SINAD dB | Max gain error dB | Unexpected spur dBFS | Residual dBFS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| DSD64 | 7th Order | −6 | 131.43 | 0.0000 | −156.17 | −137.44 |
| DSD64 | 7th Order | −20 | 118.66 | 0.0000 | −157.74 | −138.67 |
| DSD64 | 7th Order | −60 | 77.87 | 0.0000 | −156.87 | −137.88 |
| DSD64 | 7th Order | −100 | 38.11 | 0.0006 | −157.27 | −138.11 |
| DSD64 | 7th Order Search | −6 | 141.56 | 0.0000 | −167.24 | −147.59 |
| DSD64 | 7th Order Search | −20 | 127.59 | 0.0000 | −166.42 | −147.59 |
| DSD64 | 7th Order Search | −60 | 88.29 | 0.0000 | −167.41 | −148.31 |
| DSD64 | 7th Order Search | −100 | 49.14 | 0.0000 | −168.39 | −149.14 |
| DSD128 | 7th Order | −6 | 175.65 | 0.0000 | −200.22 | −181.67 |
| DSD128 | 7th Order | −20 | 162.91 | 0.0000 | −201.08 | −182.92 |
| DSD128 | 7th Order | −60 | 122.52 | 0.0000 | −199.35 | −182.95 |
| DSD128 | 7th Order | −100 | 82.33 | 0.0000 | −201.84 | −182.33 |
| DSD128 | 7th Order Search | −6 | 186.54 | 0.0000 | −212.31 | −192.57 |
| DSD128 | 7th Order Search | −20 | 172.66 | 0.0000 | −212.61 | −192.67 |
| DSD128 | 7th Order Search | −60 | 132.67 | 0.0000 | −212.15 | −192.76 |
| DSD128 | 7th Order Search | −100 | 94.07 | 0.0000 | −213.30 | −194.07 |

## DSD64 idle and tiny-signal behavior

The idle fixture contains digital silence, opposing ±0.000001 DC, and a
−120 dBFS 100 Hz tone. Values again show the conservative channel. The maximum
density deviation was 0.000001 or less in every section.

| Modulator | Section | Noise dBFS | Unexpected spur dBFS | Maximum absolute DC error |
| --- | --- | ---: | ---: | ---: |
| 7th Order | Silence | −143.74 | −162.84 | — |
| 7th Order | Tiny DC | −137.76 | −157.51 | 2.08e−10 |
| 7th Order | −120 dBFS tone | −139.15 | −158.19 | — |
| 7th Order Search | Silence | −149.09 | −168.06 | — |
| 7th Order Search | Tiny DC | −148.20 | −167.74 | 4.10e−11 |
| 7th Order Search | −120 dBFS tone | −147.60 | −167.00 | — |

## DSD128 high-frequency stress and recovery

The comparable rows use the same −4 dB effective peak for both modulators.
Each figure spans steady and recovery windows across both channels. The
separate rated-input cases also passed at each modulator's production headroom.

| Modulator | SINAD range dB | Worst declared product dBFS | Worst product-excluded residual dBFS | Worst unexpected spur dBFS | Recovery range ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| 7th Order | 174.79–175.00 | −205.18 | −181.82 | −200.85 | 14.24–14.34 |
| 7th Order Search | 185.24–185.34 | −225.56 | −192.26 | −211.38 | 14.29 |

Neither modulator produced transition overshoot in the matched-level case.
Clean-mute peak/RMS measured −169.86/−180.40 dBFS or lower for 7th Order and
−185.04/−192.80 dBFS for 7th Order Search.

## DSD128 hi-res reconstruction

This focused rate-comparison test renders a 176.4 kHz multitone to DSD128. It
is deliberately reported as a measurement rather than a canonical score cell;
the public bench's canonical through-70 kHz reconstruction rate is DSD256.
Both DSD128 cells passed their structural checks.

| Modulator | Carrier | Maximum absolute gain error dB |
| --- | --- | ---: |
| 7th Order | 1 kHz | 0.00000009 |
| 7th Order | 18 kHz | 0.00000029 |
| 7th Order | 40 kHz | 0.00016548 |
| 7th Order | 70 kHz | 0.01217751 |
| 7th Order Search | 1 kHz | 0.000000007 |
| 7th Order Search | 18 kHz | 0.000000016 |
| 7th Order Search | 40 kHz | 0.000001146 |
| 7th Order Search | 70 kHz | 0.000040989 |

| Modulator | Reconstruction band | Residual range dBFS | Worst unexpected spur dBFS |
| --- | --- | ---: | ---: |
| 7th Order | 0–20 kHz | −132.07 to −131.03 | −165.48 |
| 7th Order | 20–80 kHz | −70.07 to −70.05 | −84.89 |
| 7th Order Search | 0–20 kHz | −157.31 | −189.13 |
| 7th Order Search | 20–80 kHz | −80.87 | −96.02 |

## Scope

These results describe the generated digital DSD stream, not the analog output
of a DAC. They do not account for a DAC's reconstruction filter, analog noise,
music-dependent behavior, or listening preference. The complete bench method,
metric definitions, and reproduction command are documented in
[Public PCM-to-DSD measurement bench](dsd-public-quality.md).

Measurement contract: `dsd-public-quality-v4`; matrix contract:
`dsd-public-matrix-28-v6`; native release build on Apple M4 with
`-C target-cpu=native`, measured 20 July 2026.
