import * as QRCode from 'qrcode';
import { useEffect, useMemo, useState } from 'react';
import type { JsonRecord, RemoteSessionMetadataDto } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { useRemoteAccessSettings } from '../hooks/useRemoteAccessSettings';
import { isHostDeviceBrowser } from '../settingsModel';

function stringValue(value: unknown) {
  return typeof value === 'string' ? value : '';
}

function formatUnixTime(value?: number | null) {
  if (!value) return 'Never';
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short'
  }).format(new Date(value * 1000));
}

function groupedCode(code: string) {
  return code.match(/.{1,4}/g)?.join(' ') || code;
}

function sessionMetadataPart(value?: string | null) {
  return typeof value === 'string' ? value.trim() : '';
}

function sessionDeviceTitle(session: RemoteSessionMetadataDto) {
  const device = sessionMetadataPart(session.client?.device_family);
  const browser = sessionMetadataPart(session.client?.browser);
  if (device && browser) return `${device} · ${browser}`;
  if (device) return device;
  if (browser) return `Unknown device · ${browser}`;
  return session.label || 'Remote device';
}

function sessionDetailLine(session: RemoteSessionMetadataDto) {
  return [
    sessionMetadataPart(session.client?.os),
    sessionMetadataPart(session.client?.network_hint)
  ].filter(Boolean);
}

function cleanHost(host: string) {
  return host
    .trim()
    .replace(/^https?:\/\//i, '')
    .replace(/\/.*$/, '');
}

function remoteLinkUrl(
  code: string,
  linkUrlHint: string | null,
  externalHost: string,
  port: number
) {
  if (!code) return '';
  const hint = stringValue(linkUrlHint).trim();
  if (hint) return `${hint.replace(/\/$/, '')}/#link=${encodeURIComponent(code)}`;
  const host = cleanHost(externalHost);
  if (!host) return '';
  return `https://${host}:${port}/#link=${encodeURIComponent(code)}`;
}

function copyText(value: string, setMessage: (message: string) => void, label: string) {
  if (!value) return;
  navigator.clipboard
    ?.writeText(value)
    .then(() => setMessage(`${label} copied.`))
    .catch(() => setMessage(`${label} could not be copied.`));
}

export function remoteLinkGenerationAccess(
  issuance: unknown,
  { hostDevice, remoteSurface }: { hostDevice: boolean; remoteSurface: boolean }
) {
  if (issuance === 'host_local' || issuance === 'authenticated_lan') {
    return { allowed: true, reason: '' };
  }
  if (remoteSurface) {
    return {
      allowed: false,
      reason: 'Generate a link code on the Host Device or an authenticated LAN controller.'
    };
  }
  if (hostDevice && !issuance) {
    return { allowed: false, reason: 'Checking link-code capability…' };
  }
  return {
    allowed: false,
    reason:
      'Pair this LAN browser with the Host Device before generating a Remote Access link code.'
  };
}

export function RemoteAccessPage({ appStatus }: { appStatus: JsonRecord }) {
  const remoteSurface = appStatus.surface === 'remote';
  const canManage = !remoteSurface && isHostDeviceBrowser();
  const readOnlySurface = !canManage;
  const {
    activeSessionsCount,
    error,
    generateLinkCode,
    linkCode,
    linkCodeSecondsRemaining,
    linkUrlHint,
    loading,
    reloadSessions,
    revokeSession,
    saveSettings,
    saving,
    sessions,
    setEnabled,
    setExternalHost,
    setPort,
    settings,
    status
  } = useRemoteAccessSettings(canManage);
  const [message, setMessage] = useState('');
  const [qrDataUrl, setQrDataUrl] = useState('');

  const port = Number(settings?.port || status?.bound_port || 8443);
  const externalHost = stringValue(settings?.external_host || status?.external_host);
  const fingerprint = stringValue(status?.cert_fingerprint_sha256);
  const customCertConfigured = Boolean(settings?.custom_cert_path && settings?.custom_key_path);
  const linkUrl = useMemo(
    () => remoteLinkUrl(linkCode, linkUrlHint, externalHost, port),
    [externalHost, linkCode, linkUrlHint, port]
  );
  const validPort = Number.isInteger(port) && port > 0 && port <= 65535;
  const hostPrompt = cleanHost(externalHost) ? '' : 'Enter an external host before using the URL.';
  const remoteAccessEnabled = settings?.enabled ?? status?.enabled ?? false;
  const linkCodeIssuance = status?.link_code_issuance;
  const linkCodeAccess = remoteLinkGenerationAccess(linkCodeIssuance, {
    hostDevice: canManage,
    remoteSurface
  });
  const canGenerateLinkCode = linkCodeAccess.allowed;
  const linkDisabled =
    !canGenerateLinkCode ||
    !remoteAccessEnabled ||
    !status?.running ||
    linkCodeSecondsRemaining === 0;
  const generateDisabled = !canGenerateLinkCode || !remoteAccessEnabled || !status?.running;
  const generateReason = !canGenerateLinkCode
    ? linkCodeAccess.reason
    : !remoteAccessEnabled
      ? 'Remote Access is off.'
      : !status?.running
        ? 'Remote listener is not running.'
        : hostPrompt ||
          (linkCodeIssuance === 'authenticated_lan'
            ? 'Authenticated LAN controller. Ready to link a remote device.'
            : 'Ready to link a remote device.');
  const accessStateLabel = status?.enabled
    ? 'On'
    : status
      ? 'Off'
      : loading
        ? 'Loading...'
        : 'Status unavailable';
  const listenerStateLabel = status?.running
    ? 'Listener running'
    : status?.enabled
      ? 'Listener stopped'
      : status
        ? 'Listener off'
        : error || 'Listener status unavailable';

  useEffect(() => {
    let cancelled = false;
    if (!linkUrl) {
      setQrDataUrl('');
      return undefined;
    }
    QRCode.toDataURL(linkUrl, { margin: 1, width: 180 })
      .then((url) => {
        if (!cancelled) setQrDataUrl(url);
      })
      .catch(() => {
        if (!cancelled) setQrDataUrl('');
      });
    return () => {
      cancelled = true;
    };
  }, [linkUrl]);

  const saveCurrentSettings = async () => {
    if (!settings || !validPort) return;
    await saveSettings({
      ...settings,
      port,
      external_host: cleanHost(externalHost)
    });
  };

  const createLinkCode = async () => {
    setMessage('');
    const response = await generateLinkCode();
    if (response) setMessage('Link code created.');
  };

  return (
    <section className="settings-panel remote-access-panel">
      {readOnlySurface ? (
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Remote Access</div>
          </div>
          <div className="panel raised remote-host-required-panel">
            <div className="settings-list">
              <div className="setting-row remote-access-readonly">
                <span>
                  <strong>Host Device required</strong>
                  <small>Some Remote Access settings must be changed on the Host Device.</small>
                </span>
              </div>
              <div className="setting-row">
                <span>
                  <strong>{accessStateLabel}</strong>
                  <small>
                    {listenerStateLabel} · Port {status?.bound_port || port} · {activeSessionsCount}{' '}
                    active remote {activeSessionsCount === 1 ? 'session' : 'sessions'}
                    {status?.last_error ? ` · ${status.last_error}` : ''}
                  </small>
                </span>
              </div>
            </div>
          </div>
        </section>
      ) : (
        <>
          <section className="settings-section-block">
            <div className="settings-section-heading">
              <div className="section-label">Remote Access</div>
            </div>
            <div className="panel raised">
              <div className="settings-list">
                <div className="setting-row remote-access-warning">
                  <span>
                    <strong>Enable Remote Access</strong>
                    <small>
                      This exposes this Fozmo server to the internet through your router. Keep it
                      off unless you intend to forward the port and link only trusted devices.
                    </small>
                  </span>
                  <button
                    className={`toggle${settings?.enabled ? ' on' : ''}`}
                    type="button"
                    aria-label="Enable Remote Access"
                    aria-pressed={Boolean(settings?.enabled)}
                    disabled={loading || saving || !settings}
                    onClick={() => setEnabled(!settings?.enabled)}
                  />
                </div>
                <div className="setting-row">
                  <span>
                    <strong>{status?.running ? 'Running' : 'Stopped'}</strong>
                    <small>
                      Port {status?.bound_port || port} · {activeSessionsCount} active remote{' '}
                      {activeSessionsCount === 1 ? 'session' : 'sessions'}
                      {status?.last_error ? ` · ${status.last_error}` : ''}
                    </small>
                  </span>
                </div>
              </div>
            </div>
          </section>

          <section className="settings-section-block">
            <div className="settings-section-heading">
              <div className="section-label">Port and Host</div>
            </div>
            <div className="panel raised">
              <div className="settings-list">
                <div className="setting-row control-row">
                  <span>
                    <strong>TCP port</strong>
                    <small>{validPort ? 'Default is 8443.' : 'Use a port from 1 to 65535.'}</small>
                  </span>
                  <input
                    type="number"
                    min="1"
                    max="65535"
                    value={Number.isFinite(port) ? port : ''}
                    onChange={(event) => setPort(Number(event.target.value))}
                  />
                </div>
                <div className="setting-row control-row">
                  <span>
                    <strong>External host</strong>
                    <small>
                      Used only for link and QR hints. Configure router forwarding manually.
                    </small>
                  </span>
                  <input
                    type="text"
                    value={externalHost}
                    onChange={(event) => setExternalHost(event.target.value)}
                    placeholder="home.example.com"
                  />
                </div>
                <div className="setting-row">
                  <span>
                    <strong>Save changes</strong>
                    <small>
                      {message || error || 'Settings are applied to the listener immediately.'}
                    </small>
                  </span>
                  <button
                    className="pill primary"
                    type="button"
                    disabled={!settings || !validPort || saving}
                    onClick={saveCurrentSettings}
                  >
                    <Icon path="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2ZM17 21v-8H7v8M7 3v5h8" />
                    Save
                  </button>
                </div>
              </div>
            </div>
          </section>
        </>
      )}

      <section className="settings-section-block">
        <div className="settings-section-heading">
          <div className="section-label">TLS Trust</div>
        </div>
        <div className="panel raised">
          <div className="settings-list">
            <div className="setting-row remote-fingerprint-row">
              <span>
                <strong>
                  {customCertConfigured ? 'Custom certificate' : 'Self-signed certificate'}
                </strong>
                <small>
                  {customCertConfigured
                    ? 'Certificate trust is managed by your own certificate files.'
                    : 'Your browser may warn on first connect. Compare the certificate SHA-256 fingerprint with this value before proceeding.'}
                </small>
              </span>
              <button
                className="pill remote-copy-pill"
                type="button"
                disabled={!fingerprint}
                onClick={() => copyText(fingerprint, setMessage, 'Fingerprint')}
              >
                <Icon path="M8 8h10v10H8zM6 16H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
                Copy
              </button>
            </div>
            <div className="remote-code-block">
              {fingerprint || 'Fingerprint appears after the listener starts.'}
            </div>
          </div>
        </div>
      </section>

      {canManage ? (
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Port Forwarding</div>
          </div>
          <div className="panel raised">
            <ol className="remote-steps">
              <li>Reserve this computer's LAN IP in the router DHCP settings.</li>
              <li>Forward external TCP port {port || 8443} to this computer's same TCP port.</li>
              <li>Enter the router's public IP or DNS name as the external host.</li>
              <li>From a phone on cellular, open the generated URL.</li>
              <li>Verify the TLS fingerprint, then link the device.</li>
            </ol>
            <p className="remote-note">
              CGNAT and some IPv6-only ISP setups may not support manual IPv4 port forwarding.
            </p>
          </div>
        </section>
      ) : null}

      <section className="settings-section-block">
        <div className="settings-section-heading">
          <div className="section-label">Device Linking</div>
        </div>
        <div className="panel raised">
          <div className="settings-list">
            <div className="setting-row">
              <span>
                <strong>Generate link code</strong>
                <small>{generateReason}</small>
              </span>
              <button
                className="pill primary"
                type="button"
                disabled={generateDisabled}
                onClick={createLinkCode}
              >
                <Icon path="M12 5v14M5 12h14" />
                Generate
              </button>
            </div>
            <div className="remote-link-grid">
              <div className="remote-link-code">
                <span>Code</span>
                <strong>{linkCode ? groupedCode(linkCode) : 'No active code'}</strong>
                <small>
                  {linkCode
                    ? `${linkCodeSecondsRemaining}s remaining`
                    : 'Codes are high-entropy, single-use, and expire after 5 minutes.'}
                </small>
                <button
                  className="pill"
                  type="button"
                  disabled={!linkCode || linkDisabled}
                  onClick={() => copyText(linkCode, setMessage, 'Link code')}
                >
                  <Icon path="M8 8h10v10H8zM6 16H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
                  Copy code
                </button>
              </div>
              <div className="remote-link-code">
                <span>URL</span>
                <strong>{linkUrl || hostPrompt || 'No active link'}</strong>
                <small>The URL carries the full link token.</small>
                <button
                  className="pill"
                  type="button"
                  disabled={!linkUrl || linkDisabled}
                  onClick={() => copyText(linkUrl, setMessage, 'Link URL')}
                >
                  <Icon path="M10 13a5 5 0 0 0 7.1 0l2-2a5 5 0 0 0-7.1-7.1l-1.1 1.1M14 11a5 5 0 0 0-7.1 0l-2 2A5 5 0 0 0 12 20.1l1.1-1.1" />
                  Copy URL
                </button>
              </div>
              <div className="remote-qr-box" aria-label="Remote link QR code">
                {qrDataUrl && !linkDisabled ? (
                  <img src={qrDataUrl} alt="Remote link QR code" />
                ) : (
                  <span>{hostPrompt || 'Generate a running link code.'}</span>
                )}
              </div>
            </div>
          </div>
        </div>
      </section>

      {canManage ? (
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Active Sessions</div>
            <button className="pill" type="button" onClick={reloadSessions}>
              <Icon path="M21 3v5h-5M20.1 13.5a7.5 7.5 0 1 1-2-7.1L21 8" />
              Refresh
            </button>
          </div>
          <div className="panel raised">
            {sessions.length ? (
              <div className="remote-session-list">
                {sessions.map((session) => (
                  <RemoteSessionRow
                    key={session.id}
                    session={session}
                    onRevoke={() => revokeSession(session.id)}
                  />
                ))}
              </div>
            ) : (
              <div className="remote-empty-state">No linked remote devices.</div>
            )}
          </div>
        </section>
      ) : null}
    </section>
  );
}

function RemoteSessionRow({
  onRevoke,
  session
}: {
  onRevoke: () => void;
  session: RemoteSessionMetadataDto;
}) {
  const detailParts = sessionDetailLine(session);
  return (
    <div className="remote-session-row">
      <span>
        <strong>{sessionDeviceTitle(session)}</strong>
        {detailParts.length ? (
          <small className="remote-session-client">{detailParts.join(' · ')}</small>
        ) : null}
        <small>
          Issued {formatUnixTime(session.issued_at_unix_secs)} · Expires{' '}
          {formatUnixTime(session.expires_at_unix_secs)} · Last used{' '}
          {formatUnixTime(session.last_used_at_unix_secs)}
        </small>
      </span>
      <button className="pill" type="button" disabled={!session.active} onClick={onRevoke}>
        <Icon path="M18 6 6 18M6 6l12 12" />
        Revoke
      </button>
    </div>
  );
}
