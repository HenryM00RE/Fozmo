import { describe, expect, it } from 'vitest';
import type { ZoneProfile } from '../../../shared/types';
import type { BrowserZoneSnapshot } from '../../browserZone/browserZoneAgent';
import { isOwnBrowserZonePending, statusWithBrowserPlayback } from './zoneSelection';

function zone(id: string, extra: Partial<ZoneProfile> = {}): ZoneProfile {
  return { id, name: id, ...extra } as ZoneProfile;
}

const BROWSER_ID = 'browser-abc123';
const isOwn = (id: string) => id === BROWSER_ID;

describe('isOwnBrowserZonePending', () => {
  it('is pending when the own browser zone is absent from the list (re-registering)', () => {
    const zones = [zone('sonos'), zone('hegel')];
    expect(isOwnBrowserZonePending(BROWSER_ID, zones, isOwn)).toBe(true);
  });

  it('is pending when no zones have loaded yet on refresh', () => {
    expect(isOwnBrowserZonePending(BROWSER_ID, [], isOwn)).toBe(true);
  });

  it('is not pending once the browser zone is listed (registered)', () => {
    const zones = [zone('sonos'), zone(BROWSER_ID, { browser: true })];
    expect(isOwnBrowserZonePending(BROWSER_ID, zones, isOwn)).toBe(false);
  });

  it('is not pending for a genuinely disabled-but-listed browser zone', () => {
    // Present in the list means the server still knows about it, so a disabled
    // flag is a real "off" and should fall back, not wait.
    const zones = [zone(BROWSER_ID, { browser: true, enabled: false })];
    expect(isOwnBrowserZonePending(BROWSER_ID, zones, isOwn)).toBe(false);
  });

  it('never applies to non-browser zones or empty selection', () => {
    const zones = [zone('sonos')];
    expect(isOwnBrowserZonePending('sonos', zones, isOwn)).toBe(false);
    expect(isOwnBrowserZonePending(null, zones, isOwn)).toBe(false);
  });
});

describe('statusWithBrowserPlayback', () => {
  it('uses the media element source instead of an older server song', () => {
    const currentSource = {
      kind: 'qobuz_track' as const,
      track_id: 202,
      title: 'Current Song',
      artist: 'Current Artist',
      album: 'Current Album'
    };
    const browser = {
      connection: 'connected',
      zoneId: BROWSER_ID,
      zoneName: 'Safari on iPhone',
      notice: null,
      playback: {
        state: 'Playing',
        currentSource,
        fileName: 'Current Artist - Current Song',
        trackTitle: 'Current Song',
        trackArtist: 'Current Artist',
        trackAlbum: 'Current Album',
        positionSecs: 12,
        durationSecs: 180,
        volume: 0.8
      }
    } as BrowserZoneSnapshot;

    const status = statusWithBrowserPlayback(
      {
        state: 'Playing',
        track_title: 'Previous Song',
        current_source: { kind: 'qobuz_track', track_id: 101 }
      },
      browser
    );

    expect(status.track_title).toBe('Current Song');
    expect(status.current_source).toEqual(currentSource);
    expect(status.active_zone_id).toBe(BROWSER_ID);
    expect(status.remote_connected).toBe(true);
  });
});
