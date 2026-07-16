# Public PCM-to-DSD quality measurements

Status: **PASS**. Schema `dsd-public-quality-report-v4`; matrix `dsd-public-matrix-26-v4`; 26/26 canonical production cells completed. Canonical structural failures: 0. Optional diagnostic cells: 0/0 with 0 structural failures.

Built as `release`/opt `3` for `aarch64-apple-darwin` with target CPU `native`; source/binary snapshot match: `true`.

All dBFS figures use `full-scale-sine: peak amplitude 1.0 and RMS 1/sqrt(2) are both 0 dBFS`. Stress SINAD is conventional and includes declared IMD. Declared products, product-excluded residual, and unexpected Blackman-Harris-integrated spurs are shown separately. Density uses a 20.0 ms physical-time window.

Rated stress preserves each modulator's production headroom and is not a loudness-matched comparison. Use only `matched_effective_peak` stress rows for direct cross-modulator comparison. `Split128k` is the only canonical and scoring path; optional `SincExtreme32k` cells are a non-scoring Linear Phase diagnostic limited to modulators that support it. EcBeam2 is scored at DSD64 and DSD128; DSD256 is unsupported and intentionally omitted.

## Split128k production-path scores

Score system: `dsd-public-production-score-v2`. Fozmo PCM-to-DSD production-path score using Split128k. Scores are comparative presentation, not `--check` quality gates.

| Modulator | DSD64 | DSD128 | DSD256 | Rated DSD128 stress qualification |
| --- | ---: | ---: | ---: | --- |
| Standard | 97.81 | 100.00 | 99.34 | PASS |
| EcDepth2 | 94.17 | 92.62 | 90.82 | PASS |
| EcBeam | 100.00 | 93.31 | 90.31 | PASS |
| EcBeam2 | 100.00 | 100.00 | — | PASS |

### Score category detail

| Modulator | Rate | Category | Quality index dB | Anchor dB | Normalized /100 | Awarded points | Maximum |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| Standard | DSD64 | coherent_level_sweep | 115.0883 | 117.3953 | 97.69 | 58.62 | 60 |
| Standard | DSD64 | idle_tiny_signal | 133.4626 | 135.4762 | 97.99 | 39.19 | 40 |
| Standard | DSD128 | coherent_level_sweep | 153.6365 | 153.6365 | 100.00 | 35.00 | 35 |
| Standard | DSD128 | level_matched_stress_spectral_quality | 174.8255 | 174.8255 | 100.00 | 40.00 | 40 |
| Standard | DSD128 | mute_restart_transition_quality | 53.8232 | 53.8232 | 100.00 | 25.00 | 25 |
| Standard | DSD256 | coherent_level_sweep | 157.3813 | 159.2736 | 98.11 | 34.34 | 35 |
| Standard | DSD256 | hires_reconstruction_through_70khz | 151.1161 | 151.1161 | 100.00 | 65.00 | 65 |
| EcDepth2 | DSD64 | coherent_level_sweep | 109.8329 | 117.3953 | 92.44 | 55.46 | 60 |
| EcDepth2 | DSD64 | idle_tiny_signal | 132.2409 | 135.4762 | 96.76 | 38.71 | 40 |
| EcDepth2 | DSD128 | coherent_level_sweep | 145.2307 | 153.6365 | 91.59 | 32.06 | 35 |
| EcDepth2 | DSD128 | level_matched_stress_spectral_quality | 165.6073 | 174.8255 | 90.78 | 36.31 | 40 |
| EcDepth2 | DSD128 | mute_restart_transition_quality | 50.8070 | 53.8232 | 96.98 | 24.25 | 25 |
| EcDepth2 | DSD256 | coherent_level_sweep | 158.7739 | 159.2736 | 99.50 | 34.83 | 35 |
| EcDepth2 | DSD256 | hires_reconstruction_through_70khz | 137.2582 | 151.1161 | 86.14 | 55.99 | 65 |
| EcBeam | DSD64 | coherent_level_sweep | 117.3953 | 117.3953 | 100.00 | 60.00 | 60 |
| EcBeam | DSD64 | idle_tiny_signal | 135.4762 | 135.4762 | 100.00 | 40.00 | 40 |
| EcBeam | DSD128 | coherent_level_sweep | 145.8639 | 153.6365 | 92.23 | 32.28 | 35 |
| EcBeam | DSD128 | level_matched_stress_spectral_quality | 166.2049 | 174.8255 | 91.38 | 36.55 | 40 |
| EcBeam | DSD128 | mute_restart_transition_quality | 51.7265 | 53.8232 | 97.90 | 24.48 | 25 |
| EcBeam | DSD256 | coherent_level_sweep | 159.2736 | 159.2736 | 100.00 | 35.00 | 35 |
| EcBeam | DSD256 | hires_reconstruction_through_70khz | 136.2130 | 151.1161 | 85.10 | 55.31 | 65 |
| EcBeam2 | DSD64 | coherent_level_sweep | 120.6755 | 117.3953 | 100.00 | 60.00 | 60 |
| EcBeam2 | DSD64 | idle_tiny_signal | 138.9007 | 135.4762 | 100.00 | 40.00 | 40 |
| EcBeam2 | DSD128 | coherent_level_sweep | 161.5912 | 153.6365 | 100.00 | 35.00 | 35 |
| EcBeam2 | DSD128 | level_matched_stress_spectral_quality | 185.6247 | 174.8255 | 100.00 | 40.00 | 40 |
| EcBeam2 | DSD128 | mute_restart_transition_quality | 56.5602 | 53.8232 | 100.00 | 25.00 | 25 |

## Structural coverage

| Scenario | Filter role | Filter | Rate | Modulator | Comparison class | Effective peak | Full density dev. | Health |
| --- | --- | --- | --- | --- | --- | ---: | ---: | --- |
| coherent_level_sweep | production default | Split128k | DSD64 | Standard | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD64 | EcDepth2 | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD64 | EcBeam | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD128 | Standard | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD128 | EcDepth2 | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD128 | EcBeam | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD256 | Standard | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD256 | EcDepth2 | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD256 | EcBeam | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| idle_tiny_signal | production default | Split128k | DSD64 | Standard | production_path_level_matched | 0.000001 | 0.000000 | PASS |
| idle_tiny_signal | production default | Split128k | DSD64 | EcDepth2 | production_path_level_matched | 0.000001 | 0.000000 | PASS |
| idle_tiny_signal | production default | Split128k | DSD64 | EcBeam | production_path_level_matched | 0.000001 | 0.000000 | PASS |
| high_frequency_rated_stress | production default | Split128k | DSD128 | Standard | production_path_rated_input | 0.630326 | 0.000000 | PASS |
| high_frequency_rated_stress | production default | Split128k | DSD128 | EcDepth2 | production_path_rated_input | 0.630326 | 0.000000 | PASS |
| high_frequency_rated_stress | production default | Split128k | DSD128 | EcBeam | production_path_rated_input | 0.793534 | 0.000000 | PASS |
| high_frequency_matched_stress | production default | Split128k | DSD128 | Standard | production_path_level_matched | 0.630326 | 0.000000 | PASS |
| high_frequency_matched_stress | production default | Split128k | DSD128 | EcDepth2 | production_path_level_matched | 0.630326 | 0.000000 | PASS |
| high_frequency_matched_stress | production default | Split128k | DSD128 | EcBeam | production_path_level_matched | 0.630326 | 0.000000 | PASS |
| hires_reconstruction | production default | Split128k | DSD256 | Standard | production_path_level_matched | 0.501187 | 0.000000 | PASS |
| hires_reconstruction | production default | Split128k | DSD256 | EcDepth2 | production_path_level_matched | 0.501187 | 0.000000 | PASS |
| hires_reconstruction | production default | Split128k | DSD256 | EcBeam | production_path_level_matched | 0.501187 | 0.000000 | PASS |
| coherent_level_sweep | production default | Split128k | DSD64 | EcBeam2 | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| coherent_level_sweep | production default | Split128k | DSD128 | EcBeam2 | production_path_level_matched | 0.501187 | 0.000001 | PASS |
| idle_tiny_signal | production default | Split128k | DSD64 | EcBeam2 | production_path_level_matched | 0.000001 | 0.000000 | PASS |
| high_frequency_rated_stress | production default | Split128k | DSD128 | EcBeam2 | production_path_rated_input | 0.793534 | 0.000000 | PASS |
| high_frequency_matched_stress | production default | Split128k | DSD128 | EcBeam2 | production_path_level_matched | 0.630326 | 0.000000 | PASS |

## Coherent level sweep

| Filter | Rate | Modulator | Channel | Source dBFS | Effective dBFS | SINAD dB | Gain error dB | Unexpected spur dBFS | Residual dBFS |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Split128k | DSD64 | Standard | left | -2.00 | -6.00 | 131.52 | 0.0000 | -156.89 | -137.54 |
| Split128k | DSD64 | Standard | right | -2.00 | -6.00 | 131.55 | 0.0000 | -157.19 | -137.57 |
| Split128k | DSD64 | Standard | left | -16.00 | -20.00 | 118.60 | 0.0000 | -157.32 | -138.61 |
| Split128k | DSD64 | Standard | right | -16.00 | -20.00 | 118.66 | 0.0000 | -157.36 | -138.67 |
| Split128k | DSD64 | Standard | left | -56.00 | -60.00 | 78.96 | -0.0001 | -157.81 | -139.00 |
| Split128k | DSD64 | Standard | right | -56.00 | -60.00 | 78.76 | -0.0001 | -158.62 | -138.82 |
| Split128k | DSD64 | Standard | left | -96.00 | -100.00 | 37.82 | -0.0001 | -156.75 | -137.82 |
| Split128k | DSD64 | Standard | right | -96.00 | -100.00 | 37.80 | 0.0006 | -157.89 | -137.81 |
| Split128k | DSD64 | EcDepth2 | left | -2.00 | -6.00 | 126.18 | 0.0000 | -152.17 | -132.29 |
| Split128k | DSD64 | EcDepth2 | right | -2.00 | -6.00 | 126.18 | 0.0000 | -152.17 | -132.29 |
| Split128k | DSD64 | EcDepth2 | left | -16.00 | -20.00 | 113.15 | 0.0000 | -153.38 | -133.22 |
| Split128k | DSD64 | EcDepth2 | right | -16.00 | -20.00 | 113.15 | 0.0000 | -153.38 | -133.22 |
| Split128k | DSD64 | EcDepth2 | left | -56.00 | -60.00 | 73.44 | -0.0003 | -153.45 | -133.78 |
| Split128k | DSD64 | EcDepth2 | right | -56.00 | -60.00 | 73.44 | -0.0003 | -153.45 | -133.78 |
| Split128k | DSD64 | EcDepth2 | left | -96.00 | -100.00 | 32.37 | -0.0008 | -152.14 | -132.38 |
| Split128k | DSD64 | EcDepth2 | right | -96.00 | -100.00 | 32.37 | -0.0008 | -152.14 | -132.38 |
| Split128k | DSD64 | EcBeam | left | -4.00 | -6.00 | 134.60 | -0.0000 | -159.15 | -140.60 |
| Split128k | DSD64 | EcBeam | right | -4.00 | -6.00 | 134.60 | -0.0000 | -159.15 | -140.60 |
| Split128k | DSD64 | EcBeam | left | -18.00 | -20.00 | 120.86 | -0.0000 | -159.48 | -140.87 |
| Split128k | DSD64 | EcBeam | right | -18.00 | -20.00 | 120.86 | -0.0000 | -159.48 | -140.87 |
| Split128k | DSD64 | EcBeam | left | -58.00 | -60.00 | 80.81 | -0.0000 | -159.22 | -140.81 |
| Split128k | DSD64 | EcBeam | right | -58.00 | -60.00 | 80.81 | -0.0000 | -159.22 | -140.81 |
| Split128k | DSD64 | EcBeam | left | -98.00 | -100.00 | 40.47 | 0.0001 | -160.19 | -140.47 |
| Split128k | DSD64 | EcBeam | right | -98.00 | -100.00 | 40.47 | 0.0001 | -160.19 | -140.47 |
| Split128k | DSD128 | Standard | left | -2.00 | -6.00 | 175.67 | 0.0000 | -200.89 | -181.69 |
| Split128k | DSD128 | Standard | right | -2.00 | -6.00 | 175.76 | 0.0000 | -201.18 | -181.77 |
| Split128k | DSD128 | Standard | left | -16.00 | -20.00 | 162.71 | 0.0000 | -201.52 | -182.73 |
| Split128k | DSD128 | Standard | right | -16.00 | -20.00 | 162.97 | 0.0000 | -202.78 | -182.98 |
| Split128k | DSD128 | Standard | left | -56.00 | -60.00 | 122.97 | -0.0000 | -200.57 | -183.25 |
| Split128k | DSD128 | Standard | right | -56.00 | -60.00 | 123.11 | -0.0000 | -204.31 | -183.45 |
| Split128k | DSD128 | Standard | left | -96.00 | -100.00 | 85.00 | -0.0000 | -204.16 | -185.00 |
| Split128k | DSD128 | Standard | right | -96.00 | -100.00 | 84.97 | -0.0000 | -204.61 | -184.97 |
| Split128k | DSD128 | EcDepth2 | left | -2.00 | -6.00 | 167.87 | 0.0000 | -193.49 | -174.53 |
| Split128k | DSD128 | EcDepth2 | right | -2.00 | -6.00 | 167.87 | 0.0000 | -193.49 | -174.53 |
| Split128k | DSD128 | EcDepth2 | left | -16.00 | -20.00 | 154.64 | 0.0000 | -191.45 | -174.68 |
| Split128k | DSD128 | EcDepth2 | right | -16.00 | -20.00 | 154.64 | 0.0000 | -191.45 | -174.68 |
| Split128k | DSD128 | EcDepth2 | left | -56.00 | -60.00 | 112.18 | 0.0000 | -186.50 | -174.42 |
| Split128k | DSD128 | EcDepth2 | right | -56.00 | -60.00 | 112.18 | 0.0000 | -186.50 | -174.42 |
| Split128k | DSD128 | EcDepth2 | left | -96.00 | -100.00 | 73.64 | -0.0000 | -192.15 | -173.64 |
| Split128k | DSD128 | EcDepth2 | right | -96.00 | -100.00 | 73.64 | -0.0000 | -192.15 | -173.64 |
| Split128k | DSD128 | EcBeam | left | -4.00 | -6.00 | 167.98 | 0.0000 | -187.95 | -174.79 |
| Split128k | DSD128 | EcBeam | right | -4.00 | -6.00 | 167.98 | 0.0000 | -187.95 | -174.79 |
| Split128k | DSD128 | EcBeam | left | -18.00 | -20.00 | 154.98 | 0.0000 | -192.79 | -175.10 |
| Split128k | DSD128 | EcBeam | right | -18.00 | -20.00 | 154.98 | 0.0000 | -192.79 | -175.10 |
| Split128k | DSD128 | EcBeam | left | -58.00 | -60.00 | 114.76 | 0.0000 | -193.97 | -175.15 |
| Split128k | DSD128 | EcBeam | right | -58.00 | -60.00 | 114.76 | 0.0000 | -193.97 | -175.15 |
| Split128k | DSD128 | EcBeam | left | -98.00 | -100.00 | 73.69 | -0.0000 | -193.18 | -173.69 |
| Split128k | DSD128 | EcBeam | right | -98.00 | -100.00 | 73.69 | -0.0000 | -193.18 | -173.69 |
| Split128k | DSD256 | Standard | left | -2.00 | -6.00 | 181.36 | -0.0000 | -202.96 | -187.59 |
| Split128k | DSD256 | Standard | right | -2.00 | -6.00 | 181.42 | -0.0000 | -202.09 | -187.67 |
| Split128k | DSD256 | Standard | left | -16.00 | -20.00 | 166.37 | -0.0000 | -196.83 | -187.25 |
| Split128k | DSD256 | Standard | right | -16.00 | -20.00 | 166.39 | -0.0000 | -196.93 | -187.25 |
| Split128k | DSD256 | Standard | left | -56.00 | -60.00 | 123.40 | -0.0000 | -210.79 | -190.87 |
| Split128k | DSD256 | Standard | right | -56.00 | -60.00 | 123.31 | 0.0000 | -211.91 | -190.82 |
| Split128k | DSD256 | Standard | left | -96.00 | -100.00 | 91.54 | -0.0000 | -213.74 | -191.55 |
| Split128k | DSD256 | Standard | right | -96.00 | -100.00 | 91.46 | 0.0000 | -213.72 | -191.46 |
| Split128k | DSD256 | EcDepth2 | left | -2.00 | -6.00 | 186.47 | -0.0000 | -210.86 | -192.65 |
| Split128k | DSD256 | EcDepth2 | right | -2.00 | -6.00 | 186.47 | -0.0000 | -210.86 | -192.65 |
| Split128k | DSD256 | EcDepth2 | left | -16.00 | -20.00 | 170.40 | -0.0000 | -206.29 | -190.57 |
| Split128k | DSD256 | EcDepth2 | right | -16.00 | -20.00 | 170.40 | -0.0000 | -206.29 | -190.57 |
| Split128k | DSD256 | EcDepth2 | left | -56.00 | -60.00 | 123.86 | 0.0000 | -199.93 | -192.24 |
| Split128k | DSD256 | EcDepth2 | right | -56.00 | -60.00 | 123.86 | 0.0000 | -199.93 | -192.24 |
| Split128k | DSD256 | EcDepth2 | left | -96.00 | -100.00 | 89.40 | -0.0000 | -210.75 | -189.40 |
| Split128k | DSD256 | EcDepth2 | right | -96.00 | -100.00 | 89.40 | -0.0000 | -210.75 | -189.40 |
| Split128k | DSD256 | EcBeam | left | -4.00 | -6.00 | 186.56 | -0.0000 | -208.37 | -192.84 |
| Split128k | DSD256 | EcBeam | right | -4.00 | -6.00 | 186.56 | -0.0000 | -208.37 | -192.84 |
| Split128k | DSD256 | EcBeam | left | -18.00 | -20.00 | 170.79 | 0.0000 | -205.73 | -191.06 |
| Split128k | DSD256 | EcBeam | right | -18.00 | -20.00 | 170.79 | 0.0000 | -205.73 | -191.06 |
| Split128k | DSD256 | EcBeam | left | -58.00 | -60.00 | 123.01 | 0.0000 | -200.12 | -193.93 |
| Split128k | DSD256 | EcBeam | right | -58.00 | -60.00 | 123.01 | 0.0000 | -200.12 | -193.93 |
| Split128k | DSD256 | EcBeam | left | -98.00 | -100.00 | 91.90 | -0.0000 | -213.93 | -191.91 |
| Split128k | DSD256 | EcBeam | right | -98.00 | -100.00 | 91.90 | -0.0000 | -213.93 | -191.91 |
| Split128k | DSD64 | EcBeam2 | left | -4.00 | -6.00 | 137.83 | -0.0000 | -162.86 | -143.84 |
| Split128k | DSD64 | EcBeam2 | right | -4.00 | -6.00 | 137.83 | -0.0000 | -162.86 | -143.84 |
| Split128k | DSD64 | EcBeam2 | left | -18.00 | -20.00 | 123.93 | -0.0000 | -163.06 | -143.94 |
| Split128k | DSD64 | EcBeam2 | right | -18.00 | -20.00 | 123.93 | -0.0000 | -163.06 | -143.94 |
| Split128k | DSD64 | EcBeam2 | left | -58.00 | -60.00 | 84.59 | 0.0000 | -163.34 | -144.60 |
| Split128k | DSD64 | EcBeam2 | right | -58.00 | -60.00 | 84.59 | 0.0000 | -163.34 | -144.60 |
| Split128k | DSD64 | EcBeam2 | left | -98.00 | -100.00 | 45.41 | 0.0000 | -164.99 | -145.42 |
| Split128k | DSD64 | EcBeam2 | right | -98.00 | -100.00 | 45.41 | 0.0000 | -164.99 | -145.42 |
| Split128k | DSD128 | EcBeam2 | left | -4.00 | -6.00 | 186.46 | -0.0000 | -212.12 | -192.47 |
| Split128k | DSD128 | EcBeam2 | right | -4.00 | -6.00 | 186.46 | -0.0000 | -212.12 | -192.47 |
| Split128k | DSD128 | EcBeam2 | left | -18.00 | -20.00 | 172.80 | -0.0000 | -211.96 | -192.81 |
| Split128k | DSD128 | EcBeam2 | right | -18.00 | -20.00 | 172.80 | -0.0000 | -211.96 | -192.81 |
| Split128k | DSD128 | EcBeam2 | left | -58.00 | -60.00 | 132.74 | 0.0000 | -213.10 | -192.77 |
| Split128k | DSD128 | EcBeam2 | right | -58.00 | -60.00 | 132.74 | 0.0000 | -213.10 | -192.77 |
| Split128k | DSD128 | EcBeam2 | left | -98.00 | -100.00 | 91.85 | 0.0000 | -211.09 | -191.85 |
| Split128k | DSD128 | EcBeam2 | right | -98.00 | -100.00 | 91.85 | 0.0000 | -211.09 | -191.85 |

## Idle, tiny DC, and tiny tone

| Filter | Modulator | Section | Channel | Noise dBFS | Unexpected spur dBFS | Expected DC | Measured DC | DC error | Density dev. |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Split128k | Standard | silence | left | -143.74 | -162.84 | — | -2.811422703e-13 | — | 0.000000 |
| Split128k | Standard | silence | right | -143.75 | -162.92 | — | 4.236862299e-12 | — | 0.000000 |
| Split128k | Standard | tiny_dc | left | -138.00 | -155.81 | 1.000000000e-6 | 1.000173386e-6 | 1.733856955e-10 | 0.000000 |
| Split128k | Standard | tiny_dc | right | -137.92 | -157.39 | -1.000000000e-6 | -1.000219461e-6 | -2.194610628e-10 | 0.000000 |
| Split128k | Standard | tone_100hz_-120_dbfs | left | -139.22 | -159.77 | — | 1.422993826e-10 | — | 0.000000 |
| Split128k | Standard | tone_100hz_-120_dbfs | right | -139.23 | -159.12 | — | -1.632738358e-10 | — | 0.000000 |
| Split128k | EcDepth2 | silence | left | -140.61 | -160.20 | — | -5.464654987e-12 | — | 0.000000 |
| Split128k | EcDepth2 | silence | right | -140.61 | -160.20 | — | -5.464654987e-12 | — | 0.000000 |
| Split128k | EcDepth2 | tiny_dc | left | -132.57 | -152.33 | 1.000000000e-6 | 1.000147487e-6 | 1.474869234e-10 | 0.000000 |
| Split128k | EcDepth2 | tiny_dc | right | -132.38 | -151.78 | -1.000000000e-6 | -1.000190423e-6 | -1.904226752e-10 | 0.000000 |
| Split128k | EcDepth2 | tone_100hz_-120_dbfs | left | -137.14 | -156.67 | — | 1.627150344e-10 | — | 0.000000 |
| Split128k | EcDepth2 | tone_100hz_-120_dbfs | right | -136.36 | -154.87 | — | -1.635145383e-10 | — | 0.000000 |
| Split128k | EcBeam | silence | left | -141.31 | -160.26 | — | 8.073993765e-12 | — | 0.000000 |
| Split128k | EcBeam | silence | right | -141.31 | -160.26 | — | 8.073993765e-12 | — | 0.000000 |
| Split128k | EcBeam | tiny_dc | left | -141.39 | -159.93 | 1.000000000e-6 | 1.000115739e-6 | 1.157386007e-10 | 0.000000 |
| Split128k | EcBeam | tiny_dc | right | -141.41 | -161.47 | -1.000000000e-6 | -1.000109294e-6 | -1.092938717e-10 | 0.000000 |
| Split128k | EcBeam | tone_100hz_-120_dbfs | left | -141.10 | -160.42 | — | 2.045494181e-10 | — | 0.000000 |
| Split128k | EcBeam | tone_100hz_-120_dbfs | right | -141.19 | -161.96 | — | -2.063438745e-10 | — | 0.000000 |
| Split128k | EcBeam2 | silence | left | -145.46 | -164.01 | — | 8.553373781e-12 | — | 0.000000 |
| Split128k | EcBeam2 | silence | right | -145.46 | -164.01 | — | 8.553373781e-12 | — | 0.000000 |
| Split128k | EcBeam2 | tiny_dc | left | -143.83 | -163.80 | 1.000000000e-6 | 1.000110859e-6 | 1.108588207e-10 | 0.000001 |
| Split128k | EcBeam2 | tiny_dc | right | -143.97 | -162.58 | -1.000000000e-6 | -1.000109517e-6 | -1.095168298e-10 | 0.000000 |
| Split128k | EcBeam2 | tone_100hz_-120_dbfs | left | -143.36 | -163.41 | — | -1.367330465e-10 | — | 0.000001 |
| Split128k | EcBeam2 | tone_100hz_-120_dbfs | right | -143.60 | -164.00 | — | 1.496053624e-10 | — | 0.000001 |

## High-frequency stress spectral metrics

| Filter | Modulator | Input contract | Phase | Channel | Conventional SINAD dB | Worst declared product | Product-excluded residual dBFS | Unexpected spur dBFS |
| --- | --- | --- | --- | --- | ---: | --- | ---: | ---: |
| Split128k | Standard | rated_source_peak | steady | left | 174.91 | upper_imd (-205.89 dBFS) | -181.94 | -201.13 |
| Split128k | Standard | rated_source_peak | recovery | left | 174.91 | upper_imd (-209.93 dBFS) | -181.93 | -202.18 |
| Split128k | Standard | rated_source_peak | steady | right | 174.92 | upper_imd (-206.86 dBFS) | -181.95 | -200.82 |
| Split128k | Standard | rated_source_peak | recovery | right | 174.83 | upper_imd (-209.35 dBFS) | -181.86 | -201.21 |
| Split128k | EcDepth2 | rated_source_peak | steady | left | 166.80 | upper_imd (-184.59 dBFS) | -174.25 | -194.50 |
| Split128k | EcDepth2 | rated_source_peak | recovery | left | 166.91 | upper_imd (-184.89 dBFS) | -174.35 | -194.08 |
| Split128k | EcDepth2 | rated_source_peak | steady | right | 166.80 | upper_imd (-184.59 dBFS) | -174.25 | -194.50 |
| Split128k | EcDepth2 | rated_source_peak | recovery | right | 166.91 | upper_imd (-184.89 dBFS) | -174.35 | -194.08 |
| Split128k | EcBeam | rated_source_peak | steady | left | 168.94 | upper_imd (-183.38 dBFS) | -174.55 | -190.35 |
| Split128k | EcBeam | rated_source_peak | recovery | left | 168.85 | upper_imd (-183.16 dBFS) | -174.48 | -190.67 |
| Split128k | EcBeam | rated_source_peak | steady | right | 168.94 | upper_imd (-183.38 dBFS) | -174.55 | -190.35 |
| Split128k | EcBeam | rated_source_peak | recovery | right | 168.85 | upper_imd (-183.16 dBFS) | -174.48 | -190.67 |
| Split128k | Standard | matched_effective_peak | steady | left | 174.83 | upper_imd (-204.00 dBFS) | -181.87 | -199.52 |
| Split128k | Standard | matched_effective_peak | recovery | left | 174.87 | upper_imd (-205.82 dBFS) | -181.91 | -201.54 |
| Split128k | Standard | matched_effective_peak | steady | right | 174.79 | upper_imd (-206.53 dBFS) | -181.82 | -201.25 |
| Split128k | Standard | matched_effective_peak | recovery | right | 174.85 | upper_imd (-208.73 dBFS) | -181.87 | -200.32 |
| Split128k | EcDepth2 | matched_effective_peak | steady | left | 166.80 | upper_imd (-184.59 dBFS) | -174.25 | -194.50 |
| Split128k | EcDepth2 | matched_effective_peak | recovery | left | 166.80 | upper_imd (-184.81 dBFS) | -174.24 | -194.16 |
| Split128k | EcDepth2 | matched_effective_peak | steady | right | 166.80 | upper_imd (-184.59 dBFS) | -174.25 | -194.50 |
| Split128k | EcDepth2 | matched_effective_peak | recovery | right | 166.80 | upper_imd (-184.81 dBFS) | -174.24 | -194.16 |
| Split128k | EcBeam | matched_effective_peak | steady | left | 167.49 | upper_imd (-188.71 dBFS) | -174.71 | -190.57 |
| Split128k | EcBeam | matched_effective_peak | recovery | left | 167.46 | upper_imd (-188.58 dBFS) | -174.67 | -191.51 |
| Split128k | EcBeam | matched_effective_peak | steady | right | 167.49 | upper_imd (-188.71 dBFS) | -174.71 | -190.57 |
| Split128k | EcBeam | matched_effective_peak | recovery | right | 167.46 | upper_imd (-188.58 dBFS) | -174.67 | -191.51 |
| Split128k | EcBeam2 | rated_source_peak | steady | left | 187.19 | upper_imd (-224.17 dBFS) | -192.21 | -212.39 |
| Split128k | EcBeam2 | rated_source_peak | recovery | left | 187.27 | upper_imd (-215.84 dBFS) | -192.31 | -211.98 |
| Split128k | EcBeam2 | rated_source_peak | steady | right | 187.19 | upper_imd (-224.17 dBFS) | -192.21 | -212.39 |
| Split128k | EcBeam2 | rated_source_peak | recovery | right | 187.27 | upper_imd (-215.84 dBFS) | -192.31 | -211.98 |
| Split128k | EcBeam2 | matched_effective_peak | steady | left | 185.37 | upper_imd (-225.42 dBFS) | -192.39 | -211.23 |
| Split128k | EcBeam2 | matched_effective_peak | recovery | left | 185.42 | upper_imd (-226.19 dBFS) | -192.44 | -211.61 |
| Split128k | EcBeam2 | matched_effective_peak | steady | right | 185.37 | upper_imd (-225.42 dBFS) | -192.39 | -211.23 |
| Split128k | EcBeam2 | matched_effective_peak | recovery | right | 185.42 | upper_imd (-226.19 dBFS) | -192.44 | -211.61 |

## High-frequency stress transitions

| Filter | Modulator | Input contract | Channel | Settled peak | Waveform peak | Excess | Zero-input transition peak dBFS | Clean mute peak/RMS dBFS | Restart residual peak dBFS | Restart RMS 1/10/50 ms dBFS | Recovery ms |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- | ---: | --- | ---: |
| Split128k | Standard | rated_source_peak | left | 0.630427 | 0.657123 | 0.026695 | -5.23 | -172.18 / -180.39 | -4.09 | -8.27 / -18.07 / -25.05 | 1942.32 |
| Split128k | Standard | rated_source_peak | right | 0.630427 | 0.657123 | 0.026695 | -5.23 | -171.66 / -180.65 | -4.09 | -8.27 / -18.07 / -25.05 | 1941.71 |
| Split128k | EcDepth2 | rated_source_peak | left | 0.630427 | 0.657123 | 0.026695 | -5.23 | -162.67 / -170.44 | -4.09 | -8.27 / -18.07 / -25.05 | 1947.97 |
| Split128k | EcDepth2 | rated_source_peak | right | 0.630427 | 0.657123 | 0.026695 | -5.23 | -162.67 / -170.44 | -4.09 | -8.27 / -18.07 / -25.05 | 1947.97 |
| Split128k | EcBeam | rated_source_peak | left | 0.793661 | 0.827268 | 0.033607 | -3.23 | -166.25 / -173.92 | -2.09 | -6.27 / -16.07 / -23.05 | 1935.62 |
| Split128k | EcBeam | rated_source_peak | right | 0.793661 | 0.827268 | 0.033607 | -3.23 | -166.25 / -173.92 | -2.09 | -6.27 / -16.07 / -23.05 | 1935.62 |
| Split128k | Standard | matched_effective_peak | left | 0.630427 | 0.657123 | 0.026695 | -5.23 | -172.14 / -180.14 | -4.09 | -8.27 / -18.07 / -25.05 | 1946.49 |
| Split128k | Standard | matched_effective_peak | right | 0.630427 | 0.657123 | 0.026695 | -5.23 | -172.16 / -180.52 | -4.09 | -8.27 / -18.07 / -25.05 | 1943.39 |
| Split128k | EcDepth2 | matched_effective_peak | left | 0.630427 | 0.657123 | 0.026695 | -5.23 | -161.96 / -170.43 | -4.09 | -8.27 / -18.07 / -25.05 | 1949.21 |
| Split128k | EcDepth2 | matched_effective_peak | right | 0.630427 | 0.657123 | 0.026695 | -5.23 | -161.96 / -170.43 | -4.09 | -8.27 / -18.07 / -25.05 | 1949.21 |
| Split128k | EcBeam | matched_effective_peak | left | 0.630427 | 0.657123 | 0.026695 | -5.23 | -165.18 / -173.35 | -4.09 | -8.27 / -18.07 / -25.05 | 1953.28 |
| Split128k | EcBeam | matched_effective_peak | right | 0.630427 | 0.657123 | 0.026695 | -5.23 | -165.18 / -173.35 | -4.09 | -8.27 / -18.07 / -25.05 | 1953.28 |
| Split128k | EcBeam2 | rated_source_peak | left | 0.793661 | 0.827268 | 0.033607 | -3.23 | -183.28 / -191.06 | -2.09 | -6.27 / -16.07 / -23.05 | 1928.82 |
| Split128k | EcBeam2 | rated_source_peak | right | 0.793661 | 0.827268 | 0.033607 | -3.23 | -183.28 / -191.06 | -2.09 | -6.27 / -16.07 / -23.05 | 1928.82 |
| Split128k | EcBeam2 | matched_effective_peak | left | 0.630427 | 0.657123 | 0.026695 | -5.23 | -180.88 / -189.83 | -4.09 | -8.27 / -18.07 / -25.05 | 1940.52 |
| Split128k | EcBeam2 | matched_effective_peak | right | 0.630427 | 0.657123 | 0.026695 | -5.23 | -180.88 / -189.83 | -4.09 | -8.27 / -18.07 / -25.05 | 1940.52 |

## Hi-res reconstruction carriers

| Filter | Modulator | Channel | Carrier | Frequency Hz | Gain error dB |
| --- | --- | --- | --- | ---: | ---: |
| Split128k | Standard | left | hires_1khz | 1001.294 | -0.0000 |
| Split128k | Standard | left | hires_18khz | 18001.758 | 0.0000 |
| Split128k | Standard | left | hires_40khz | 39997.925 | -0.0000 |
| Split128k | Standard | left | hires_70khz | 69999.060 | -0.0000 |
| Split128k | Standard | right | hires_1khz | 1001.294 | -0.0000 |
| Split128k | Standard | right | hires_18khz | 18001.758 | 0.0000 |
| Split128k | Standard | right | hires_40khz | 39997.925 | -0.0000 |
| Split128k | Standard | right | hires_70khz | 69999.060 | -0.0000 |
| Split128k | EcDepth2 | left | hires_1khz | 1001.294 | 0.0000 |
| Split128k | EcDepth2 | left | hires_18khz | 18001.758 | 0.0000 |
| Split128k | EcDepth2 | left | hires_40khz | 39997.925 | -0.0000 |
| Split128k | EcDepth2 | left | hires_70khz | 69999.060 | -0.0003 |
| Split128k | EcDepth2 | right | hires_1khz | 1001.294 | 0.0000 |
| Split128k | EcDepth2 | right | hires_18khz | 18001.758 | 0.0000 |
| Split128k | EcDepth2 | right | hires_40khz | 39997.925 | -0.0000 |
| Split128k | EcDepth2 | right | hires_70khz | 69999.060 | -0.0003 |
| Split128k | EcBeam | left | hires_1khz | 1001.294 | -0.0000 |
| Split128k | EcBeam | left | hires_18khz | 18001.758 | -0.0000 |
| Split128k | EcBeam | left | hires_40khz | 39997.925 | -0.0000 |
| Split128k | EcBeam | left | hires_70khz | 69999.060 | -0.0000 |
| Split128k | EcBeam | right | hires_1khz | 1001.294 | -0.0000 |
| Split128k | EcBeam | right | hires_18khz | 18001.758 | -0.0000 |
| Split128k | EcBeam | right | hires_40khz | 39997.925 | -0.0000 |
| Split128k | EcBeam | right | hires_70khz | 69999.060 | -0.0000 |

## Hi-res reconstruction bands

| Filter | Modulator | Channel | Band Hz | Residual dBFS | Unexpected spur dBFS |
| --- | --- | --- | --- | ---: | ---: |
| Split128k | Standard | left | 0–20000 | -185.88 | -217.99 |
| Split128k | Standard | left | 20000–80000 | -119.67 | -133.73 |
| Split128k | Standard | right | 0–20000 | -191.03 | -218.48 |
| Split128k | Standard | right | 20000–80000 | -119.81 | -135.77 |
| Split128k | EcDepth2 | left | 0–20000 | -166.07 | -201.45 |
| Split128k | EcDepth2 | left | 20000–80000 | -106.45 | -121.64 |
| Split128k | EcDepth2 | right | 0–20000 | -166.07 | -201.45 |
| Split128k | EcDepth2 | right | 20000–80000 | -106.45 | -121.64 |
| Split128k | EcBeam | left | 0–20000 | -162.25 | -197.59 |
| Split128k | EcBeam | left | 20000–80000 | -106.89 | -120.44 |
| Split128k | EcBeam | right | 0–20000 | -162.25 | -197.59 |
| Split128k | EcBeam | right | 20000–80000 | -106.89 | -120.44 |
