// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppleMusicAlbumPage } from './AppleMusicAlbumPage';

const mocks = vi.hoisted(() => ({
  addItemsToQueue: vi.fn(),
  appleMusicCatalogAlbum: vi.fn(),
  onOpenArtist: vi.fn(),
  onSelectionItemsChange: vi.fn(),
  onToggleSelection: vi.fn(),
  openPlaylistPickerForItems: vi.fn(),
  playItems: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({
  endpoints: {
    appleMusicCatalogAlbum: mocks.appleMusicCatalogAlbum,
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
    expect(screen.getAllByText('Apple Music').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Lossless').length).toBeGreaterThan(0);
    expect(screen.getAllByLabelText('Apple Music').length).toBeGreaterThan(0);
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
});
