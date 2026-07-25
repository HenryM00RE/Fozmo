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
    expect(model.currentAlbumTarget).toEqual({
      source: 'apple_music',
      id: '1440880968',
      storefront: null
    });
    expect(model.sourceProvider).toBe('Apple Music');
  });

  it('uses the requested Apple identity instead of a stale queue cursor during startup', () => {
    const queue: QueueState = {
      kind: 'apple_music',
      cursor: 0,
      items: [
        {
          title: '2 + 2 = 5 (Live)',
          artist: 'Radiohead',
          album: 'Hail to the Thief (Live Recordings 2003–2009)',
          durationSecs: 216,
          filename: 'apple_music:old',
          resolvedSource: { kind: 'apple_music_track', song_id: 'old' }
        },
        {
          title: 'There, There (Live)',
          artist: 'Radiohead',
          album: 'Hail to the Thief (Live Recordings 2003–2009)',
          durationSecs: 333,
          filename: 'apple_music:new',
          resolvedSource: { kind: 'apple_music_track', song_id: 'new' }
        }
      ],
      loopMode: 'off'
    };

    const model = playbackChromeTrackModel({
      pendingArtSrc: 'https://example.test/new.jpg',
      pendingPlaybackIntent: {
        artist: 'Radiohead',
        fileName: 'apple_music:new',
        sourceKey: 'apple_music:new',
        title: 'There, There (Live)'
      },
      playbackLoading: true,
      queue,
      status: {
        state: 'Starting',
        file_name: 'apple_music:old',
        track_title: '2 + 2 = 5 (Live)',
        track_artist: 'Radiohead',
        current_source: { kind: 'apple_music_track', song_id: 'old' }
      }
    });

    expect(model.currentTrackName).toBe('There, There (Live)');
    expect(model.currentQueueItem?.title).toBe('There, There (Live)');
    expect(model.currentArt).toBe('https://example.test/new.jpg');
  });
});
