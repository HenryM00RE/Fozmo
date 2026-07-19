// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { PlaybackChromeState } from '../model/playbackChromeState';
import { MobileNowPlayingSheet } from './MobileNowPlayingSheet';

vi.mock('../model/playbackControlStore', () => ({
  usePlaybackControlSnapshot: () => ({ pendingArtSrc: null, playbackLoading: false })
}));

vi.mock('./NowPlayingQueueIsland', () => ({
  NowPlayingQueueIsland: () => <div>Queue content</div>
}));

vi.mock('./PlaybackControlsIsland', () => ({
  PlaybackControlsIsland: () => null
}));

vi.mock('./SignalPopover', () => ({
  SignalPopover: () => null
}));

vi.mock('./VolumeControl', () => ({
  VolumeControl: () => null
}));

vi.mock('./ZonePicker', () => ({
  ZonePicker: () => null
}));

function MobileNowPlayingSheetHarness() {
  const [nowPlayingOpen, setNowPlayingOpen] = useState(true);
  const [, setRenderCount] = useState(0);
  const playbackChrome: PlaybackChromeState = {
    activeZoneId: '',
    albums: [],
    nowPlayingOpen,
    onClearQueue: vi.fn(),
    onOpenAlbum: vi.fn(),
    onSelectZone: vi.fn().mockResolvedValue(undefined),
    onShuffleQueue: vi.fn(),
    onToggleLoop: vi.fn(),
    queue: { kind: null, cursor: -1, items: [], loopMode: 'off' },
    setNowPlayingOpen,
    setSignalOpen: vi.fn(),
    signalOpen: false,
    status: {},
    zones: []
  };

  return (
    <>
      <button type="button" onClick={() => setNowPlayingOpen(true)}>
        Open player
      </button>
      <button type="button" onClick={() => setRenderCount((count) => count + 1)}>
        Rerender
      </button>
      <MobileNowPlayingSheet
        onOpenArtist={vi.fn()}
        playbackChrome={playbackChrome}
        playbackPosition={0}
      />
    </>
  );
}

describe('MobileNowPlayingSheet', () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('returns to Now Playing after closing from Queue and reopening', () => {
    vi.useFakeTimers();
    render(<MobileNowPlayingSheetHarness />);

    const nowPlayingTab = screen.getByRole('button', { name: 'Now Playing' });
    const queueTab = screen.getByRole('button', { name: 'Queue' });
    expect(nowPlayingTab).toHaveAttribute('aria-pressed', 'true');

    fireEvent.click(queueTab);
    expect(queueTab).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByText('Queue content')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Rerender' }));
    expect(queueTab).toHaveAttribute('aria-pressed', 'true');

    fireEvent.click(screen.getByRole('button', { name: 'Close now playing' }));
    act(() => vi.advanceTimersByTime(420));
    expect(screen.queryByRole('region', { name: 'Now playing' })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Open player' }));
    expect(screen.getByRole('button', { name: 'Now Playing' })).toHaveAttribute(
      'aria-pressed',
      'true'
    );
    expect(screen.getByRole('button', { name: 'Queue' })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.queryByText('Queue content')).not.toBeInTheDocument();
  });
});
