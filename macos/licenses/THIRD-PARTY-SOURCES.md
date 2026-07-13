# Third-party source locations

- Sparkle 2.9.4: <https://github.com/sparkle-project/Sparkle>, revision
  `b6496a74a087257ef5e6da1c5b29a447a60f5bd7`.
- FFmpeg 8.1.2: <https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz>, SHA-256
  `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`.
- libopus 1.6.1: <https://downloads.xiph.org/releases/opus/opus-1.6.1.tar.gz>,
  SHA-256 `6ffcb593207be92584df15b32466ed64bbec99109f007c82205f0194572411a1`.
- AirPlay network stack: <https://github.com/Pabldi08/airplay2-rs>, revision
  `1baeaae336ca3a9828e732500082f5fd1767d2fd`; the FDK-free local patch is
  shipped under `airplay-helper/vendor/airplay-audio` in corresponding source.
- Anton font: <https://github.com/googlefonts/AntonFont> and
  <https://github.com/google/fonts/tree/main/ofl/anton>.
- QBZ: <https://github.com/vicrodh/qbz>. QBZ documentation and implementation
  research informed Fozmo's understanding of the unofficial Qobuz web API;
  Fozmo's web-player token extraction and request-signature handling adapt
  portions of that MIT-licensed work. The upstream copyright and MIT notice are
  preserved in `LICENSES/QBZ-MIT.txt`. QBZ did not author Fozmo's independent
  integration, and neither project is affiliated with or endorsed by Qobuz.
- Bundled EQ presets are project-maintained experimental configuration data.
  Their original measurement source and method were not recorded; device names
  identify test targets and do not imply manufacturer endorsement or guarantee
  calibration accuracy.
- Qobuz API terms: <https://static.qobuz.com/apps/api/QobuzAPI-TermsofUse.pdf>.
  The application displays the required notice: "This application uses the
  Qobuz API but is not certified by Qobuz."
- Last.fm API terms: <https://www.last.fm/api/tos>. Fozmo does not distribute
  Qobuz or Last.fm logo images; service names identify unofficial integrations.
- SPDX canonical texts: <https://github.com/spdx/license-list-data>, revision
  `c4a7237ec8f4654e867546f9f409749300f1bf4c` (license-list-data v3.28.0).

Every public DMG is accompanied by a version-matched corresponding-source
archive containing lockfiles, build scripts, vendored Cargo sources and exact
FFmpeg/libopus source archives. `cargo-packages.json` and `npm-packages.json`
inside the app record the exact resolved dependency versions and source URLs.
