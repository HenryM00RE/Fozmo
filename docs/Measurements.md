# Split Phase DSD Measurements

These are reproducible digital measurements of Fozmo's current **Split Phase**
filter (`SplitPhase128kE3`, the promoted P17 bundle) with the two modulators
exposed in the UI:

- **7th Order** (`Standard`), measured at its tuned −4 dB headroom;
- **7th Order Search** (`EcBeam2`), measured at its tuned −2 dB headroom.

P17 is now the only filter presented as Split Phase. The earlier E2v3 Split
Phase and Smooth Phase selections are retained only as internal and migration
identifiers. This focused run covered DSD64, DSD128, and DSD256 level sweeps,
DSD64 idle behavior, DSD128 stress, and hi-res reconstruction at both DSD128
and DSD256. All 16 cells completed with zero structural failures, stability
resets, state clamps, limiter events, or truncation events.

The tables report measurements directly and do not assign a presentation
score. The public bench's current versioned scoring contract predates the P17
product consolidation and remains tied to E2v3, so it is not applied here.

## Coherent level sweep

The values below are the conservative channel result at each level: the lower
SINAD, largest absolute gain error, and least-negative residual or unexpected
spur. Gain error closer to zero is better; more-negative noise and spur values
are lower.

| Rate | Modulator | Effective level dBFS | SINAD dB | Max gain error dB | Unexpected spur dBFS | Residual dBFS |
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
| DSD256 | 7th Order Search | −6 | 186.61 | 0.0000000 | −215.21 | −192.63 |
| DSD256 | 7th Order Search | −20 | 172.14 | 0.0000000 | −210.29 | −192.19 |
| DSD256 | 7th Order Search | −60 | 129.35 | 0.0000000 | −215.39 | −192.52 |
| DSD256 | 7th Order Search | −100 | 93.30 | 0.0000011 | −216.48 | −193.30 |

The regular 7th Order modulator is therefore included at DSD256 as a normal
selectable path, not merely as an internal reference. In this fixture it leads
7th Order Search slightly at −100 dBFS, while Search leads at −6, −20, and
−60 dBFS.

## DSD64 idle and tiny-signal behavior

The idle fixture contains digital silence, opposing ±0.000001 DC, and a
−120 dBFS 100 Hz tone. The maximum full-stream density deviation was 0.000001
or less in every section.

| Modulator | Section | Noise dBFS | Unexpected spur dBFS | Maximum absolute DC error |
| --- | --- | ---: | ---: | ---: |
| 7th Order | Silence | −143.74 | −162.84 | — |
| 7th Order | Tiny DC | −137.67 | −156.00 | 1.90e−10 |
| 7th Order | −120 dBFS tone | −139.08 | −157.56 | — |
| 7th Order Search | Silence | −149.09 | −168.06 | — |
| 7th Order Search | Tiny DC | −148.28 | −167.95 | 4.68e−11 |
| 7th Order Search | −120 dBFS tone | −147.45 | −166.62 | — |

## DSD128 high-frequency stress and recovery

These rows use the same −4 dB effective peak for both modulators. Each range
spans steady and recovery windows across both channels. The separate rated
input cases also passed at each modulator's production headroom.

| Modulator | SINAD range dB | Worst declared product dBFS | Worst product-excluded residual dBFS | Worst unexpected spur dBFS | Recovery range ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| 7th Order | 174.78–175.00 | −204.32 | −181.81 | −200.02 | 15.83–16.05 |
| 7th Order Search | 185.36–185.49 | −220.07 | −192.38 | −212.20 | 15.68 |

Neither modulator produced material transition overshoot. Conservative
clean-mute peak/RMS measured −176.27/−185.84 dBFS for 7th Order and
−180.82/−189.71 dBFS for 7th Order Search.

## Hi-res reconstruction

The generated 176.4 kHz fixture contains coherent carriers at 1, 18, 40, and
70 kHz. DSD256 is the canonical rate for this through-70 kHz fixture; DSD128 is
included as a focused rate comparison. Carrier values are the largest absolute
gain error across both channels.

| Rate | Modulator | 1 kHz | 18 kHz | 40 kHz | 70 kHz |
| --- | --- | ---: | ---: | ---: | ---: |
| DSD128 | 7th Order | 0.000000673 | 0.000002135 | 0.000160074 | 0.012043037 |
| DSD128 | 7th Order Search | 0.000000090 | 0.000000342 | 0.000001789 | 0.000080826 |
| DSD256 | 7th Order | 0.000000016 | 0.000000021 | 0.000000183 | 0.000021074 |
| DSD256 | 7th Order Search | 0.000000016 | 0.000000019 | 0.000000014 | 0.000000284 |

| Rate | Modulator | Reconstruction band | Conservative residual dBFS | Worst unexpected spur dBFS |
| --- | --- | --- | ---: | ---: |
| DSD128 | 7th Order | 0–20 kHz | −113.85 | −149.26 |
| DSD128 | 7th Order | 20–80 kHz | −70.00 | −85.72 |
| DSD128 | 7th Order Search | 0–20 kHz | −130.12 | −165.53 |
| DSD128 | 7th Order Search | 20–80 kHz | −80.93 | −95.63 |
| DSD256 | 7th Order | 0–20 kHz | −173.19 | −208.79 |
| DSD256 | 7th Order | 20–80 kHz | −119.81 | −135.15 |
| DSD256 | 7th Order Search | 0–20 kHz | −174.73 | −210.05 |
| DSD256 | 7th Order Search | 20–80 kHz | −126.32 | −143.46 |

## M4 performance at DSD128

Measured on an Apple M4 Mac mini with 16 GB RAM, macOS 26.5.2, Rust 1.96.0,
an optimized native-CPU build, two warmups, and five measured passes.

| Benchmark | Result | Health |
| --- | --- | --- |
| P17 Split Phase + 7th Order Search stereo renderer | 52.907 ms minimum, 53.762 ms average, and 55.101 ms maximum for 8,192 source frames (185.76 ms at 44.1 kHz), or 3.46× real-time average wall throughput | 0 resets, 0 clamps |
| 7th Order Search modulator only | 76.69 ns per DSD sample; 43.29% of one core per channel and 86.58% aggregate for stereo | 0 resets, 0 clamps |

The renderer figure includes P17 upsampling, both channel modulators, EOF flush,
and native packing. It is a short synthetic throughput benchmark, not a claim
about worst-case whole-library playback. The modulator percentages are CPU
cost; the renderer's 3.46× figure is wall throughput and can benefit from the
two channel workers, so the two figures should not be added together.

Reproduce the focused performance run with:

```sh
DSD_RENDERER_BENCH_FILTER="Split Phase DSD128 Search" \
DSD_MODULATOR_BENCH_FILTER="EcBeam2 playback DSD128" \
tools/dsd-perf-mac.sh
```

## Scope and provenance

These results describe the generated digital DSD stream, not the analog output
of a DAC. They do not account for a DAC's reconstruction filter, analog noise,
music-dependent behavior, or listening preference.

The quality run used measurement contract `dsd-public-quality-v5`, matrix
contract `dsd-public-matrix-28-v6`, commit `51d9ab3`, and a native Apple M4
release build on 22 July 2026. Reproduce it with:

```sh
RUSTFLAGS="-C target-cpu=native" cargo run --locked --release \
  --bin dsd_public_quality -- \
  --filter SplitPhaseB \
  --modulator Standard,EcBeam2 \
  --rates 64,128,256 \
  --include-rate-comparison
```

The complete method and metric definitions are documented in
[Public PCM-to-DSD measurement bench](dsd-public-quality.md).
