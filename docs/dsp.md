# DSP

The filters and modulators are more a matter of personal preference and hardware than there being one combination that works universally. Below is a guide to the DSP options to help you test them and find the combination that works best for you and your setup.

The DSP can output PCM at up to 32-bit/384 kHz, or convert PCM to DSD at up to DSD256.

## Parametric EQ

Fozmo has a 10-band parametric EQ with a separate preamp. Each band can be turned on individually and set to peaking, low shelf, high shelf, low pass, high pass, notch, or all pass. You can adjust the frequency, gain, and Q where the selected filter type uses them.

Underneath, the EQ is a cascade of trapezoidal-integrated state-variable filters running with 64-bit processing. Changes are ramped so moving a control does not cause a hard jump in the filter. If you add positive EQ gain, remember to lower the preamp or leave enough headroom for the boosted peaks.

## Upsampling

I am currently developing and testing the upsampling path on an Apple M4 Mac mini. The heavier options may not run as well on other hardware, so you will need to see what your own machine can handle.

This processing mainly depends on single-core speed rather than the total number of CPU cores. Because of that, an M4 Mac mini may outperform M3 Pro or M3 Max configurations in this particular workload even though those chips have more cores.

Fozmo can upsample to higher-rate PCM or convert the signal to 1-bit DSD. Upsampling does not add back information that was not in the source. The selected filter removes the images created while raising the sample rate, and for DSD the modulator then turns that filtered PCM into the final 1-bit stream.

The available DSD rates are DSD64, DSD128, and DSD256. Going higher also increases the amount of work, and a bigger number may not make it the better option for you. Your DAC, the output path, and the available CPU time all matter. I would try the combinations that play reliably on your setup and decide from there.

### Filters

The selectable filters are 128k-class reconstruction filters, but they arrange their impulse response and phase in different ways. The exact tap counts are odd so each FIR has a well-defined centre sample. Both modulators can use every filter in this list.

| Filter | First-stage taps | What it does |
| --- | ---: | --- |
| Linear Phase | 131,073 | Uses a long symmetric FIR matched to the Split Phase magnitude target, with constant group delay that keeps relative phase aligned through the passband. |
| Minimum Phase | 131,071 | Converts the long reconstruction response to minimum phase, moving the impulse energy after its leading edge instead of spreading it symmetrically. |
| Split Phase | 131,073 | Uses the P17 frozen bundle (`SplitPhase128kE3`). It retains linear phase at low frequencies, transitions toward minimum phase at high frequencies, and reduces ringing, side energy, main-lobe width, step excursion, and measured decay relative to the earlier E2v3 implementation. |

It is best to use integer upsampling and keep the source in the same sample-rate family. For example, 44.1 kHz sources should go to 88.2, 176.4, or 352.8 kHz, while 48 kHz sources should go to 96, 192, or 384 kHz. These integer-multiple paths are what I tuned the upsampling filters for.

The tap count is for the first and main reconstruction stage. In my testing I did not find better results from going beyond the filter lengths in the current list, so I left the longer experiments out for now. I am open to feedback and can reinstate some longer filters if people want them. The long filters use partitioned FFT convolution for the first stage, then shorter half-band filters for each extra 2× step up to the selected output rate.

Older saved Linear Phase selections move to the current 128k Linear Phase filter. Saved 16k Minimum Phase and Compact Phase selections move to the current 128k Minimum Phase filter. Saved E2v3 Split Phase, Split Phase B, and Smooth Phase selections move to P17, now named simply Split Phase. Those older identifiers remain available internally for migration, diagnostics, and benchmarks, but they are not offered during normal playback setup.

### DSD modulators

The two selectable modulators use seventh-order cascaded-resonator-feedback delta-sigma loops. The main difference is how they choose the next 1-bit output.

| Modulator | Architecture | Tuned headroom |
| --- | --- | ---: |
| 7th Order | Makes each decision directly from the current loop output. This is the simplest and lightest option. | −4 dB |
| 7th Order Search | Uses the production fixed M4/N8 beam search with a raw quantizer-error path objective. It supports DSD64, DSD128, and DSD256. | −2 dB |

The headroom here is important. I tuned 7th Order at **−4 dB**, while 7th Order Search uses **−2 dB**. 7th Order Search fixes that headroom and its DSD ISI compensation at zero. Its DSD64, DSD128, and DSD256 plants use one matched production input calibration while retaining their own rate-specific noise-transfer functions. Both modulators work with all supported filters and DSD64, DSD128, or DSD256. The EQ page has its own separate headroom control, which works well for EQ boosts.

### What I am currently using

My default setup is:

```text
Output:     DSD128
Filter:     Split Phase
Modulator:  7th Order Search
Headroom:   -2 dB
```

I use Split Phase with 7th Order Search at DSD128 as my default. This is a
personal listening preference rather than a claim that it is universally the
best combination. Give the other filters and modulators a listen as well,
while keeping the matching headroom values above. The reproducible digital
results for Split Phase with 7th Order and 7th Order Search are collected in
[Split Phase DSD Measurements](Measurements.md).

## Performance

Performance depends on the song and its source sample rate, along with the processor, background load, DSD rate, filter, and modulator. If you want to measure a combination, run the benchmarks on the machine that will actually be playing the music:

```sh
RUSTFLAGS="-C target-cpu=native" cargo run --release --bin resampler_bench
RUSTFLAGS="-C target-cpu=native" cargo run --release --bin dsd_renderer_bench
RUSTFLAGS="-C target-cpu=native" cargo run --release --bin dsd_modulator_bench
RUSTFLAGS="-C target-cpu=native" cargo run --locked --release --bin filter_timing_bench
```

If playback cannot keep up, try a lower DSD rate or a lighter modulator. PCM is also there as the safer option when a DAC or output path does not handle DSD reliably.

On an Apple M4 Mac mini, the focused P17 Split Phase + 7th Order Search DSD128
renderer benchmark averaged **53.762 ms** for 8,192 stereo source frames,
equivalent to **3.46× real-time** wall throughput. The modulator-only benchmark
measured **76.69 ns per DSD sample**, or **43.29% of one core per channel** and
**86.58% aggregate for stereo**; neither benchmark reported a reset or clamp.
See [Split Phase DSD Measurements](Measurements.md) for the test conditions and
interpretation.

The [production filter timing bench](filter-timing-bench.md) is a quality
measurement rather than a throughput benchmark. It compares impulse and step
behavior, windowed 5-20 kHz tone packets, energy asymmetry, decay, and group
delay for the three selectable reconstruction filters, plus retained diagnostic
comparisons, under one controlled integer-upsampling setup.

## Testing so far

The reproducible [public PCM-to-DSD measurement bench](dsd-public-quality.md)
tests the production renderer with generated signals and reports digital
linearity, noise, spurs, stability, recovery, and hi-res reconstruction. Its
versioned score still targets the earlier E2v3 path. The current focused P17
run therefore reports measurements without applying that older presentation
score: all 16 cells across DSD64, DSD128, and DSD256 completed with zero
structural failures. A separate legacy 32k linear-phase path remains available
as an internal, non-scoring diagnostic; it is not the selectable 128k Linear
Phase filter described above. The current P17 level, stress, idle, hi-res, and
M4 performance results are collected in
[Split Phase DSD Measurements](Measurements.md). The bench embeds and verifies
the native-CPU release build contract rather than trusting launch-time
environment metadata.

The DSP has not yet been verified with external measurement hardware. Software
measurements describe the generated digital stream, not the analog behavior of
a particular DAC. Feedback from people trying it with different systems and
measurement setups is welcome.
