import { useState } from 'react';
import { artFallback } from '../../../shared/lib/appSupport';
import { playbackChromeTrackModel } from '../model/playbackChromeModel';
import type { PlaybackChromeState } from '../model/playbackChromeState';
import {
  backendPlayControlIsLoading,
  getPlaybackControlActions,
  isSettledPlaybackState,
  type PlaybackControlAction,
  usePlaybackControlSnapshot
} from '../model/playbackControlStore';
import { PlayLoadingSpinner, PlayPauseIcon, SkipIcon } from './PlaybackControlsIsland';

type MobileMiniPlayerProps = {
  playbackChrome: PlaybackChromeState;
};

export function MobileMiniPlayer({ playbackChrome }: MobileMiniPlayerProps) {
  const { albums, queue, setNowPlayingOpen, status } = playbackChrome;
  const { pendingArtSrc, playbackLoading } = usePlaybackControlSnapshot();
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const model = playbackChromeTrackModel({
    pendingArtSrc,
    albums,
    playbackLoading,
    queue,
    status
  });
  const playing = status.state === 'Playing';
  const playLabel = playing ? 'Pause' : status.state === 'Paused' ? 'Resume' : 'Play';
  const settledTransportState = isSettledPlaybackState(status.state);
  const playBusy =
    (playbackLoading && !settledTransportState) ||
    busyAction === 'playPause' ||
    backendPlayControlIsLoading(status.state, status.transport_pending);

  const runAction = (name: string, action: PlaybackControlAction | undefined) => {
    if (!action || busyAction) return;
    setBusyAction(name);
    const clear = () => window.setTimeout(() => setBusyAction(null), 120);
    try {
      const result = action();
      if (result && typeof result.finally === 'function') result.finally(clear);
      else window.setTimeout(clear, 350);
    } catch (error) {
      clear();
      throw error;
    }
  };

  return (
    <aside className="mobile-mini-player" aria-label="Now playing">
      <button
        className="mobile-mini-track"
        type="button"
        title="Open now playing"
        aria-label="Open now playing"
        onClick={() => setNowPlayingOpen(true)}
      >
        <span className={`mobile-mini-art${model.currentArt ? ' has-cover' : ''}`}>
          {model.currentArt ? <img alt="" src={model.currentArt} /> : artFallback()}
        </span>
        <span className="mobile-mini-text">
          <strong className={model.trackTitleClass}>{model.currentTrackName}</strong>
          <span>{model.currentArtist || model.currentAlbum || 'No active stream'}</span>
        </span>
      </button>
      <div className="mobile-mini-controls react-transport-controls">
        <button
          className="round-btn transport-skip-button mobile-mini-control"
          type="button"
          title="Previous"
          aria-label="Previous"
          disabled={busyAction === 'previous'}
          onClick={() => runAction('previous', getPlaybackControlActions().previous)}
        >
          <SkipIcon direction="previous" />
        </button>
        <button
          className={`round-btn play transport-play-button mobile-mini-control${playBusy ? ' is-loading' : ''}`}
          type="button"
          title={playBusy ? 'Loading' : playLabel}
          aria-label={playBusy ? 'Loading' : playLabel}
          aria-busy={playBusy ? 'true' : undefined}
          disabled={playBusy}
          onClick={() => runAction('playPause', getPlaybackControlActions().playPause)}
        >
          {playBusy ? (
            <PlayLoadingSpinner />
          ) : (
            <PlayPauseIcon state={status.state as string | undefined} />
          )}
        </button>
        <button
          className="round-btn transport-skip-button mobile-mini-control"
          type="button"
          title="Next"
          aria-label="Next"
          disabled={busyAction === 'next'}
          onClick={() => runAction('next', getPlaybackControlActions().next)}
        >
          <SkipIcon direction="next" />
        </button>
      </div>
    </aside>
  );
}
