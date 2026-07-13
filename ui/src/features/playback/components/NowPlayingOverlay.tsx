import { artFallback } from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum, QueueState } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { playbackChromeTrackModel } from '../model/playbackChromeModel';
import type { PlaybackAlbumTarget } from '../model/playbackChromeState';
import { usePlaybackControlSnapshot } from '../model/playbackControlStore';
import { NowPlayingQueueIsland } from './NowPlayingQueueIsland';

function queueRemainingSeconds(queue: QueueState, status: JsonRecord) {
  if (!queue.items.length) return 0;
  const startIndex = queue.cursor >= 0 ? queue.cursor : 0;
  return queue.items.slice(startIndex).reduce((sum, item, offset) => {
    const duration = Math.max(0, Number(item.durationSecs) || 0);
    if (offset !== 0 || queue.cursor < 0) return sum + duration;
    const statusDuration = Math.max(0, Number(status.duration_secs) || duration);
    const currentDuration = duration || statusDuration;
    const position = Math.max(0, Number(status.position_secs) || 0);
    return sum + Math.max(0, currentDuration - position);
  }, 0);
}

function formatQueueDuration(seconds: number) {
  const totalMinutes = Math.ceil(Math.max(0, seconds) / 60);
  if (!totalMinutes) return '';
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (!hours) return `${minutes}m`;
  return minutes ? `${hours}h ${String(minutes).padStart(2, '0')}m` : `${hours}h`;
}

export function NowPlayingOverlay({
  open,
  albums,
  onClose,
  status,
  queue,
  onClear,
  onOpenAlbum,
  onOpenArtist
}: {
  open: boolean;
  albums: LibraryAlbum[];
  onClose: () => void;
  status: JsonRecord;
  queue: QueueState;
  onClear: () => void;
  onOpenAlbum: (target: PlaybackAlbumTarget) => void;
  onOpenArtist: (name: string) => void;
}) {
  const { pendingArtSrc, playbackLoading } = usePlaybackControlSnapshot();
  if (!open) return null;
  const {
    currentAlbum,
    currentAlbumTarget,
    currentArtist,
    currentArt,
    currentTrackName,
    trackTitleClass
  } = playbackChromeTrackModel({ pendingArtSrc, albums, playbackLoading, queue, status });
  const queueDurationLabel = formatQueueDuration(queueRemainingSeconds(queue, status));

  return (
    <section className="now-playing-view" aria-hidden="false">
      <div className="now-playing-left">
        <button className="btn-ghost now-playing-close" type="button" onClick={onClose}>
          <Icon path="m6 9 6 6 6-6" />
        </button>
        <div className={`now-playing-art${currentArt ? ' has-cover' : ''}`}>
          {currentArt ? <img alt="" src={currentArt} /> : artFallback()}
        </div>
        <div className="now-playing-meta">
          <h1 className={trackTitleClass}>{currentTrackName}</h1>
          {currentArtist ? (
            <button
              className="artist-link now-playing-artist"
              type="button"
              onClick={() => onOpenArtist(currentArtist)}
            >
              {currentArtist}
            </button>
          ) : (
            <div className="now-playing-artist" />
          )}
          {currentAlbum && currentAlbumTarget ? (
            <button
              className="album-link now-playing-album"
              type="button"
              onClick={() => onOpenAlbum(currentAlbumTarget)}
            >
              {currentAlbum}
            </button>
          ) : (
            <div className="now-playing-album">{currentAlbum}</div>
          )}
        </div>
      </div>
      <div className="now-playing-right">
        <div className="now-playing-queue-header">
          <div>
            <div className="section-label">Up next</div>
            <h2 className="now-playing-queue-title">
              <span>Queue</span>
              <span className="queue-count">{queue.items.length}</span>
              {queueDurationLabel ? (
                <span className="queue-duration">{queueDurationLabel}</span>
              ) : null}
            </h2>
          </div>
          <div className="queue-actions">
            <button className="pill danger" type="button" onClick={onClear}>
              Clear
            </button>
          </div>
        </div>
        <NowPlayingQueueIsland scrollToCurrentOnMount />
      </div>
    </section>
  );
}
