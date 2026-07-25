import { describe, expect, it } from 'vitest';
import type { JsonRecord, QueueState } from '../../../shared/types';
import { playbackChromeTrackModel } from './playbackChromeModel';

describe('playbackChromeTrackModel', () => {
  it('uses Apple Music artwork instead of a stale player cover', () => {
    const status: JsonRecord = {
      active_zone_id: 'local-hegel',
      state: 'Playing',
      cover_version: 12,
      current_source: {
        kind: 'apple_music_track',
        song_id: '1440880976',
        title: 'Jóga',
        artist: 'Björk',
        album: 'Homogenic',
        album_id: '1440880968',
        artwork_url: 'https://is1-ssl.mzstatic.com/image/thumb/example/1200x1200bb.jpg',
        duration_secs: 312
      }
    };
    const queue: QueueState = {
      kind: 'apple_music',
      cursor: -1,
      items: [],
      loopMode: 'off'
    };

    const model = playbackChromeTrackModel({
      pendingArtSrc: null,
      playbackLoading: false,
      queue,
      status
    });

    expect(model.currentArt).toBe(
      'https://is1-ssl.mzstatic.com/image/thumb/example/1200x1200bb.jpg'
    );
    expect(model.currentTrackName).toBe('Jóga');
    expect(model.currentArtist).toBe('Björk');
    expect(model.currentAlbum).toBe('Homogenic');
    expect(model.sourceProvider).toBe('Apple Music');
  });
});
