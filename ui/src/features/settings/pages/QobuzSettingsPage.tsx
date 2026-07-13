import { useEffect, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import { errorMessage } from '../model/metadataAssignerModel';
import { qobuzAccountSummary } from '../model/qobuzSettingsModel';

export function QobuzSettingsPage({
  onRefresh,
  qobuzStatus
}: {
  onRefresh: () => Promise<void>;
  qobuzStatus: JsonRecord | null;
}) {
  const [qobuzSnapshot, setQobuzSnapshot] = useState<JsonRecord | null>(qobuzStatus);
  const [qobuzOpen, setQobuzOpen] = useState(false);
  const [qobuzSigningOut, setQobuzSigningOut] = useState(false);
  const [qobuzMessage, setQobuzMessage] = useState('');
  const [lastfmOpen, setLastfmOpen] = useState(false);
  const [lastfmStatus, setLastfmStatus] = useState<JsonRecord | null>(null);
  const [lastfmKeyDraft, setLastfmKeyDraft] = useState('');
  const [lastfmSaving, setLastfmSaving] = useState(false);
  const [lastfmMessage, setLastfmMessage] = useState('');
  const connected = Boolean(qobuzSnapshot?.logged_in || qobuzSnapshot?.authenticated);
  const lastfmConnected = Boolean(lastfmStatus?.configured);

  useEffect(() => {
    setQobuzSnapshot(qobuzStatus);
  }, [qobuzStatus]);

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

  const signOutQobuz = async () => {
    if (qobuzSigningOut) return;
    setQobuzSigningOut(true);
    setQobuzMessage('');
    try {
      const nextStatus = await endpoints.qobuzLogout();
      setQobuzSnapshot(nextStatus);
      await onRefresh().catch(() => undefined);
    } catch (error) {
      setQobuzMessage(`Qobuz sign out failed. ${errorMessage(error)}`);
    } finally {
      setQobuzSigningOut(false);
    }
  };

  const saveLastfmKey = async () => {
    if (lastfmSaving) return;
    setLastfmSaving(true);
    setLastfmMessage('');
    try {
      const saved = await endpoints.saveLastfmSettings({ api_key: lastfmKeyDraft.trim() || null });
      setLastfmStatus(saved);
      setLastfmKeyDraft('');
      await onRefresh().catch(() => undefined);
    } catch (error) {
      setLastfmMessage(`Last.fm settings could not be saved. ${errorMessage(error)}`);
    } finally {
      setLastfmSaving(false);
    }
  };

  const clearLastfmKey = async () => {
    if (lastfmSaving) return;
    setLastfmSaving(true);
    setLastfmMessage('');
    try {
      const saved = await endpoints.saveLastfmSettings({ api_key: null });
      setLastfmStatus(saved);
      setLastfmKeyDraft('');
      await onRefresh().catch(() => undefined);
    } catch (error) {
      setLastfmMessage(`Last.fm settings could not be cleared. ${errorMessage(error)}`);
    } finally {
      setLastfmSaving(false);
    }
  };

  return (
    <section className="settings-panel">
      <div className="settings-grid">
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Services</div>
          </div>
          <div className="panel raised qobuz-provider-panel">
            <ServiceProviderRow
              connected={connected}
              name="Qobuz"
              onSettings={() => {
                setQobuzMessage('');
                setQobuzOpen(true);
              }}
              summary={qobuzAccountSummary(qobuzSnapshot)}
            />
            <ServiceProviderRow
              connected={lastfmConnected}
              name="Last.fm"
              onSettings={() => {
                setLastfmMessage('');
                setLastfmOpen(true);
              }}
              summary={lastfmServiceSummary(lastfmStatus)}
            />
          </div>
        </section>
      </div>
      <QobuzServiceModal
        connected={connected}
        message={qobuzMessage}
        onClose={() => setQobuzOpen(false)}
        onSignOut={signOutQobuz}
        open={qobuzOpen}
        signingOut={qobuzSigningOut}
        summary={qobuzAccountSummary(qobuzSnapshot)}
      />
      <LastFmServiceModal
        keyDraft={lastfmKeyDraft}
        message={lastfmMessage}
        onClear={clearLastfmKey}
        onClose={() => setLastfmOpen(false)}
        onKeyDraft={setLastfmKeyDraft}
        onSave={saveLastfmKey}
        open={lastfmOpen}
        saving={lastfmSaving}
        status={lastfmStatus}
      />
    </section>
  );
}

function ServiceProviderRow({
  connected,
  name,
  onSettings,
  summary
}: {
  connected: boolean;
  name: string;
  onSettings: () => void;
  summary: string;
}) {
  return (
    <div className="qobuz-provider-row">
      <div className="qobuz-provider-main">
        <span className="qobuz-provider-copy">
          <span className="qobuz-provider-title">
            <span
              className={`qobuz-provider-status${connected ? ' is-connected' : ''}`}
              aria-hidden="true"
            />
            <strong>{name}</strong>
          </span>
          <small>{summary}</small>
        </span>
      </div>
      <div className="qobuz-provider-actions">
        <button
          className="service-settings-cog"
          type="button"
          aria-label={`${name} settings`}
          onClick={onSettings}
        >
          <Icon path="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2ZM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z" />
        </button>
      </div>
    </div>
  );
}

function QobuzServiceModal({
  connected,
  message,
  onClose,
  onSignOut,
  open,
  signingOut,
  summary
}: {
  connected: boolean;
  message: string;
  onClose: () => void;
  onSignOut: () => void;
  open: boolean;
  signingOut: boolean;
  summary: string;
}) {
  if (!open) return null;
  return (
    <Modal
      open
      className="metadata-assigner-backdrop service-settings-backdrop"
      ariaLabelledBy="qobuz-service-title"
      onClose={onClose}
    >
      <section
        className="metadata-assigner-panel service-settings-panel"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="metadata-assigner-head">
          <div>
            <strong id="qobuz-service-title">Qobuz</strong>
            <span className={`service-settings-status${connected ? ' is-connected' : ''}`}>
              {summary}
            </span>
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
        <div className="metadata-assigner-body service-settings-body">
          <p className="service-settings-notice">
            This application uses the Qobuz API but is not certified by Qobuz.
          </p>
          {message ? (
            <div className="metadata-assigner-message" data-testid="qobuz-service-message">
              {message}
            </div>
          ) : null}
          <div className="service-settings-actions is-centered">
            <a className={connected ? 'pill' : 'pill primary'} href="/api/qobuz/oauth/start">
              {connected ? 'Reconnect' : 'Connect'}
            </a>
            {connected ? (
              <button
                className="pill service-settings-danger"
                type="button"
                onClick={onSignOut}
                disabled={signingOut}
              >
                {signingOut ? 'Signing out...' : 'Sign out'}
              </button>
            ) : null}
          </div>
        </div>
      </section>
    </Modal>
  );
}

function LastFmServiceModal({
  keyDraft,
  message,
  onClear,
  onClose,
  onKeyDraft,
  onSave,
  open,
  saving,
  status
}: {
  keyDraft: string;
  message: string;
  onClear: () => void;
  onClose: () => void;
  onKeyDraft: (value: string) => void;
  onSave: () => void;
  open: boolean;
  saving: boolean;
  status: JsonRecord | null;
}) {
  if (!open) return null;
  const connected = Boolean(status?.configured);
  return (
    <Modal
      open
      className="metadata-assigner-backdrop service-settings-backdrop"
      ariaLabelledBy="lastfm-service-title"
      onClose={onClose}
    >
      <section
        className="metadata-assigner-panel service-settings-panel"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="metadata-assigner-head">
          <div>
            <strong id="lastfm-service-title">Last.fm</strong>
            <span className={`service-settings-status${connected ? ' is-connected' : ''}`}>
              {lastfmServiceSummary(status)}
            </span>
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
        <div className="metadata-assigner-body service-settings-body">
          {message ? <div className="metadata-assigner-message">{message}</div> : null}
          <label className="service-settings-field">
            <span>
              <strong>API key</strong>
            </span>
            <input
              type="password"
              value={keyDraft}
              onChange={(event) => onKeyDraft(event.target.value)}
              placeholder="Last.fm API key"
              autoComplete="off"
            />
          </label>
          <div className="service-settings-actions">
            <a
              className="service-settings-help"
              href="https://www.last.fm/api/authentication"
              target="_blank"
              rel="noreferrer"
            >
              Instructions
            </a>
            <button className="pill primary" type="button" onClick={onSave} disabled={saving}>
              {saving ? 'Saving...' : 'Save'}
            </button>
            <button
              className="pill service-settings-danger"
              type="button"
              onClick={onClear}
              disabled={saving}
            >
              Clear
            </button>
          </div>
        </div>
      </section>
    </Modal>
  );
}

function lastfmServiceSummary(status: JsonRecord | null) {
  if (!status) return 'Checking Last.fm API key.';
  if (status.configured) {
    if (status.radio_active) return 'Connected. Radio enabled.';
    if (status.radio_enabled) return 'Connected. Radio needs an API key refresh.';
    if (status.source === 'env') return 'Connected from environment. Radio disabled.';
    return 'Connected through API. Radio disabled.';
  }
  if (status.radio_enabled) {
    return 'API key needed before radio can run.';
  }
  return 'API key not configured.';
}
