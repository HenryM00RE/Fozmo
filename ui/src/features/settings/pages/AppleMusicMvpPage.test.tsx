// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppleMusicMvpPage } from './AppleMusicMvpPage';

const mocks = vi.hoisted(() => ({
  appleMusicStatus: vi.fn(),
  status: vi.fn(),
  nowPlayingQueue: vi.fn(),
  appleMusicCatalogSong: vi.fn(),
  appleMusicCatalogAlbum: vi.fn(),
  playAppleMusicScenario: vi.fn(),
  appleMusicAlbumPreview: vi.fn(),
  appleMusicAlbumLink: vi.fn(),
  appleMusicAlbumUnlink: vi.fn(),
  albumPlaySources: vi.fn(),
  launchAppleMusicHelper: vi.fn(),
  authorizeAppleMusic: vi.fn(),
  pause: vi.fn(),
  resume: vi.fn(),
  next: vi.fn(),
  stop: vi.fn(),
  seek: vi.fn(),
  playAppleMusicSong: vi.fn(),
  controlAppleMusic: vi.fn(),
  startAppleMusicProcessTap: vi.fn(),
  stopAppleMusicProcessTap: vi.fn(),
  shutdownAppleMusicHelper: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({ endpoints: mocks }));

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
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
  mocks.status.mockResolvedValue({
    active_zone_id: 'local-core',
    active_zone_name: 'Mac output',
    zone_protocol: 'local_core_audio',
    current_source: null
  });
  mocks.nowPlayingQueue.mockResolvedValue({
    current_source: null,
    queued_sources: []
  });
  mocks.appleMusicCatalogSong.mockResolvedValue({
    song_id: '2037093408',
    storefront: 'nz',
    title: 'Test Song',
    artist: 'Test Artist',
    album_title: 'Test Album'
  });
  mocks.appleMusicCatalogAlbum.mockResolvedValue({
    album_id: 'album-1',
    storefront: 'nz',
    title: 'Test Album',
    artist: 'Test Artist',
    tracks: []
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
    'pause',
    'resume',
    'next',
    'stop',
    'seek',
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
  it('looks up a song, adds it to a scenario, and submits through the router endpoint', async () => {
    render(<AppleMusicMvpPage />);
    await screen.findByText('awaiting provisioned signing', { exact: false });

    fireEvent.change(screen.getByLabelText('Song ID'), {
      target: { value: '2037093408' }
    });
    fireEvent.click(screen.getByRole('button', { name: 'Lookup song' }));
    await screen.findByText('Normalized Apple Music song loaded.');
    fireEvent.click(screen.getByRole('button', { name: 'Add song to scenario' }));

    expect(screen.getByText(/apple music · Test Artist · Test Song/i)).toBeInTheDocument();
    fireEvent.click(
      screen.getByLabelText(/Allow Fozmo to capture MusicKit's isolated audio renderer/i)
    );
    fireEvent.click(screen.getByRole('button', { name: 'Play from selected row' }));

    await waitFor(() =>
      expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
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
    render(<AppleMusicMvpPage />);
    await screen.findByText('4 · Normal transport');

    fireEvent.click(screen.getByRole('button', { name: 'Pause', hidden: false }));

    await waitFor(() => expect(mocks.pause).toHaveBeenCalledOnce());
    expect(mocks.controlAppleMusic).not.toHaveBeenCalled();
  });

  it('submits a canned Local, Apple, Apple, Qobuz queue from the selected row', async () => {
    render(<AppleMusicMvpPage />);
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
    fireEvent.click(
      screen.getByLabelText(/Allow Fozmo to capture MusicKit's isolated audio renderer/i)
    );
    fireEvent.click(screen.getByRole('button', { name: 'Play from selected row' }));

    await waitFor(() =>
      expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
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
    mocks.status.mockResolvedValue({
      active_zone_id: 'remote-agent-1',
      active_zone_name: 'Remote Mac',
      zone_protocol: 'remote_agent',
      current_source: null
    });

    render(<AppleMusicMvpPage />);
    await screen.findByText(/Remote Mac · local output required/);
    fireEvent.click(screen.getByRole('button', { name: 'Load canned scenario' }));
    fireEvent.click(
      screen.getByLabelText(/Allow Fozmo to capture MusicKit's isolated audio renderer/i)
    );

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

    render(<AppleMusicMvpPage />);

    await screen.findByText('provisioned and enabled');
    expect(screen.getByText('authorized')).toBeInTheDocument();
    expect(screen.getByText('available')).toBeInTheDocument();
    expect(screen.getAllByText(/"queue_revision": 12/)).toHaveLength(2);
    expect(screen.getByText(/"event_type": "entry_changed"/)).toBeInTheDocument();
    expect(screen.getByText(/Authorization revoked during playback/)).toBeInTheDocument();
  });

  it('previews and links an Apple Music album version', async () => {
    render(<AppleMusicMvpPage />);
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
