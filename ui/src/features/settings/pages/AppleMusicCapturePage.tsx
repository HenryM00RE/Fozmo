import { useCallback, useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { errorMessage } from '../model/metadataAssignerModel';

const SUPPORTED_CAPTURE_RATES = [44100, 48000, 88200, 96000, 176400, 192000];

export function AppleMusicCapturePage() {
  const [status, setStatus] = useState<JsonRecord | null>(null);
  const [devices, setDevices] = useState<JsonRecord | null>(null);
  const [captureDeviceDraft, setCaptureDeviceDraft] = useState('');
  const [outputDeviceDraft, setOutputDeviceDraft] = useState('');
  const [bufferMsDraft, setBufferMsDraft] = useState(250);
  const [autoRouteDraft, setAutoRouteDraft] = useState(false);
  const [manualRateDraft, setManualRateDraft] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState('');

  const captureDevices = useMemo(
    () => safeRecords(devices?.capture_devices),
    [devices?.capture_devices]
  );
  const outputDevices = useMemo(
    () => safeRecords(devices?.output_devices),
    [devices?.output_devices]
  );
  const driverInstalled = Boolean(status?.driver_installed);
  const inputVisible = Boolean(status?.capture_device_input_visible);
  const outputVisible = Boolean(status?.capture_device_output_visible);
  const captureReady = inputVisible;

  const loadAll = useCallback(async () => {
    const [nextStatus, nextSettings, nextDevices] = await Promise.all([
      endpoints.appleMusicCaptureStatus(),
      endpoints.appleMusicCaptureSettings(),
      endpoints.appleMusicCaptureDevices()
    ]);
    setStatus(nextStatus);
    setDevices(nextDevices);
    setCaptureDeviceDraft(String(nextSettings.capture_device_name || ''));
    setOutputDeviceDraft(String(nextSettings.output_device_name || ''));
    setBufferMsDraft(Number(nextSettings.buffer_ms || 250) || 250);
    setAutoRouteDraft(nextSettings.auto_route_system_output === true);
  }, []);

  useEffect(() => {
    loadAll().catch((error) =>
      setMessage(`Apple Music capture status failed. ${errorMessage(error)}`)
    );
  }, [loadAll]);

  useEffect(() => {
    const id = window.setInterval(() => {
      endpoints
        .appleMusicCaptureMetrics()
        .then(setStatus)
        .catch(() => undefined);
    }, 2500);
    return () => window.clearInterval(id);
  }, []);

  const saveSettings = async () => {
    if (busy) return;
    setBusy('save');
    setMessage('');
    try {
      await endpoints.saveAppleMusicCaptureSettings({
        enabled: true,
        capture_device_name: captureDeviceDraft.trim() || null,
        output_device_name: outputDeviceDraft.trim() || null,
        buffer_ms: bufferMsDraft,
        auto_route_system_output: autoRouteDraft
      });
      setMessage('Apple Music capture settings saved.');
      await endpoints
        .appleMusicCaptureStatus()
        .then(setStatus)
        .catch(() => undefined);
    } catch (error) {
      setMessage(`Settings could not be saved. ${errorMessage(error)}`);
    } finally {
      setBusy('');
    }
  };

  const controlMusicApp = async (command: string, label: string) => {
    if (busy) return;
    setBusy(command);
    setMessage('');
    try {
      const musicStatus = await endpoints.controlAppleMusicApp(command);
      setStatus((current) => ({
        ...(current || {}),
        music_app_running: musicStatus.running,
        music_app_player_state: musicStatus.player_state,
        music_app_track_title: musicStatus.track_title,
        music_app_track_artist: musicStatus.track_artist,
        music_app_track_album: musicStatus.track_album,
        music_app_message: musicStatus.message
      }));
      setMessage(`${label} sent to Apple Music.`);
    } catch (error) {
      setMessage(`${label} failed. ${errorMessage(error)}`);
    } finally {
      setBusy('');
    }
  };

  const startCapture = async () => {
    if (busy) return;
    const confirmed = window.confirm(
      'Start live system-audio capture? Audio playing on this Mac will be captured and sent to the selected local physical output.'
    );
    if (!confirmed) return;
    setBusy('start');
    setMessage('');
    try {
      const nextStatus = await endpoints.startAppleMusicCapture({
        capture_device_name: captureDeviceDraft.trim() || null,
        output_device_name: outputDeviceDraft.trim() || null,
        confirm_system_audio_capture: true
      });
      setStatus(nextStatus);
      setMessage('Capture session started.');
    } catch (error) {
      setMessage(`Capture did not start. ${errorMessage(error)}`);
    } finally {
      setBusy('');
    }
  };

  const stopCapture = async () => {
    if (busy) return;
    setBusy('stop');
    setMessage('');
    try {
      const nextStatus = await endpoints.stopAppleMusicCapture();
      setStatus(nextStatus);
      setMessage('Capture session stopped.');
    } catch (error) {
      setMessage(`Capture did not stop. ${errorMessage(error)}`);
    } finally {
      setBusy('');
    }
  };

  const applyManualRate = async () => {
    const rateHz = Number(manualRateDraft);
    if (busy || !rateHz) return;
    setBusy('rate');
    setMessage('');
    try {
      const nextStatus = await endpoints.setAppleMusicCaptureRate(rateHz);
      setStatus(nextStatus);
      setMessage(`Capture rate set to ${formatRate(rateHz)}.`);
    } catch (error) {
      setMessage(`Rate override failed. ${errorMessage(error)}`);
    } finally {
      setBusy('');
    }
  };

  return (
    <section className="settings-panel apple-music-capture-page">
      {message ? (
        <div className="metadata-assigner-message apple-music-message">{message}</div>
      ) : null}

      <div className="settings-grid apple-music-grid">
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Apple Music App</div>
            <button
              className="settings-heading-refresh"
              type="button"
              aria-label="Refresh Apple Music app status"
              onClick={() => loadAll().catch((error) => setMessage(errorMessage(error)))}
            >
              <Icon path="M21 12a9 9 0 0 1-15.3 6.36M3 12A9 9 0 0 1 18.3 5.64M18 2v4h-4M6 22v-4h4" />
            </button>
          </div>
          <div className="panel raised apple-music-app-panel">
            <div className="apple-music-now-playing">
              <div className="apple-music-note-tile" aria-hidden="true">
                <Icon path="M9 18V5l12-2v13M9 18a3 3 0 1 1-2-2.83M21 16a3 3 0 1 1-2-2.83M9 9l12-2" />
              </div>
              <div className="apple-music-result-copy">
                <strong>{currentTrackTitle(status)}</strong>
                <small>{currentTrackSubtitle(status)}</small>
              </div>
            </div>
            <div className="settings-list compact-list">
              <StatusRow
                label="App"
                value={status?.music_app_running ? 'running' : 'not running'}
              />
              <StatusRow
                label="Playback"
                value={String(status?.music_app_player_state || 'not available')}
              />
              {status?.music_app_message ? (
                <StatusRow label="Automation" value={String(status.music_app_message)} />
              ) : null}
            </div>
            <div className="apple-music-transport-row">
              <button
                className="pill"
                type="button"
                onClick={() => controlMusicApp('open', 'Open')}
                disabled={busy === 'open'}
              >
                Open Music
              </button>
              <button
                className="settings-row-icon-button"
                type="button"
                aria-label="Previous Apple Music track"
                onClick={() => controlMusicApp('previous', 'Previous')}
                disabled={busy === 'previous'}
              >
                <Icon path="m11 19-9-7 9-7v14ZM22 19l-9-7 9-7v14Z" />
              </button>
              <button
                className="settings-row-icon-button apple-music-play-button"
                type="button"
                aria-label="Play or pause Apple Music"
                onClick={() => controlMusicApp('play_pause', 'Play/Pause')}
                disabled={busy === 'play_pause'}
              >
                <Icon path="m8 5 11 7-11 7V5Z" />
              </button>
              <button
                className="settings-row-icon-button"
                type="button"
                aria-label="Next Apple Music track"
                onClick={() => controlMusicApp('next', 'Next')}
                disabled={busy === 'next'}
              >
                <Icon path="m13 5 9 7-9 7V5ZM2 5l9 7-9 7V5Z" />
              </button>
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Capture Status</div>
          </div>
          <div className="panel raised">
            <div className="settings-list compact-list">
              <StatusRow label="Platform" value={String(status?.platform || 'checking')} />
              <StatusRow label="Feature" value={status?.feature_enabled ? 'enabled' : 'disabled'} />
              <StatusRow label="Supported" value={status?.supported ? 'yes' : 'no'} />
              <StatusRow label="Driver installed" value={driverInstalled ? 'yes' : 'no'} />
              <StatusRow label="Device visible as output" value={outputVisible ? 'yes' : 'no'} />
              <StatusRow label="Device visible as input" value={inputVisible ? 'yes' : 'no'} />
              <StatusRow label="Capture active" value={status?.capture_running ? 'yes' : 'no'} />
              <StatusRow
                label="Current nominal rate"
                value={formatRate(Number(status?.capture_rate_hz || 0))}
              />
              <StatusRow
                label="Detected track rate"
                value={
                  status?.detected_track_rate_hz
                    ? formatRate(Number(status.detected_track_rate_hz))
                    : 'not reported'
                }
              />
              <StatusRow
                label="Rate switch"
                value={status?.rate_switch_pending ? 'switching...' : 'idle'}
              />
              <StatusRow label="Format" value={`${status?.capture_format || 'f32'} stereo`} />
              <StatusRow
                label="Buffer frames"
                value={
                  status?.buffer_frames ? formatCount(Number(status.buffer_frames)) : 'not reported'
                }
              />
              <StatusRow
                label="Frames received"
                value={formatCount(Number(status?.frames_received || 0))}
              />
              <StatusRow
                label="Callbacks"
                value={formatCount(Number(status?.callbacks_received || 0))}
              />
              <StatusRow
                label="RMS level L/R"
                value={`${formatRms(Number(status?.rms_l || 0))} / ${formatRms(Number(status?.rms_r || 0))}`}
              />
              <StatusRow
                label="Ring fill"
                value={`${formatCount(Number(status?.ring_fill_frames || 0))} frames / ${Number(status?.ring_fill_ms || 0).toFixed(1)} ms`}
              />
              <StatusRow label="Underruns" value={String(status?.underruns || 0)} />
              <StatusRow label="Overruns" value={String(status?.overruns || 0)} />
              <StatusRow label="Latency snaps" value={String(status?.snaps || 0)} />
              <StatusRow
                label="Capture ring overruns"
                value={String(status?.capture_ring_overruns || 0)}
              />
              <StatusRow
                label="Music volume"
                value={
                  status?.music_app_sound_volume != null
                    ? `${status.music_app_sound_volume}%`
                    : 'not observed'
                }
              />
              <StatusRow
                label="Diagnostic dropouts"
                value={String(status?.diagnostic_dropouts || 0)}
              />
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Capture Controls</div>
          </div>
          <div className="panel raised apple-music-form-panel">
            {!driverInstalled ? (
              <div className="apple-music-driver-callout">
                <strong>Fozmo Capture is not installed yet.</strong>
                <span>
                  Build and install the HAL driver with drivers/fozmo-capture/scripts/install.sh,
                  then restart this page.
                </span>
              </div>
            ) : null}
            {driverInstalled && (!inputVisible || !outputVisible) ? (
              <div className="apple-music-driver-callout">
                <strong>CoreAudio can only see part of Fozmo Capture.</strong>
                <span>
                  Restart CoreAudio, then confirm Fozmo Capture appears in both Sound Output and
                  Sound Input.
                </span>
              </div>
            ) : null}
            {driverInstalled && outputVisible && !autoRouteDraft ? (
              <div className="apple-music-routing-callout">
                <strong>Route macOS audio before testing.</strong>
                <span>
                  Auto-routing is off. Open System Settings, choose Sound, set Output to Fozmo
                  Capture, then play Apple Music. Capture will show silence until that route is
                  active.
                </span>
              </div>
            ) : null}
            {captureWarnings(status).map((warning) => (
              <div className="apple-music-routing-callout" key={warning}>
                <strong>Check capture quality.</strong>
                <span>{warning}</span>
              </div>
            ))}
            <label className="service-settings-field">
              <span>
                <strong>Capture device</strong>
                {!inputVisible ? (
                  <small>
                    This appears after CoreAudio loads the input side of the HAL driver.
                  </small>
                ) : null}
              </span>
              <select
                value={captureDeviceDraft}
                onChange={(event) => setCaptureDeviceDraft(event.target.value)}
                disabled={!inputVisible}
              >
                <option value="">Fozmo Capture</option>
                {captureDevices.map((device) => (
                  <option key={String(device.name)} value={String(device.name)}>
                    {String(device.name)}
                  </option>
                ))}
              </select>
            </label>
            <label className="service-settings-field">
              <span>
                <strong>Output device</strong>
              </span>
              <select
                value={outputDeviceDraft}
                onChange={(event) => setOutputDeviceDraft(event.target.value)}
              >
                <option value="">Current Fozmo output</option>
                {outputDevices.map((device) => (
                  <option key={String(device.name)} value={String(device.name)}>
                    {String(device.name)}
                  </option>
                ))}
              </select>
            </label>
            <label className="service-settings-field">
              <span>
                <strong>Buffer</strong>
              </span>
              <input
                type="number"
                min={50}
                max={2000}
                step={50}
                value={bufferMsDraft}
                onChange={(event) => setBufferMsDraft(Number(event.target.value) || 250)}
              />
            </label>
            <label className="service-settings-field">
              <span>
                <strong>Auto-route macOS output</strong>
                <small>
                  Switch the system default output to Fozmo Capture on start and restore it on stop.
                </small>
              </span>
              <input
                type="checkbox"
                checked={autoRouteDraft}
                onChange={(event) => setAutoRouteDraft(event.target.checked)}
              />
            </label>
            <label className="service-settings-field">
              <span>
                <strong>Manual rate override</strong>
                <small>
                  For streaming tracks whose rate Apple Music does not report. Restarts a running
                  capture at the selected rate.
                </small>
              </span>
              <span className="service-settings-actions">
                <select
                  value={manualRateDraft}
                  onChange={(event) => setManualRateDraft(event.target.value)}
                  disabled={!driverInstalled}
                >
                  <option value="">Select rate</option>
                  {SUPPORTED_CAPTURE_RATES.map((rate) => (
                    <option key={rate} value={rate}>
                      {formatRate(rate)}
                    </option>
                  ))}
                </select>
                <button
                  className="pill"
                  type="button"
                  onClick={applyManualRate}
                  disabled={!manualRateDraft || busy === 'rate'}
                >
                  {busy === 'rate' ? 'Applying...' : 'Apply Rate'}
                </button>
              </span>
            </label>
            <div className="service-settings-actions">
              <button
                className="pill"
                type="button"
                onClick={saveSettings}
                disabled={busy === 'save'}
              >
                {busy === 'save' ? 'Saving...' : 'Save'}
              </button>
              <button
                className="pill primary"
                type="button"
                onClick={startCapture}
                disabled={!captureReady || busy === 'start' || Boolean(status?.capture_running)}
              >
                {captureReady
                  ? busy === 'start'
                    ? 'Starting...'
                    : 'Start Capture'
                  : 'Input Required'}
              </button>
              <button
                className="pill service-settings-danger"
                type="button"
                onClick={stopCapture}
                disabled={busy === 'stop' || !status?.capture_running}
              >
                {busy === 'stop' ? 'Stopping...' : 'Stop Capture'}
              </button>
            </div>
          </div>
        </section>

        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Boundaries</div>
          </div>
          <div className="panel raised apple-music-warning-panel">
            <p>
              Live processing only. No recording, export, library import, queue integration, or
              history writes.
            </p>
            <p>
              Bit-perfect checklist: Music volume 100%, Sound Check off, EQ off, crossfade off,
              Lossless enabled, and the Fozmo zone set to a real physical output device.
            </p>
            <p>
              Streaming tracks often hide their sample rate; capture falls back to 44.1 kHz until
              the manual rate override is used.
            </p>
            {status?.message ? <p>{String(status.message)}</p> : null}
          </div>
        </section>
      </div>
    </section>
  );
}

function StatusRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="settings-kv">
      <strong>{label}</strong>
      <small>{value}</small>
    </div>
  );
}

function safeRecords(value: unknown): JsonRecord[] {
  return Array.isArray(value)
    ? value.filter((item): item is JsonRecord => Boolean(item && typeof item === 'object'))
    : [];
}

function captureWarnings(status: JsonRecord | null): string[] {
  return Array.isArray(status?.warnings)
    ? status.warnings.filter((warning): warning is string => typeof warning === 'string')
    : [];
}

function currentTrackTitle(status: JsonRecord | null) {
  return String(status?.music_app_track_title || 'Apple Music');
}

function currentTrackSubtitle(status: JsonRecord | null) {
  const parts = [
    status?.music_app_track_artist,
    status?.music_app_track_album,
    status?.music_app_player_state
  ]
    .map((part) => String(part || '').trim())
    .filter(Boolean);
  return parts.length ? parts.join(' - ') : 'Native Music app source';
}

function formatRate(rate: number) {
  if (!rate) return 'not observed';
  return `${(rate / 1000).toFixed(rate % 1000 === 0 ? 0 : 1)} kHz`;
}

function formatCount(value: number) {
  return new Intl.NumberFormat().format(Math.max(0, Math.floor(value || 0)));
}

function formatRms(value: number) {
  if (!value) return '0.0000';
  return value.toFixed(4);
}
