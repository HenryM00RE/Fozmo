import { storageKey } from '../identity';
import type { ZoneProfile } from '../types';

/**
 * Identity for this browser's private playback zone.
 *
 * The agent id doubles as the zone id and as the ownership capability: the
 * server only reveals and controls a browser zone for requests that carry
 * this id in the `x-fozmo-browser-zone` header, so it stays in this
 * browser's local storage and is never rendered in shareable URLs.
 */
export const BROWSER_ZONE_HEADER = 'x-fozmo-browser-zone';

const AGENT_ID_STORAGE_KEY = storageKey('BrowserZoneAgentId');

let cachedAgentId: string | null = null;

function randomAgentId() {
  try {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return `browser-${crypto.randomUUID()}`;
    }
    if (typeof crypto !== 'undefined' && typeof crypto.getRandomValues === 'function') {
      const bytes = crypto.getRandomValues(new Uint8Array(32));
      const suffix = Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
      return `browser-${suffix}`;
    }
  } catch {
    // A browser without a working CSPRNG cannot safely mint an ownership capability.
  }
  throw new Error('Secure browser-zone identity generation is unavailable');
}

/** Stable per-browser agent id, created on first use. */
export function browserZoneAgentId(): string {
  if (cachedAgentId) return cachedAgentId;
  try {
    const stored = localStorage.getItem(AGENT_ID_STORAGE_KEY);
    if (stored?.startsWith('browser-')) {
      cachedAgentId = stored;
      return stored;
    }
  } catch {
    // Storage unavailable: fall back to a session-scoped id.
  }
  const created = randomAgentId();
  cachedAgentId = created;
  try {
    localStorage.setItem(AGENT_ID_STORAGE_KEY, created);
  } catch {
    // Session-scoped id still works for this tab.
  }
  return created;
}

/** True when `zoneId` is this browser's own private zone. */
export function isOwnBrowserZoneId(zoneId: unknown) {
  return typeof zoneId === 'string' && zoneId.length > 0 && zoneId === browserZoneAgentId();
}

/** True for any browser-private zone profile reported by the server. */
export function isBrowserZone(zone: ZoneProfile | null | undefined) {
  return zone?.browser === true;
}

/** Browser-only label for registered names such as “Chrome on Mac”. */
export function browserZoneDisplayName(value: unknown) {
  const name = String(value || '').trim();
  if (!name) return 'Browser';
  return (
    name.replace(/\s+on\s+(?:iphone|ipad|android|mac|windows|linux|device)$/i, '').trim() ||
    'Browser'
  );
}
