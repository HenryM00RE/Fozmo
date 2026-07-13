import { useCallback, useEffect, useMemo, useState } from 'react';
import { ApiError, endpoints } from '../../../shared/lib/api';
import type {
  RemoteAccessSettingsDto,
  RemoteAccessStatus,
  RemoteLinkCodeResponse,
  RemoteSessionMetadataDto
} from '../../../shared/types';

const localOnlyMessage = 'Remote Access settings can only be changed on the Host Device.';

function unavailableMessage(error: unknown, fallback: string) {
  if (error instanceof ApiError && [401, 403, 404].includes(error.status)) {
    return localOnlyMessage;
  }
  return error instanceof Error ? error.message : fallback;
}

function nowUnixSecs() {
  return Math.floor(Date.now() / 1000);
}

export function useRemoteAccessSettings(canManage = false, canGenerateLinkCode = canManage) {
  const [settings, setSettings] = useState<RemoteAccessSettingsDto | null>(null);
  const [status, setStatus] = useState<RemoteAccessStatus | null>(null);
  const [sessions, setSessions] = useState<RemoteSessionMetadataDto[]>([]);
  const [linkCode, setLinkCode] = useState('');
  const [linkCodeExpiresAt, setLinkCodeExpiresAt] = useState<number | null>(null);
  const [linkUrlHint, setLinkUrlHint] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [tick, setTick] = useState(() => nowUnixSecs());

  const loadSettings = useCallback(async () => {
    setLoading(true);
    try {
      if (canManage) {
        const response = await endpoints.remoteSettings();
        setSettings(response.settings);
        setStatus(response.status);
      } else {
        setSettings(null);
        setStatus(await endpoints.remoteAccessStatus());
      }
      setError('');
    } catch (loadError) {
      setError(unavailableMessage(loadError, 'Remote Access settings are unavailable.'));
    } finally {
      setLoading(false);
    }
  }, [canManage]);

  const reloadSessions = useCallback(async () => {
    if (!canManage) return;
    try {
      const response = await endpoints.remoteSessions();
      setSessions(response.sessions || []);
    } catch (loadError) {
      setError(unavailableMessage(loadError, 'Remote sessions are unavailable.'));
    }
  }, [canManage]);

  useEffect(() => {
    loadSettings().catch(() => undefined);
    reloadSessions().catch(() => undefined);
  }, [loadSettings, reloadSessions]);

  useEffect(() => {
    if (canManage) return undefined;
    const timer = window.setInterval(() => {
      endpoints
        .remoteAccessStatus()
        .then((nextStatus) => {
          setStatus(nextStatus);
          setError('');
        })
        .catch(() => undefined);
    }, 10_000);
    return () => window.clearInterval(timer);
  }, [canManage]);

  useEffect(() => {
    if (!linkCodeExpiresAt) return undefined;
    const timer = window.setInterval(() => setTick(nowUnixSecs()), 1000);
    return () => window.clearInterval(timer);
  }, [linkCodeExpiresAt]);

  const saveSettings = useCallback(
    async (next: RemoteAccessSettingsDto) => {
      if (!canManage) {
        setError(localOnlyMessage);
        return;
      }
      setSaving(true);
      try {
        const response = await endpoints.saveRemoteSettings({
          enabled: Boolean(next.enabled),
          port: Number(next.port),
          external_host: next.external_host || '',
          custom_cert_path: next.custom_cert_path || '',
          custom_key_path: next.custom_key_path || ''
        });
        setSettings(response.settings);
        setStatus(response.status);
        setError(response.status.last_error || '');
        await reloadSessions();
      } catch (saveError) {
        setError(unavailableMessage(saveError, 'Remote Access settings could not be saved.'));
      } finally {
        setSaving(false);
      }
    },
    [canManage, reloadSessions]
  );

  const setEnabled = useCallback(
    (enabled: boolean) => {
      if (!settings) return;
      saveSettings({ ...settings, enabled }).catch(() => undefined);
    },
    [saveSettings, settings]
  );

  const setPort = useCallback((port: number) => {
    setSettings((current) => (current ? { ...current, port } : current));
  }, []);

  const setExternalHost = useCallback((host: string) => {
    setSettings((current) => (current ? { ...current, external_host: host } : current));
  }, []);

  const generateLinkCode = useCallback(async (): Promise<RemoteLinkCodeResponse | null> => {
    if (!canGenerateLinkCode) {
      setError(localOnlyMessage);
      return null;
    }
    try {
      const response = await endpoints.createRemoteLinkCode();
      setLinkCode(response.code);
      setLinkCodeExpiresAt(response.expires_at_unix_secs);
      setLinkUrlHint(response.url_hint || null);
      setTick(nowUnixSecs());
      return response;
    } catch (linkError) {
      setError(unavailableMessage(linkError, 'Remote link code could not be created.'));
      return null;
    }
  }, [canGenerateLinkCode]);

  const revokeSession = useCallback(
    async (id: string) => {
      if (!canManage) {
        setError(localOnlyMessage);
        return;
      }
      try {
        await endpoints.revokeRemoteSession(id);
        await reloadSessions();
      } catch (revokeError) {
        setError(unavailableMessage(revokeError, 'Remote session could not be revoked.'));
      }
    },
    [canManage, reloadSessions]
  );

  const linkCodeSecondsRemaining = useMemo(() => {
    if (!linkCodeExpiresAt) return 0;
    return Math.max(0, linkCodeExpiresAt - tick);
  }, [linkCodeExpiresAt, tick]);

  const activeSessionsCount = canManage
    ? sessions.filter((session) => session.active).length
    : Number(status?.active_remote_sessions || 0);

  return {
    activeSessionsCount,
    error,
    generateLinkCode,
    linkCode,
    linkCodeExpiresAt,
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
  };
}
