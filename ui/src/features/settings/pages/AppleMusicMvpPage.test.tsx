// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppleMusicMvpPage } from './AppleMusicMvpPage';

const mocks = vi.hoisted(() => ({
  addItemsToQueue: vi.fn(),
  appleMusicStatus: vi.fn(),
  status: vi.fn(),
  zoneStatus: vi.fn(),
  nowPlayingQueue: vi.fn(),
  appleMusicCatalogSearch: vi.fn(),
  appleMusicCatalogSong: vi.fn(),
  appleMusicCatalogAlbum: vi.fn(),
  playAppleMusicScenario: vi.fn(),
  appleMusicAlbumPreview: vi.fn(),
  appleMusicAlbumLink: vi.fn(),
  appleMusicAlbumUnlink: vi.fn(),
  albumPlaySources: vi.fn(),
  launchAppleMusicHelper: vi.fn(),
  authorizeAppleMusic: vi.fn(),
  confirmAppleMusicCapture: vi.fn(),
  pause: vi.fn(),
  pauseZone: vi.fn(),
  resume: vi.fn(),
  resumeZone: vi.fn(),
  next: vi.fn(),
  nextZone: vi.fn(),
  stop: vi.fn(),
  stopZone: vi.fn(),
  seek: vi.fn(),
  seekZone: vi.fn(),
  playAppleMusicSong: vi.fn(),
  controlAppleMusic: vi.fn(),
  startAppleMusicProcessTap: vi.fn(),
  stopAppleMusicProcessTap: vi.fn(),
  shutdownAppleMusicHelper: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({ endpoints: mocks }));

const defaultActiveZoneStatus = {
  active_zone_id: 'local-core',
  active_zone_name: 'Mac output',
  zone_protocol: 'local_core_audio',
  current_source: null
};

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.addItemsToQueue.mockResolvedValue(true);
  mocks.appleMusicStatus.mockResolvedValue({
    helper_present: true,
    helper_version: '0.2.0',
    helper_musickit_entitled: false,
    authorization: 'not_determined',
    playback_state: 'stopped',
    state: 'ready',
    process_tap: { state: 'stopped', metrics: {} },
    recent_events: []
  });
  mocks.status.mockResolvedValue(defaultActiveZoneStatus);
  mocks.zoneStatus.mockResolvedValue(defaultActiveZoneStatus);
  mocks.nowPlayingQueue.mockResolvedValue({
    current_source: null,
    queued_sources: []
  });
  mocks.appleMusicCatalogSearch.mockResolvedValue({
    term: 'joga bjork',
    storefront: 'nz',
    albums: [
      {
        album_id: '1440880938',
        storefront: 'nz',
        title: 'Homogenic',
        artist: 'Björk',
        release_date: '1997-09-22T00:00:00Z',
        artwork_url: 'https://example.test/homogenic.jpg',
        audio_variants: ['lossless'],
        tracks: []
      }
    ],
    songs: [
      {
        song_id: '1440880976',
        storefront: 'nz',
        title: 'Jóga',
        artist: 'Björk',
        album_title: 'Homogenic',
        artwork_url: 'https://example.test/joga.jpg',
        duration_secs: 312
      }
    ]
  });
  mocks.appleMusicCatalogSong.mockResolvedValue({
    song_id: '2037093408',
    storefront: 'nz',
    title: 'Test Song',
    artist: 'Test Artist',
    album_title: 'Test Album'
  });
  mocks.appleMusicCatalogAlbum.mockResolvedValue({
    album_id: '1440880938',
    storefront: 'nz',
    title: 'Homogenic',
    artist: 'Björk',
    release_date: '1997-09-22T00:00:00Z',
    artwork_url: 'https://example.test/homogenic.jpg',
    audio_variants: ['lossless'],
    tracks: [
      {
        song_id: '1440880967',
        storefront: 'nz',
        album_id: '1440880938',
        title: 'Hunter',
        artist: 'Björk',
        album_title: 'Homogenic',
        album_artist: 'Björk',
        duration_secs: 255,
        track_number: 1,
        disc_number: 1,
        artwork_url: 'https://example.test/homogenic.jpg',
        audio_variants: ['lossless']
      },
      {
        song_id: '1440880976',
        storefront: 'nz',
        album_id: '1440880938',
        title: 'Jóga',
        artist: 'Björk',
        album_title: 'Homogenic',
        album_artist: 'Björk',
        duration_secs: 312,
        track_number: 2,
        disc_number: 1,
        artwork_url: 'https://example.test/homogenic.jpg',
        audio_variants: ['lossless']
      }
    ]
  });
  mocks.playAppleMusicScenario.mockResolvedValue({});
  mocks.appleMusicAlbumPreview.mockResolvedValue({
    confidence: 100,
    safe_to_link: true,
    pairings: []
  });
  mocks.appleMusicAlbumLink.mockResolvedValue({ id: 42, provider: 'apple_music' });
  mocks.appleMusicAlbumUnlink.mockResolvedValue([]);
  mocks.albumPlaySources.mockResolvedValue({ sources: [] });
  for (const name of [
    'launchAppleMusicHelper',
    'authorizeAppleMusic',
    'confirmAppleMusicCapture',
    'pause',
    'pauseZone',
    'resume',
    'resumeZone',
    'next',
    'nextZone',
    'stop',
    'stopZone',
    'seek',
    'seekZone',
    'playAppleMusicSong',
    'controlAppleMusic',
    'startAppleMusicProcessTap',
    'stopAppleMusicProcessTap',
    'shutdownAppleMusicHelper'
  ] as const) {
    mocks[name].mockResolvedValue({});
  }
});

afterEach(cleanup);

describe('AppleMusicMvpPage backend integration harness', () => {
  it('searches Apple Music and plays a selected result on the active zone', async () => {
    const hegelStatus = {
      active_zone_id: 'local-U_107GUN3uT4',
      active_zone_name: 'Hegel H390',
      zone_protocol: 'local_core_audio',
      current_source: null
    };
    mocks.zoneStatus.mockResolvedValue(hegelStatus);

    render(
      <AppleMusicMvpPage activeZoneStatus={hegelStatus} addItemsToQueue={mocks.addItemsToQueue} />
    );
    await screen.findByText('2 · Search and play');

    fireEvent.change(screen.getByLabelText('Search Apple Music albums and songs'), {
      target: { value: 'joga bjork' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Search' }));

    await waitFor(() =>
      expect(mocks.appleMusicCatalogSearch).toHaveBeenCalledWith('joga bjork', 'nz', 10)
    );
    const result = await screen.findByRole('button', {
      name: 'Play Jóga by Björk from Homogenic on Hegel H390'
    });
    const captureRoute = screen.getByLabelText(
      /Allow Fozmo to route native Music\.app playback/i
    );
    expect(captureRoute).toBeChecked();
    expect(captureRoute).toHaveProperty('readOnly', true);
    expect(result).toBeEnabled();
    fireEvent.click(result);

    await waitFor(() =>
      expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
        'local-U_107GUN3uT4',
        expect.objectContaining({
          kind: 'apple_music_track',
          song_id: '1440880976',
          storefront: 'nz',
          title: 'Jóga',
          artist: 'Björk',
          album: 'Homogenic'
        }),
        [],
        true
      )
    );
  });

  it('links an album search result to the normal routed album page', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={defaultActiveZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );
    await screen.findByText('2 · Search and play');

    fireEvent.change(screen.getByLabelText('Search Apple Music albums and songs'), {
      target: { value: 'homogenic bjork' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Search' }));

    const openAlbum = await screen.findByRole('link', {
      name: 'Open Homogenic by Björk'
    });
    expect(openAlbum).toHaveAttribute(
      'href',
      '#/album/1440880938?provider=apple_music&storefront=nz'
    );
    expect(mocks.appleMusicCatalogAlbum).not.toHaveBeenCalled();
  });

  it('adds Apple Music search results to the shared mixed-provider queue', async () => {
    const hegelStatus = {
      active_zone_id: 'local-U_107GUN3uT4',
      active_zone_name: 'Hegel H390',
      zone_protocol: 'local_core_audio',
      current_source: null
    };
    mocks.zoneStatus.mockResolvedValue(hegelStatus);

    render(
      <AppleMusicMvpPage activeZoneStatus={hegelStatus} addItemsToQueue={mocks.addItemsToQueue} />
    );
    await screen.findByText('2 · Search and play');
    fireEvent.change(screen.getByLabelText('Search Apple Music albums and songs'), {
      target: { value: 'joga bjork' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Search' }));
    await screen.findByText('Jóga');
    fireEvent.click(screen.getByRole('button', { name: 'Play Jóga by Björk from Homogenic next' }));

    await waitFor(() => expect(mocks.confirmAppleMusicCapture).toHaveBeenCalledTimes(1));
    expect(mocks.addItemsToQueue).toHaveBeenCalledWith(
      [
        expect.objectContaining({
          title: 'Jóga',
          artist: 'Björk',
          album: 'Homogenic',
          imageUrl: 'https://example.test/joga.jpg',
          resolvedSource: expect.objectContaining({
            kind: 'apple_music_track',
            song_id: '1440880976',
            storefront: 'nz'
          })
        })
      ],
      'next'
    );

    await screen.findByText('Jóga added to play next.');
    fireEvent.click(
      screen.getByRole('button', {
        name: 'Add Jóga by Björk from Homogenic to the end of the queue'
      })
    );
    await waitFor(() => expect(mocks.confirmAppleMusicCapture).toHaveBeenCalledTimes(2));
    expect(mocks.addItemsToQueue).toHaveBeenLastCalledWith(
      [expect.objectContaining({ title: 'Jóga' })],
      'end'
    );
  });

  it('looks up a song, adds it to a scenario, and submits through the router endpoint', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={defaultActiveZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );
    await screen.findByText('awaiting provisioned signing', { exact: false });

    fireEvent.change(screen.getByLabelText('Song ID'), {
      target: { value: '2037093408' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Lookup song' }));
    await screen.findByText('Normalized Apple Music song loaded.');
    fireEvent.click(screen.getByRole('button', { name: 'Add song to scenario' }));

    expect(screen.getByText(/apple music · Test Artist · Test Song/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Play from selected row' }));

    await waitFor(() =>
      expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
        'local-core',
        expect.objectContaining({
          kind: 'apple_music_track',
          song_id: '2037093408'
        }),
        [],
        true
      )
    );
  });

  it('uses normal playback transport instead of the raw helper controls', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={defaultActiveZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );
    await screen.findByText('4 · Normal transport');

    fireEvent.click(screen.getByRole('button', { name: 'Pause', hidden: false }));

    await waitFor(() => expect(mocks.pauseZone).toHaveBeenCalledWith('local-core'));
    expect(mocks.controlAppleMusic).not.toHaveBeenCalled();
  });

  it('submits a canned Local, Apple, Apple, Qobuz queue from the selected row', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={defaultActiveZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );
    await screen.findByText('3 · Mixed queue scenario');

    fireEvent.change(screen.getByLabelText('Local track ID'), {
      target: { value: '11' }
    });
    fireEvent.change(screen.getByLabelText('Qobuz track ID'), {
      target: { value: '22' }
    });
    fireEvent.change(screen.getByLabelText('Song ID'), {
      target: { value: 'apple-33' }
    });
    fireEvent.change(screen.getByLabelText('Canned mixed queue scenario'), {
      target: { value: 'mixed_run' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Load canned scenario' }));
    fireEvent.click(screen.getByRole('button', { name: 'Play from selected row' }));

    await waitFor(() =>
      expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
        'local-core',
        expect.objectContaining({ kind: 'local_track', track_id: 11 }),
        [
          expect.objectContaining({ kind: 'apple_music_track', song_id: 'apple-33' }),
          expect.objectContaining({ kind: 'apple_music_track', song_id: 'apple-33' }),
          expect.objectContaining({ kind: 'qobuz_track', track_id: 22 })
        ],
        true
      )
    );
  });

  it('disables scenario playback on a non-local zone', async () => {
    const remoteStatus = {
      active_zone_id: 'remote-agent-1',
      active_zone_name: 'Remote Mac',
      zone_protocol: 'remote_agent',
      current_source: null
    };
    mocks.zoneStatus.mockResolvedValue(remoteStatus);

    render(
      <AppleMusicMvpPage activeZoneStatus={remoteStatus} addItemsToQueue={mocks.addItemsToQueue} />
    );
    await screen.findByText(/Remote Mac · local output required/);
    fireEvent.click(screen.getByRole('button', { name: 'Load canned scenario' }));

    expect(screen.getByRole('button', { name: 'Play from selected row' })).toBeDisabled();
    expect(mocks.playAppleMusicScenario).not.toHaveBeenCalled();
  });

  it('renders provisioning, session revision, helper events, and structured failures', async () => {
    mocks.appleMusicStatus.mockResolvedValue({
      helper_present: true,
      helper_version: '0.2.0',
      helper_musickit_entitled: true,
      authorization: 'authorized',
      can_play_catalog_content: true,
      playback_state: 'paused',
      active_audio_variant: 'highResolutionLossless',
      state: 'paused',
      process_tap: { state: 'running', metrics: { ring_overruns: 0 } },
      playback_session: {
        queue_revision: 12,
        current_segment_index: 1,
        playback_state: 'paused'
      },
      recent_events: [{ event_type: 'entry_changed', queue_revision: 12, segment_index: 1 }],
      last_error: {
        code: 'music_authorization_denied',
        message: 'Authorization revoked during playback.'
      }
    });

    render(
      <AppleMusicMvpPage
        activeZoneStatus={defaultActiveZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );

    await screen.findByText('provisioned and enabled');
    expect(screen.getByText('authorized')).toBeInTheDocument();
    expect(screen.getByText('available')).toBeInTheDocument();
    expect(screen.getByText('Hi-Res Lossless')).toBeInTheDocument();
    expect(screen.getAllByText(/"queue_revision": 12/)).toHaveLength(2);
    expect(screen.getByText(/"event_type": "entry_changed"/)).toBeInTheDocument();
    expect(screen.getByText(/Authorization revoked during playback/)).toBeInTheDocument();
  });

  it('previews and links an Apple Music album version', async () => {
    render(
      <AppleMusicMvpPage
        activeZoneStatus={defaultActiveZoneStatus}
        addItemsToQueue={mocks.addItemsToQueue}
      />
    );
    await screen.findByText('6 · Album-version harness');

    fireEvent.change(screen.getByLabelText('Local Fozmo album ID'), {
      target: { value: '7' }
    });
    fireEvent.change(screen.getByLabelText('Apple Music album ID'), {
      target: { value: 'album-1' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Preview match' }));

    await screen.findByText('Apple Music album-version match preview loaded.');
    expect(screen.getByText(/"safe_to_link": true/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Link as version' }));

    await waitFor(() =>
      expect(mocks.appleMusicAlbumLink).toHaveBeenCalledWith('7', 'album-1', 'nz')
    );
    expect(await screen.findByDisplayValue('42')).toBeInTheDocument();
  });
});
