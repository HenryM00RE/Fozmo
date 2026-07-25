// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppleMusicMvpPage } from './AppleMusicMvpPage';

const mocks = vi.hoisted(() => ({
  addItemsToQueue: vi.fn(),
  appleMusicStatus: vi.fn(),
  appleMusicCatalogSearch: vi.fn(),
  playAppleMusicScenario: vi.fn(),
  launchAppleMusicHelper: vi.fn(),
  authorizeAppleMusic: vi.fn(),
  shutdownAppleMusicHelper: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({ endpoints: mocks }));

const activeZoneStatus = {
  active_zone_id: 'local-core',
  active_zone_name: 'Studio DAC',
  zone_protocol: 'local_core_audio'
};

const readyStatus = {
  helper_present: true,
  helper_pid: 123,
  authorization: 'authorized',
  can_play_catalog_content: true,
  state: 'ready'
};

const song = {
  song_id: '1440880976',
  storefront: 'nz',
  title: 'Jóga',
  artist: 'Björk',
  album_title: 'Homogenic',
  duration_secs: 312
};

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.addItemsToQueue.mockResolvedValue(true);
  mocks.appleMusicStatus.mockResolvedValue(readyStatus);
  mocks.appleMusicCatalogSearch.mockResolvedValue({ songs: [song], albums: [] });
  mocks.playAppleMusicScenario.mockResolvedValue({});
  mocks.launchAppleMusicHelper.mockResolvedValue({});
  mocks.authorizeAppleMusic.mockResolvedValue({});
  mocks.shutdownAppleMusicHelper.mockResolvedValue({});
});

afterEach(cleanup);

describe('AppleMusicMvpPage', () => {
  it('searches the MusicKit catalog and plays through the product route', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={activeZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );

    expect(await screen.findByText('Music.app playback')).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText('Artist, album, or track'), {
      target: { value: 'joga bjork' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Search' }));

    expect(await screen.findByText('Jóga')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Play' }));

    await waitFor(() =>
      expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
        'local-core',
        expect.objectContaining({
          kind: 'apple_music_track',
          song_id: '1440880976',
          storefront: 'nz'
        }),
        []
      )
    );
  });

  it('adds a catalog result to the product queue', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={activeZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );
    await screen.findByText('Music.app playback');
    fireEvent.change(screen.getByPlaceholderText('Artist, album, or track'), {
      target: { value: 'joga' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Search' }));
    await screen.findByText('Jóga');
    fireEvent.click(screen.getByRole('button', { name: 'Play next' }));

    await waitFor(() => expect(mocks.addItemsToQueue).toHaveBeenCalledTimes(1));
    expect(mocks.addItemsToQueue).toHaveBeenCalledWith(
      [
        expect.objectContaining({
          resolvedSource: expect.objectContaining({
            kind: 'apple_music_track',
            song_id: '1440880976'
          })
        })
      ],
      'next'
    );
  });

  it('offers the helper launch and authorization lifecycle', async () => {
    mocks.appleMusicStatus.mockResolvedValue({
      helper_present: true,
      helper_pid: null,
      authorization: 'not_determined',
      state: 'stopped'
    });
    render(
      <AppleMusicMvpPage
        activeZoneStatus={activeZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );

    await screen.findByText('Music.app playback');
    fireEvent.click(screen.getByRole('button', { name: 'Launch helper' }));
    await waitFor(() => expect(mocks.launchAppleMusicHelper).toHaveBeenCalledTimes(1));
  });
});
