# Filter timing

Source: 44100 Hz  
Output: 176400 Hz  
Headroom: -12.0 dBFS  
Packet: 8.0-cycle Hann window  
Alignment: impulse by principal peak; packets by historical centroid and actual onset bounds  

## Impulse and step metrics

| Filter | Pre energy (dB total) | Max pre lobe (dB peak) | Post energy (dB total) | Max post lobe (dB peak) | Decay -80 (ms) | Decay -120 (ms) | Main lobe (us) | Step overshoot (%) | Centroid vs peak (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Split Phase | -5.24 | -18.25 | -2.39 | -7.75 | 3.36 | 4.25 | 68.74 | 13.982 | 0.0760 |
| Split Phase E3 (experimental) | -4.86 | -22.84 | -2.85 | -8.70 | 3.82 | 6.59 | 61.94 | 12.564 | 0.0639 |

## Windowed tone packets

The historical columns split energy around the quadrature-envelope centroid. The onset columns use the principal impulse peak plus the nominal source-packet bounds and are the actual pre-echo/post-decay measures.

| Filter | Frequency (Hz) | Before centroid (dB total) | Max before centroid (dB peak) | After centroid (dB total) | Max after centroid (dB peak) | Onset pre-echo (dB total) | Onset post-decay (dB total) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Split Phase | 5000 | -77.24 | -61.08 | -74.76 | -59.65 | -71.67 | -81.81 |
| Split Phase | 10000 | -67.60 | -54.19 | -50.96 | -42.00 | -59.12 | -53.58 |
| Split Phase | 15000 | -38.20 | -28.83 | -30.03 | -24.64 | -48.84 | -25.57 |
| Split Phase | 18000 | -17.05 | -8.99 | -11.44 | -9.90 | -53.86 | -5.31 |
| Split Phase | 20000 | -5.80 | -0.01 | -7.18 | -4.82 | -52.60 | -1.26 |
| Split Phase E3 (experimental) | 5000 | -77.04 | -61.04 | -75.13 | -59.69 | -71.70 | -83.12 |
| Split Phase E3 (experimental) | 10000 | -63.15 | -51.64 | -52.78 | -42.58 | -57.41 | -56.35 |
| Split Phase E3 (experimental) | 15000 | -29.90 | -25.67 | -28.06 | -26.13 | -31.02 | -27.97 |
| Split Phase E3 (experimental) | 18000 | -13.70 | -6.46 | -10.92 | -7.49 | -31.99 | -5.96 |
| Split Phase E3 (experimental) | 20000 | -6.08 | -0.36 | -7.76 | -5.79 | -34.75 | -1.53 |

## Group delay relative to principal peak

| Filter | 100 Hz | 1 kHz | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Split Phase | 0.0009 ms | 0.0009 ms | -0.0102 ms | -0.0079 ms | 0.0580 ms | 0.1657 ms | 0.4650 ms |
| Split Phase E3 (experimental) | 0.0009 ms | 0.0009 ms | -0.0102 ms | -0.0079 ms | 0.0349 ms | 0.1357 ms | 0.3981 ms |

The full 20 Hz-20 kHz group-delay curves are in `group-delay.csv`. The production filters retain their intrinsic transition shapes; the bench does not add a compensating equalizer that would change the filters under test.
