import { type FormEvent, useEffect, useState } from 'react';
import type { LibraryTrack, Playlist, QueueItem } from '../../../shared/types';
import { AlbumCoverPlayButton } from '../../../shared/ui/AlbumCoverPlayButton';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import { PlaylistCover } from '../components/PlaylistCover';
import { playPlaylist, songCountLabel } from '../model/playlistModel';

export type PlaylistPageProps = {
  onCreatePlaylist: (name: string) => Promise<Playlist>;
  playlists: Playlist[];
  selectedPlaylistIds: Set<string>;
  selectionActive: boolean;
  onToggleSelection: (playlistId: string) => void;
  onOpen: (id: string) => void;
  onRefresh: () => Promise<void>;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  tracks: LibraryTrack[];
};

export function PlaylistsPage({
  playlists,
  selectedPlaylistIds,
  selectionActive,
  onToggleSelection,
  onCreatePlaylist,
  onOpen,
  playItems,
  tracks
}: PlaylistPageProps) {
  const [creating, setCreating] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [playlistName, setPlaylistName] = useState('');
  const [createError, setCreateError] = useState('');
  useEffect(() => {
    document.body.classList.toggle('playlist-selection-mode', selectionActive);
    return () => document.body.classList.remove('playlist-selection-mode');
  }, [selectionActive]);

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

  return (
    <section className="view playlists-view">
      <div className="library-page-heading">
        <div>
          <h1>Playlists</h1>
        </div>
        <button
          className="pill primary playlist-primary-action"
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
            const selected = selectedPlaylistIds.has(playlist.id);
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
                  selectionActive ? onToggleSelection(playlist.id) : onOpen(playlist.id)
                }
                onContextMenu={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  onToggleSelection(playlist.id);
                }}
                onKeyDown={(event) => {
                  if (event.key !== 'Enter' && event.key !== ' ') return;
                  event.preventDefault();
                  if (selectionActive) onToggleSelection(playlist.id);
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
                      if (selectionActive) onToggleSelection(playlist.id);
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
                className="zone-settings-input playlist-create-name-input"
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
              className="pill primary playlist-primary-action"
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
