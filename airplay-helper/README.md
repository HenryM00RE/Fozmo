# Fozmo AirPlay Helper

`fozmo-airplay-helper` is a standalone GPL-2.0-only program. It owns all direct
network AirPlay discovery and transport behavior; the MIT Fozmo server talks to
it only through the public, MIT-licensed JSON + PCM protocol in
`../crates/fozmo-airplay-protocol`.

The helper can be used without Fozmo:

```sh
cargo run --manifest-path airplay-helper/Cargo.toml -- list
cargo run --manifest-path airplay-helper/Cargo.toml -- play RECEIVER_ID song.wav
cat song.s16le | cargo run --manifest-path airplay-helper/Cargo.toml -- \
  play RECEIVER_ID - --format pcm-s16le
cargo run --manifest-path airplay-helper/Cargo.toml -- serve \
  --socket "$TMPDIR/fozmo-airplay/control.sock"
```

`serve` also reads `FOZMO_AIRPLAY_SOCKET`. It creates the socket directory as
mode `0700`, both sockets as `0600`, exits on SIGINT/SIGTERM, and by default
exits when its supervising parent's stdin pipe reaches EOF.

## Source and licensing

This complete project, its lockfile, the MIT protocol source, and the pinned
AirPlay 2 source patch must accompany distributed binaries as corresponding
source under GPLv2 section 3. The helper is GPL-2.0-only; it is not covered by
the MIT license used by the Fozmo server and launcher. `vendor/airplay-audio`
comes from airplay2-rs revision
`1baeaae336ca3a9828e732500082f5fd1767d2fd` and is patched only to make FDK AAC
optional. Release builds leave that feature disabled and use ALAC exclusively.

Do not enable the `aac` feature for a distributed GPL helper. The FDK AAC
dependency and symbols must remain absent from `Cargo.lock` and the final
binary.
