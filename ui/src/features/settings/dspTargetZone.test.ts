import { describe, expect, it } from 'vitest';
import type { ZoneProfile } from '../../shared/types';
import {
  defaultDspTargetZoneId,
  resolveSettingsTargetZoneId,
  zoneIsPlaying,
  zoneIsPlayingNow
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

describe('zoneIsPlaying', () => {
  it('counts only states where audio is actually running', () => {
    expect(zoneIsPlaying(zone('kef', 'Playing'))).toBe(true);
    expect(zoneIsPlaying(zone('kef', 'Starting'))).toBe(true);
    expect(zoneIsPlaying(zone('kef', 'Stopped'))).toBe(false);
    expect(zoneIsPlaying(zone('kef', null))).toBe(false);
  });

  it('does not count a zone left paused from an earlier session', () => {
    expect(zoneIsPlaying(zone('hegel', 'Paused'))).toBe(false);
  });
});

describe('zoneIsPlayingNow', () => {
  const status = { state: 'Playing', active_zone_id: 'kef' };

  it('trusts the global status for a zone that reports no state of its own', () => {
    expect(zoneIsPlayingNow(zone('kef', null), status)).toBe(true);
  });

  it('leaves a paused zone alone while another zone is the active one', () => {
    expect(zoneIsPlayingNow(zone('hegel', 'Paused'), status)).toBe(false);
  });

  it('still reads per-zone state for zones that are not the active one', () => {
    expect(zoneIsPlayingNow(zone('hegel', 'Playing'), status)).toBe(true);
  });

  it('ignores the active zone when playback is not running', () => {
    expect(zoneIsPlayingNow(zone('kef', null), { state: 'Paused', active_zone_id: 'kef' })).toBe(
      false
    );
  });

  it('tolerates a missing status', () => {
    expect(zoneIsPlayingNow(zone('kef', 'Playing'), null)).toBe(true);
    expect(zoneIsPlayingNow(zone('kef', null), null)).toBe(false);
  });
});
