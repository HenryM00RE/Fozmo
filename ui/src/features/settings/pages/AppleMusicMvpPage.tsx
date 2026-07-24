import { useCallback, useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';

export function AppleMusicMvpPage() {
  const [status, setStatus] = useState<JsonRecord | null>(null);
  const [songID, setSongID] = useState('');
  const [storefront, setStorefront] = useState('nz');
  const [busy, setBusy] = useState('');
  const [message, setMessage] = useState('');
  const [captureConfirmed, setCaptureConfirmed] = useState(false);
  const [matchPosition, setMatchPosition] = useState(true);

  const processTap = recordValue(status?.process_tap);
  const tapMetrics = recordValue(processTap?.metrics);
  const tapState = String(processTap?.state || 'stopped');
  const tapRunning = tapState === 'running';
  const tapSupported = processTap?.supported !== false;
  const musicAppRunning = processTap?.music_app_running === true;
  const comparison = recordValue(status?.comparison);
  const comparisonReference = recordValue(comparison?.reference);
  const comparisonAppleTrack = recordValue(comparison?.apple_music_track);
  const comparisonSide = String(comparison?.active_side || (tapRunning ? 'apple_music' : 'fozmo'));
  const appleSideActive = comparisonSide === 'apple_music';
  const canSwitchToFozmo = comparison?.can_switch_to_fozmo === true;
  const fozmoSideActive = comparisonSide === 'fozmo' && canSwitchToFozmo;

  const loadStatus = useCallback(async () => {
    const next = await endpoints.appleMusicStatus();
    setStatus(next);
    return next;
  }, []);

  useEffect(() => {
    loadStatus().catch((error) =>
      setMessage(`Apple Music helper status failed. ${appleMusicErrorMessage(error)}`)
    );
  }, [loadStatus]);

  useEffect(() => {
    const timer = window.setInterval(
      () => {
        loadStatus().catch(() => undefined);
      },
      tapRunning ? 1000 : 2500
    );
    return () => window.clearInterval(timer);
  }, [loadStatus, tapRunning]);

  const run = async (key: string, action: () => Promise<JsonRecord>, success: string) => {
    if (busy) return;
    setBusy(key);
    setMessage('');
    try {
      setStatus(await action());
      setMessage(success);
    } catch (error) {
      setMessage(appleMusicErrorMessage(error));
      await loadStatus().catch(() => undefined);
    } finally {
      setBusy('');
    }
  };

  const nowPlaying = useMemo(() => recordValue(status?.now_playing), [status?.now_playing]);
  const helperPresent = status?.helper_present === true;
  const musicKitEntitled = status?.helper_musickit_entitled === true;
  const authorized = status?.authorization === 'authorized';
  const canPlay = status?.can_play_catalog_content === true;
  const playbackState = String(status?.playback_state || 'stopped');
  const isPlaying = playbackState === 'playing';
  const isPaused = playbackState === 'paused';
  const canPrepare = helperPresent && musicKitEntitled && authorized && canPlay && !busy;

  return (
    <section className="settings-panel apple-music-capture-page apple-music-mvp-page">
      {message ? (
        <div className="metadata-assigner-message apple-music-message">{message}</div>
      ) : null}

      <div className="apple-music-mvp-banner">
        <div>
          <span className="section-label">Isolated native experiment</span>
          <h2>Apple Music MVP</h2>
          <p>
            Route the normal Music app into Fozmo&apos;s live PCM and DSP path without a MusicKit
            provisioning profile. The Music app remains the transport and catalog UI.
          </p>
        </div>
        <span className={`stamp ${statusStampClass(status)}`}>{statusLabel(status)}</span>
      </div>

      <div className="settings-grid apple-music-grid">
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Quick A/B comparison</div>
            <span className="apple-music-ab-route">
              Same {String(comparisonReference?.zone_name || 'Fozmo output')} + DSP
            </span>
          </div>
          <div className="panel raised apple-music-ab-panel">
            <div
              className="apple-music-ab-switch"
              role="group"
              aria-label="Choose the active comparison source"
            >
              <button
                className={`apple-music-ab-choice ${appleSideActive ? 'is-active' : ''}`}
                type="button"
                aria-pressed={appleSideActive}
                disabled={
                  Boolean(busy) ||
                  appleSideActive ||
                  !tapSupported ||
                  !musicAppRunning ||
                  !captureConfirmed
                }
                onClick={() =>
                  run(
                    'comparison-apple',
                    () =>
                      endpoints.switchAppleMusicComparison(
                        'apple_music',
                        captureConfirmed,
                        matchPosition
                      ),
                    matchPosition
                      ? 'Switched to Apple Music at the matching position.'
                      : 'Switched to Apple Music.'
                  )
                }
              >
                <span className="apple-music-ab-letter">A</span>
                <span className="apple-music-ab-copy">
                  <strong>Apple Music</strong>
                  <small>
                    {comparisonTrackLabel(
                      comparisonAppleTrack,
                      musicAppRunning ? 'Current track in Music' : 'Open Music first'
                    )}
                  </small>
                </span>
                <span className="apple-music-ab-state">
                  {appleSideActive ? 'Playing' : 'Switch'}
                </span>
              </button>

              <button
                className={`apple-music-ab-choice ${fozmoSideActive ? 'is-active' : ''}`}
                type="button"
                aria-pressed={fozmoSideActive}
                disabled={Boolean(busy) || fozmoSideActive || !canSwitchToFozmo}
                onClick={() =>
                  run(
                    'comparison-fozmo',
                    () => endpoints.switchAppleMusicComparison('fozmo', false, matchPosition),
                    matchPosition
                      ? 'Switched to the Fozmo reference at the matching position.'
                      : 'Switched to the Fozmo reference.'
                  )
                }
              >
                <span className="apple-music-ab-letter">B</span>
                <span className="apple-music-ab-copy">
                  <strong>{comparisonProviderLabel(comparisonReference)}</strong>
                  <small>
                    {comparisonTrackLabel(
                      comparisonReference,
                      canSwitchToFozmo ? 'Remembered Fozmo source' : 'Play it in Fozmo first'
                    )}
                  </small>
                </span>
                <span className="apple-music-ab-state">
                  {fozmoSideActive && canSwitchToFozmo
                    ? 'Playing'
                    : canSwitchToFozmo
                      ? 'Switch'
                      : 'Not set'}
                </span>
              </button>
            </div>

            {!canSwitchToFozmo ? (
              <div className="apple-music-ab-guide">
                <strong>Set up the reference once</strong>
                <span>
                  Play the matching Qobuz or local track in Fozmo. The first switch to Apple Music
                  remembers that source for this server run and makes both buttons one-click.
                </span>
              </div>
            ) : (
              <div className="apple-music-ab-details">
                <span>
                  Fozmo handoff: {comparisonProviderLabel(comparisonReference)} ·{' '}
                  {formatDuration(numberValue(comparisonReference?.position_secs))}
                </span>
                <span>
                  Apple handoff · {formatDuration(numberValue(comparisonAppleTrack?.position_secs))}
                </span>
              </div>
            )}

            <div className="apple-music-ab-options">
              <label className="apple-music-capture-confirmation">
                <input
                  type="checkbox"
                  checked={captureConfirmed}
                  disabled={tapRunning || Boolean(busy)}
                  onChange={(event) => setCaptureConfirmed(event.target.checked)}
                />
                <span>
                  Allow the comparison to capture only Music app audio and feed it through the
                  selected Fozmo path.
                </span>
              </label>
              <label className="apple-music-capture-confirmation">
                <input
                  type="checkbox"
                  checked={matchPosition}
                  disabled={Boolean(busy)}
                  onChange={(event) => setMatchPosition(event.target.checked)}
                />
                <span>Match elapsed time when switching (recommended for A/B listening).</span>
              </label>
            </div>
            <p className="apple-music-tap-rate-note">
              The handoff keeps the selected output, DSP, upsampling, and −2 dB seventh-order
              headroom unchanged. Qobuz/local playback may take a moment to reopen and seek.
            </p>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Music app → Fozmo DSP</div>
            <button
              className="settings-heading-refresh"
              type="button"
              aria-label="Refresh Music app process tap status"
              onClick={() =>
                loadStatus().catch((error) => setMessage(appleMusicErrorMessage(error)))
              }
            >
              <Icon path="M21 12a9 9 0 0 1-15.3 6.36M3 12A9 9 0 0 1 18.3 5.64M18 2v4h-4M6 22v-4h4" />
            </button>
          </div>
          <div className="panel raised apple-music-tap-panel">
            <div className="settings-list compact-list">
              <StatusRow
                label="Music app"
                value={
                  musicAppRunning
                    ? `open · PID ${String(processTap?.music_app_pid || '—')}`
                    : 'not detected'
                }
              />
              <StatusRow label="Process tap" value={formatProtocolLabel(tapState)} />
              <StatusRow
                label="Core Audio source"
                value={
                  processTap?.audio_process_object_id
                    ? `process ${String(processTap.audio_process_object_id)} · tap ${String(
                        processTap.tap_object_id || '—'
                      )}`
                    : musicAppRunning
                      ? 'ready when audio is playing'
                      : 'waiting for Music'
                }
              />
              <StatusRow label="Captured PCM" value={tapFormatLabel(processTap)} />
              <StatusRow label="PCM precision" value={tapPrecisionLabel(processTap)} />
              <StatusRow label="Fozmo ingress" value={tapIngressLabel(processTap)} />
              <StatusRow
                label="DSP handoff"
                value={processTap?.dsp_handoff_active === true ? 'active' : 'inactive'}
              />
              <StatusRow
                label="Fozmo output"
                value={String(processTap?.output_device || 'current selected output')}
              />
              <StatusRow
                label="Direct Music path"
                value={
                  tapRunning && processTap?.original_audio_muted_while_tapped === true
                    ? 'muted while Fozmo reads'
                    : 'unchanged'
                }
              />
            </div>

            <div className="apple-music-tap-metrics" aria-label="Process tap telemetry">
              <div>
                <span>Callbacks</span>
                <strong>{formatInteger(tapMetrics?.callbacks_received)}</strong>
              </div>
              <div>
                <span>Frames</span>
                <strong>{formatInteger(tapMetrics?.frames_received)}</strong>
              </div>
              <div>
                <span>Input RMS</span>
                <strong>
                  {formatRms(tapMetrics?.rms_l)} / {formatRms(tapMetrics?.rms_r)}
                </strong>
              </div>
              <div>
                <span>Last audio</span>
                <strong>{formatAge(tapMetrics?.last_callback_age_ms)}</strong>
              </div>
              <div>
                <span>Ring overruns</span>
                <strong>{formatInteger(tapMetrics?.ring_overruns)}</strong>
              </div>
            </div>
            <p className="apple-music-tap-rate-note">
              Core Audio supplies rendered Float32 PCM and Fozmo preserves those sample values
              without Int16/Int24 quantization. Float32 has 24 bits of numerical precision, but the
              album&apos;s original sample rate, bit depth, and selected lossless variant remain
              unknown at this tap.
            </p>

            {!tapSupported ? (
              <div className="apple-music-driver-callout">
                <strong>macOS 14.2 or newer is required</strong>
                <span>This Mac cannot create a Core Audio process tap.</span>
              </div>
            ) : !musicAppRunning ? (
              <div className="apple-music-driver-callout">
                <strong>Open Music and start a song</strong>
                <span>Fozmo only includes the Music app process; other Mac audio is excluded.</span>
              </div>
            ) : (
              <div className="apple-music-routing-callout">
                <strong>System-audio permission may appear once</strong>
                <span>
                  Allow Fozmo under System Settings → Privacy &amp; Security → Screen &amp; System
                  Audio Recording. Stopping this experiment destroys the tap and restores
                  Music&apos;s direct audio.
                </span>
              </div>
            )}

            <div className="service-settings-actions">
              {!tapRunning ? (
                <button
                  className="pill is-active"
                  type="button"
                  disabled={Boolean(busy) || !tapSupported || !musicAppRunning || !captureConfirmed}
                  onClick={() =>
                    run(
                      'tap-start',
                      () => endpoints.startAppleMusicProcessTap(captureConfirmed, true),
                      'Music app audio is now feeding the selected Fozmo DSP path.'
                    )
                  }
                >
                  Start Music → DSP
                </button>
              ) : (
                <button
                  className="pill service-settings-danger"
                  type="button"
                  disabled={Boolean(busy)}
                  onClick={() =>
                    run(
                      'tap-stop',
                      () => endpoints.stopAppleMusicProcessTap(),
                      'Process tap stopped; Music app direct audio was restored.'
                    )
                  }
                >
                  Stop &amp; restore Music audio
                </button>
              )}
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Helper session</div>
            <button
              className="settings-heading-refresh"
              type="button"
              aria-label="Refresh Apple Music helper status"
              onClick={() =>
                loadStatus().catch((error) => setMessage(appleMusicErrorMessage(error)))
              }
            >
              <Icon path="M21 12a9 9 0 0 1-15.3 6.36M3 12A9 9 0 0 1 18.3 5.64M18 2v4h-4M6 22v-4h4" />
            </button>
          </div>
          <div className="panel raised">
            <div className="settings-list compact-list">
              <StatusRow label="Stage" value="MusicKit helper proof" />
              <StatusRow label="Helper" value={helperPresent ? 'available' : 'missing'} />
              <StatusRow
                label="Process"
                value={status?.helper_pid ? `PID ${String(status.helper_pid)}` : 'not running'}
              />
              <StatusRow label="Authorization" value={formatProtocolLabel(status?.authorization)} />
              <StatusRow
                label="MusicKit capability"
                value={musicKitEntitled ? 'signed & provisioned' : 'not in this build'}
              />
              <StatusRow
                label="Subscription playback"
                value={
                  status?.can_play_catalog_content === true
                    ? 'available'
                    : status?.can_play_catalog_content === false
                      ? 'unavailable'
                      : 'not checked'
                }
              />
              <StatusRow label="Playback" value={formatProtocolLabel(playbackState)} />
              <StatusRow
                label="Helper version"
                value={String(status?.helper_version || 'not connected')}
              />
              <StatusRow
                label="Protocol capabilities"
                value={safeStrings(status?.helper_capabilities).join(', ') || 'not connected'}
              />
            </div>

            {!helperPresent ? (
              <div className="apple-music-driver-callout">
                <strong>Build the native helper first</strong>
                <span>
                  Run <code>./apple-music-helper/build-app.sh</code>, then refresh this page.
                </span>
              </div>
            ) : null}
            {helperPresent && !musicKitEntitled ? (
              <div className="apple-music-routing-callout">
                <strong>Handshake-only development build</strong>
                <span>
                  Launch and private IPC are testable. Sign with a MusicKit-enabled identity and
                  provisioning profile to authorize or play a song.
                </span>
              </div>
            ) : null}

            <div className="service-settings-actions">
              {!status?.helper_pid ? (
                <button
                  className="pill"
                  type="button"
                  disabled={Boolean(busy) || !helperPresent}
                  onClick={() =>
                    run(
                      'launch',
                      () => endpoints.launchAppleMusicHelper(),
                      'Apple Music helper connected over private IPC.'
                    )
                  }
                >
                  Launch helper
                </button>
              ) : null}
              <button
                className="pill"
                type="button"
                disabled={Boolean(busy) || !musicKitEntitled}
                onClick={() =>
                  run(
                    'authorize',
                    () => endpoints.authorizeAppleMusic(),
                    'Apple Music authorization state refreshed.'
                  )
                }
              >
                {authorized ? 'Check authorization' : 'Authorize Apple Music'}
              </button>
              {status?.helper_pid ? (
                <button
                  className="pill ghost"
                  type="button"
                  disabled={Boolean(busy)}
                  onClick={() =>
                    run(
                      'shutdown',
                      () => endpoints.shutdownAppleMusicHelper(),
                      'Apple Music helper stopped cleanly.'
                    )
                  }
                >
                  Quit helper
                </button>
              ) : null}
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Development song</div>
          </div>
          <div className="panel raised apple-music-form-panel">
            <label className="service-settings-field">
              <span>Apple Music song ID</span>
              <input
                className="input"
                value={songID}
                maxLength={256}
                placeholder="2037093408"
                spellCheck={false}
                onChange={(event) => setSongID(event.target.value)}
              />
            </label>
            <label className="service-settings-field">
              <span>Storefront</span>
              <input
                className="input"
                value={storefront}
                maxLength={8}
                placeholder="nz"
                spellCheck={false}
                onChange={(event) => setStorefront(event.target.value.toLowerCase())}
              />
              <small>
                Recorded with the queue request for the later provider layer. The native MusicKit
                lookup currently uses the signed-in account&apos;s storefront.
              </small>
            </label>
            <div className="service-settings-actions">
              <button
                className="pill is-active"
                type="button"
                disabled={!canPrepare || !songID.trim()}
                onClick={() =>
                  run(
                    'play',
                    () => endpoints.playAppleMusicSong(songID.trim(), storefront.trim()),
                    'Song prepared and playback started in the native helper.'
                  )
                }
              >
                Prepare &amp; play
              </button>
              <button
                className="pill"
                type="button"
                disabled={!isPlaying || Boolean(busy)}
                onClick={() =>
                  run('pause', () => endpoints.controlAppleMusic('pause'), 'Playback paused.')
                }
              >
                Pause
              </button>
              <button
                className="pill"
                type="button"
                disabled={!isPaused || Boolean(busy)}
                onClick={() =>
                  run('resume', () => endpoints.controlAppleMusic('resume'), 'Playback resumed.')
                }
              >
                Resume
              </button>
              <button
                className="pill service-settings-danger"
                type="button"
                disabled={(!isPlaying && !isPaused) || Boolean(busy)}
                onClick={() => run('stop', () => endpoints.stopAppleMusic(), 'Playback stopped.')}
              >
                Stop
              </button>
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Now playing</div>
          </div>
          <div className="panel raised apple-music-app-panel">
            <div className="apple-music-now-playing">
              <div className="apple-music-note-tile" aria-hidden="true">
                <Icon path="M9 18V5l12-2v13M9 18a3 3 0 1 1-2-2.83M21 16a3 3 0 1 1-2-2.83M9 9l12-2" />
              </div>
              <div className="apple-music-result-copy">
                <strong>{String(nowPlaying?.title || 'No prepared song')}</strong>
                <small>
                  {[nowPlaying?.artist, nowPlaying?.album].filter(Boolean).join(' · ') ||
                    'Enter a valid song ID to exercise MusicKit.'}
                </small>
              </div>
            </div>
            <div className="settings-list compact-list">
              <StatusRow label="Song ID" value={String(nowPlaying?.song_id || '—')} />
              <StatusRow
                label="Position"
                value={formatDuration(numberValue(status?.playback_time_secs))}
              />
              <StatusRow
                label="Duration"
                value={formatDuration(numberValue(nowPlaying?.duration_secs))}
              />
              <StatusRow label="Queue revision" value={String(status?.queue_revision || 0)} />
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Experiment boundary</div>
          </div>
          <div className="panel raised apple-music-warning-panel">
            <p>
              The Music app process-tap path is the quickest way to hear whether Fozmo&apos;s EQ,
              resampling, volume, and selected local output improve Apple Music enough to justify
              the full integration.
            </p>
            <p>
              It does not provide catalog search, native Apple Music metadata, or Fozmo transport
              ownership. Those still require the signed MusicKit helper and an Apple Developer
              provisioning profile.
            </p>
            <p>No Apple token or PCM is returned to this page, logged, or written to disk.</p>
          </div>
        </section>
      </div>
    </section>
  );
}

function StatusRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="settings-kv">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function statusLabel(status: JsonRecord | null) {
  if (!status) return 'Checking';
  const processTap = recordValue(status.process_tap);
  if (processTap?.state === 'running') return 'DSP tap active';
  if (processTap?.music_app_running === true) return 'Music ready';
  if (!status.helper_present) return 'Helper missing';
  return formatProtocolLabel(status.state);
}

function statusStampClass(status: JsonRecord | null) {
  const processTap = recordValue(status?.process_tap);
  if (processTap?.state === 'running') return 'sage';
  const state = String(status?.state || '');
  if (state === 'playing' || state === 'ready' || state === 'paused') return 'sage';
  if (state === 'failed' || state === 'helper_missing') return 'terra';
  return 'ochre';
}

function appleMusicErrorMessage(error: unknown) {
  const raw = error instanceof Error ? error.message : String(error);
  try {
    const parsed = JSON.parse(raw) as JsonRecord;
    return String(parsed.message || raw);
  } catch {
    return raw;
  }
}

function formatProtocolLabel(value: unknown) {
  const raw = String(value || 'not available');
  return raw.replaceAll('_', ' ');
}

function recordValue(value: unknown): JsonRecord | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as JsonRecord) : null;
}

function safeStrings(value: unknown) {
  return Array.isArray(value) ? value.map(String) : [];
}

function comparisonProviderLabel(reference: JsonRecord | null) {
  const provider = String(reference?.provider || '').toLowerCase();
  if (provider === 'qobuz') return 'Qobuz via Fozmo';
  if (provider === 'local') return 'Local via Fozmo';
  return 'Fozmo reference';
}

function comparisonTrackLabel(track: JsonRecord | null, fallback: string) {
  if (!track) return fallback;
  const title = String(track.title || '').trim();
  const artist = String(track.artist || '').trim();
  return [title, artist].filter(Boolean).join(' · ') || fallback;
}

function numberValue(value: unknown) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function formatDuration(value: number | null) {
  if (value === null) return '—';
  const total = Math.max(0, Math.floor(value));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

function tapFormatLabel(processTap: JsonRecord | null) {
  const rate = numberValue(processTap?.sample_rate_hz);
  const channels = numberValue(processTap?.channels);
  if (rate === null || channels === null) return 'available after start';
  const containerBits = numberValue(processTap?.sample_container_bits);
  const sampleFormat = String(processTap?.sample_format || '').toLowerCase();
  const formatLabel =
    sampleFormat === 'pcm_f32'
      ? `${containerBits !== null && containerBits > 0 ? Math.round(containerBits) : 32}-bit float`
      : sampleFormat
        ? formatProtocolLabel(sampleFormat)
        : 'float PCM';
  return `${Math.round(rate).toLocaleString()} Hz · ${channels}ch · ${
    processTap?.interleaved === true ? 'interleaved' : 'planar'
  } ${formatLabel}`;
}

function tapPrecisionLabel(processTap: JsonRecord | null) {
  const precisionBits = numberValue(processTap?.sample_precision_bits);
  if (precisionBits === null || precisionBits <= 0) return 'available after start';
  const sourceBits = numberValue(processTap?.source_bit_depth_bits);
  return `${Math.round(precisionBits)}-bit · ${
    sourceBits !== null && sourceBits > 0
      ? `${Math.round(sourceBits)}-bit source`
      : 'catalog depth unknown'
  }`;
}

function tapIngressLabel(processTap: JsonRecord | null) {
  if (processTap?.sample_values_preserved === true) {
    return 'unchanged samples · no integer quantization';
  }
  return processTap?.state === 'running' ? 'native Float32 handoff' : 'available after start';
}

function formatInteger(value: unknown) {
  const number = numberValue(value);
  return number === null ? '—' : Math.max(0, Math.round(number)).toLocaleString();
}

function formatRms(value: unknown) {
  const number = numberValue(value);
  if (number === null) return '—';
  if (number <= 0) return '−∞ dB';
  return `${Math.max(-120, 20 * Math.log10(number)).toFixed(1)} dB`;
}

function formatAge(value: unknown) {
  const age = numberValue(value);
  if (age === null) return 'waiting';
  if (age < 1000) return `${Math.round(age)} ms ago`;
  return `${(age / 1000).toFixed(1)} s ago`;
}
