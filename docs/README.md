# Documentation

- [macOS Installation](install.md): Downloading the unsigned DMG, approving its first launch, and optionally setting up `fozmoctl` and the agent skill.

Current product documentation, architecture notes, operating guides, and release checks live here. Research plans, experiment logs, tuning runs, and historical handoffs are kept in the [development archive](dev/README.md).

- [Architecture](architecture.md): Current application shape and target ownership boundaries.
- [Code Quality](code-quality.md): Clippy allow policy, frontend API facade policy, and verification expectations.
- [DSP](dsp.md): Parametric EQ, upsampling filters, DSD modulators, tuned headroom, and M4 performance.
- [Split Phase DSD Measurements](Measurements.md): P17 results for both modulators at DSD64/128, regular 7th Order at DSD256, and M4 DSD128 Search performance.
- [Public PCM-to-DSD Measurements](dsd-public-quality.md): Versioned measurement contract, reconstruction profiles, metrics, provenance, completeness, and structural gates; its current presentation score predates the P17 consolidation.
- [Audio Pipeline](audio-pipeline.md): Decode, DSP, DSD, output, and sink routing notes.
- [Local Data](local-data.md): Runtime settings, library data, cleanup, and release checks.
- [Generated Assets](generated-assets.md): Frontend build artifact policy and release checks.
- [LAN Mode And Optional Pairing](lan-pairing.md): trusted-LAN startup, browser pairing, native-agent stream sessions, and release checks.
- [Remote Access](remote-access.md): Manual port forwarding, remote sessions, TLS fingerprint verification, and remote-device revocation.
- [Platform Support](platform-support.md): Current platform expectations, caveats, and smoke-test requirements.
- [macOS Dev Signing](macos-dev-signing.md): Keychain prompt consolidation and the one-time stable dev code-signing setup.
- [Packaging And License Notes](packaging.md): Release shapes, feature flags, asset policy, runtime exclusions, and license review notes.
- [macOS Launcher And DMG](../macos/README.md): Menu-bar lifecycle, ad-hoc builds, signing/notarization, Sparkle, FFmpeg provenance, and public release gates.
- [Security And Trust Debt](security.md): Tracked advisory ignores and release-review security debt.
- [Manual Smoke Tests](manual-smoke-tests.md): Evidence matrix for hardware, LAN agents, and external services.
- [Manual Smoke Evidence Template](manual-smoke-evidence.example.md): Ignored local evidence workflow and sanitization checklist.
