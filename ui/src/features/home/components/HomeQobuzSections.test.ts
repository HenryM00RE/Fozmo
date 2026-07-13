import { describe, expect, it } from 'vitest';
import type { JsonRecord } from '../../../shared/types';
import {
  filterQobuzAlbumsByGenre,
  qobuzEditorialPlaylists,
  qobuzGenreOptions,
  qobuzPlaylistCategoryOptions,
  qobuzPlaylistGenreOptions
} from './HomeQobuzSections';

describe('HomeQobuzSections genre filters', () => {
  const albums = [
    { id: '1', title: 'Black Classical Music', genre: 'Electronic' },
    { id: '2', title: 'New Moon Daughter', genre: { id: 64, name: 'Jazz' } },
    { id: '3', title: 'Space 1.8', genres: ['Ambient', 'Electronic'] },
    { id: '4', title: 'Blue Light', genre: 'Jazz', genre_id: 64 }
  ] as JsonRecord[];

  it('builds stable genre options from Qobuz album metadata', () => {
    expect(qobuzGenreOptions(albums)).toEqual([
      { value: 'all', label: 'All genres' },
      { value: 'ambient', label: 'Ambient' },
      { value: 'electronic', label: 'Electronic' },
      { value: 'id:64', label: 'Jazz', genreId: 64 }
    ]);
  });

  it('can omit unavailable generated genre options', () => {
    expect(
      qobuzGenreOptions(
        [...albums, { id: '5', title: 'After Hours', genre: { id: 78, name: 'Vocal jazz' } }],
        new Set(['vocal jazz'])
      )
    ).toEqual([
      { value: 'all', label: 'All genres' },
      { value: 'ambient', label: 'Ambient' },
      { value: 'electronic', label: 'Electronic' },
      { value: 'id:64', label: 'Jazz', genreId: 64 }
    ]);
  });

  it('filters albums by the selected normalized genre', () => {
    expect(filterQobuzAlbumsByGenre(albums, 'id:64').map((album) => album.id)).toEqual(['2', '4']);
    expect(filterQobuzAlbumsByGenre(albums, 'all')).toEqual(albums);
  });

  it('extracts editorial Qobuz playlists from the home payload', () => {
    const qobuzHome = {
      sections: [
        { id: 'new-releases', item_type: 'album', albums },
        {
          id: 'editorial-playlists',
          item_type: 'playlist',
          playlists: [
            { id: 'pl-1', title: 'Qobuzissime', owner: 'Qobuz', tracks_count: 24 },
            { title: 'Missing id' }
          ]
        }
      ]
    };

    expect(qobuzEditorialPlaylists(qobuzHome).map((playlist) => playlist.id)).toEqual(['pl-1']);
  });

  it('builds playlist category options from Qobuz tags without duplicate fallbacks', () => {
    const options = qobuzPlaylistCategoryOptions([
      { id: 'working-qobuz-digs', label: 'Qobuz Digs' },
      { id: 'duplicate-qobuz-digs', label: 'Qobuz Digs' },
      { id: 'new-tag', label: 'New Tag' }
    ]);

    expect(options[0]).toEqual({ value: 'all', label: 'All categories' });
    expect(options.filter((option) => option.label === 'Qobuz Digs')).toEqual([
      { value: 'working-qobuz-digs', label: 'Qobuz Digs' }
    ]);
    expect(options.find((option) => option.value === 'new-tag')).toEqual({
      value: 'new-tag',
      label: 'New Tag'
    });
  });

  it('builds playlist genre options from Qobuz genre ids', () => {
    expect(
      qobuzPlaylistGenreOptions([
        { id: 64, label: 'Jazz' },
        { genre_id: '44', name: 'Electronic' }
      ])
    ).toEqual([
      { value: 'all', label: 'All genres' },
      { value: 'id:44', label: 'Electronic', genreId: 44 },
      { value: 'id:64', label: 'Jazz', genreId: 64 }
    ]);
  });
});
