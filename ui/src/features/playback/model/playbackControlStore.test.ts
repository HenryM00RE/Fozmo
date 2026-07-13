import { describe, expect, it } from 'vitest';
import { backendPlayControlIsLoading, isSettledPlaybackState } from './playbackControlStore';

describe('playback control loading state', () => {
  it('treats renderer terminal states as settled even when a stale pending flag remains', () => {
    for (const state of ['Playing', 'Paused', 'Stopped']) {
      expect(isSettledPlaybackState(state)).toBe(true);
      expect(backendPlayControlIsLoading(state, 'loading')).toBe(false);
    }
  });

  it('shows loading for actual transitions but leaves seek progress on the slider', () => {
    expect(backendPlayControlIsLoading('Transitioning', 'loading')).toBe(true);
    expect(backendPlayControlIsLoading('Starting', 'none')).toBe(true);
    expect(backendPlayControlIsLoading('Unknown', 'loading')).toBe(true);
    expect(backendPlayControlIsLoading('Unknown', 'seeking')).toBe(false);
  });
});
