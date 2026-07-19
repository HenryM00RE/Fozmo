import {
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
  useState
} from 'react';
import { artFallback } from '../../../shared/lib/appSupport';
import { Icon } from '../../../shared/ui/Icon';
import { playbackChromeTrackModel, signalTriggerLabel } from '../model/playbackChromeModel';
import type { PlaybackChromeState } from '../model/playbackChromeState';
import { usePlaybackControlSnapshot } from '../model/playbackControlStore';
import type { PlaybackStatus } from '../model/playbackStore';
import { NowPlayingQueueIsland } from './NowPlayingQueueIsland';
import { PlaybackControlsIsland } from './PlaybackControlsIsland';
import { SignalPopover } from './SignalPopover';
import { VolumeControl } from './VolumeControl';
import { ZonePicker } from './ZonePicker';

type MobileNowPlayingSheetProps = {
  onOpenArtist: (rawName: unknown) => void;
  playbackChrome: PlaybackChromeState;
  playbackPosition: number;
};

const MOBILE_SHEET_CLOSE_FALLBACK_MS = 420;

export function MobileNowPlayingSheet({
  onOpenArtist,
  playbackChrome,
  playbackPosition
}: MobileNowPlayingSheetProps) {
  const {
    activeZoneId,
    albums,
    nowPlayingOpen,
    onClearQueue,
    onOpenAlbum,
    onSelectZone,
    queue,
    setSignalOpen,
    setNowPlayingOpen,
    signalOpen,
    status,
    zones
  } = playbackChrome;
  const { pendingArtSrc, playbackLoading } = usePlaybackControlSnapshot();
  const [view, setView] = useState<'now-playing' | 'queue'>('now-playing');
  const [sheetDragY, setSheetDragY] = useState(0);
  const [sheetDragging, setSheetDragging] = useState(false);
  const [sheetClosing, setSheetClosing] = useState(false);
  const dragRef = useRef<{
    active: boolean;
    offsetY: number;
    pointerId: number;
    startY: number;
  } | null>(null);
  const closeTimerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (closeTimerRef.current !== null) {
        window.clearTimeout(closeTimerRef.current);
      }
    };
  }, []);

  if (!nowPlayingOpen) return null;

  const model = playbackChromeTrackModel({
    pendingArtSrc,
    albums,
    playbackLoading,
    queue,
    status
  });
  const signalLabel = signalTriggerLabel(status);
  const signalTriggerClass = `signal-quality-trigger${signalLabel.length > 10 ? ' is-wide' : ''}`;
  const albumTarget = model.currentAlbumTarget;
  const sheetStyle =
    sheetDragY > 0 ? ({ '--mobile-sheet-drag-y': `${sheetDragY}px` } as CSSProperties) : undefined;

  const finishCollapse = () => {
    if (closeTimerRef.current !== null) window.clearTimeout(closeTimerRef.current);
    setNowPlayingOpen(false);
    setSheetClosing(false);
    setSheetDragY(0);
    dragRef.current = null;
    closeTimerRef.current = null;
  };

  const collapseSheet = () => {
    if (closeTimerRef.current !== null) return;
    setSheetDragging(false);
    setSheetClosing(true);
    setSheetDragY(window.innerHeight);
    closeTimerRef.current = window.setTimeout(finishCollapse, MOBILE_SHEET_CLOSE_FALLBACK_MS);
  };

  const resetSheetDrag = () => {
    setSheetDragging(false);
    setSheetClosing(false);
    setSheetDragY(0);
    dragRef.current = null;
  };

  const isInteractiveDragTarget = (target: EventTarget | null) => {
    if (!(target instanceof Element)) return false;
    return Boolean(
      target.closest(
        'button, a, input, select, textarea, [role="button"], [role="slider"], .seek-slider-shell, .zone-menu, .volume-popover, .signal-popover'
      )
    );
  };

  const onSheetPointerDown = (event: ReactPointerEvent<HTMLElement>) => {
    if (view !== 'now-playing' || event.button !== 0 || isInteractiveDragTarget(event.target)) {
      return;
    }
    const rect = event.currentTarget.getBoundingClientRect();
    if (event.clientY > rect.top + rect.height * 0.52) return;

    dragRef.current = {
      active: false,
      offsetY: 0,
      pointerId: event.pointerId,
      startY: event.clientY
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const onSheetPointerMove = (event: ReactPointerEvent<HTMLElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;

    const offset = Math.max(0, event.clientY - drag.startY);
    if (!drag.active && offset < 8) return;

    drag.active = true;
    drag.offsetY = offset;
    setSheetDragging(true);
    setSheetDragY(Math.min(offset, window.innerHeight));
    event.preventDefault();
  };

  const onSheetPointerUp = (event: ReactPointerEvent<HTMLElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;

    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    if (drag.active && drag.offsetY > 88) {
      collapseSheet();
    } else {
      resetSheetDrag();
    }
  };

  return (
    <section
      className={`mobile-now-playing-sheet${sheetDragging ? ' is-dragging' : ''}${sheetClosing ? ' is-closing' : ''}`}
      style={sheetStyle}
      aria-label="Now playing"
      onPointerDown={onSheetPointerDown}
      onPointerMove={onSheetPointerMove}
      onPointerUp={onSheetPointerUp}
      onPointerCancel={resetSheetDrag}
      onTransitionEnd={(event) => {
        if (
          sheetClosing &&
          event.target === event.currentTarget &&
          event.propertyName === 'transform'
        ) {
          finishCollapse();
        }
      }}
    >
      <div className="mobile-sheet-head">
        <button
          className="btn-ghost"
          type="button"
          title="Close now playing"
          aria-label="Close now playing"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => {
            event.stopPropagation();
            collapseSheet();
          }}
        >
          <Icon path="m6 9 6 6 6-6" />
        </button>
        <div className="segmented mobile-sheet-tabs" role="group" aria-label="Now playing view">
          <button
            type="button"
            className={view === 'now-playing' ? 'on' : undefined}
            aria-pressed={view === 'now-playing'}
            onClick={() => setView('now-playing')}
          >
            Now Playing
          </button>
          <button
            type="button"
            className={view === 'queue' ? 'on' : undefined}
            aria-pressed={view === 'queue'}
            onClick={() => setView('queue')}
          >
            Queue
          </button>
        </div>
      </div>

      {view === 'now-playing' ? (
        <div className="mobile-sheet-now">
          <div className={`mobile-sheet-art${model.currentArt ? ' has-cover' : ''}`}>
            {model.currentArt ? <img alt="" src={model.currentArt} /> : artFallback()}
          </div>
          <div className="mobile-sheet-meta">
            <h1 className={model.trackTitleClass}>{model.currentTrackName}</h1>
            {model.currentArtist || model.currentAlbum ? (
              <div className="mobile-sheet-submeta">
                {model.currentArtist ? (
                  <button
                    className="artist-link"
                    type="button"
                    onClick={() => onOpenArtist(model.currentArtist)}
                  >
                    {model.currentArtist}
                  </button>
                ) : null}
                {model.currentArtist && model.currentAlbum ? (
                  <span className="mobile-sheet-meta-separator" aria-hidden="true">
                    /
                  </span>
                ) : null}
                {model.currentAlbum && albumTarget ? (
                  <button
                    className="album-link"
                    type="button"
                    onClick={() => onOpenAlbum(albumTarget)}
                  >
                    {model.currentAlbum}
                  </button>
                ) : model.currentAlbum ? (
                  <span>{model.currentAlbum}</span>
                ) : null}
              </div>
            ) : null}
          </div>
          <PlaybackControlsIsland status={status as PlaybackStatus} position={playbackPosition} />
          <div className="mobile-sheet-output">
            <ZonePicker
              zones={zones}
              activeZoneId={activeZoneId}
              activeZoneName={String(status.active_zone_name || '')}
              status={status}
              onSelect={onSelectZone}
            />
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
              {signalOpen ? (
                <SignalPopover status={status} sourceProvider={model.sourceProvider} />
              ) : null}
            </div>
            <VolumeControl activeZoneId={activeZoneId} status={status} />
          </div>
        </div>
      ) : (
        <div className="mobile-sheet-queue">
          <div className="now-playing-queue-header">
            <div>
              <div className="section-label">Up next</div>
              <h2 className="now-playing-queue-title">
                <span>Queue</span>
                <span className="queue-count">{queue.items.length}</span>
              </h2>
            </div>
            <button className="pill danger" type="button" onClick={onClearQueue}>
              Clear
            </button>
          </div>
          <NowPlayingQueueIsland scrollToCurrentOnMount />
        </div>
      )}
    </section>
  );
}
