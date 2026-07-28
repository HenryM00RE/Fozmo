// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { QobuzSettingsPage } from './QobuzSettingsPage';

const mocks = vi.hoisted(() => ({
  appleMusicStatus: vi.fn(),
  lastfmStatus: vi.fn(),
  launchAppleMusicHelper: vi.fn(),
  qobuzLogout: vi.fn(),
  saveLastfmSettings: vi.fn(),
  shutdownAppleMusicHelper: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({ endpoints: mocks }));

const stoppedStatus = {
  helper_present: true,
  helper_pid: null,
  state: 'stopped'
};

const runningStatus = {
  helper_present: true,
  helper_pid: 123,
  state: 'ready'
};

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.appleMusicStatus.mockResolvedValue(stoppedStatus);
  mocks.lastfmStatus.mockResolvedValue({ configured: false });
  mocks.launchAppleMusicHelper.mockResolvedValue(runningStatus);
  mocks.shutdownAppleMusicHelper.mockResolvedValue(stoppedStatus);
});

afterEach(cleanup);

describe('QobuzSettingsPage Apple Music service', () => {
  it('enables and disables the experimental Apple Music helper from its modal', async () => {
    const onRefresh = vi.fn().mockResolvedValue(undefined);
    render(
      <QobuzSettingsPage
        appleMusicAvailable
        onRefresh={onRefresh}
        qobuzAvailable
        qobuzStatus={{ logged_in: false }}
      />
    );

    expect(await screen.findByText('Experimental helper disabled.')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Apple Music settings' }));
    fireEvent.click(screen.getByRole('button', { name: 'Enable' }));

    await waitFor(() => expect(mocks.launchAppleMusicHelper).toHaveBeenCalledTimes(1));
    expect(await screen.findByText('Apple Music helper enabled.')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Disable' }));

    await waitFor(() => expect(mocks.shutdownAppleMusicHelper).toHaveBeenCalledTimes(1));
    expect(await screen.findByText('Apple Music helper disabled.')).toBeInTheDocument();
  });

  it('hides Apple Music when the capability is unavailable', async () => {
    render(
      <QobuzSettingsPage
        appleMusicAvailable={false}
        onRefresh={vi.fn().mockResolvedValue(undefined)}
        qobuzAvailable
        qobuzStatus={{ logged_in: false }}
      />
    );

    await screen.findByText('Last.fm');
    expect(screen.queryByText('Apple Music')).not.toBeInTheDocument();
    expect(mocks.appleMusicStatus).not.toHaveBeenCalled();
  });
});
