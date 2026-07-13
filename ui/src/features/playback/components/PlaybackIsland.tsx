import { useMemo } from 'react';
import { formatTime, stripFileExtension } from '../../../shared/lib/format';
import { useInterpolatedPosition } from '../hooks/useInterpolatedPosition';
import { PlaybackStatus, usePlaybackSnapshot } from '../model/playbackStore';

function displayTitle(status: PlaybackStatus) {
  return status.track_title?.trim() || stripFileExtension(status.file_name) || 'Select a track';
}

function displayArtist(status: PlaybackStatus) {
  return status.track_artist?.trim() || status.track_album?.trim() || 'No active stream';
}

export function PlaybackIsland() {
  const { connection, status, lastMessageAt, error } = usePlaybackSnapshot();
  const position = useInterpolatedPosition(status);
  const duration = Number(status.duration_secs) || 0;
  const progress = duration > 0 ? Math.max(0, Math.min(100, (position / duration) * 100)) : 0;
  const title = displayTitle(status);
  const subtitle = displayArtist(status);

  const ageLabel = useMemo(() => {
    if (!lastMessageAt) return 'waiting';
    const ageSeconds = Math.max(0, Math.round((Date.now() - lastMessageAt) / 1000));
    return ageSeconds <= 1 ? 'live' : `${ageSeconds}s ago`;
  }, [lastMessageAt]);

  return (
    <section className="react-playback-island" aria-label="React playback status">
      <div className="react-playback-row">
        <span className={`react-playback-dot is-${connection}`} aria-hidden="true" />
        <span className="react-playback-state">{status.state || 'Stopped'}</span>
        <span className="react-playback-age">{ageLabel}</span>
      </div>
      <div className="react-playback-title" title={title}>
        {title}
      </div>
      <div className="react-playback-subtitle" title={subtitle}>
        {subtitle}
      </div>
      <div className="react-playback-progress" aria-hidden="true">
        <span style={{ width: `${progress}%` }} />
      </div>
      <div className="react-playback-times">
        <span>{formatTime(position)}</span>
        <span>{formatTime(duration)}</span>
      </div>
      {error ? (
        <div className="react-playback-error" data-testid="playback-error">
          {error}
        </div>
      ) : null}
    </section>
  );
}
