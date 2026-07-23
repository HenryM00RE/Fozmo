# Fozmo component and license map

This repository is a mixed-license aggregate. The root `LICENSE` remains the
MIT license; it does not relicense the separately identified GPL program.

| Component / distributed path | Source | SPDX license | Boundary |
| --- | --- | --- | --- |
| Fozmo Rust server, DSP, browser client, and CLI tools | `src/`, `ui/` | `MIT` | Main application |
| macOS menu-bar launcher | `macos/` | `MIT` | Main application launcher |
| AirPlay helper IPC schema | `crates/fozmo-airplay-protocol/` | `MIT` | Versioned JSON and standard PCM only |
| Standalone AirPlay helper | `airplay-helper/` excluding separately listed dependencies | `GPL-2.0-only` | Independent process and executable |
| Patched airplay2-rs audio crate | `airplay-helper/vendor/airplay-audio/` | `GPL-2.0-only` | Helper dependency; pinned ALAC-only patch |
| Other pinned airplay2-rs crates | Exact revisions in `airplay-helper/Cargo.lock` | `GPL-2.0-only` | Helper dependencies only |
| Sparkle updater | Swift package revision recorded by Xcode | `MIT` | Third-party framework |
| Bundled FFmpeg executable | Release build manifest and corresponding source archive | `LGPL-2.1-or-later` | Separate executable; GPL/nonfree features disabled |
| libopus used by bundled FFmpeg | Release build manifest and corresponding source archive | `BSD-3-Clause` | Statically linked into FFmpeg helper |
| Anton display font | `static/fonts/anton.ttf` | `OFL-1.1` | Anton 2.116; notice at `macos/licenses/Anton-OFL-1.1.txt` |
| QBZ-informed Qobuz client portions | `src/services/qobuz/` | `MIT` | Adapted token-extraction and request-signing work; upstream notice at `LEGAL/QBZ-MIT.txt` |
| Bundled EQ presets | `presets/*.json` | `MIT` | Project-maintained experimental tuning data; see limitations below |

The server opens the AirPlay helper by opaque receiver ID and communicates over
owner-only Unix-domain sockets using the protocol crate. Receiver connection
targets, DNS-SD TXT records, encryption/pairing details, AirPlay libraries, and
AirPlay protocol state remain inside the helper. The control protocol contains
only an opaque ID plus coarse receiver name/kind/support data; audio is stereo 44.1 kHz signed
16-bit little-endian PCM. There is no FFI, shared memory, dynamic linking, or
serialization of internal Rust application structures across this boundary.

The bundled EQ presets were introduced and are maintained in this repository.
Their original measurement source and method were not recorded. They are not
manufacturer-provided or endorsed, and their device names identify intended
test targets rather than promise calibration accuracy. Treat them as optional,
experimental starting points and verify levels before listening.

Qobuz and Last.fm names are used only to identify integrations. Their marks
remain the property of their respective owners; no affiliation or endorsement
is implied. Fozmo does not distribute their logo image files. See
`LEGAL/THIRD-PARTY-ASSETS.md` for source and terms references.

Canonical project license texts are in `LEGAL/`; the Anton notice used by
the macOS bundle is in `macos/licenses/`. Every binary release must also
provide the corresponding source archive described in `airplay-helper/README.md`
and the license/notice files for all other bundled third-party components.
