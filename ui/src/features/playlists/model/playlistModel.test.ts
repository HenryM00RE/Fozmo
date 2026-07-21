import { describe, expect, it } from 'vitest';
import type { LibraryTrack, Playlist, QueueItem } from '../../../shared/types';
import {
  mostRecentPlaylists,
  playlistCsv,
  playlistCsvFilename,
  playlistItems
} from './playlistModel';

describe('mostRecentPlaylists', () => {
  it('returns only the five most recently updated playlists', () => {
    const playlists: Playlist[] = [
      { id: 'missing-date', name: 'Missing date' },
      { id: 'three', name: 'Three', updated_at: 300 },
      { id: 'one', name: 'One', updatedAt: 100 },
      { id: 'six', name: 'Six', updatedAt: 600 },
      { id: 'two', name: 'Two', created_at: 200 },
      { id: 'five', name: 'Five', updated_at: 500 },
      { id: 'four', name: 'Four', createdAt: 400 }
    ];

    expect(mostRecentPlaylists(playlists).map((playlist) => playlist.id)).toEqual([
      'six',
      'five',
      'four',
      'three',
      'two'
    ]);
  });
});

describe('playlistItems', () => {
  it('recovers local album ids for legacy playlist items with only track refs', () => {
    const playlist: Playlist = {
      id: 'playlist-1',
      name: 'Legacy playlist',
      items: [
        {
          title: 'Legacy Song',
          artist: 'Legacy Artist',
          album: '',
          durationSecs: 120,
          filename: 'legacy.flac',
          ref: { track_id: 42 }
        }
      ]
    };
    const tracks: LibraryTrack[] = [
      {
        id: 42,
        title: 'Legacy Song',
        artist: 'Legacy Artist',
        album: 'Resolved Album',
        album_artist: 'Resolved Artist',
        album_id: 7,
        art_id: 99,
        image_url: '/cover/7'
      }
    ];

    const [item] = playlistItems(playlist, tracks);

    expect(item.albumId).toBe(7);
    expect(item.album).toBe('Resolved Album');
    expect(item.artId).toBe(99);
    expect(item.imageUrl).toBe('/cover/7');
  });

  it('preserves existing Qobuz album navigation metadata', () => {
    const playlist: Playlist = {
      id: 'playlist-1',
      name: 'Qobuz playlist',
      items: [
        {
          title: 'Qobuz Song',
          artist: 'Qobuz Artist',
          album: 'Qobuz Album',
          albumId: 'qobuz-album',
          durationSecs: 120,
          filename: 'Qobuz Artist - Qobuz Song',
          qobuzTrack: {
            id: 12,
            title: 'Qobuz Song',
            artist: 'Qobuz Artist',
            album: 'Qobuz Album',
            album_id: 'qobuz-album'
          }
        }
      ]
    };

    const [item] = playlistItems(playlist, [{ id: 12, album_id: 7 }]);

    expect(item.albumId).toBe('qobuz-album');
    expect(item.qobuzTrack?.album_id).toBe('qobuz-album');
  });
});

describe('playlistCsv', () => {
  it('exports ordered playlist metadata with CSV escaping', () => {
    const items: QueueItem[] = [
      {
        title: 'Song, "Part Two"',
        artist: '=Unexpected Formula',
        album: 'Album\nDeluxe',
        albumArtist: 'Album Artist',
        durationSecs: 123.5,
        filename: 'song.flac',
        resolvedSource: { kind: 'local_track' }
      },
      {
        title: 'Streamed Song',
        artist: 'Artist',
        album: 'Album',
        durationSecs: 240,
        filename: null,
        qobuzTrack: { id: 42, title: 'Streamed Song', artist: 'Artist', album: 'Album' }
      }
    ];

    expect(playlistCsv(items)).toBe(
      [
        'Position,Title,Artist,Album,Album Artist,Duration (seconds),Source,Filename',
        '1,"Song, ""Part Two""",\'=Unexpected Formula,"Album\nDeluxe",Album Artist,123.5,Local,song.flac',
        '2,Streamed Song,Artist,Album,,240,Qobuz,'
      ].join('\r\n')
    );
  });

  it('creates a filesystem-safe CSV filename', () => {
    expect(playlistCsvFilename('  Night/Drive: 01  ')).toBe('Night-Drive- 01.csv');
    expect(playlistCsvFilename('...')).toBe('playlist.csv');
  });
});
