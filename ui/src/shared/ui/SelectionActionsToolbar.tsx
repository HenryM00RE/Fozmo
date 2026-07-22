import { Icon } from './Icon';
import { PlaybarPlayIcon } from './PlaybarPlayIcon';
import { PlayNextIcon } from './PlayNextIcon';
import type { SelectionToolbarState } from './selectionToolbar';

type SelectionActionsToolbarProps = {
  selectionToolbar: SelectionToolbarState;
};

export function SelectionActionsToolbar({ selectionToolbar }: SelectionActionsToolbarProps) {
  const {
    activeSelectionBusy,
    activeSelectionCount,
    activeSelectionType,
    addSelectedAlbumTracksToPlaylist,
    addSelectedRecentlyPlayedToPlaylist,
    albumSelectionMenuOpen,
    playSelectedAlbumTracks,
    playSelectedPlaylists,
    playSelectedRecentlyPlayed,
    playlistSelectionMenuOpen,
    queueSelectedAlbumTracks,
    queueSelectedPlaylists,
    queueSelectedRecentlyPlayed,
    recentSelectionMenuOpen,
    setAlbumSelectionMenuOpen,
    setPlaylistSelectionMenuOpen,
    setRecentSelectionMenuOpen
  } = selectionToolbar;

  if (!activeSelectionType) return null;

  const playlistSelection = activeSelectionType === 'playlists';
  const menuOpen =
    activeSelectionType === 'album-tracks'
      ? albumSelectionMenuOpen
      : playlistSelection
        ? playlistSelectionMenuOpen
        : recentSelectionMenuOpen;
  const playSelected = () => {
    if (activeSelectionType === 'album-tracks') playSelectedAlbumTracks();
    else if (playlistSelection) playSelectedPlaylists();
    else playSelectedRecentlyPlayed().catch(() => undefined);
  };
  const toggleMenu = () => {
    if (activeSelectionType === 'album-tracks') setAlbumSelectionMenuOpen((open) => !open);
    else if (playlistSelection) setPlaylistSelectionMenuOpen((open) => !open);
    else setRecentSelectionMenuOpen((open) => !open);
  };
  const queueNext = () => {
    if (activeSelectionType === 'album-tracks') queueSelectedAlbumTracks('next');
    else if (playlistSelection) queueSelectedPlaylists('next');
    else queueSelectedRecentlyPlayed('next').catch(() => undefined);
  };
  const addToPlaylist = () => {
    if (activeSelectionType === 'album-tracks') addSelectedAlbumTracksToPlaylist();
    else addSelectedRecentlyPlayedToPlaylist().catch(() => undefined);
  };
  const queueEnd = () => {
    if (activeSelectionType === 'album-tracks') queueSelectedAlbumTracks('end');
    else if (playlistSelection) queueSelectedPlaylists('end');
    else queueSelectedRecentlyPlayed('end').catch(() => undefined);
  };

  return (
    <div className="toolbar-selection-actions" aria-live="polite">
      <span className="toolbar-selection-count">{activeSelectionCount} selected</span>
      <div
        className="album-play-split toolbar-selection-play"
        role="group"
        aria-label="Selected item playback actions"
      >
        <button
          className="album-play-main"
          type="button"
          disabled={activeSelectionBusy}
          onClick={playSelected}
        >
          <PlaybarPlayIcon />
          <span>Play now</span>
        </button>
        <button
          className="album-play-menu-trigger"
          type="button"
          disabled={activeSelectionBusy}
          aria-label="Selected queue options"
          title="Selected queue options"
          onClick={toggleMenu}
        >
          <Icon path="m6 9 6 6 6-6" />
        </button>
        {menuOpen ? (
          <div
            className="track-actions-menu track-actions-menu-wide react-selection-menu is-open"
            role="menu"
          >
            <button className="track-action-item" type="button" role="menuitem" onClick={queueNext}>
              <PlayNextIcon />
              <span>{playlistSelection ? 'Queue next' : 'Add selected next'}</span>
            </button>
            {!playlistSelection ? (
              <button
                className="track-action-item"
                type="button"
                role="menuitem"
                onClick={addToPlaylist}
              >
                <Icon path="M4 7h12M4 12h9M4 17h7M18 15v6M15 18h6" />
                <span>Add selected to playlist</span>
              </button>
            ) : null}
            <button className="track-action-item" type="button" role="menuitem" onClick={queueEnd}>
              <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
              <span>{playlistSelection ? 'Add to queue' : 'Add selected to queue'}</span>
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}
