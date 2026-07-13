import { useCallback, useEffect, useRef, useState } from 'react';
import { endpoints } from '../../shared/lib/api';

export type RemoteAuthState =
  | 'checking'
  | 'linking'
  | 'authorised'
  | 'unauthorised'
  | 'exchange_failed';

export const remoteLinkFailedMessage =
  'Remote link code is invalid or expired. Generate a new code from the local/LAN app.';

export const remoteLinkRequiredMessage =
  'This Fozmo server is reachable, but this browser is not linked yet.';

export const pairingLinkFailedMessage =
  'This pairing link is invalid, expired, or has already been used. Generate a new QR code from the Fozmo menu.';

export function remoteLinkCodeFromHash(hash: string) {
  const raw = hash.startsWith('#') ? hash.slice(1) : hash;
  const params = new URLSearchParams(raw.startsWith('?') ? raw.slice(1) : raw);
  return String(params.get('link') || '').trim();
}

export function pairingTokenFromHash(hash: string) {
  const match = hash.match(/^#\/pair\/([^/?#]+)$/);
  if (!match) return '';
  try {
    return decodeURIComponent(match[1]).trim();
  } catch {
    return '';
  }
}

export function isLoopbackHostname(hostname: string) {
  const normalized = hostname
    .trim()
    .replace(/^\[|\]$/g, '')
    .toLowerCase();
  return normalized === 'localhost' || normalized === '127.0.0.1' || normalized === '::1';
}

export function remoteAuthStateFromStatusProbe({
  protocol,
  status,
  surface,
  hostname = 'localhost'
}: {
  protocol: string;
  status: number;
  surface?: unknown;
  hostname?: string;
}): RemoteAuthState {
  if (status >= 200 && status < 300) return 'authorised';
  if (
    [401, 403, 429].includes(status) &&
    (protocol === 'https:' || surface === 'remote' || !isLoopbackHostname(hostname))
  ) {
    return 'unauthorised';
  }
  return 'authorised';
}

export function useRemoteLinkExchange(setNotice: (message: string) => void) {
  const exchangingRef = useRef(false);
  const [authState, setAuthState] = useState<RemoteAuthState>('checking');
  const [authMessage, setAuthMessage] = useState('');

  const probeRemoteAuth = useCallback(async () => {
    const response = await fetch('/api/status', {
      cache: 'no-store',
      credentials: 'same-origin'
    });
    let surface: unknown = '';
    if (response.ok) {
      const statusBody = await response.json().catch(() => null);
      surface =
        statusBody && typeof statusBody === 'object'
          ? (statusBody as { surface?: unknown }).surface
          : '';
    }
    const nextState = remoteAuthStateFromStatusProbe({
      protocol: window.location.protocol,
      status: response.status,
      surface,
      hostname: window.location.hostname
    });
    setAuthState(nextState);
    setAuthMessage(nextState === 'unauthorised' ? remoteLinkRequiredMessage : '');
  }, []);

  useEffect(() => {
    let cancelled = false;

    const syncRemoteAuth = async () => {
      const pairingToken = pairingTokenFromHash(window.location.hash);
      if (pairingToken) {
        if (exchangingRef.current) return;
        exchangingRef.current = true;
        // Remove the one-time secret before any other request, route render, or
        // history entry can retain it. URL fragments are never sent to the
        // server, and the token remains only in this closure during exchange.
        window.history.replaceState(
          null,
          '',
          `${window.location.pathname}${window.location.search}#/home`
        );
        setAuthState('linking');
        setAuthMessage('');
        try {
          await endpoints.exchangePairingSession(pairingToken);
          if (cancelled) return;
          setNotice('Device paired.');
          await probeRemoteAuth();
        } catch {
          if (cancelled) return;
          setAuthState('exchange_failed');
          setAuthMessage(pairingLinkFailedMessage);
          setNotice(pairingLinkFailedMessage);
        } finally {
          exchangingRef.current = false;
        }
        return;
      }

      const code = remoteLinkCodeFromHash(window.location.hash);
      if (code) {
        if (exchangingRef.current) return;
        exchangingRef.current = true;
        setAuthState('linking');
        setAuthMessage('');
        try {
          await endpoints.exchangeRemoteSession(code);
          if (cancelled) return;
          window.history.replaceState(
            null,
            '',
            `${window.location.pathname}${window.location.search}`
          );
          setNotice('Remote device linked.');
          window.dispatchEvent(new HashChangeEvent('hashchange'));
          await probeRemoteAuth();
        } catch {
          if (cancelled) return;
          setAuthState('exchange_failed');
          setAuthMessage(remoteLinkFailedMessage);
          setNotice(remoteLinkFailedMessage);
        } finally {
          exchangingRef.current = false;
        }
        return;
      }

      setAuthState('checking');
      setAuthMessage('');
      try {
        await probeRemoteAuth();
      } catch {
        if (!cancelled) {
          setAuthState('authorised');
          setAuthMessage('');
        }
      }
    };

    syncRemoteAuth();
    window.addEventListener('hashchange', syncRemoteAuth);
    return () => {
      cancelled = true;
      window.removeEventListener('hashchange', syncRemoteAuth);
    };
  }, [probeRemoteAuth, setNotice]);

  const retryRemoteAuth = useCallback(() => {
    window.dispatchEvent(new HashChangeEvent('hashchange'));
  }, []);

  return {
    authMessage,
    authState,
    retryRemoteAuth
  };
}
