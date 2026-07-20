# DSD512/DSD1024 Standard-modulator measurement

Measured on 2026-07-20 from commit `b45b76b` plus the working changes in this
experiment. The host was an AMD Ryzen 7 9800X3D (8 cores), Windows x86-64,
Rust 1.96.0.

## Scope

- Standard hard-sign seventh-order CRFB modulator only.
- No output-mode, settings, transport-capability, or UI exposure.
- No ECBeam/ECBeam2 high-rate table or policy.
- 44.1 kHz-family wire rates are 22.5792 MHz (DSD512) and 45.1584 MHz
  (DSD1024); the 48 kHz-family rates are 24.576 MHz and 49.152 MHz.

## OBG candidate calibration

Each row was generated independently with seed `0xD5D` using:

```text
python tools/gen_crfb.py --single standard <OSR> <OBG> --report <report.json>
```

`In-band NTF power` is the generator's modeled mean NTF power over the nominal
audio band. It is not an end-to-end reconstructed SINAD measurement.

| OSR | OBG | In-band NTF power | Calibrated input peak | DC stable through | Low/high sine stable through |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 512 | 1.20 | -195.73 dB | 0.779000 | 0.82 | 0.84 / 0.86 |
| 512 | 1.30 | -217.15 dB | 0.684000 | 0.72 | 0.74 / 0.76 |
| 512 | 1.40 | -231.63 dB | 0.570000 | 0.60 | 0.64 / 0.62 |
| 512 | 1.45 | -237.35 dB | 0.513000 | 0.54 | 0.56 / 0.58 |
| 512 | 1.50 | -242.36 dB | 0.456000 | 0.48 | 0.52 / 0.50 |
| 512 | 1.60 | -250.77 dB | 0.290700 | 0.36 | 0.36 / 0.40 |
| 1024 | 1.40 | -273.77 dB | 0.570000 | 0.60 | 0.62 / 0.64 |
| 1024 | 1.50 | -284.50 dB | 0.403750 | 0.50 | 0.52 / 0.52 |
| 1024 | 1.60 | -292.91 dB | 0.307800 | 0.36 | 0.38 / 0.38 |

OBG 1.60 was the initial measurement default at both rates because it gives the
strongest modeled shaping while retaining a calibrated stable range. The
end-to-end optimization below promotes OBG 1.50 for DSD512; DSD1024 remains at
OBG 1.60 pending an equivalent sweep. All nine candidates remain in
`ALL_VARIANTS` for later A/B work.

## Runtime cost and stability

The locked release `dsd_modulator_bench` used a 0.25-second three-tone stream,
two warmups, and the best of five measured passes. Core percentages are
normalized to real-time duration. DSD256 is included as a same-host baseline.

| Rate | Samples | ns/sample | One channel | Stereo aggregate | Clamps | Resets |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| DSD256 | 11,289,600 | 30.65 | 34.61% | 69.21% | 0 | 0 |
| DSD512 | 5,644,800 | 30.81 | 69.57% | 139.14% | 0 | 0 |
| DSD1024 | 11,289,600 | 30.74 | 138.80% | 277.59% | 0 | 0 |

The modulator cost remains essentially constant per DSD sample and therefore
scales linearly with rate. On this host, stereo DSD512 is plausible across the
two persistent channel workers, though it consumes about 1.39 cores before
upsampling and packing overhead. DSD1024 is not real-time in the current
one-worker-per-channel design because each channel alone needs about 1.39
real-time cores.

An end-to-end native-render smoke test also completed at both rates with exact
expected byte counts and zero limiter events, state clamps, stability resets,
or truncation. This validates the experimental renderer path, but it is not a
transport qualification.

## Split Phase E2v3 reconstructed quality

### Initial coherent level sweep (OBG 1.60 fallback)

A focused `dsd-public-quality-v4` coherent level-sweep cell requested
`SplitPhase128kE2v3`, then used the production renderer, native one-bit packing,
and the public reconstruction decoder. Inspection during optimization found
that the integer E2v3 planner stopped at 256x, so this 44.1 kHz-to-DSD512 run
fell back to the generic linear-phase rational kernel. The native-CPU release
render took 219.06 seconds and completed with zero structural failures, state
clamps, or resets. These figures remain useful as the initial OBG 1.60
baseline, but they are not the final E2v3 result.
The table uses the more conservative channel at each level. `Delta` is DSD512
Standard minus the published DSD128 EcBeam2 E2v3 result, so a positive SINAD
delta would favor DSD512.

| Effective level | DSD512 Standard SINAD | DSD128 EcBeam2 SINAD | SINAD delta | DSD512 residual | DSD128 EcBeam2 residual |
| ---: | ---: | ---: | ---: | ---: | ---: |
| -6 dBFS | 183.01 dB | 186.54 dB | -3.53 dB | -189.06 dBFS | -192.57 dBFS |
| -20 dBFS | 169.21 dB | 172.66 dB | -3.45 dB | -189.31 dBFS | -192.67 dBFS |
| -60 dBFS | 116.76 dB | 132.67 dB | -15.91 dB | -190.74 dBFS | -192.76 dBFS |
| -100 dBFS | 92.61 dB | 94.07 dB | -1.46 dB | -192.61 dBFS | -194.07 dBFS |

DSD128 EcBeam2 is better at every tested level. At -6, -20, and -100 dBFS the
difference mostly follows its roughly 1.5-3.5 dB lower reconstructed residual.
The much larger -60 dBFS gap is distortion-limited in DSD512 Standard: THD was
-116.94 dB relative to the carrier even though its residual noise was -190.74
dBFS. DSD512 gain error remained negligible and its worst unexpected spur was
-200.60 dBFS or lower at every level.

### Initial hi-res reconstruction (OBG 1.60)

The matching 176.4 kHz hi-res fixture contains coherent carriers at 1, 18, 40,
and 70 kHz. It was rendered to DSD512 Standard through the same E2v3 production
path and decoded with the same public 80-96 kHz reconstruction profile used by
the published DSD128 comparison (`reconstruction-v1-hires-80k-96k`). The
DSD512 cell used
`CRFB7_STANDARD_OSR512` at OBG 1.60 and completed with zero structural
failures, limiter events, state clamps, or stability resets.

The carrier table reports the largest absolute gain error across the two
channels. The published baseline calls `EcBeam2` **7th Order Search**.

| Carrier | DSD512 Standard | DSD128 EcBeam2 | Comparison |
| --- | ---: | ---: | ---: |
| 1 kHz | 0.000000009 dB | 0.000000007 dB | 0.000000002 dB worse |
| 18 kHz | 0.000000002 dB | 0.000000016 dB | about 8.5x lower error |
| 40 kHz | 0.000000012 dB | 0.000001146 dB | about 99x lower error |
| 70 kHz | 0.000000124 dB | 0.000040989 dB | about 331x lower error |

For each reconstruction band, the DSD512 row gives the two-channel residual
range and the less-negative unexpected spur. `Residual advantage` and `spur
advantage` are how much lower the conservative DSD512 result is than the
published DSD128 EcBeam2 result.

| Band | DSD512 residual range | DSD128 EcBeam2 residual | Residual advantage | DSD512 worst spur | DSD128 EcBeam2 worst spur | Spur advantage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 0-20 kHz | -188.74 to -188.71 dBFS | -157.31 dBFS | 31.40 dB | -207.40 dBFS | -189.13 dBFS | 18.27 dB |
| 20-80 kHz | -163.97 to -163.81 dBFS | -80.87 dBFS | 82.94 dB | -178.72 dBFS | -96.02 dBFS | 82.70 dB |

At hi-res frequencies, DSD512 Standard decisively improves on DSD128 EcBeam2:
its conservative reconstructed residual is 31.40 dB lower in-band and 82.94 dB
lower from 20-80 kHz. The only nominal loss is a two-billionths-of-a-decibel
larger 1 kHz gain error, which is immaterial at this measurement precision.
This does not contradict the coherent level sweep: the sweep tests distortion
and noise versus amplitude at 1 kHz, while this fixture specifically exposes
wideband reconstruction accuracy and ultrasonic noise shaping.

### Optimized DSD512 E2v3 result

The integer planner now supports the complete 512x cascade. Its additional
late 2x stage conservatively reuses the terminal frozen E2v3 halfband response;
the normalized audio band is half as wide at that stage, so this preserves the
certified response instead of falling back to a generic rational kernel.

OBG 1.20, 1.30, 1.40, 1.45, 1.50, and 1.60 were then measured through the
corrected path. OBG 1.20 maximized the 1 kHz sweep but degraded wideband
reconstruction. OBG 1.50 produced the highest combined index when the existing
DSD256 score weighting was applied unchanged: 35% coherent level sweep and 65%
hi-res reconstruction. Its combined index was 164.10 dB, compared with 163.67
for OBG 1.60 and 163.50 for OBG 1.45. Comparator-dither scale and white versus
high-pass shape changed the -60 dBFS probe by less than 0.04 dB, so the shipped
Standard dither remains unchanged.

The optimized OBG 1.50 result uses the conservative channel at each level.

| Effective level | OBG 1.50 SINAD | OBG 1.60 SINAD | Improvement | OBG 1.50 residual | OBG 1.50 worst spur |
| ---: | ---: | ---: | ---: | ---: | ---: |
| -6 dBFS | 186.47 dB | 182.72 dB | 3.76 dB | -192.66 dBFS | -208.12 dBFS |
| -20 dBFS | 170.74 dB | 167.42 dB | 3.31 dB | -191.39 dBFS | -201.56 dBFS |
| -60 dBFS | 119.62 dB | 119.02 dB | 0.60 dB | -188.96 dBFS | -190.63 dBFS |
| -100 dBFS | 94.97 dB | 90.90 dB | 4.06 dB | -194.97 dBFS | -217.53 dBFS |

Against the published DSD128 EcBeam2 result, optimized DSD512 is within 0.07 dB
at -6 dBFS, trails by 1.92 dB at -20 dBFS and 13.05 dB at the distortion-limited
-60 dBFS point, and leads by 0.90 dB at -100 dBFS.

The optimized hi-res carrier errors remain negligible: 0.000000009 dB at
1 kHz, 0.000000002 dB at 18 kHz, 0.000000009 dB at 40 kHz, and 0.000000420 dB
at 70 kHz. Relative to the initial OBG 1.60 table, OBG 1.50 lowers the 0-20 kHz
residual by 3.40 dB and its worst spur by 3.99 dB, while giving back 3.50 dB of
20-80 kHz residual and 4.33 dB of ultrasonic spur margin.

| Band | Optimized residual range | DSD128 EcBeam2 residual | Residual advantage | Optimized worst spur | DSD128 EcBeam2 worst spur | Spur advantage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 0-20 kHz | -192.23 to -192.11 dBFS | -157.31 dBFS | 34.80 dB | -211.39 dBFS | -189.13 dBFS | 22.26 dB |
| 20-80 kHz | -160.40 to -160.31 dBFS | -80.87 dBFS | 79.44 dB | -174.39 dBFS | -96.02 dBFS | 78.37 dB |

The DSD512 Standard default is therefore `CRFB_OSR512_OBG150`. The lower OBG
tables remain measurement candidates rather than production defaults.

A final CPU-native release run selected `CRFB7_STANDARD_OSR512` directly (OBG
1.50, input peak 0.456), completed both the level and hi-res cells in 17.2
seconds, and reported zero structural failures, stability resets, or state
clamps. Its native DSD hashes are identical to the winning OBG 1.50 override
run, confirming that the promoted default is the measured candidate rather
than a nearby regenerated table.

## Decision before UI exposure

Keep both rates measurement-only. DSD512 now has coherent level-sweep and
hi-res reconstruction results through the actual 512x E2v3 cascade. It is now
competitive with DSD128 EcBeam2 at three of the four coherent levels and is
substantially better on the hi-res reconstruction fixture, but the -60 dBFS
distortion gap remains. Idle, stress, complete renderer profiling, and
transport qualification still remain. DSD1024 needs a substantial throughput
improvement before it can be considered for real-time stereo UI exposure.
