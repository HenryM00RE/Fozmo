// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MetaBrainzSettingsPage } from './MetaBrainzSettingsPage';

const mocks = vi.hoisted(() => ({
  autoMetaJob: vi.fn(),
  autoMetaPause: vi.fn(),
  autoMetaResume: vi.fn(),
  autoMetaStatus: vi.fn(),
  autoMetaStop: vi.fn(),
  lastfmStatus: vi.fn(),
  saveLastfmSettings: vi.fn(),
  saveQobuzSettings: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({ endpoints: mocks }));

const completedProgress = {
  job_id: 12,
  status: 'completed',
  running: false,
  processed: 4,
  total: 4,
  musicbrainz_matched: 3,
  qobuz_matched: 2,
  no_proper_match: 1,
  errors: 1,
  link_qobuz: true,
  updated_at: 1_788_000_000
};

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.autoMetaStatus.mockResolvedValue(completedProgress);
  mocks.autoMetaJob.mockResolvedValue({ ...completedProgress, status: 'running', running: true });
  mocks.lastfmStatus.mockResolvedValue({ configured: false, radio_enabled: false });
});

afterEach(cleanup);

describe('MetaBrainzSettingsPage AutoMetadata controls', () => {
  it('shows only a new-run action after a job completes', async () => {
    render(
      <div className="react-app">
        <MetaBrainzSettingsPage
          onRefresh={vi.fn().mockResolvedValue(undefined)}
          qobuzStatus={{ logged_in: true, radio_enabled: true }}
        />
      </div>
    );

    await waitFor(() => expect(mocks.autoMetaStatus).toHaveBeenCalled());
    fireEvent.click(screen.getByRole('button', { name: 'AutoMetadata' }));

    const startButton = await screen.findByRole('button', { name: 'Start new run' });
    expect(startButton).toBeInTheDocument();
    expect(startButton.querySelector('.lucide-refresh-cw')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Close' }).querySelector('.lucide-x')
    ).toBeInTheDocument();
    expect(screen.queryByText('Batch metadata tagging')).not.toBeInTheDocument();
    expect(
      screen.getByText(/assigns MusicBrainz metadata to each local album/)
    ).toBeInTheDocument();
    expect(screen.queryByText('Current version')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Start remaining' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Resume' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry errors' })).not.toBeInTheDocument();

    fireEvent.click(startButton);
    await waitFor(() =>
      expect(mocks.autoMetaJob).toHaveBeenCalledWith({
        link_qobuz: true,
        mode: 'remaining'
      })
    );
  });
});
