import { useEffect, useMemo, useRef, useState } from 'react';
import type { PlaybackStatus } from '../model/playbackStore';

const SOFT_CORRECTION_SECONDS = 0.18;
const HARD_CORRECTION_SECONDS = 1.25;

interface PositionAnchor {
  trackKey: string;
  state: string | undefined;
  duration: number;
  position: number;
  anchoredAt: number;
}

function playbackPosition(status: PlaybackStatus) {
  return Math.max(0, Number(status.position_secs) || 0);
}

function playbackDuration(status: PlaybackStatus) {
  return Math.max(0, Number(status.duration_secs) || 0);
}

function projectedPosition(anchor: PositionAnchor, now: number) {
  if (anchor.state !== 'Playing' || anchor.duration <= 0) {
    return anchor.position;
  }
  return Math.min(anchor.duration, anchor.position + (now - anchor.anchoredAt) / 1000);
}

function clampPosition(position: number, duration: number) {
  return Math.max(0, duration > 0 ? Math.min(duration, position) : position);
}

export function useInterpolatedPosition(status: PlaybackStatus, isScrubbing = false) {
  const basePosition = playbackPosition(status);
  const duration = playbackDuration(status);
  const playing = status.state === 'Playing';
  const trackKey = useMemo(
    () =>
      [
        status.active_zone_id || '',
        status.file_name || '',
        status.track_title || '',
        duration.toFixed(3)
      ].join('|'),
    [duration, status.active_zone_id, status.file_name, status.track_title]
  );
  const anchorRef = useRef<PositionAnchor>({
    trackKey,
    state: status.state,
    duration,
    position: clampPosition(basePosition, duration),
    anchoredAt: performance.now()
  });
  const [position, setPosition] = useState(() => clampPosition(basePosition, duration));

  useEffect(() => {
    if (isScrubbing) return;

    const now = performance.now();
    const measured = clampPosition(basePosition, duration);
    const anchor = anchorRef.current;
    const trackChanged = anchor.trackKey !== trackKey;
    const stateChanged = anchor.state !== status.state;

    if (trackChanged || stateChanged || !playing || duration <= 0) {
      anchorRef.current = {
        trackKey,
        state: status.state,
        duration,
        position: measured,
        anchoredAt: now
      };
      setPosition(measured);
      return;
    }

    const projected = projectedPosition(anchor, now);
    const delta = measured - projected;

    if (Math.abs(delta) >= HARD_CORRECTION_SECONDS) {
      anchorRef.current = {
        trackKey,
        state: status.state,
        duration,
        position: measured,
        anchoredAt: now
      };
      setPosition(measured);
      return;
    }

    if (Math.abs(delta) >= SOFT_CORRECTION_SECONDS) {
      anchorRef.current = {
        trackKey,
        state: status.state,
        duration,
        position: clampPosition(projected + delta * 0.08, duration),
        anchoredAt: now
      };
    }
  }, [basePosition, duration, isScrubbing, playing, status.state, trackKey]);

  useEffect(() => {
    if (isScrubbing) return undefined;

    const update = () => setPosition(projectedPosition(anchorRef.current, performance.now()));

    if (!playing || duration <= 0) {
      update();
      return undefined;
    }

    let rafId = 0;
    const tick = () => {
      update();
      rafId = window.requestAnimationFrame(tick);
    };

    rafId = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(rafId);
  }, [duration, isScrubbing, playing, status.state, trackKey]);

  return clampPosition(position, duration);
}
