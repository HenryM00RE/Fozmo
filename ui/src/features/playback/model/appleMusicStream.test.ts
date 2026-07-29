import { describe, expect, it } from 'vitest';
import {
  APPLE_MUSIC_HOST_BROWSER_LOOP,
  APPLE_MUSIC_OUTPUT_UNSUPPORTED,
  APPLE_MUSIC_STREAM_IN_USE,
  appleMusicBlockedMessage,
  appleMusicStreamZoneName
} from './appleMusicStream';

describe('appleMusicBlockedMessage', () => {
  it('names the output already holding the single Apple Music stream', () => {
    const message = appleMusicBlockedMessage(APPLE_MUSIC_STREAM_IN_USE, {
      apple_music_stream_zone_name: 'Living Room'
    });

    expect(message).toContain('Living Room');
    expect(message).toContain('one output at a time');
  });

  it('still explains the conflict when the holding zone has no name yet', () => {
    const message = appleMusicBlockedMessage(APPLE_MUSIC_STREAM_IN_USE, {});

    expect(message).toContain('another output');
  });

  it('explains outputs that cannot carry a live capture at all', () => {
    expect(appleMusicBlockedMessage(APPLE_MUSIC_OUTPUT_UNSUPPORTED)).toContain(
      'cannot play Apple Music'
    );
  });

  it('explains why a browser on the capturing Mac cannot be the output', () => {
    expect(appleMusicBlockedMessage(APPLE_MUSIC_HOST_BROWSER_LOOP)).toContain('its own audio');
  });

  it('leaves ordinary playback failures to the transient notice', () => {
    expect(appleMusicBlockedMessage('Track not found')).toBeNull();
    expect(appleMusicBlockedMessage('')).toBeNull();
  });
});

describe('appleMusicStreamZoneName', () => {
  it('reads the holding zone from status and tolerates its absence', () => {
    expect(appleMusicStreamZoneName({ apple_music_stream_zone_name: ' Study ' })).toBe('Study');
    expect(appleMusicStreamZoneName({})).toBe('');
    expect(appleMusicStreamZoneName(null)).toBe('');
  });
});
