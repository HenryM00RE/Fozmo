import { useEffect, useRef, useState } from 'react';
import type { AutoMetaProgress } from '../../../shared/lib/api';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import { useQobuzRadio } from '../hooks/useQobuzRadio';
import { errorMessage } from '../model/metadataAssignerModel';
import { qobuzAccountIdentity } from '../model/qobuzSettingsModel';

type MetaBrainzSettingsPageProps = {
  onRefresh: () => Promise<void>;
  qobuzStatus: JsonRecord | null;
};

export function MetaBrainzSettingsPage({ onRefresh, qobuzStatus }: MetaBrainzSettingsPageProps) {
  const { radioEnabled, radioMessage, saveRadioEnabled } = useQobuzRadio(qobuzStatus);
  const qobuzConnected = Boolean(qobuzStatus?.logged_in || qobuzStatus?.authenticated);
  const [lastfmStatus, setLastfmStatus] = useState<JsonRecord | null>(null);
  const [lastfmRadioSaving, setLastfmRadioSaving] = useState(false);
  const [lastfmMessage, setLastfmMessage] = useState('');
  const [autoMetaOpen, setAutoMetaOpen] = useState(false);
  const [autoMetaLinkQobuz, setAutoMetaLinkQobuz] = useState(true);
  const [autoMetaProgress, setAutoMetaProgress] = useState<AutoMetaProgress | null>(null);
  const [autoMetaMessage, setAutoMetaMessage] = useState('');
  const [autoMetaAction, setAutoMetaAction] = useState<string | null>(null);
  const autoMetaRunning = Boolean(autoMetaProgress?.running);

  useEffect(() => {
    let active = true;
    endpoints
      .lastfmStatus()
      .then((status) => {
        if (active) setLastfmStatus(status);
      })
      .catch(() => {
        if (active) setLastfmStatus(null);
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    const load = () => {
      endpoints
        .autoMetaStatus()
        .then((progress) => {
          if (active) {
            setAutoMetaProgress((current) =>
              mergeAutoMetaProgress(progress, autoMetaLinkQobuz, current)
            );
          }
        })
        .catch(() => undefined);
    };
    load();
    const timer =
      autoMetaOpen || autoMetaRunning
        ? window.setInterval(load, autoMetaRunning ? 1000 : 5000)
        : null;
    return () => {
      active = false;
      if (timer !== null) window.clearInterval(timer);
    };
  }, [autoMetaLinkQobuz, autoMetaOpen, autoMetaRunning]);

  const openAutoMeta = () => {
    setAutoMetaOpen(true);
    setAutoMetaMessage('');
    endpoints
      .autoMetaStatus()
      .then((progress) =>
        setAutoMetaProgress((current) =>
          mergeAutoMetaProgress(progress, autoMetaLinkQobuz, current)
        )
      )
      .catch(() => undefined);
  };

  const closeAutoMeta = () => {
    setAutoMetaOpen(false);
  };

  const runAutoMeta = async (mode = 'remaining') => {
    if (autoMetaAction || autoMetaRunning) return;
    setAutoMetaAction(mode);
    setAutoMetaMessage('');
    try {
      const progress = await endpoints.autoMetaJob({ link_qobuz: autoMetaLinkQobuz, mode });
      setAutoMetaProgress((current) => mergeAutoMetaProgress(progress, autoMetaLinkQobuz, current));
    } catch (error) {
      setAutoMetaMessage(`AutoMetadata could not start. ${errorMessage(error)}`);
      endpoints
        .autoMetaStatus()
        .then((progress) =>
          setAutoMetaProgress((current) =>
            mergeAutoMetaProgress(progress, autoMetaLinkQobuz, current)
          )
        )
        .catch(() => undefined);
    } finally {
      setAutoMetaAction(null);
    }
  };

  const controlAutoMeta = async (action: 'pause' | 'resume' | 'stop') => {
    const jobId = autoMetaProgress?.job_id;
    if (!jobId || autoMetaAction) return;
    setAutoMetaAction(action);
    setAutoMetaMessage('');
    try {
      const progress =
        action === 'pause'
          ? await endpoints.autoMetaPause(jobId)
          : action === 'resume'
            ? await endpoints.autoMetaResume(jobId)
            : await endpoints.autoMetaStop(jobId);
      setAutoMetaProgress((current) => mergeAutoMetaProgress(progress, autoMetaLinkQobuz, current));
    } catch (error) {
      setAutoMetaMessage(`AutoMetadata ${action} failed. ${errorMessage(error)}`);
    } finally {
      setAutoMetaAction(null);
    }
  };

  const saveLastfmRadioEnabled = async (enabled: boolean) => {
    if (lastfmRadioSaving) return;
    setLastfmRadioSaving(true);
    setLastfmMessage('');
    setLastfmStatus((status) =>
      status
        ? { ...status, radio_enabled: enabled, radio_active: enabled && Boolean(status.configured) }
        : status
    );
    try {
      const saved = await endpoints.saveLastfmSettings({ radio_enabled: enabled });
      setLastfmStatus(saved);
      if (enabled) {
        await onRefresh().catch(() => undefined);
      }
    } catch (error) {
      setLastfmMessage(`Last.fm Radio setting could not be saved. ${errorMessage(error)}`);
      endpoints
        .lastfmStatus()
        .then(setLastfmStatus)
        .catch(() => undefined);
    } finally {
      setLastfmRadioSaving(false);
    }
  };

  const saveQobuzRadioEnabled = async (enabled: boolean) => {
    await saveRadioEnabled(enabled);
    if (enabled) {
      endpoints
        .lastfmStatus()
        .then(setLastfmStatus)
        .catch(() => undefined);
    }
  };

  return (
    <section className="settings-panel">
      <div className="settings-grid two-col">
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">AutoMetadata</div>
          </div>
          <div className="panel raised metadata-settings-card">
            <div className="settings-list">
              <div className="setting-row">
                <span>
                  <strong>AutoMetadata</strong>
                  <small>{autoMetaLauncherText(autoMetaProgress, autoMetaLinkQobuz)}</small>
                </span>
                <button
                  className={`pill primary${autoMetaRunning ? ' is-busy' : ''}`}
                  type="button"
                  onClick={openAutoMeta}
                  aria-busy={autoMetaRunning ? 'true' : undefined}
                >
                  {autoMetaRunning ? (
                    <span className="autometa-button-spinner" aria-hidden="true" />
                  ) : (
                    <Icon path="M4 6h11M4 12h8M4 18h11M17 8l2 2 4-5M17 18l2 2 4-5" />
                  )}
                  {autoMetaRunning ? 'AutoMetadata running' : 'AutoMetadata'}
                </button>
              </div>
            </div>
          </div>
        </section>
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Radio providers</div>
          </div>
          <div className="panel raised metadata-settings-card">
            <div className="settings-list">
              <div className="setting-row">
                <span>
                  <strong>Qobuz Radio</strong>
                  <small>
                    {qobuzConnected
                      ? radioMessage
                      : `Sign in as ${qobuzAccountIdentity(qobuzStatus)} to enable radio.`}
                  </small>
                </span>
                <button
                  className={`toggle${radioEnabled ? ' on' : ''}`}
                  type="button"
                  aria-label="Toggle Qobuz Radio"
                  aria-pressed={radioEnabled}
                  disabled={!qobuzConnected}
                  onClick={() => saveQobuzRadioEnabled(!radioEnabled)}
                />
              </div>
              <div className="setting-row">
                <span>
                  <strong>Last.fm Radio</strong>
                  <small>{lastfmRadioStatusText(lastfmStatus)}</small>
                </span>
                <button
                  className={`toggle${lastfmStatus?.radio_enabled ? ' on' : ''}`}
                  type="button"
                  aria-label="Toggle Last.fm Radio"
                  aria-pressed={Boolean(lastfmStatus?.radio_enabled)}
                  disabled={lastfmRadioSaving}
                  onClick={() => saveLastfmRadioEnabled(!lastfmStatus?.radio_enabled)}
                />
              </div>
              {lastfmMessage ? (
                <div className="metadata-assigner-message">{lastfmMessage}</div>
              ) : null}
            </div>
          </div>
        </section>
      </div>

      <AutoMetaModal
        action={autoMetaAction}
        linkQobuz={autoMetaLinkQobuz}
        message={autoMetaMessage}
        onClose={closeAutoMeta}
        onLinkQobuzChange={setAutoMetaLinkQobuz}
        onPause={() => controlAutoMeta('pause')}
        onResume={() => controlAutoMeta('resume')}
        onRun={runAutoMeta}
        onStop={() => controlAutoMeta('stop')}
        open={autoMetaOpen}
        progress={autoMetaProgress}
      />
    </section>
  );
}

function AutoMetaModal({
  action,
  linkQobuz,
  message,
  onClose,
  onLinkQobuzChange,
  onPause,
  onResume,
  onRun,
  onStop,
  open,
  progress
}: {
  action: string | null;
  linkQobuz: boolean;
  message: string;
  onClose: () => void;
  onLinkQobuzChange: (enabled: boolean) => void;
  onPause: () => void;
  onResume: () => void;
  onRun: (mode?: string) => void;
  onStop: () => void;
  open: boolean;
  progress: AutoMetaProgress | null;
}) {
  if (!open) return null;
  const running = Boolean(progress?.running);
  return (
    <Modal
      open
      className="metadata-assigner-backdrop autometa-backdrop"
      ariaLabelledBy="autometa-title"
      onClose={onClose}
    >
      <section
        className="metadata-assigner-panel autometa-panel"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="metadata-assigner-head">
          <div>
            <strong id="autometa-title">AutoMetadata</strong>
            <span>{running ? 'Tagging local versions' : 'Batch metadata tagging'}</span>
          </div>
          <button
            className="metadata-assigner-close"
            type="button"
            aria-label="Close"
            onClick={onClose}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </header>
        <div className="metadata-assigner-body autometa-modal-body">
          <div className="metadata-assigner-message">
            This can take a while because MusicBrainz limits clients to about one API request per
            second.
          </div>
          <AutoMetaPanel
            action={action}
            linkQobuz={linkQobuz}
            message={message}
            onLinkQobuzChange={onLinkQobuzChange}
            onPause={onPause}
            onResume={onResume}
            onRun={onRun}
            onStop={onStop}
            progress={progress}
          />
        </div>
      </section>
    </Modal>
  );
}

function AutoMetaPanel({
  action,
  linkQobuz,
  message,
  onLinkQobuzChange,
  onPause,
  onResume,
  onRun,
  onStop,
  progress
}: {
  action: string | null;
  linkQobuz: boolean;
  message: string;
  onLinkQobuzChange: (enabled: boolean) => void;
  onPause: () => void;
  onResume: () => void;
  onRun: (mode?: string) => void;
  onStop: () => void;
  progress: AutoMetaProgress | null;
}) {
  const status = stringValue(progress?.status) || 'idle';
  const running = Boolean(progress?.running);
  const paused = status === 'paused';
  const interrupted = status === 'interrupted';
  const stopping = status === 'stopping';
  const [etaTick, setEtaTick] = useState(() => Date.now());
  const etaBaselineRef = useRef<{ at: number; processed: number } | null>(null);

  useEffect(() => {
    if (!running) {
      etaBaselineRef.current = null;
      return undefined;
    }
    if (!etaBaselineRef.current || processedValue(progress) < etaBaselineRef.current.processed) {
      etaBaselineRef.current = { at: Date.now(), processed: processedValue(progress) };
    }
    setEtaTick(Date.now());
    const timer = window.setInterval(() => setEtaTick(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [progress, running]);

  const processed = numberValue(progress?.processed);
  const total = numberValue(progress?.total);
  const runLinksQobuz = progress?.link_qobuz ?? linkQobuz;
  const providerMatched = runLinksQobuz
    ? numberValue(progress?.qobuz_matched ?? progress?.exact_matched)
    : numberValue(progress?.exact_matched ?? progress?.musicbrainz_matched);
  const musicbrainzMatched = numberValue(progress?.musicbrainz_matched);
  const noProper = numberValue(progress?.no_proper_match);
  const skipped = optionalCount(progress, ['skipped']);
  const errors = optionalCount(progress, ['error_count', 'errors', 'failed', 'failures']);
  const percent = total > 0 ? Math.min(100, Math.round((processed / total) * 100)) : 0;
  const etaLabel = progress?.eta_secs
    ? formatRemainingSeconds(numberValue(progress.eta_secs))
    : autometaEtaLabel({
        baseline: etaBaselineRef.current,
        now: etaTick,
        processed,
        running,
        total
      });
  const statusLabel = autometaStatusLabel(progress, running, processed, total, etaLabel, percent);
  const resultLabel = autometaResultLabel(
    progress,
    running,
    processed,
    total,
    skipped,
    errors,
    noProper
  );
  const countLabel =
    total > 0 ? `${processed} / ${total}` : running ? 'Preparing' : 'No queued albums';
  const providerMetric = runLinksQobuz ? 'Qobuz linked' : 'Tagged this run';
  const phaseLabel = stringValue(progress?.phase) || status;
  const metrics = [
    { label: 'MusicBrainz matched', value: String(musicbrainzMatched) },
    { label: providerMetric, value: String(providerMatched) },
    { label: 'Needs review', value: String(noProper) },
    ...(skipped === null || skipped <= 0 ? [] : [{ label: 'Skipped', value: String(skipped) }]),
    ...(errors === null ? [] : [{ label: 'Errors', value: String(errors) }])
  ];
  const tooltip = metrics.map((metric) => `${metric.label}: ${metric.value}`).join(' / ');
  const canStart = !running && !paused && !stopping;
  const canResume = paused || interrupted;
  const canPauseOrResume = running || canResume;
  return (
    <div className="autometa-body">
      {message ? <div className="metadata-assigner-message">{message}</div> : null}
      {stringValue(progress?.error) ? (
        <div className="metadata-assigner-message">{stringValue(progress?.error)}</div>
      ) : null}
      <div className="setting-row autometa-toggle-row">
        <span>
          <strong>AutoMetadata</strong>
          <small>{autometaPanelSummary(status, runLinksQobuz)}</small>
        </span>
        <button
          className={`toggle${linkQobuz ? ' on' : ''}`}
          type="button"
          aria-label="Toggle Qobuz linking"
          aria-pressed={linkQobuz}
          disabled={running || paused}
          onClick={() => onLinkQobuzChange(!linkQobuz)}
        />
      </div>
      <div className="autometa-progress-shell" title={tooltip}>
        <div className="autometa-progress-head">
          <strong>{countLabel}</strong>
          <span>{statusLabel}</span>
        </div>
        <div className="autometa-progress-track" aria-label={tooltip}>
          <div className="autometa-progress-fill" style={{ width: `${percent}%` }} />
        </div>
      </div>
      <div className="autometa-stats">
        {metrics.map((metric) => (
          <AutoMetaMetric key={metric.label} label={metric.label} value={metric.value} />
        ))}
      </div>
      <div className="autometa-current-work">
        <AutoMetaDetail label="Phase" value={phaseLabel} />
        <AutoMetaDetail
          label="Current album"
          value={stringValue(progress?.current_album) || 'None'}
        />
        <AutoMetaDetail
          label="Current version"
          value={stringValue(progress?.current_version) || 'None'}
        />
        <AutoMetaDetail
          label="Last update"
          value={progress?.updated_at ? timeLabel(numberValue(progress.updated_at)) : 'No job yet'}
        />
        <AutoMetaDetail label="Summary" value={resultLabel} />
      </div>
      <RecentAutoMetaResults progress={progress} />
      <div className="metadata-assigner-actions autometa-actions">
        <button
          className={`pill primary${action === 'remaining' ? ' is-busy' : ''}`}
          type="button"
          onClick={() => onRun('remaining')}
          disabled={!canStart || Boolean(action)}
        >
          {action === 'remaining' ? (
            <span className="autometa-button-spinner" aria-hidden="true" />
          ) : (
            <Icon path="M17 3v4h-4M7 21v-4h4M17 7A7 7 0 0 0 5.6 4.6M7 17a7 7 0 0 0 11.4 2.4" />
          )}
          Start remaining
        </button>
        <button
          className="pill"
          type="button"
          onClick={running ? onPause : onResume}
          disabled={!canPauseOrResume || Boolean(action)}
        >
          <Icon path={running ? 'M8 5v14M16 5v14' : 'M8 5v14l11-7z'} />
          {running ? 'Pause' : 'Resume'}
        </button>
        <button
          className="pill"
          type="button"
          onClick={onStop}
          disabled={(!running && !paused) || Boolean(action)}
        >
          <Icon path="M6 6h12v12H6z" />
          Stop
        </button>
        <button
          className="pill"
          type="button"
          onClick={() => onRun('retry_errors')}
          disabled={!canStart || Boolean(action)}
        >
          <Icon path="M21 12a9 9 0 1 1-3-6.7M21 3v6h-6" />
          Retry errors
        </button>
        <div className="autometa-action-note">
          Start remaining creates a fresh run for unfinished versions. Pause/Resume controls the
          current run without rebuilding its queue.
        </div>
      </div>
    </div>
  );
}

function RecentAutoMetaResults({ progress }: { progress: AutoMetaProgress | null }) {
  const results = Array.isArray(progress?.recent_results)
    ? progress.recent_results.slice(0, 4)
    : [];
  if (!results.length) return null;
  return (
    <div className="autometa-current-work autometa-recent-results">
      {results.map((item) => (
        <AutoMetaDetail
          key={item.id}
          label={item.status}
          value={`${item.album_title} / ${item.message || item.phase}`}
        />
      ))}
    </div>
  );
}

function autometaPanelSummary(status: string, linkQobuz: boolean) {
  if (status === 'running')
    return linkQobuz ? 'Running MusicBrainz and Qobuz matching.' : 'Running MusicBrainz matching.';
  if (status === 'paused') return 'Paused. Resume continues from stored job state.';
  if (status === 'interrupted') return 'Interrupted by restart. Resume continues remaining items.';
  if (status === 'completed') return 'Complete. Remaining runs skip finished versions.';
  return 'Tag local versions with MusicBrainz first, then optionally link Qobuz.';
}

function autoMetaLauncherText(progress: AutoMetaProgress | null, fallbackLinkQobuz: boolean) {
  const status = stringValue(progress?.status) || 'idle';
  const running = Boolean(progress?.running);
  const processed = numberValue(progress?.processed);
  const total = numberValue(progress?.total);
  if (running && total > 0) return `Running ${processed} / ${total}.`;
  if (running) return 'Running.';
  if (status === 'paused') return 'Paused. Open to resume.';
  if (status === 'interrupted') return 'Interrupted by restart. Open to resume.';
  if (status === 'completed' && total > 0) return `Last run completed ${processed} / ${total}.`;
  return fallbackLinkQobuz
    ? 'Tag local versions with MusicBrainz, then link safe Qobuz matches.'
    : 'Tag local versions with MusicBrainz.';
}

function timeLabel(epochSeconds: number) {
  if (!epochSeconds) return 'No job yet';
  return new Date(epochSeconds * 1000).toLocaleString();
}

function AutoMetaMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="autometa-metric">
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function AutoMetaDetail({ label, value }: { label: string; value: string }) {
  return (
    <div className="autometa-detail-row">
      <span>{label}</span>
      <strong title={value}>{value}</strong>
    </div>
  );
}

function lastfmRadioStatusText(status: JsonRecord | null) {
  if (!status) return 'Checking Last.fm Radio.';
  if (!status.radio_enabled) return 'Disabled. Qobuz Radio can run when enabled.';
  if (status.radio_active) return 'Enabled. Qobuz Radio is disabled while this is on.';
  return 'Enabled, but an API key is needed before radio can run.';
}

function processedValue(progress: AutoMetaProgress | null) {
  return numberValue(progress?.processed);
}

function numberValue(value: unknown) {
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}

function optionalCount(progress: AutoMetaProgress | null, keys: string[]) {
  if (!progress) return null;
  for (const key of keys) {
    if (!Object.hasOwn(progress, key)) continue;
    const value = progress[key];
    if (Array.isArray(value)) return value.length;
    const numeric = Number(value);
    if (Number.isFinite(numeric)) return numeric;
  }
  return null;
}

function mergeAutoMetaProgress(
  progress: AutoMetaProgress,
  fallbackLinkQobuz: boolean,
  current: AutoMetaProgress | null
) {
  if (typeof progress.link_qobuz === 'boolean') return progress;
  const linkQobuz =
    hasAutoMetaActivity(progress) && typeof current?.link_qobuz === 'boolean'
      ? current.link_qobuz
      : fallbackLinkQobuz;
  return { ...progress, link_qobuz: linkQobuz };
}

function hasAutoMetaActivity(progress: AutoMetaProgress) {
  return Boolean(
    numberValue(progress.processed) > 0 ||
      numberValue(progress.total) > 0 ||
      progress.running ||
      progress.current_album ||
      progress.current_version ||
      progress.last_result ||
      progress.error
  );
}

function autometaStatusLabel(
  progress: AutoMetaProgress | null,
  running: boolean,
  processed: number,
  total: number,
  etaLabel: string,
  percent: number
) {
  if (running) return total > 0 ? `${percent}% / ${etaLabel}` : 'Preparing';
  if (stringValue(progress?.error)) return 'Stopped with error';
  if (total > 0 && processed >= total) return 'Complete';
  if (total === 0 && progress) return 'Nothing queued';
  return 'No run started';
}

function autometaResultLabel(
  progress: AutoMetaProgress | null,
  running: boolean,
  processed: number,
  total: number,
  skipped: number | null,
  errors: number | null,
  noProper: number
) {
  const lastResult = stringValue(progress?.last_result);
  if (running) return lastResult || 'Starting';
  if (stringValue(progress?.error)) return stringValue(progress?.error);
  if (lastResult) return lastResult;
  if (total === 0 && progress) return 'No albums queued.';
  if (total > 0 && processed >= total) {
    return [
      `${processed} processed`,
      skipped === null ? '' : `${skipped} skipped`,
      `${noProper} needs review`,
      errors === null ? '' : `${errors} errors`
    ]
      .filter(Boolean)
      .join(', ');
  }
  return progress ? 'Stopped before finishing.' : 'No run started.';
}

function autometaEtaLabel({
  baseline,
  now,
  processed,
  running,
  total
}: {
  baseline: { at: number; processed: number } | null;
  now: number;
  processed: number;
  running: boolean;
  total: number;
}) {
  if (!running) {
    if (total > 0 && processed >= total) return 'Complete';
    return 'Not running';
  }
  if (total <= 0 || !baseline) return 'Estimating...';
  const completed = processed - baseline.processed;
  const elapsedSeconds = Math.max(1, (now - baseline.at) / 1000);
  if (completed <= 0) return 'Estimating...';
  const remaining = Math.max(0, total - processed);
  if (remaining <= 0) return 'Finishing';
  return formatRemainingSeconds(remaining / (completed / elapsedSeconds));
}

function formatRemainingSeconds(seconds: number) {
  if (!Number.isFinite(seconds) || seconds <= 0) return 'Estimating...';
  if (seconds < 60) return '< 1 min';
  const minutes = Math.ceil(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const remainder = minutes % 60;
  return remainder ? `${hours} hr ${remainder} min` : `${hours} hr`;
}

function stringValue(value: unknown) {
  if (value === null || value === undefined) return '';
  return String(value);
}
