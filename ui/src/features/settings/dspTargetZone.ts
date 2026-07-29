import type { ZoneProfile } from '../../shared/types';

const DSP_TARGET_ZONE_KEY = 'fozmo.settings.dspTargetZoneId';

export function loadDspTargetZoneId(): string {
  try {
    return window.sessionStorage.getItem(DSP_TARGET_ZONE_KEY) || '';
  } catch {
    return '';
  }
}

export function saveDspTargetZoneId(zoneId: string) {
  try {
    if (zoneId) window.sessionStorage.setItem(DSP_TARGET_ZONE_KEY, zoneId);
    else window.sessionStorage.removeItem(DSP_TARGET_ZONE_KEY);
  } catch {
    // Storage can be unavailable (private mode, disabled); selection then
    // falls back to the playing-zone default on the next load.
  }
}

/**
 * Zone playback states are Stopped / Starting / Playing / Paused. Only the two
 * that mean audio is actually running count: a zone left Paused from an earlier
 * session is not playing, and treating it as such made the DSP page claim music
 * was on a device that was silent.
 */
export function zoneIsPlaying(zone: ZoneProfile) {
  const state = String(zone.playing_state || '').toLowerCase();
  return state === 'playing' || state === 'starting';
}

/**
 * Not every backend reports per-zone `playing_state` — network zones can leave
 * it null while playing perfectly happily. The global status knows which zone
 * playback is on, so it is the authority for the active zone and the per-zone
 * field only has to answer for the others.
 */
export function zoneIsPlayingNow(
  zone: ZoneProfile,
  status: { state?: unknown; active_zone_id?: unknown } | null | undefined
) {
  const globalState = String(status?.state || '').toLowerCase();
  const globalRunning = globalState === 'playing' || globalState === 'starting';
  if (globalRunning && zone.id && zone.id === status?.active_zone_id) return true;
  return zoneIsPlaying(zone);
}

export function defaultDspTargetZoneId(zones: ZoneProfile[], activeZoneId: string): string {
  if (zones.some((zone) => zone.id === activeZoneId)) return activeZoneId;
  return zones[0]?.id || activeZoneId;
}

export function resolveSettingsTargetZoneId(
  zones: ZoneProfile[],
  activeZoneId: string,
  currentZoneId: string,
  enteringAudioSettings: boolean
) {
  if (!enteringAudioSettings && currentZoneId && zones.some((zone) => zone.id === currentZoneId)) {
    return currentZoneId;
  }
  return defaultDspTargetZoneId(zones, activeZoneId);
}
