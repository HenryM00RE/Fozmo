import { type FormEvent, useEffect, useMemo, useState } from 'react';
import type { LibraryTrack, Playlist, QueueItem } from '../../../shared/types';
import { AlbumCoverPlayButton } from '../../../shared/ui/AlbumCoverPlayButton';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { PlayNextIcon } from '../../../shared/ui/PlayNextIcon';
import { PlaylistCover } from '../components/PlaylistCover';
import { playPlaylist, queueItemsForPlayback, songCountLabel } from '../model/playlistModel';

type QueuePlacement = 'next' | 'end';

export type PlaylistPageProps = {
  onCreatePlaylist: (name: string) => Promise<Playlist>;
  playlists: Playlist[];
  onOpen: (id: string) => void;
  onRefresh: () => Promise<void>;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  addItemsToQueue: (items: QueueItem[], placement: QueuePlacement) => void;
  tracks: LibraryTrack[];
};

export function PlaylistsPage({
  playlists,
  onCreatePlaylist,
  onOpen,
  playItems,
  addItemsToQueue,
  tracks
}: PlaylistPageProps) {
  const [creating, setCreating] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [playlistName, setPlaylistName] = useState('');
  const [createError, setCreateError] = useState('');
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const selectionActive = selectedIds.size > 0;
  const selectedPlaylists = useMemo(
    () => playlists.filter((playlist) => selectedIds.has(playlist.id)),
    [playlists, selectedIds]
  );

  useEffect(() => {
    document.body.classList.toggle('playlist-selection-mode', selectionActive);
    return () => document.body.classList.remove('playlist-selection-mode');
  }, [selectionActive]);

  useEffect(() => {
    setSelectedIds((current) => {
      const liveIds = new Set(playlists.map((playlist) => playlist.id));
      const next = new Set(Array.from(current).filter((id) => liveIds.has(id)));
      return next.size === current.size ? current : next;
    });
  }, [playlists]);

  const openCreatePlaylist = () => {
    setPlaylistName('');
    setCreateError('');
    setCreateOpen(true);
  };

  const closeCreatePlaylist = () => {
    if (creating) return;
    setCreateOpen(false);
    setCreateError('');
  };

  const createPlaylist = async (event: FormEvent) => {
    event.preventDefault();
    if (creating) return;
    const name = playlistName.trim();
    if (!name) {
      setCreateError('Playlist name is required');
      return;
    }
    setCreating(true);
    setCreateError('');
    try {
      const saved = await onCreatePlaylist(name);
      setCreateOpen(false);
      onOpen(saved.id);
    } catch (error) {
      setCreateError(error instanceof Error ? error.message : 'Unable to create playlist');
    } finally {
      setCreating(false);
    }
  };

  const toggleSelection = (playlistId: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(playlistId)) next.delete(playlistId);
      else next.add(playlistId);
      return next;
    });
  };

  const selectedItems = () =>
    selectedPlaylists.flatMap((playlist) => queueItemsForPlayback(playlist, false, tracks));

  const playSelected = () => {
    const items = selectedItems();
    if (items.length) playItems(items, 0);
    setSelectedIds(new Set());
  };

  const queueSelected = (placement: QueuePlacement) => {
    const items = selectedItems();
    if (items.length) addItemsToQueue(items, placement);
    setSelectedIds(new Set());
  };

  return (
    <section className="view playlists-view">
      <div className="library-page-heading">
        <div>
          <h1>Playlists</h1>
        </div>
        <button
          className="pill primary"
          type="button"
          id="playlist-new"
          disabled={creating}
          onClick={openCreatePlaylist}
        >
          <Icon path="M12 5v14M5 12h14" />
          New playlist
        </button>
      </div>
      <div className="playlists-shell">
        {selectionActive ? (
          <div className="toolbar-selection-actions playlist-selection-actions" aria-live="polite">
            <span className="toolbar-selection-count">{selectedIds.size} selected</span>
            <div
              className="album-play-split toolbar-selection-play"
              role="group"
              aria-label="Selected playlist playback actions"
            >
              <button className="album-play-main" type="button" onClick={playSelected}>
                <PlaybarPlayIcon />
                <span>Play now</span>
              </button>
              <button
                className="album-play-menu-trigger"
                type="button"
                aria-label="Add selected next"
                title="Add selected next"
                onClick={() => queueSelected('next')}
              >
                <PlayNextIcon />
              </button>
            </div>
            <button className="pill" type="button" onClick={() => queueSelected('end')}>
              Add to queue
            </button>
            <button className="pill" type="button" onClick={() => setSelectedIds(new Set())}>
              Cancel
            </button>
          </div>
        ) : null}
        <div
          className={`playlist-empty${playlists.length ? ' is-hidden' : ''}`}
          id="playlist-empty"
        >
          <strong>No playlists yet</strong>
          <span>Create a playlist from here or any song menu.</span>
        </div>
        <div className="playlist-grid" id="playlist-grid">
          {playlists.map((playlist) => {
            const count = playlist.items?.length || 0;
            const selected = selectedIds.has(playlist.id);
            return (
              <article
                className={`playlist-card${selectionActive ? ' is-selection-mode' : ''}${selected ? ' is-selected' : ''}`}
                data-playlist-id={playlist.id}
                tabIndex={0}
                role="button"
                aria-label={`Open ${playlist.name}`}
                aria-pressed={selectionActive ? selected : undefined}
                key={playlist.id}
                onClick={() =>
                  selectionActive ? toggleSelection(playlist.id) : onOpen(playlist.id)
                }
                onContextMenu={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  toggleSelection(playlist.id);
                }}
                onKeyDown={(event) => {
                  if (event.key !== 'Enter' && event.key !== ' ') return;
                  event.preventDefault();
                  if (selectionActive) toggleSelection(playlist.id);
                  else onOpen(playlist.id);
                }}
              >
                <div className="playlist-card-art">
                  <PlaylistCover playlist={playlist} />
                  <span className="playlist-selection-check" aria-hidden="true">
                    <Icon path="M20 6 9 17l-5-5" />
                  </span>
                  <AlbumCoverPlayButton
                    title="Play playlist"
                    ariaLabel="Play playlist"
                    onClick={(event) => {
                      event.preventDefault();
                      event.stopPropagation();
                      if (selectionActive) toggleSelection(playlist.id);
                      else playPlaylist(playlist, playItems, false, 0, tracks);
                    }}
                  />
                </div>
                <div className="playlist-card-text">
                  <strong title={playlist.name}>{playlist.name}</strong>
                  <span>{songCountLabel(count)}</span>
                </div>
              </article>
            );
          })}
        </div>
      </div>
      <Modal
        open={createOpen}
        className="history-import-backdrop"
        ariaLabelledBy="playlist-create-title"
        onClose={closeCreatePlaylist}
      >
        <form className="history-import-panel app-modal-surface" onSubmit={createPlaylist}>
          <div className="history-import-head">
            <div>
              <div className="section-label">Playlists</div>
              <h2 id="playlist-create-title">New playlist</h2>
            </div>
            <button
              className="history-import-close"
              type="button"
              aria-label="Close new playlist"
              disabled={creating}
              onClick={closeCreatePlaylist}
            >
              <Icon path="M18 6 6 18M6 6l12 12" />
            </button>
          </div>
          <div className="playlist-create-body">
            <label className="zone-settings-field">
              <span>Name</span>
              <input
                className="zone-settings-input"
                type="text"
                value={playlistName}
                maxLength={80}
                autoFocus
                autoComplete="off"
                placeholder="Playlist name"
                onChange={(event) => {
                  setPlaylistName(event.target.value);
                  if (createError) setCreateError('');
                }}
              />
            </label>
            {createError ? <div className="playlist-create-error">{createError}</div> : null}
          </div>
          <div className="history-import-foot">
            <button
              className="pill"
              type="button"
              disabled={creating}
              onClick={closeCreatePlaylist}
            >
              Cancel
            </button>
            <button
              className="pill primary"
              type="submit"
              disabled={creating || !playlistName.trim()}
            >
              {creating ? 'Creating…' : 'Create playlist'}
            </button>
          </div>
        </form>
      </Modal>
    </section>
  );
}
