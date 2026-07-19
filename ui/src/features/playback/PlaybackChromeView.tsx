import { artFallback } from '../../shared/lib/appSupport';
import { MobileMiniPlayer } from './components/MobileMiniPlayer';
import { MobileNowPlayingSheet } from './components/MobileNowPlayingSheet';
import { NowPlayingOverlay } from './components/NowPlayingOverlay';
import { PlaybackControlsIsland } from './components/PlaybackControlsIsland';
import { SignalPopover } from './components/SignalPopover';
import { VolumeControl } from './components/VolumeControl';
import { ZonePicker } from './components/ZonePicker';
import { useInterpolatedPosition } from './hooks/useInterpolatedPosition';
import { usePersistentSeekPosition } from './hooks/usePersistentSeekPosition';
import { playbackChromeTrackModel, signalTriggerLabel } from './model/playbackChromeModel';
import type { PlaybackChromeState } from './model/playbackChromeState';
import { usePlaybackControlSnapshot } from './model/playbackControlStore';
import { type PlaybackStatus, usePlaybackSnapshot } from './model/playbackStore';

type PlaybackChromeViewProps = {
  onOpenArtist: (rawName: unknown) => void;
  playbackChrome: PlaybackChromeState;
};

export function PlaybackChromeView({ onOpenArtist, playbackChrome }: PlaybackChromeViewProps) {
  const {
    activeZoneId,
    albums,
    nowPlayingOpen,
    onClearQueue,
    onOpenAlbum,
    onSelectZone,
    queue,
    setNowPlayingOpen,
    setSignalOpen,
    signalOpen,
    status,
    zones
  } = playbackChrome;
  const { connection } = usePlaybackSnapshot();
  const { pendingArtSrc, playbackLoading, transportPending } = usePlaybackControlSnapshot();
  const interpolatedPosition = useInterpolatedPosition(status as PlaybackStatus);
  const playbackPosition = usePersistentSeekPosition(
    status as PlaybackStatus,
    interpolatedPosition,
    transportPending
  );
  const {
    currentAlbum,
    currentAlbumTarget,
    currentArtist,
    currentArt,
    currentTrackName,
    sourceProvider,
    trackTitleClass
  } = playbackChromeTrackModel({ pendingArtSrc, albums, playbackLoading, queue, status });
  const signalLabel = signalTriggerLabel(status);
  const signalTriggerClass = `signal-quality-trigger${signalLabel.length > 10 ? ' is-wide' : ''}`;

  return (
    <>
      <NowPlayingOverlay
        open={nowPlayingOpen}
        albums={albums}
        onClose={() => setNowPlayingOpen(false)}
        status={status}
        queue={queue}
        onClear={onClearQueue}
        onOpenAlbum={onOpenAlbum}
        onOpenArtist={onOpenArtist}
      />
      <MobileNowPlayingSheet
        playbackChrome={playbackChrome}
        playbackPosition={playbackPosition}
        onOpenArtist={onOpenArtist}
      />

      <footer className="player-bar" data-testid="player-bar" data-playback-connection={connection}>
        <div className="player-track">
          <button
            className="player-art-button"
            type="button"
            title="Open now playing"
            aria-label="Open now playing"
            onClick={() => setNowPlayingOpen(true)}
          >
            <div className={`player-art${currentArt ? ' has-cover' : ''}`}>
              {currentArt ? <img alt="" src={currentArt} /> : artFallback()}
            </div>
          </button>
          <div className="player-track-text">
            <h2 className={trackTitleClass}>{currentTrackName}</h2>
            {currentArtist ? (
              <button
                className="artist-link track-artist"
                type="button"
                onClick={() => onOpenArtist(currentArtist)}
              >
                {currentArtist}
              </button>
            ) : (
              <div className="track-artist" />
            )}
            {currentAlbum && currentAlbumTarget ? (
              <button
                className="album-link track-meta"
                type="button"
                onClick={() => onOpenAlbum(currentAlbumTarget)}
              >
                {currentAlbum}
              </button>
            ) : (
              <div className="track-meta">
                {currentAlbum || String(status.selected_device || 'No active stream')}
              </div>
            )}
          </div>
        </div>
        <div className="player-center">
          <PlaybackControlsIsland status={status as PlaybackStatus} position={playbackPosition} />
        </div>
        <div className="volume-control">
          <div className="signal-control">
            <button
              className={signalTriggerClass}
              type="button"
              title="Playback Chain"
              aria-label={`Playback Chain, ${signalLabel}`}
              onClick={() => setSignalOpen((value) => !value)}
            >
              <span>{signalLabel}</span>
            </button>
            {signalOpen ? <SignalPopover status={status} sourceProvider={sourceProvider} /> : null}
          </div>
          <ZonePicker
            zones={zones}
            activeZoneId={activeZoneId}
            activeZoneName={String(status.active_zone_name || '')}
            status={status}
            onSelect={onSelectZone}
          />
          <VolumeControl activeZoneId={activeZoneId} status={status} />
        </div>
      </footer>
      <MobileMiniPlayer playbackChrome={playbackChrome} />
    </>
  );
}
