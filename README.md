# Fozmo

A local music player that combines a self-hosted library with Qobuz streaming, built with Rust and React.

> **Pre-alpha:** macOS on Apple silicon with a USB DAC is the known-good setup. Other platforms, devices, and network outputs remain experimental.

## Highlights

- Local-library and Qobuz playback through Core Audio.
- PCM-to-PCM and PCM-to-DSD upsampling.
- A 10-band parametric equalizer.
- Browser-based control and playlist management.
- Command-line and agent control through `fozmoctl`.

Qobuz is the most complete streaming path and requires your own account and subscription. Experimental integrations include Sonos, AirPlay, Hegel, UPnP, Windows WASAPI/ASIO, and remote agents; treat these as test targets rather than compatibility claims.

See the [DSP guide](docs/dsp.md), [measurements](docs/Measurements.md), and [audio pipeline](docs/audio-pipeline.md) for implementation and quality details.

## Screenshots

### Home

![Home screen](docs/screenshots/home-light.png)

### DSP settings

![DSP settings](docs/screenshots/dsp-dark.png)

### Outputs

![Output settings](docs/screenshots/output-dark.png)

## Install and run

### macOS application

The release is an Apple-silicon menu-bar application for macOS 13 or later and does not require a separate Rust, Node, or FFmpeg installation.

The current DMG is distributed without Apple Developer ID signing, notarization, or automatic updates. Follow the [macOS installation guide](docs/install.md) for first-launch approval and optional command-line setup. Build and release details are in the [packaging guide](docs/packaging.md).

### Source checkout

Use the pinned Rust toolchain with Node.js 22 and npm 10:

```sh
npm --prefix ui ci
npm --prefix ui run build
cargo run --locked --release
```

For frontend development, run the React dev server separately:

```sh
npm --prefix ui run dev
```

## Command-line and agent control

The macOS DMG includes the MIT-licensed `fozmoctl` client. Once the optional shell link from the [installation guide](docs/install.md) is configured, check the running server with:

```sh
fozmoctl doctor
fozmoctl status
```

Agents can use the repository's [DJ skill](.agents/skills/fozmo-dj/SKILL.md) for playback, queue, search, zone, and playlist workflows. The agent must be allowed to execute `fozmoctl` locally.

## Network access

Fresh installations listen on loopback only. Enable **Allow LAN Access** in the macOS menu or start a source checkout with `--lan`:

```sh
cargo run --locked --release -- --port=3001 --lan
```

LAN mode is unauthenticated by default and should only be used on a trusted private network. See [LAN access](docs/lan-pairing.md) for discovery and agent setup.

Internet-facing Remote Access is separate, authenticated, and off by default. Read the [Remote Access guide](docs/remote-access.md) before forwarding any router port.

## Development

Run the main verification suite before larger changes:

```sh
./tools/verify.sh
```

Before publishing artifacts or screenshots, run:

```sh
./tools/public-readiness.sh
```

Release expectations and manual checks are documented in [platform support](docs/platform-support.md), [packaging](docs/packaging.md), [manual smoke tests](docs/manual-smoke-tests.md), and [local data](docs/local-data.md).

## Privacy

Library data, playlists, listening history, and settings stay local. There is no project-operated analytics or telemetry, but Qobuz, metadata, artwork, fonts, network outputs, and Remote Access can make external or local-network connections.

See [Privacy and network behavior](PRIVACY.md) for the full service and data-flow overview.

## Qobuz

The unofficial integration requires your own Qobuz account and active subscription. It uses the Qobuz API but is not certified by Qobuz. Qobuz is a trademark of Qobuz; this project is not affiliated with, endorsed by, sponsored by, or certified by Qobuz.

Streamed audio is held in a temporary playback cache for reliability. It is not intended for exporting, archiving, sharing, or creating a permanent copy. Use remains subject to the [Qobuz Terms of Service](https://www.qobuz.com/us-en/legal/terms).

Documentation and implementation research from the MIT-licensed [QBZ project](https://github.com/vicrodh/qbz) informed this integration, including adapted web-player token extraction and request signing. The integration is otherwise independently implemented; QBZ did not author it. Its upstream copyright and MIT notice are preserved in [`LICENSES/QBZ-MIT.txt`](LICENSES/QBZ-MIT.txt).

## Repository layout

- `src/` — Rust server, playback, library, services, and API code.
- `ui/` and `static/` — React source and built frontend assets.
- `macos/` — Swift menu-bar launcher and DMG tooling.
- `airplay-helper/` and `crates/fozmo-airplay-protocol/` — the standalone AirPlay process and IPC protocol.
- `presets/` — audio presets.
- `docs/` and `tools/` — technical documentation and development scripts.

## License

The launcher, server, web client, DSP, documentation, and AirPlay IPC protocol are released under the MIT License. See [LICENSE](LICENSE) and the [component map](COMPONENTS.md).

Direct-network AirPlay is provided by the separate `fozmo-airplay-helper` process under GPL-2.0-only. See the [GPL aggregation assessment](docs/gpl-aggregation-assessment.md) for the distribution boundary.

Third-party services, trademarks, fonts, album artwork, and other external assets are not relicensed by this repository. Only upload or package assets you have rights to use.
