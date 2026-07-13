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

export function zoneIsPlaying(zone: ZoneProfile) {
  const state = String(zone.playing_state || '');
  return state !== '' && state !== 'Stopped';
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
