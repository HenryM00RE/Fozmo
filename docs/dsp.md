# DSP

The filters and modulators are more a matter of personal preference and hardware than there being one combination that works universally. Below is a guide to the DSP options to help you test them and find the combination that works best for you and your setup.

The DSP can output PCM at up to 32-bit/384 kHz, or convert PCM to DSD at up to DSD256.

## Parametric EQ

Fozmo has a 10-band parametric EQ with a separate preamp. Each band can be turned on individually and set to peaking, low shelf, high shelf, low pass, high pass, notch, or all pass. You can adjust the frequency, gain, and Q where the selected filter type uses them.

Underneath, the EQ is a cascade of trapezoidal-integrated state-variable filters running with 64-bit processing. Changes are ramped so moving a control does not cause a hard jump in the filter. If you add positive EQ gain, remember to lower the preamp or leave enough headroom for the boosted peaks.

## Upsampling

I am currently developing and testing the upsampling path on an Apple M4 Mac mini. The heavier options may not run as well on other hardware, especially at the higher DSD rates, so you will need to see what your own machine can handle. For example, combinations such as 7th Order Search at DSD256 may be too intensive for an M3 Pro to run in real time.

This processing mainly depends on single-core speed rather than the total number of CPU cores. Because of that, an M4 Mac mini may outperform M3 Pro or M3 Max configurations in this particular workload even though those chips have more cores. **7th Order Search is only supported on Apple M-series chips** because I have optimized its main processing path specifically for those chips.

Fozmo can upsample to higher-rate PCM or convert the signal to 1-bit DSD. Upsampling does not add back information that was not in the source. The selected filter removes the images created while raising the sample rate, and for DSD the modulator then turns that filtered PCM into the final 1-bit stream.

The available DSD rates are DSD64, DSD128, and DSD256. Going higher also increases the amount of work, and a bigger number may not make it the better option for you. Your DAC, the output path, and the available CPU time all matter. I would try the combinations that play reliably on your setup and decide from there.

### Filters

There are five filters to try. They are all reconstruction filters, but they arrange their impulse response and phase in different ways. Take a listen yourself and see what you think.

| Filter | First-stage taps | What it does |
| --- | ---: | --- |
| Split Phase | 131,073 | Keeps linear phase in the low frequencies, changes to minimum phase in the high frequencies, and blends between the two. |
| Linear Phase | 32,769 | Uses a symmetric FIR response with constant group delay, keeping relative phase aligned through the passband. |
| Min Phase | 16,385 | Converts the response to minimum phase, moving the impulse energy after its leading edge instead of spreading it symmetrically. |
| Compact Phase | 131,071 | Uses a long minimum-phase FIR built from a controlled magnitude response, with a tapered tail and shorter cleanup filters afterward. |
| Smooth Phase | 131,071 | Uses the same compact minimum-phase structure, with a gradual high-frequency taper before the cutoff. |

It is best to use integer upsampling and keep the source in the same sample-rate family. For example, 44.1 kHz sources should go to 88.2, 176.4, or 352.8 kHz, while 48 kHz sources should go to 96, 192, or 384 kHz. These integer-multiple paths are what I tuned the upsampling filters for.

The tap count is for the first and main reconstruction stage. In my testing I did not find better results from going beyond the filter lengths in the current list, so I left the longer experiments out for now. I am open to feedback and can reinstate some longer filters if people want them. The long filters use partitioned FFT convolution for the first stage, then shorter half-band filters for each extra 2× step up to the selected output rate.

### DSD modulators

The three selectable modulators all use a seventh-order cascaded-resonator-feedback delta-sigma loop. The main difference is how they choose the next 1-bit output.

| Modulator | Architecture | Tuned headroom |
| --- | --- | ---: |
| 7th Order | Makes each decision directly from the current loop output. This is the simplest and lightest option. | −4 dB |
| 7th Order EC | Adds a short error-compensated lookahead to the same seventh-order loop. It checks possible decisions against the predicted future state before choosing the next bit. | −4 dB |
| 7th Order Search | Uses a delayed-commitment M-algorithm search. It keeps the best four paths over an eight-sample window and commits decisions as it moves forward. | −2 dB |

The headroom here is important. I tuned 7th Order and 7th Order EC at **−4 dB**, while 7th Order Search was tuned at **−2 dB**. I would use those values with their matching modulator rather than treating them as interchangeable defaults. The EQ page has its own separate headroom control, which works well for EQ boosts, so I suggest adjusting that before changing the tuned headroom on the DSP page.

### What I am currently using

My current setup is:

```text
Output:     DSD128
Filter:     Smooth Phase
Modulator:  7th Order Search
Headroom:   -2 dB
```

This is the combination I am using at the moment. Give the other filters and modulators a listen as well, while keeping the matching headroom values above.

## Performance

I ran the end-to-end renderer benchmark on an Apple M4 Mac mini using a release build with `-C target-cpu=native`. Each row renders 8,192 frames of stereo 44.1 kHz audio, or about 185.8 ms, through Smooth Phase. The times below are the average of five runs after two warm-up passes.

### DSD128

| Modulator | Average render time | One-core real-time load | Real-time margin | Resets / clamps |
| --- | ---: | ---: | ---: | ---: |
| 7th Order | 38.7 ms | 20.8% | 4.81× | 0 / 0 |
| 7th Order EC | 63.8 ms | 34.3% | 2.91× | 0 / 0 |
| 7th Order Search | 94.2 ms | 50.7% | 1.97× | 0 / 0 |

### DSD256

| Modulator | Average render time | One-core real-time load | Real-time margin | Resets / clamps |
| --- | ---: | ---: | ---: | ---: |
| 7th Order | 67.4 ms | 36.3% | 2.75× | 0 / 0 |
| 7th Order EC | 115.9 ms | 62.4% | 1.60× | 0 / 0 |
| 7th Order Search | 177.7 ms | 95.7% | 1.05× | 0 / 0 |

DSD256 with 7th Order Search is very close to the real-time limit on the M4 Mac mini. Its 1.05× margin does not leave much room for background load or changes in the source material.

These numbers are only showing performance on my test machine, not audio quality. The load can move around depending on the song and its source sample rate, along with the processor, background load, DSD rate, and filter. If you want to use one of the heavier combinations, it is worth running the benchmark on the machine that will actually be playing the music:

```sh
RUSTFLAGS="-C target-cpu=native" cargo run --release --bin dsd_renderer_bench
RUSTFLAGS="-C target-cpu=native" cargo run --release --bin dsd_modulator_bench
```

If playback cannot keep up, try a lower DSD rate or a lighter modulator. PCM is also there as the safer option when a DAC or output path does not handle DSD reliably.

## Testing so far

The DSP has not yet been verified with external measurement hardware. So far I have relied on software measurements and tuning by ear. Feedback from people trying it with different systems and measurement setups is welcome.
