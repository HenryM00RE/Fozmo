# GPL Aggregation Assessment For Fozmo 0.0.2

## Decision

The project accepts distribution of the MIT-licensed Fozmo application and the
GPL-2.0-only `fozmo-airplay-helper` in the same Fozmo 0.0.2 DMG as an aggregate
of independently executable programs, subject to every invariant and release
obligation below passing at build time.

This is the project's recorded distribution decision for the unsigned 0.0.2
pre-alpha release. It is a scoped open-source compliance assessment, not legal
advice or a conclusion that every future architecture or release shape has
automatically been approved.

Assessment recorded: 2026-07-22.

## Licence Basis

[GPLv2 section 2](https://www.gnu.org/licenses/old-licenses/gpl-2.0.en.html#SEC2)
states that mere aggregation of a separate work on the same storage or
distribution medium does not bring that other work under the GPL. The
[GNU GPL FAQ on mere aggregation](https://www.gnu.org/licenses/gpl-faq.en.html#MereAggregation)
also explains that separate executables communicating through pipes, sockets,
or command-line arguments are normally separate programs, while noting that
the semantics and intimacy of the communication still matter.

The assessment applies those factors to the release rather than assuming that
putting two binaries in one DMG is sufficient by itself.

## Boundary Facts

- Fozmo and `fozmo-airplay-helper` are separately compiled Mach-O executables
  with separate Cargo manifests, lockfiles, dependency graphs, and licences.
- The MIT application does not link, load, or use FFI into the GPL helper or
  its AirPlay dependencies.
- The processes share no address space, shared memory, database, settings
  objects, Rust data structures, or dynamic libraries.
- Communication uses owner-only Unix-domain sockets. The control surface is a
  versioned JSON protocol containing commands, opaque receiver identifiers,
  and coarse receiver status. Audio is a standard stereo 44.1 kHz signed
  16-bit PCM stream; the helper also has an independent WAV/PCM command-line
  interface.
- Receiver discovery, DNS-SD records, pairing, encryption, AirPlay protocol
  state, timing, ALAC transport, metadata transport, and receiver volume stay
  entirely inside the GPL helper.
- The shared `fozmo-airplay-protocol` crate is independently MIT licensed and
  contains only the IPC schema; it does not contain the GPL AirPlay
  implementation.
- Removing or stopping the helper degrades direct-network AirPlay without
  preventing the MIT application from providing its other outputs and
  features.

These boundaries are enforced by `tools/check-release-boundaries.sh`, the
separate build manifests, binary symbol checks, and app-bundle verification.

## Distribution Obligations

Every DMG covered by this decision must:

1. Identify the MIT application and GPL helper as separately licensed
   components without applying a restrictive umbrella EULA.
2. Include the complete MIT and GPL-2.0-only licence texts and generated
   third-party dependency notices.
3. Publish the exact corresponding-source archive and SHA-256 checksum beside
   the DMG. The archive must include the helper, its patches, lockfile, every
   pinned/vendored Rust dependency, build instructions, and the exact upstream
   FFmpeg/libopus sources used by the release.
4. Preserve recipients' rights to inspect, rebuild, modify, and redistribute
   the GPL helper under GPL-2.0-only.
5. Pass the release-boundary, source-archive, licence, secret, provenance,
   architecture, signature, and DMG-content checks from a clean tracked
   snapshot.

## Re-review Triggers

This decision no longer applies without a new assessment if any of the
following changes:

- the helper is linked or loaded into the Fozmo process;
- FFI, shared memory, internal application structures, or an equivalently
  intimate interface crosses the boundary;
- direct AirPlay implementation code or GPL dependencies enter the MIT graph;
- the IPC schema begins exposing AirPlay protocol internals rather than opaque
  targets, commands, status, and standard PCM;
- the helper can no longer be built, invoked, or replaced independently;
- corresponding source or required licence notices are no longer distributed
  beside the binary;
- an umbrella licence or distribution term restricts rights granted by a
  component licence; or
- the helper, its dependency licences, or the release distribution model
  materially changes.

The release pipeline must fail closed when the tracked assessment, its scope,
or any mechanically checked boundary is missing.
