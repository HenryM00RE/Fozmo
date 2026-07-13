import { PLAYBACK_CLIENT_HEADER, PLAYBACK_SEQUENCE_HEADER, storageKey } from '../identity';
import { BROWSER_ZONE_HEADER, browserZoneAgentId } from './browserZone';
import { storedProfileId } from './profileSelection';

const PROFILE_HEADER = 'x-fozmo-profile-id';

function randomClientId() {
  if (window.crypto && typeof window.crypto.randomUUID === 'function') {
    return window.crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function playbackSequenceClientForPath(clientId: string, path: string) {
  const cleanPath = path.split('?')[0];
  return /\/qobuz\/prefetch$/.test(cleanPath) ? `${clientId}:prefetch` : clientId;
}

function playbackSequenceClientId(path: string) {
  try {
    const existing = localStorage.getItem(storageKey('PlaybackSequenceClient'));
    if (existing) return playbackSequenceClientForPath(existing, path);
    const next = randomClientId();
    localStorage.setItem(storageKey('PlaybackSequenceClient'), next);
    return playbackSequenceClientForPath(next, path);
  } catch {
    return playbackSequenceClientForPath(randomClientId(), path);
  }
}

function nextPlaybackRequestSequence() {
  try {
    const stored = Number(localStorage.getItem(storageKey('PlaybackRequestSeq')) || 0) || 0;
    const next = stored + 1;
    localStorage.setItem(storageKey('PlaybackRequestSeq'), String(next));
    return next;
  } catch {
    return Date.now();
  }
}

function isPlaybackIntent(path: string) {
  return (
    path === '/api/play' ||
    path === '/api/artist-radio/play' ||
    path === '/api/qobuz/play' ||
    path === '/api/qobuz/prefetch' ||
    /^\/api\/zones\/[^/]+\/play$/.test(path) ||
    /^\/api\/zones\/[^/]+\/artist-radio\/play$/.test(path) ||
    /^\/api\/zones\/[^/]+\/qobuz\/play$/.test(path) ||
    /^\/api\/zones\/[^/]+\/qobuz\/prefetch$/.test(path)
  );
}

export function profileIdFromBody(body: unknown) {
  if (!body || typeof body !== 'object') return '';
  return String((body as { profile_id?: unknown }).profile_id || '').trim();
}

export function requestHeaders(path: string, body?: unknown) {
  const playbackSeq = isPlaybackIntent(path) ? nextPlaybackRequestSequence() : null;
  const profileId = storedProfileId();
  return {
    [BROWSER_ZONE_HEADER]: browserZoneAgentId(),
    ...(profileId ? { [PROFILE_HEADER]: profileId } : {}),
    ...(playbackSeq
      ? {
          [PLAYBACK_CLIENT_HEADER]: playbackSequenceClientId(path),
          [PLAYBACK_SEQUENCE_HEADER]: String(playbackSeq)
        }
      : {}),
    ...(body === undefined ? {} : { 'Content-Type': 'application/json' })
  };
}
