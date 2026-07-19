// @vitest-environment jsdom

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  clearTransportPending,
  setTransportPending,
  usePlaybackControlSnapshot
} from '../model/playbackControlStore';
import type { PlaybackStatus } from '../model/playbackStore';
import { usePersistentSeekPosition } from './usePersistentSeekPosition';

const status: PlaybackStatus = {
  state: 'Playing',
  file_name: 'track.flac',
  track_title: 'Track',
  position_secs: 15,
  duration_secs: 243,
  transport_pending: 'none'
};

function PersistentSeekHarness() {
  const { transportPending } = usePlaybackControlSnapshot();
  const position = usePersistentSeekPosition(status, 15, transportPending);
  return <output>{Math.floor(position)}</output>;
}

describe('usePersistentSeekPosition', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-07-19T00:00:00Z'));
    setTransportPending({
      kind: 'seek',
      requestedAt: Date.now(),
      expectedPosition: 150
    });
  });

  afterEach(() => {
    cleanup();
    clearTransportPending();
    vi.useRealTimers();
  });

  it('survives player controls unmounting before authoritative status catches up', () => {
    const firstRender = render(<PersistentSeekHarness />);
    expect(screen.getByText('150')).toBeInTheDocument();

    firstRender.unmount();
    render(<PersistentSeekHarness />);
    expect(screen.getByText('150')).toBeInTheDocument();
  });
});
