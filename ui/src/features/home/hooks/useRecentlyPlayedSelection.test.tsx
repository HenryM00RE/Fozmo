// @vitest-environment jsdom
import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { JsonRecord } from '../../../shared/types';
import { invalidateAppleMusicCatalogAlbumCache } from '../../albums/model/albumData';
import { useRecentlyPlayedSelection } from './useRecentlyPlayedSelection';

const mocks = vi.hoisted(() => ({
  appleMusicCatalogAlbum: vi.fn(),
  appleMusicCatalogSong: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({
  endpoints: {
    appleMusicCatalogAlbum: mocks.appleMusicCatalogAlbum,
    appleMusicCatalogSong: mocks.appleMusicCatalogSong
  }
}));

const appleRecent: JsonRecord = {
  recent_type: 'album',
  id: '1828175826',
  provider: 'apple_music',
  is_apple_music: true,
  apple_music_album_id: '1828175826',
  title: 'Hail to the Thief (Live Recordings 2003–2009)',
  album_artist: 'Radiohead'
};

function renderRecentSelection() {
  const navigate = vi.fn();
  const playAlbum = vi.fn().mockResolvedValue(undefined);
  const playItems = vi.fn();
  const setNotice = vi.fn();
  const result = renderHook(() =>
    useRecentlyPlayedSelection({
      albums: [],
      playlists: [],
      recentAlbums: [appleRecent],
      recentPlaylists: [],
      navigate,
      playAlbum,
      playItems,
      addItemsToQueue: vi.fn(),
      openPlaylistPickerForItems: vi.fn(),
      setNotice,
      onSelectionStart: vi.fn()
    })
  );
  return { ...result, navigate, playAlbum, playItems, setNotice };
}

beforeEach(() => {
  invalidateAppleMusicCatalogAlbumCache();
  mocks.appleMusicCatalogAlbum.mockReset();
  mocks.appleMusicCatalogSong.mockReset();
  mocks.appleMusicCatalogAlbum.mockResolvedValue({
    album_id: '1828175826',
    storefront: 'nz',
    title: appleRecent.title,
    artist: 'Radiohead',
    tracks: [
      {
        song_id: '1828175833',
        storefront: 'nz',
        album_id: '1828175826',
        title: '2 + 2 = 5 (Live)',
        artist: 'Radiohead',
        album_title: appleRecent.title
      }
    ]
  });
});

describe('recently played Apple Music albums', () => {
  it('keeps Apple Music selection identity separate from local numeric album ids', () => {
    const { result } = renderRecentSelection();

    act(() => {
      result.current.toggleSelection(appleRecent);
    });

    expect(result.current.selectionKeys).toEqual(new Set(['apple_music:1828175826']));
  });

  it('opens the catalog route with an explicit Apple Music provider', async () => {
    const { result, navigate } = renderRecentSelection();

    await act(async () => {
      await result.current.openItem(appleRecent);
    });

    expect(navigate).toHaveBeenCalledWith({
      view: 'album',
      id: '1828175826',
      provider: 'apple_music'
    });
  });

  it('plays catalog sources directly instead of treating the catalog id as a local album id', async () => {
    const { result, playAlbum, playItems } = renderRecentSelection();

    await act(async () => {
      await result.current.playItem(appleRecent);
    });

    expect(mocks.appleMusicCatalogAlbum).toHaveBeenCalledWith('1828175826', undefined);
    expect(playItems).toHaveBeenCalledWith(
      [
        expect.objectContaining({
          filename: 'apple_music:1828175833',
          resolvedSource: expect.objectContaining({
            kind: 'apple_music_track',
            song_id: '1828175833',
            album_id: '1828175826',
            storefront: 'nz'
          })
        })
      ],
      0
    );
    expect(playAlbum).not.toHaveBeenCalled();
  });
});
