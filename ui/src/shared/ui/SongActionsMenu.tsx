import { Icon } from './Icon';
import { Menu } from './Menu';
import { PlaybarPlayIcon } from './PlaybarPlayIcon';
import { PlayNextIcon } from './PlayNextIcon';

export function SongActionsMenu({
  ariaLabel = 'Track options',
  onAddNext,
  onAddToPlaylist,
  onAddToQueue,
  onGoToAlbum,
  onGoToArtist,
  onPlay,
  x,
  y
}: {
  ariaLabel?: string;
  onAddNext: () => void;
  onAddToPlaylist?: () => void;
  onAddToQueue: () => void;
  onGoToAlbum?: () => void;
  onGoToArtist?: () => void;
  onPlay: () => void;
  x: number;
  y: number;
}) {
  return (
    <Menu
      className="track-actions-menu track-actions-menu-wide is-open"
      ariaLabel={ariaLabel}
      style={{ left: Math.max(12, x), top: y }}
      onClick={(event) => event.stopPropagation()}
    >
      <button
        className="track-action-item has-filled-icon"
        type="button"
        role="menuitem"
        onClick={onPlay}
      >
        <PlaybarPlayIcon className="track-action-play-icon" />
        <span>Play</span>
      </button>
      <button className="track-action-item" type="button" role="menuitem" onClick={onAddNext}>
        <PlayNextIcon />
        <span>Add next</span>
      </button>
      {onAddToPlaylist ? (
        <button
          className="track-action-item"
          type="button"
          role="menuitem"
          onClick={onAddToPlaylist}
        >
          <Icon path="M4 7h12M4 12h9M4 17h7M18 15v6M15 18h6" />
          <span>Add to playlist</span>
        </button>
      ) : null}
      {onGoToAlbum ? (
        <button className="track-action-item" type="button" role="menuitem" onClick={onGoToAlbum}>
          <Icon path="M5 4h14v16H5zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM12 12h.01" />
          <span>Go to album</span>
        </button>
      ) : null}
      {onGoToArtist ? (
        <button className="track-action-item" type="button" role="menuitem" onClick={onGoToArtist}>
          <Icon path="M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM4 20c1.8-4 4.5-6 8-6s6.2 2 8 6" />
          <span>Go to artist</span>
        </button>
      ) : null}
      <button className="track-action-item" type="button" role="menuitem" onClick={onAddToQueue}>
        <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
        <span>Add to queue</span>
      </button>
    </Menu>
  );
}
