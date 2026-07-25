// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppleMusicAlbumPage } from './AppleMusicAlbumPage';

const mocks = vi.hoisted(() => ({
  addItemsToQueue: vi.fn(),
  albumByAppleMusicId: vi.fn(),
  appleMusicCatalogAlbum: vi.fn(),
  appleMusicAlbumVersionDetail: vi.fn(),
  onOpenArtist: vi.fn(),
  onSelectionItemsChange: vi.fn(),
  onToggleSelection: vi.fn(),
  openPlaylistPickerForItems: vi.fn(),
  playAlbum: vi.fn(),
  playItems: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({
  endpoints: {
    appleMusicCatalogAlbum: mocks.appleMusicCatalogAlbum,
    albumByAppleMusicId: mocks.albumByAppleMusicId,
    appleMusicAlbumVersionDetail: mocks.appleMusicAlbumVersionDetail,
    artUrl: () => null
  }
}));

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.appleMusicCatalogAlbum.mockResolvedValue({
    album_id: '1109714933',
    storefront: 'nz',
    title: 'In Rainbows',
    artist: 'Radiohead',
    release_date: '2007-10-10T00:00:00Z',
    artwork_url: 'https://example.test/in-rainbows.jpg',
    editorial_notes_standard: 'Apple Music editorial notes for In Rainbows.',
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
        disc_number: 1,
        artwork_url: 'https://example.test/in-rainbows.jpg'
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
        disc_number: 1,
        artwork_url: 'https://example.test/in-rainbows.jpg'
      }
    ]
  });
  mocks.albumByAppleMusicId.mockResolvedValue(null);
  mocks.appleMusicAlbumVersionDetail.mockResolvedValue({ apple_album: null });
});

afterEach(cleanup);

describe('AppleMusicAlbumPage', () => {
  it('loads a routed catalog album and plays it through the normal queue actions', async () => {
    render(
      <AppleMusicAlbumPage
        id="1109714933"
        storefront="nz"
        onOpenArtist={mocks.onOpenArtist}
        playItems={mocks.playItems}
        addItemsToQueue={mocks.addItemsToQueue}
        selectedTrackKeys={new Set()}
        selectionActive={false}
        onSelectionItemsChange={mocks.onSelectionItemsChange}
        onToggleSelection={mocks.onToggleSelection}
        openPlaylistPickerForItems={mocks.openPlaylistPickerForItems}
        playbackStatus={{ state: 'Stopped' }}
        customDisplayFont={null}
      />
    );

    expect(screen.getByRole('status', { name: 'Loading album' })).toBeInTheDocument();
    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    expect(mocks.appleMusicCatalogAlbum).toHaveBeenCalledWith('1109714933', 'nz');
    expect(screen.getAllByText('15 Step').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Bodysnatchers').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Lossless').length).toBeGreaterThan(0);
    expect(screen.getAllByLabelText('Apple Music').length).toBeGreaterThan(0);
    expect(screen.getByText('Apple Music editorial notes for In Rainbows.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Favorite' })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Play now' }));

    await waitFor(() =>
      expect(mocks.playItems).toHaveBeenCalledWith(
        [
          expect.objectContaining({
            title: '15 Step',
            album: 'In Rainbows',
            resolvedSource: expect.objectContaining({
              kind: 'apple_music_track',
              song_id: '1109715066',
              storefront: 'nz'
            })
          }),
          expect.objectContaining({
            title: 'Bodysnatchers',
            resolvedSource: expect.objectContaining({
              kind: 'apple_music_track',
              song_id: '1109715161'
            })
          })
        ],
        0
      )
    );
  });

  it('prefers the linked Qobuz description over Apple editorial notes', async () => {
    mocks.albumByAppleMusicId.mockResolvedValue({
      album: {
        id: 7,
        title: 'In Rainbows',
        album_artist: 'Radiohead',
        primary_version_id: 11
      },
      tracks: [],
      canonical_album: {
        qobuz_album_id: 'qobuz-in-rainbows',
        description: 'Qobuz editorial description.'
      },
      canonical_tracks: [],
      qobuz_track_links: [],
      versions: [
        {
          id: 11,
          provider: 'local',
          provider_id: 'local:7',
          source_label: 'Library',
          title: 'In Rainbows',
          is_primary: true
        },
        {
          id: 12,
          provider: 'apple_music',
          provider_id: '1109714933',
          source_label: 'Apple Music',
          title: 'In Rainbows',
          is_primary: false
        }
      ]
    });
    mocks.appleMusicAlbumVersionDetail.mockResolvedValue({
      apple_album: await mocks.appleMusicCatalogAlbum()
    });

    render(
      <AppleMusicAlbumPage
        id="1109714933"
        storefront="nz"
        onOpenArtist={mocks.onOpenArtist}
        playItems={mocks.playItems}
        addItemsToQueue={mocks.addItemsToQueue}
        selectedTrackKeys={new Set()}
        selectionActive={false}
        onSelectionItemsChange={mocks.onSelectionItemsChange}
        onToggleSelection={mocks.onToggleSelection}
        openPlaylistPickerForItems={mocks.openPlaylistPickerForItems}
        playbackStatus={{ state: 'Stopped' }}
        customDisplayFont={null}
      />
    );

    expect(await screen.findByText('Qobuz editorial description.')).toBeInTheDocument();
    expect(
      screen.queryByText('Apple Music editorial notes for In Rainbows.')
    ).not.toBeInTheDocument();
  });

  it('plays linked catalog albums directly instead of falling back through the library version', async () => {
    mocks.albumByAppleMusicId.mockResolvedValue({
      album: {
        id: 7,
        title: 'In Rainbows',
        album_artist: 'Radiohead',
        primary_version_id: 11
      },
      tracks: [],
      versions: [
        {
          id: 11,
          provider: 'local',
          source_label: 'Library',
          title: 'In Rainbows',
          is_primary: true
        },
        {
          id: 12,
          provider: 'apple_music',
          provider_id: '1109714933',
          source_label: 'Apple Music',
          title: 'In Rainbows',
          is_primary: false
        }
      ]
    });
    mocks.appleMusicAlbumVersionDetail.mockResolvedValue({
      apple_album: await mocks.appleMusicCatalogAlbum()
    });

    render(
      <AppleMusicAlbumPage
        id="1109714933"
        storefront="nz"
        onOpenArtist={mocks.onOpenArtist}
        playAlbum={mocks.playAlbum}
        playItems={mocks.playItems}
        addItemsToQueue={mocks.addItemsToQueue}
        selectedTrackKeys={new Set()}
        selectionActive={false}
        onSelectionItemsChange={mocks.onSelectionItemsChange}
        onToggleSelection={mocks.onToggleSelection}
        openPlaylistPickerForItems={mocks.openPlaylistPickerForItems}
        playbackStatus={{ state: 'Stopped' }}
        customDisplayFont={null}
      />
    );

    expect(
      await screen.findByRole('heading', { level: 1, name: 'In Rainbows' })
    ).toBeInTheDocument();
    await waitFor(() => expect(mocks.appleMusicAlbumVersionDetail).toHaveBeenCalledWith(7, 12));

    fireEvent.click(screen.getByRole('button', { name: 'Play now' }));

    await waitFor(() =>
      expect(mocks.playItems).toHaveBeenCalledWith(
        [
          expect.objectContaining({
            resolvedSource: expect.objectContaining({
              kind: 'apple_music_track',
              song_id: '1109715066'
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
});
