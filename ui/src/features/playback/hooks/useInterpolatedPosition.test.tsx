// @vitest-environment jsdom

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PlaybackStatus } from '../model/playbackStore';
import { useInterpolatedPosition } from './useInterpolatedPosition';

function PositionHarness({ status }: { status: PlaybackStatus }) {
  const position = useInterpolatedPosition(status);
  return <output>{position.toFixed(1)}</output>;
}

describe('useInterpolatedPosition mobile power use', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal(
      'matchMedia',
      vi.fn().mockReturnValue({
        matches: true,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn()
      })
    );
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('updates a playing position once per second instead of every animation frame', () => {
    const requestAnimationFrame = vi.spyOn(window, 'requestAnimationFrame');
    render(
      <PositionHarness
        status={{
          state: 'Playing',
          file_name: 'track.flac',
          position_secs: 10,
          duration_secs: 100
        }}
      />
    );

    expect(screen.getByText('10.0')).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(999));
    expect(screen.getByText('10.0')).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.getByText('11.0')).toBeInTheDocument();
    expect(requestAnimationFrame).not.toHaveBeenCalled();
  });
});
