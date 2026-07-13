import type { CSSProperties } from 'react';
import { useEffect, useId, useMemo, useState } from 'react';
import { formatTime } from '../../../shared/lib/format';
import { ShuffleIcon } from '../../../shared/ui/ShuffleIcon';
import { useNowPlayingQueueSnapshot } from '../model/nowPlayingQueueStore';
import {
  backendPlayControlIsLoading,
  clearTransportPending,
  getPlaybackControlActions,
  isSettledPlaybackState,
  type PlaybackControlAction,
  setTransportPending,
  usePlaybackControlSnapshot
} from '../model/playbackControlStore';
import type { PlaybackStatus } from '../model/playbackStore';
import { refreshPlaybackStatus } from '../model/playbackStore';

const SEEK_PENDING_TIMEOUT_MS = 25_000;

function playbackDuration(status: PlaybackStatus) {
  return Math.max(0, Number(status.duration_secs) || 0);
}

function playbackPosition(status: PlaybackStatus) {
  return Math.max(0, Number(status.position_secs) || 0);
}

function playbackTrackKey(status: PlaybackStatus, duration: number) {
  return [
    status.active_zone_id || '',
    status.file_name || '',
    status.track_title || '',
    duration.toFixed(3)
  ].join('|');
}

interface OptimisticSeek {
  seconds: number;
  trackKey: string;
  state: string | undefined;
  anchoredAt: number;
}

function TransportIconGradient({ id }: { id: string }) {
  return (
    <defs>
      <linearGradient id={id} x1="30" y1="26" x2="72" y2="76" gradientUnits="userSpaceOnUse">
        <stop offset="0" stopColor="var(--transport-icon-face-start)" />
        <stop offset="0.5" stopColor="var(--transport-icon-face-mid)" />
        <stop offset="1" stopColor="var(--transport-icon-face-end)" />
      </linearGradient>
    </defs>
  );
}

export function PlayPauseIcon({ state }: { state: string | undefined }) {
  const gradientId = `transport-gradient-${useId().replace(/:/g, '')}`;

  if (state === 'Playing') {
    return (
      <svg
        viewBox="0 0 100 100"
        className="pause-icon"
        aria-hidden="true"
        shapeRendering="geometricPrecision"
      >
        <TransportIconGradient id={gradientId} />
        <g className="transport-glyph-face" fill={`url(#${gradientId})`}>
          <rect x="28" y="24" width="16" height="52" rx="8" />
          <rect x="56" y="24" width="16" height="52" rx="8" />
        </g>
      </svg>
    );
  }

  return (
    <svg
      viewBox="0 0 100 100"
      className="play-icon"
      aria-hidden="true"
      shapeRendering="geometricPrecision"
    >
      <TransportIconGradient id={gradientId} />
      <path
        className="transport-glyph-face"
        d="M 39 32 C 36.2 33.3 35 35.7 35 39 L 35 61 C 35 64.3 36.2 66.7 39 68 C 41.4 69.1 43.5 68.2 46.2 66.6 L 66.4 54.4 C 71.2 51.5 71.2 48.5 66.4 45.6 L 46.2 33.4 C 43.5 31.8 41.4 30.9 39 32 Z"
        fill={`url(#${gradientId})`}
      />
    </svg>
  );
}

export function SkipIcon({ direction }: { direction: 'previous' | 'next' }) {
  const gradientId = `transport-gradient-${useId().replace(/:/g, '')}`;
  const paths =
    direction === 'previous'
      ? [
          'M 39 32 C 41.8 33.3 43 35.7 43 39 L 43 61 C 43 64.3 41.8 66.7 39 68 C 36.6 69.1 34.5 68.2 31.8 66.6 L 12.6 54.4 C 7.8 51.5 7.8 48.5 12.6 45.6 L 31.8 33.4 C 34.5 31.8 36.6 30.9 39 32 Z',
          'M 75 32 C 77.8 33.3 79 35.7 79 39 L 79 61 C 79 64.3 77.8 66.7 75 68 C 72.6 69.1 70.5 68.2 67.8 66.6 L 48.6 54.4 C 43.8 51.5 43.8 48.5 48.6 45.6 L 67.8 33.4 C 70.5 31.8 72.6 30.9 75 32 Z'
        ]
      : [
          'M 25 32 C 22.2 33.3 21 35.7 21 39 L 21 61 C 21 64.3 22.2 66.7 25 68 C 27.4 69.1 29.5 68.2 32.2 66.6 L 51.4 54.4 C 56.2 51.5 56.2 48.5 51.4 45.6 L 32.2 33.4 C 29.5 31.8 27.4 30.9 25 32 Z',
          'M 61 32 C 58.2 33.3 57 35.7 57 39 L 57 61 C 57 64.3 58.2 66.7 61 68 C 63.4 69.1 65.5 68.2 68.2 66.6 L 87.4 54.4 C 92.2 51.5 92.2 48.5 87.4 45.6 L 68.2 33.4 C 65.5 31.8 63.4 30.9 61 32 Z'
        ];

  return (
    <svg
      viewBox="0 0 100 100"
      className="skip-icon"
      aria-hidden="true"
      shapeRendering="geometricPrecision"
    >
      <TransportIconGradient id={gradientId} />
      {paths.map((path) => (
        <path
          key={`face-${path}`}
          className="transport-glyph-face"
          d={path}
          fill={`url(#${gradientId})`}
        />
      ))}
    </svg>
  );
}

export function PlayLoadingSpinner() {
  return <span className="play-loading-spinner" aria-hidden="true" />;
}

function LoopIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        fill="currentColor"
        d="M 22.44 4.69 L 22.35 4.48 L 19.43 1.71 L 18.97 1.50 L 18.70 1.50 L 18.37 1.62 L 18.16 1.80 L 17.98 2.13 L 17.92 3.82 L 5.33 3.82 L 4.51 3.97 L 3.76 4.27 L 2.89 4.87 L 2.34 5.48 L 1.89 6.23 L 1.62 7.01 L 1.50 7.86 L 1.50 10.21 L 1.56 10.48 L 1.89 10.96 L 2.34 11.17 L 2.92 11.14 L 3.31 10.90 L 3.55 10.54 L 3.64 10.21 L 3.64 7.83 L 3.76 7.28 L 4.00 6.83 L 4.39 6.41 L 5.12 6.05 L 5.45 5.99 L 17.92 5.99 L 17.98 7.83 L 18.28 8.28 L 18.58 8.43 L 18.88 8.46 L 19.37 8.28 L 22.29 5.51 L 22.47 5.12 Z M 22.47 11.17 L 22.26 10.75 L 21.99 10.51 L 21.69 10.39 L 21.36 10.36 L 20.99 10.45 L 20.57 10.78 L 20.36 11.29 L 20.36 13.58 L 20.21 14.21 L 19.76 14.88 L 19.31 15.21 L 18.67 15.42 L 6.11 15.45 L 6.08 14.06 L 5.96 13.64 L 5.66 13.31 L 5.48 13.22 L 4.96 13.19 L 4.60 13.37 L 1.71 16.05 L 1.53 16.47 L 1.56 16.84 L 1.77 17.20 L 4.51 19.67 L 4.84 19.88 L 5.09 19.94 L 5.63 19.82 L 5.87 19.61 L 6.02 19.34 L 6.11 17.65 L 18.46 17.65 L 19.55 17.47 L 20.21 17.20 L 21.05 16.62 L 21.69 15.93 L 22.14 15.15 L 22.35 14.55 L 22.47 13.88 Z"
      />
    </svg>
  );
}

export function PlaybackControlsIsland({
  status,
  position
}: {
  status: PlaybackStatus;
  position: number;
}) {
  const { playbackLoading, transportPending } = usePlaybackControlSnapshot();
  const queueSnapshot = useNowPlayingQueueSnapshot();
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [isScrubbing, setIsScrubbing] = useState(false);
  const backendPending = status.transport_pending && status.transport_pending !== 'none';
  const transportStateLoading = backendPlayControlIsLoading(status.state, status.transport_pending);
  const seekPending = status.transport_pending === 'seeking' || transportPending?.kind === 'seek';
  const duration = playbackDuration(status);
  const trackKey = useMemo(
    () => playbackTrackKey(status, duration),
    [duration, status.active_zone_id, status.file_name, status.track_title]
  );
  const [scrubValue, setScrubValue] = useState(position);
  const [optimisticSeek, setOptimisticSeek] = useState<OptimisticSeek | null>(null);
  const [optimisticFrame, setOptimisticFrame] = useState(0);
  const optimisticPosition = useMemo(() => {
    if (!optimisticSeek || optimisticSeek.trackKey !== trackKey) return null;
    if (optimisticSeek.state !== 'Playing' || duration <= 0) return optimisticSeek.seconds;
    return Math.min(
      duration,
      optimisticSeek.seconds + (performance.now() - optimisticSeek.anchoredAt) / 1000
    );
  }, [duration, optimisticFrame, optimisticSeek, trackKey]);
  const backendPendingPosition =
    status.transport_pending === 'seeking' ? status.transport_pending_position_secs : null;
  const displayedPosition = isScrubbing
    ? scrubValue
    : (optimisticPosition ?? backendPendingPosition ?? position);
  const percent =
    duration > 0 ? Math.max(0, Math.min(100, (displayedPosition / duration) * 100)) : 0;
  const playLabel =
    status.state === 'Playing' ? 'Pause' : status.state === 'Paused' ? 'Resume' : 'Play';
  const nextTrackLoading =
    busyAction === 'next' ||
    transportPending?.kind === 'next' ||
    transportPending?.kind === 'auto-advance';
  const settledTransportState = isSettledPlaybackState(status.state);
  const playButtonLoading =
    (playbackLoading && !settledTransportState) ||
    transportStateLoading ||
    busyAction === 'playPause' ||
    nextTrackLoading;
  const playButtonLabel = nextTrackLoading
    ? 'Finding next track'
    : playButtonLoading
      ? 'Loading'
      : playLabel;
  const loopActive = queueSnapshot.loopMode !== 'off';
  const loopLabel = loopActive ? 'Loop on' : 'Loop';

  useEffect(() => {
    if (!isScrubbing) setScrubValue(optimisticPosition ?? position);
  }, [isScrubbing, optimisticPosition, position]);

  useEffect(() => {
    if (!transportPending && !backendPending) return undefined;
    refreshPlaybackStatus({ force: true }).catch(() => undefined);
    const intervalId = window.setInterval(() => {
      refreshPlaybackStatus({ force: true }).catch(() => undefined);
    }, 1200);
    return () => window.clearInterval(intervalId);
  }, [backendPending, transportPending]);

  useEffect(() => {
    if (!optimisticSeek || optimisticSeek.state !== 'Playing') return undefined;
    let rafId = 0;
    const tick = () => {
      setOptimisticFrame((frame) => frame + 1);
      rafId = window.requestAnimationFrame(tick);
    };
    rafId = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(rafId);
  }, [optimisticSeek]);

  useEffect(() => {
    if (!optimisticSeek) return undefined;
    if (optimisticSeek.trackKey !== trackKey || duration <= 0) {
      setOptimisticSeek(null);
      clearTransportPending('seek');
      return undefined;
    }

    const rawPosition = Math.max(
      0,
      duration > 0 ? Math.min(duration, playbackPosition(status)) : playbackPosition(status)
    );
    const elapsed =
      optimisticSeek.state === 'Playing'
        ? (performance.now() - optimisticSeek.anchoredAt) / 1000
        : 0;
    const expectedPosition = Math.min(duration, optimisticSeek.seconds + elapsed);
    const backendStillSeeking = status.transport_pending === 'seeking';
    if (!backendStillSeeking && Math.abs(rawPosition - expectedPosition) < 0.75) {
      setOptimisticSeek(null);
      clearTransportPending('seek');
      return undefined;
    }

    const timeoutId = window.setTimeout(() => {
      setOptimisticSeek(null);
      clearTransportPending('seek');
      refreshPlaybackStatus({ force: true }).catch(() => undefined);
    }, SEEK_PENDING_TIMEOUT_MS);
    return () => window.clearTimeout(timeoutId);
  }, [duration, optimisticSeek, status, trackKey]);

  const sliderStyle = useMemo(
    () =>
      ({
        '--slider-fill': `${percent}%`
      }) as CSSProperties,
    [percent]
  );

  const runAction = (name: string, action: PlaybackControlAction | undefined) => {
    if (!action || busyAction) return;
    setBusyAction(name);
    const clearBusyAction = () => {
      window.setTimeout(() => {
        setBusyAction((current) => (current === name ? null : current));
      }, 120);
    };
    try {
      const result = action();
      if (result && typeof result.finally === 'function') {
        result.finally(clearBusyAction);
      } else {
        window.setTimeout(clearBusyAction, 350);
      }
    } catch (error) {
      clearBusyAction();
      throw error;
    }
  };

  const commitSeek = (seconds: number) => {
    const clamped = Math.max(0, duration > 0 ? Math.min(duration, seconds) : seconds);
    setScrubValue(clamped);
    setOptimisticSeek({
      seconds: clamped,
      trackKey,
      state: status.state,
      anchoredAt: performance.now()
    });
    setTransportPending({
      kind: 'seek',
      requestedAt: Date.now(),
      expectedPosition: clamped,
      expectedTrackKey: trackKey
    });
    getPlaybackControlActions().seek?.(clamped);
  };

  return (
    <section className="react-playback-controls" aria-label="Playback controls">
      <div className="react-transport-controls">
        <button
          className="react-transport-side-button"
          type="button"
          title="Shuffle upcoming"
          aria-label="Shuffle upcoming"
          disabled={busyAction === 'shuffle'}
          onClick={() => runAction('shuffle', getPlaybackControlActions().shuffle)}
        >
          <ShuffleIcon />
        </button>
        <div className="react-transport-main">
          <button
            className="round-btn transport-skip-button"
            type="button"
            title="Previous"
            aria-label="Previous"
            disabled={busyAction === 'previous'}
            onClick={() => runAction('previous', getPlaybackControlActions().previous)}
          >
            <SkipIcon direction="previous" />
          </button>
          <button
            className={`round-btn play transport-play-button${playButtonLoading ? ' is-loading' : ''}`}
            type="button"
            title={playButtonLabel}
            aria-label={playButtonLabel}
            aria-busy={playButtonLoading ? 'true' : undefined}
            disabled={playButtonLoading}
            onClick={() => runAction('playPause', getPlaybackControlActions().playPause)}
          >
            {playButtonLoading ? <PlayLoadingSpinner /> : <PlayPauseIcon state={status.state} />}
          </button>
          <button
            className="round-btn transport-skip-button"
            type="button"
            title="Next"
            aria-label="Next"
            disabled={busyAction === 'next'}
            onClick={() => runAction('next', getPlaybackControlActions().next)}
          >
            <SkipIcon direction="next" />
          </button>
        </div>
        <button
          className={`react-transport-side-button${loopActive ? ' is-active' : ''}`}
          type="button"
          title={loopLabel}
          aria-label={loopLabel}
          aria-pressed={loopActive ? 'true' : 'false'}
          disabled={busyAction === 'loop'}
          onClick={() => runAction('loop', getPlaybackControlActions().toggleLoop)}
        >
          <LoopIcon />
        </button>
      </div>
      <div className="react-progress-stack">
        <span>{formatTime(displayedPosition)}</span>
        <div
          className={`seek-slider-shell${isScrubbing ? ' is-scrubbing' : ''}${seekPending ? ' is-loading' : ''}`}
          style={sliderStyle}
          aria-busy={seekPending ? 'true' : undefined}
        >
          <input
            type="range"
            min="0"
            max={duration || 100}
            step="any"
            value={displayedPosition}
            className={`custom-slider seek-slider${isScrubbing ? ' is-scrubbing' : ''}`}
            aria-label="Seek"
            onChange={(event) => {
              const value = Number(event.currentTarget.value) || 0;
              setScrubValue(value);
            }}
            onKeyDown={(event) => {
              if (
                ['ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', 'Home', 'End'].includes(event.key)
              ) {
                setIsScrubbing(true);
              }
            }}
            onKeyUp={(event) => {
              if (!isScrubbing) return;
              setIsScrubbing(false);
              commitSeek(Number(event.currentTarget.value) || 0);
            }}
            onPointerDown={() => setIsScrubbing(true)}
            onPointerUp={(event) => {
              setIsScrubbing(false);
              commitSeek(Number(event.currentTarget.value) || 0);
            }}
            onPointerCancel={() => setIsScrubbing(false)}
          />
        </div>
        <span>{formatTime(duration)}</span>
      </div>
    </section>
  );
}
