import { useEffect, useRef, useState } from 'react';
import { nextPlaylistName } from '../../../shared/lib/appSupport';
import type { Playlist, QueueItem } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { PlaylistTrackArt } from './PlaylistTrackArt';

export type PlaylistPickerState = {
  items: QueueItem[];
  title: string;
  onAdded?: () => void;
} | null;

export function PlaylistPicker({
  picker,
  playlists,
  onClose,
  onAddToPlaylist,
  onCreatePlaylist
}: {
  picker: Exclude<PlaylistPickerState, null>;
  playlists: Playlist[];
  onClose: () => void;
  onAddToPlaylist: (playlist: Playlist, items: QueueItem[]) => Promise<boolean>;
  onCreatePlaylist: (name: string, items: QueueItem[]) => Promise<boolean>;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const isMultiple = picker.items.length > 1;
  const primaryItem = picker.items[0] || null;
  const emptyCopy = isMultiple
    ? 'Create a playlist to add these songs.'
    : 'Create a playlist to add this song.';

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    const closeOnKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', closeOnKey);
    return () => document.removeEventListener('keydown', closeOnKey);
  }, [onClose]);

  const finish = async (run: () => Promise<boolean>) => {
    if (busy) return;
    setBusy(true);
    try {
      const added = await run();
      if (!added) return;
      picker.onAdded?.();
      onClose();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="playlist-picker-backdrop app-modal-backdrop is-open"
      role="dialog"
      aria-modal="true"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="playlist-picker-panel app-modal-surface">
        <header className="playlist-picker-head">
          <div className="playlist-picker-title-wrap">
            {primaryItem ? (
              <div className="playlist-picker-art" aria-hidden="true">
                <PlaylistTrackArt item={primaryItem} />
              </div>
            ) : null}
            <div className="playlist-picker-title-copy">
              <h2>{picker.title}</h2>
              {primaryItem ? (
                <span>
                  {isMultiple
                    ? `${picker.items.length} songs`
                    : [primaryItem.artist, primaryItem.album].filter(Boolean).join(' · ')}
                </span>
              ) : null}
            </div>
          </div>
          <button
            className="global-search-close"
            type="button"
            aria-label="Close"
            onClick={onClose}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </header>
        <div className="playlist-picker-body">
          <div className="playlist-create-row">
            <input
              className="zone-settings-input playlist-create-input"
              ref={inputRef}
              type="text"
              placeholder="Playlist name"
              value={name}
              disabled={busy}
              onChange={(event) => setName(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter')
                  finish(() =>
                    onCreatePlaylist(name || nextPlaylistName(playlists), picker.items)
                  ).catch(() => undefined);
              }}
            />
            <button
              className="pill primary playlist-primary-action"
              type="button"
              disabled={busy}
              onClick={() => {
                finish(() =>
                  onCreatePlaylist(name || nextPlaylistName(playlists), picker.items)
                ).catch(() => undefined);
              }}
            >
              <Icon path="M12 5v14M5 12h14" />
              New playlist
            </button>
          </div>
          <div className="playlist-picker-list">
            {playlists.length ? (
              playlists.map((playlist) => (
                <button
                  className="playlist-picker-option"
                  type="button"
                  disabled={busy}
                  key={playlist.id}
                  onClick={() => {
                    finish(() => onAddToPlaylist(playlist, picker.items)).catch(() => undefined);
                  }}
                >
                  <span>{playlist.name}</span>
                  <small>
                    {playlist.items?.length || 0} song
                    {(playlist.items?.length || 0) === 1 ? '' : 's'}
                  </small>
                </button>
              ))
            ) : (
              <div className="playlist-picker-empty">{emptyCopy}</div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
