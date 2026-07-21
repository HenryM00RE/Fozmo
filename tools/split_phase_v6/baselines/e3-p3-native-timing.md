# Production filter timing

Source: 44100 Hz  
Output: 176400 Hz  
Headroom: -12.0 dBFS  
Packet: 8.0-cycle Hann window  
Alignment: impulse by principal peak; packets by energy centroid  

## Impulse and step metrics

| Filter | Pre energy (dB total) | Max pre lobe (dB peak) | Post energy (dB total) | Max post lobe (dB peak) | Decay -80 (ms) | Decay -120 (ms) | Main lobe (us) | Step overshoot (%) | Centroid vs peak (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Split Phase | -5.24 | -18.25 | -2.39 | -7.75 | 3.36 | 4.25 | 68.74 | 13.982 | 0.0760 |
| Split Phase E3 (experimental) | -4.93 | -23.17 | -2.83 | -8.30 | 4.05 | 6.76 | 62.23 | 13.295 | 0.0579 |

## Windowed tone packets

Energy outside the nominal packet window is measured after aligning its quadrature envelope by energy centroid.

| Filter | Frequency (Hz) | Pre-echo energy (dB total) | Max pre-echo (dB peak) | Post-echo energy (dB total) | Max post-echo (dB peak) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Split Phase | 5000 | -77.24 | -61.08 | -74.76 | -59.65 |
| Split Phase | 10000 | -67.60 | -54.19 | -50.96 | -42.00 |
| Split Phase | 15000 | -38.20 | -28.83 | -30.03 | -24.64 |
| Split Phase | 18000 | -17.05 | -8.99 | -11.44 | -9.90 |
| Split Phase | 20000 | -5.80 | -0.01 | -7.18 | -4.82 |
| Split Phase E3 (experimental) | 5000 | -77.04 | -61.05 | -75.17 | -59.69 |
| Split Phase E3 (experimental) | 10000 | -62.85 | -51.50 | -53.15 | -42.69 |
| Split Phase E3 (experimental) | 15000 | -27.75 | -24.90 | -25.88 | -23.25 |
| Split Phase E3 (experimental) | 18000 | -17.43 | -9.49 | -12.12 | -10.55 |
| Split Phase E3 (experimental) | 20000 | -5.66 | -0.29 | -7.51 | -6.60 |

## Group delay relative to principal peak

| Filter | 100 Hz | 1 kHz | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Split Phase | 0.0009 ms | 0.0009 ms | -0.0102 ms | -0.0079 ms | 0.0580 ms | 0.1657 ms | 0.4650 ms |
| Split Phase E3 (experimental) | 0.0009 ms | 0.0009 ms | -0.0102 ms | -0.0079 ms | 0.0099 ms | 0.1066 ms | 0.3760 ms |

The full 20 Hz-20 kHz group-delay curves are in `group-delay.csv`. The production filters retain their intrinsic transition shapes; the bench does not add a compensating equalizer that would change the filters under test.
