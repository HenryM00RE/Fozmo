# PCM-to-DSD Verification

Fozmo includes a repeatable check for its PCM-to-DSD converter. It creates its
own test signals, runs them through the playback path, and measures the digital
output.

It checks levels, quiet signals, unwanted tones, busy passages, clean restarts
and stability. It does not require music files or a network connection.

The supported product modes are:

- 7th Order at DSD64, DSD128 and DSD256;
- 7th Order Search at DSD64 and DSD128; and
- the current Linear Phase, Minimum Phase and Split Phase filters.

## Run it

Run the full check with:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality \
  --check
```

The command writes a readable report and a machine-readable report under
`target/dsd-public-quality/`.

## What a pass means

A pass means the software produced a healthy digital stream in the tested
cases. It does not compare listening quality or measure a DAC's analog output.

See [Audio Measurements](Measurements.md) for the recorded results.
