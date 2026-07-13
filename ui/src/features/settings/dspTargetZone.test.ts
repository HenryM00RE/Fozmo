import { describe, expect, it } from 'vitest';
import type { ZoneProfile } from '../../shared/types';
import {
  defaultDspTargetZoneId,
  resolveSettingsTargetZoneId,
  zoneIsPlaying
} from './dspTargetZone';

function zone(id: string, playingState: string | null): ZoneProfile {
  return { id, name: id, playing_state: playingState } as ZoneProfile;
}

describe('defaultDspTargetZoneId', () => {
  it('uses the active zone when it is playing', () => {
    const zones = [zone('sonos', 'Playing'), zone('hegel', 'Playing')];
    expect(defaultDspTargetZoneId(zones, 'sonos')).toBe('sonos');
  });

  it('uses the active zone even when another zone is playing', () => {
    const zones = [zone('sonos', 'Stopped'), zone('hegel', 'Playing')];
    expect(defaultDspTargetZoneId(zones, 'sonos')).toBe('sonos');
  });

  it('falls back to the active zone when nothing is playing', () => {
    const zones = [zone('sonos', 'Stopped'), zone('hegel', null)];
    expect(defaultDspTargetZoneId(zones, 'sonos')).toBe('sonos');
    expect(zoneIsPlaying(zones[0])).toBe(false);
  });

  it('falls back to the first available zone when the active zone is unavailable', () => {
    const zones = [zone('sonos', 'Stopped'), zone('hegel', null)];
    expect(defaultDspTargetZoneId(zones, 'missing')).toBe('sonos');
  });
});

describe('resolveSettingsTargetZoneId', () => {
  const zones = [zone('sonos', 'Stopped'), zone('hegel', 'Playing')];

  it('starts DSP or EQ on the active zone', () => {
    expect(resolveSettingsTargetZoneId(zones, 'sonos', 'hegel', true)).toBe('sonos');
  });

  it('preserves the selected output when moving between DSP and EQ', () => {
    expect(resolveSettingsTargetZoneId(zones, 'sonos', 'hegel', false)).toBe('hegel');
  });

  it('repairs a selection when its output is no longer available', () => {
    expect(resolveSettingsTargetZoneId(zones, 'sonos', 'missing', false)).toBe('sonos');
  });
});
