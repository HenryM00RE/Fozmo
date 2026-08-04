import { describe, expect, it } from 'vitest';
import type { LibraryAlbum, LibraryTrack, Playlist, QueueItem } from '../../../shared/types';
import {
  mostRecentPlaylists,
  playlistCsv,
  playlistCsvFilename,
  playlistItems,
  queueItemsForPlayback
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

  it('inherits missing WAV metadata from the linked library album', () => {
    const playlist: Playlist = {
      id: 'playlist-1',
      name: 'Local WAVs',
      items: [
        {
          title: 'Kid A',
          artist: '',
          album: 'Kid A',
          albumId: 23,
          durationSecs: 284,
          filename: 'Kid A.wav',
          ref: { track_id: 81 }
        }
      ]
    };
    const tracks: LibraryTrack[] = [
      { id: 81, album_id: 23, title: 'Kid A', artist: '', album: 'Kid A' }
    ];
    const albums: LibraryAlbum[] = [
      { id: 23, title: 'Kid A', album_artist: 'Radiohead', art_id: 44 }
    ];

    const [item] = playlistItems(playlist, tracks, albums);

    expect(item.artist).toBe('Radiohead');
    expect(item.album).toBe('Kid A');
    expect(item.albumArtist).toBe('Radiohead');
    expect(item.artId).toBe(44);
  });

  it('keeps a song-level artist when the linked album has a different album artist', () => {
    const playlist: Playlist = {
      id: 'playlist-1',
      name: 'Compilation',
      items: [
        {
          title: 'Guest Track',
          artist: 'Guest Artist',
          album: 'Compilation',
          albumId: 24,
          durationSecs: 180,
          ref: { track_id: 82 }
        }
      ]
    };

    const [item] = playlistItems(
      playlist,
      [{ id: 82, album_id: 24 }],
      [{ id: 24, title: 'Compilation', album_artist: 'Various Artists' }]
    );

    expect(item.artist).toBe('Guest Artist');
    expect(item.albumArtist).toBe('Various Artists');
  });

  it('keeps inherited album metadata when a resolved WAV source is prepared for playback', () => {
    const playlist: Playlist = {
      id: 'playlist-1',
      name: 'Resolved WAVs',
      items: [
        {
          title: 'There, There',
          artist: '',
          album: 'Hail to the Thief',
          durationSecs: 323,
          resolvedSource: {
            kind: 'local_track',
            track_id: 91,
            title: 'There, There',
            artist: '',
            album: 'Hail to the Thief',
            album_id: 25,
            file_name: 'There, There.wav'
          }
        }
      ]
    };

    const [item] = queueItemsForPlayback(
      playlist,
      false,
      [{ id: 91, album_id: 25 }],
      [{ id: 25, title: 'Hail to the Thief', album_artist: 'Radiohead' }]
    );

    expect(item.artist).toBe('Radiohead');
    expect(item.album).toBe('Hail to the Thief');
    expect(item.resolvedSource?.artist).toBe('Radiohead');
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
