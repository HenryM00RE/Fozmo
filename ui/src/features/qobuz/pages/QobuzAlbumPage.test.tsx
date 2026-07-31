// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invalidateAppleMusicCatalogAlbumCache } from '../../albums/model/albumData';
import { QobuzAlbumPage } from './QobuzAlbumPage';

const mocks = vi.hoisted(() => ({
  addItemsToQueue: vi.fn(),
  albumByQobuzId: vi.fn(),
  appleMusicAlbumMatch: vi.fn(),
  appleMusicCatalogAlbum: vi.fn(),
  favoriteAlbums: vi.fn(),
  onOpenArtist: vi.fn(),
  onSelectionItemsChange: vi.fn(),
  onToggleSelection: vi.fn(),
  openPlaylistPickerForItems: vi.fn(),
  playAlbum: vi.fn(),
  playItems: vi.fn(),
  qobuzAlbum: vi.fn(),
  qobuzAppleMusicVersion: vi.fn(),
  qobuzArtistCore: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({
  endpoints: {
    albumByQobuzId: mocks.albumByQobuzId,
    appleMusicAlbumMatch: mocks.appleMusicAlbumMatch,
    appleMusicCatalogAlbum: mocks.appleMusicCatalogAlbum,
    artUrl: () => null,
    favoriteAlbums: mocks.favoriteAlbums,
    qobuzAlbum: mocks.qobuzAlbum,
    qobuzAppleMusicVersion: mocks.qobuzAppleMusicVersion,
    qobuzArtistCore: mocks.qobuzArtistCore
  }
}));

const appleCatalogAlbum = {
  album_id: '1109714933',
  storefront: 'nz',
  title: 'In Rainbows',
  artist: 'Radiohead',
  release_date: '2007-10-10T00:00:00Z',
  artwork_url: 'https://example.test/in-rainbows.jpg',
  audio_variants: ['lossless'],
  tracks: [
    {
      song_id: '1109715066',
      storefront: 'nz',
      album_id: '1109714933',
      title: '15 Step',
      artist: 'Radiohead',
      album_title: 'In Rainbows',
      album_artist: 'Radiohead',
      duration_secs: 237,
      track_number: 1,
      disc_number: 1
    },
    {
      song_id: '1109715161',
      storefront: 'nz',
      album_id: '1109714933',
      title: 'Bodysnatchers',
      artist: 'Radiohead',
      album_title: 'In Rainbows',
      album_artist: 'Radiohead',
      duration_secs: 242,
      track_number: 2,
      disc_number: 1
    }
  ]
};

const appleVersion = {
  id: 'apple_music:nz:1109714933',
  provider: 'apple_music',
  provider_id: '1109714933',
  source_label: 'Apple Music',
  title: 'In Rainbows',
  artist: 'Radiohead',
  year: 2007,
  track_count: 2,
  format: 'Apple Music',
  storefront: 'nz',
  audio_variants: ['lossless'],
  is_primary: false
};

function renderPage(id: string, remoteSurface = false) {
  render(
    <StrictMode>
      <QobuzAlbumPage
        id={id}
        onOpenArtist={mocks.onOpenArtist}
        playAlbum={mocks.playAlbum}
        playItems={mocks.playItems}
        addItemsToQueue={mocks.addItemsToQueue}
        selectedTrackKeys={new Set()}
        selectionActive={false}
        onSelectionItemsChange={mocks.onSelectionItemsChange}
        onToggleSelection={mocks.onToggleSelection}
        openPlaylistPickerForItems={mocks.openPlaylistPickerForItems}
        remoteSurface={remoteSurface}
        playbackStatus={{ state: 'Stopped' }}
        customDisplayFont={null}
      />
    </StrictMode>
  );
}

beforeEach(() => {
  invalidateAppleMusicCatalogAlbumCache();
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.qobuzAlbum.mockImplementation(async (albumId: string) => ({
    album: {
      id: albumId,
      title: 'In Rainbows',
      artist: 'Radiohead',
      year: 2007,
      hires: true,
      maximum_sampling_rate: 44.1,
      maximum_bit_depth: 24
    },
    tracks: [
      {
        id: 501,
        title: '15 Step',
        artist: 'Radiohead',
        album: 'In Rainbows',
        album_id: albumId,
        track_number: 1,
        disc_number: 1,
        duration: 237
      },
      {
        id: 502,
        title: 'Bodysnatchers',
        artist: 'Radiohead',
        album: 'In Rainbows',
        album_id: albumId,
        track_number: 2,
        disc_number: 1,
        duration: 242
      }
    ]
  }));
  mocks.albumByQobuzId.mockResolvedValue(null);
  mocks.appleMusicAlbumMatch.mockResolvedValue({ status: 'no_match' });
  mocks.qobuzArtistCore.mockResolvedValue({ albums: [] });
  mocks.favoriteAlbums.mockResolvedValue([]);
  mocks.appleMusicCatalogAlbum.mockResolvedValue(appleCatalogAlbum);
  mocks.qobuzAppleMusicVersion.mockResolvedValue({
    status: 'linked',
    version: appleVersion,
    apple_album: appleCatalogAlbum
  });
});

afterEach(cleanup);

describe('QobuzAlbumPage', () => {
  it('starts finding the Apple Music edition before Versions is opened', async () => {
    renderPage('qobuz-in-rainbows');

    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(mocks.qobuzAppleMusicVersion).toHaveBeenCalledWith('qobuz-in-rainbows')
    );
    expect(mocks.qobuzAppleMusicVersion).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('tab', { name: 'Versions' }));

    expect(await screen.findByText('Apple Music')).toBeInTheDocument();
    expect(screen.getByText('3 versions')).toBeInTheDocument();
  });

  it('uses the matched Apple payload without refetching the catalog', async () => {
    renderPage('qobuz-in-rainbows-play');

    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole('tab', { name: 'Versions' }));
    expect(await screen.findByText('Apple Music')).toBeInTheDocument();

    expect(mocks.appleMusicCatalogAlbum).not.toHaveBeenCalled();

    fireEvent.click(screen.getByText('Apple Music'));
    fireEvent.click(screen.getByRole('button', { name: 'Play now' }));

    await waitFor(() =>
      expect(mocks.playItems).toHaveBeenCalledWith(
        [
          expect.objectContaining({
            resolvedSource: expect.objectContaining({
              kind: 'apple_music_track',
              song_id: '1109715066',
              storefront: 'nz'
            })
          }),
          expect.objectContaining({
            resolvedSource: expect.objectContaining({
              kind: 'apple_music_track',
              song_id: '1109715161'
            })
          })
        ],
        0
      )
    );
    expect(mocks.playAlbum).not.toHaveBeenCalled();
  });

  it('falls back to the catalog when an older match response has no album payload', async () => {
    mocks.qobuzAppleMusicVersion.mockResolvedValue({
      status: 'linked',
      version: appleVersion
    });

    renderPage('qobuz-in-rainbows-fallback');

    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(mocks.appleMusicCatalogAlbum).toHaveBeenCalledWith('1109714933', 'nz')
    );
  });

  it('leaves a Qobuz album that a local album covers to the local Apple link', async () => {
    mocks.albumByQobuzId.mockResolvedValue({
      album: { id: 7, title: 'In Rainbows', album_artist: 'Radiohead' },
      tracks: [],
      versions: [
        {
          id: 11,
          provider: 'local',
          provider_id: 'local:7',
          source_label: 'Library',
          title: 'In Rainbows',
          is_primary: true
        }
      ]
    });

    renderPage('qobuz-in-rainbows-linked');

    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole('tab', { name: 'Versions' }));

    expect(await screen.findByText('Library')).toBeInTheDocument();
    await waitFor(() => expect(mocks.appleMusicAlbumMatch).toHaveBeenCalledWith(7, undefined));
    expect(mocks.qobuzAppleMusicVersion).not.toHaveBeenCalled();
  });

  it('does not start background matching on a remote surface', async () => {
    renderPage('qobuz-in-rainbows-remote', true);

    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole('tab', { name: 'Versions' }));

    expect(mocks.qobuzAppleMusicVersion).not.toHaveBeenCalled();
    expect(mocks.appleMusicAlbumMatch).not.toHaveBeenCalled();
  });
});
