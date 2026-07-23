# Playback

Product-level playback orchestration lives here.

Current modules:

- `commands.rs`: playback request sequencing and stale-command rejection.
- `dispatcher.rs`: traced playback command entry, common validation and state
  updates, and delegation to already-resolved sinks.
- `auto_advance.rs`: Qobuz completion detection and zone-scoped auto-advance decisions.
- `airplay_volume.rs`: AirPlay default startup-volume policy for zone settings.
- `apply_settings.rs`: Active-zone and zone-scoped playback setting application.
- `config.rs`: Generic playback config validation, effective DSD rules, and
  player config updates.
- `intent.rs`: playback intent, outcome, and loop-mode types shared by playback
  entry points.
- `request.rs`: grouped play request data and request guards passed from intents
  to resolved sinks.
- `queue_advance.rs`: queue exhaustion and Last.fm, Qobuz, and local
  artist-radio fallback policy.
- `sinks/`: enum-based sink resolution plus local, remote-agent, Sonos, and
  UPnP command execution.
- `control.rs`: active-zone and zone-scoped pause, resume, stop, seek, next,
  loop-mode, playback volume, and device-volume wrappers that route through
  `PlaybackDispatcher`.
- `error.rs`: Playback-domain error type used by routes and playback modules.
- `hegel_control.rs`: Hegel settings normalization, target validation, passive
  status cache updates, and control helpers.
- `local.rs`: local file play request parsing and source/queue resolution before
  handing play intent to `PlaybackDispatcher`.
- `monitor.rs`: playback polling, listening observations, and auto-advance monitor startup.
- `now_playing.rs`: current-track matching helpers used to guard queue and prefetch mutations.
- `qobuz.rs`: active-zone and zone-scoped Qobuz request parsing, endpoint
  prefetch, queued stream prefetch, and radio recommendation assembly before
  handing play intent to `PlaybackDispatcher`.
- `queue.rs`: active-zone and zone-scoped queue request conversion, queue snapshot assembly, now-playing queue persistence, shuffle orchestration, and queue mutation side effects for local players, Sonos, Qobuz stream queues, and remote agents.
- `resolver.rs`: local source request resolution and local player queue item assembly.
- `service.rs`: Compatibility re-export surface for playback service helpers.
- `sonos.rs`: Sonos target resolution, source asset preparation, play startup, and next-track prefetch.
- `source.rs`: common `SourceRef` and Qobuz queue/play request conversion helpers.
- `status.rs`: active playback status refresh, zone-aware status response assembly, Sonos polling, and passive Hegel status polling.
- `output_devices.rs`: shared output-device availability and discovery helpers.
- `zone_service.rs`: Zone selection, enable/disable, discovery/list refresh,
  remote-agent registration/state refresh, disconnect cleanup, rename/settings
  updates, and remote-agent signal updates.

The API route layer should stay close to request parsing and response shaping.
Playback actions should enter this package as `PlaybackIntent` values wherever
practical; sink selection belongs in `SinkResolver`, not in route handlers or
source-specific playback modules. `PlaybackDispatcher` owns common command
guards and tracing, while sink-specific execution stays in `sinks/`.

Playback behavior regression tests should live beside the playback module they
exercise. Use `test_support.rs` for shared app-state setup instead of adding
new playback behavior coverage to the API route test hub.
