# Upsampling Filter Verification

This developer tool checks that Fozmo's current upsampling filters behave as
expected after changes to audio processing.

It feeds each filter a few generated clicks and tones, then checks how quickly
the response starts and settles. The results help identify regressions; they
are not a listening-quality ranking.

Run it with:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin filter_timing_bench
```

Reports are written under `target/filter-timing/`.
