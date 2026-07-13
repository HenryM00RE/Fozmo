import { describe, expect, it } from 'vitest';
import {
  normalizeQobuzFeaturedPlaylistsResponse,
  qobuzPlaylistImage,
  qobuzPlaylistQueueItems,
  qobuzPlaylistTracks
} from './qobuzPlaylistData';

describe('qobuzPlaylistData', () => {
  const detail = {
    playlist: {
      id: 'pl-1',
      title: 'Qobuz playlist',
      image_url: 'https://static.qobuz.com/images/playlists/pl-1.jpg'
    },
    tracks: [
      {
        id: 11,
        title: 'Wrapped',
        artist: 'Artist A',
        album: 'Album A',
        album_id: 'album-a',
        image_url: 'https://static.qobuz.com/images/covers/a.jpg',
        duration: 180,
        playlist_context: { playlist_id: 'local-playlist' }
      },
      {
        track_id: 12,
        title: 'Direct',
        artist: 'Artist B',
        album: 'Album B',
        duration_secs: 200
      }
    ]
  };

  it('normalizes playlist tracks without local playlist context', () => {
    const tracks = qobuzPlaylistTracks(detail);

    expect(tracks.map((track) => track.id ?? track.track_id)).toEqual([11, 12]);
    expect(tracks[0].playlist_context).toBeNull();
  });

  it('converts Qobuz playlist tracks to playable queue items', () => {
    const items = qobuzPlaylistQueueItems(detail);

    expect(items).toHaveLength(2);
    expect(items[0].qobuzTrack?.id).toBe(11);
    expect(items[0].playlistContext).toBeNull();
    expect(items[1].durationSecs).toBe(200);
  });

  it('prefers the Qobuz playlist image from detail payloads', () => {
    expect(qobuzPlaylistImage(detail)).toBe('https://static.qobuz.com/images/playlists/pl-1.jpg');
    expect(
      qobuzPlaylistImage({ image_url: 'https://static.qobuz.com/images/playlists/summary.jpg' })
    ).toBe('https://static.qobuz.com/images/playlists/summary.jpg');
  });

  it('keeps legacy playlist arrays usable as paged featured playlist responses', () => {
    const response = normalizeQobuzFeaturedPlaylistsResponse(
      [
        { id: 'pl-1', title: 'One' },
        { id: 'pl-2', title: 'Two' }
      ],
      2,
      4
    );

    expect(response.playlists.map((playlist) => playlist.id)).toEqual(['pl-1', 'pl-2']);
    expect(response.limit).toBe(2);
    expect(response.offset).toBe(4);
    expect(response.total).toBeNull();
    expect(response.has_more).toBe(true);
  });

  it('normalizes featured playlist response metadata for exact page counts', () => {
    const response = normalizeQobuzFeaturedPlaylistsResponse(
      {
        playlists: [{ id: 'pl-3', title: 'Three' }],
        limit: '12',
        offset: '24',
        count: '1',
        total: '25'
      },
      12,
      24
    );

    expect(response.limit).toBe(12);
    expect(response.offset).toBe(24);
    expect(response.count).toBe(1);
    expect(response.total).toBe(25);
    expect(response.has_more).toBe(false);
  });
});
