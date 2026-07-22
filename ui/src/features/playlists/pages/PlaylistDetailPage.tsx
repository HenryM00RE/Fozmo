import {
  type FormEvent,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
  useState
} from 'react';
import { endpoints } from '../../../shared/lib/api';
import { formatLongDuration, sourceTrack } from '../../../shared/lib/appSupport';
import { displayTitleUsesFallbackFont } from '../../../shared/lib/displayTitle';
import { formatTime } from '../../../shared/lib/format';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { QueueItem } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Menu } from '../../../shared/ui/Menu';
import { Modal } from '../../../shared/ui/Modal';
import { actionMenuPosition } from '../../../shared/ui/menuPosition';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { PlayNextIcon } from '../../../shared/ui/PlayNextIcon';
import { ShuffleIcon } from '../../../shared/ui/ShuffleIcon';
import { useActionMenuScrollLock } from '../../../shared/ui/useActionMenuScrollLock';
import { PlaylistCover } from '../components/PlaylistCover';
import { PlaylistTrackArt } from '../components/PlaylistTrackArt';
import {
  playlistCreatedAt,
  playlistCsv,
  playlistCsvFilename,
  playlistItems,
  playlistUpdatedAt,
  playPlaylist,
  queueItemsForPlayback,
  savePlaylistItems,
  savePlaylistName,
  songCountLabel,
  subtitleForItem
} from '../model/playlistModel';
import type { PlaylistPageProps } from './PlaylistsPage';

type PlaylistDetailProps = Pick<
  PlaylistPageProps,
  'onRefresh' | 'playItems' | 'playlists' | 'tracks'
> & {
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  id: string;
  onBack: () => void;
  onOpenAlbum: (id: string | number) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onOpenArtist: (name: string) => void;
  customDisplayFont: CustomDisplayFontSettings | null;
};

type TrackMenuState = {
  index: number;
  x: number;
  y: number;
} | null;

type QueueMenuState = {
  x: number;
  y: number;
} | null;

type PointerReorderState = {
  from: number;
  over: number;
  placement: 'above' | 'below';
  pointerId: number;
} | null;

export function PlaylistDetailPage({
  id,
  playlists,
  onBack,
  onRefresh,
  playItems,
  addItemsToQueue,
  tracks,
  onOpenAlbum,
  onOpenQobuzAlbum,
  onOpenArtist,
  customDisplayFont
}: PlaylistDetailProps) {
  const playlist = playlists.find((item) => item.id === id);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsName, setSettingsName] = useState('');
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [settingsError, setSettingsError] = useState('');
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);
  const [trackMenu, setTrackMenu] = useState<TrackMenuState>(null);
  const [queueMenu, setQueueMenu] = useState<QueueMenuState>(null);
  const [dragState, setDragState] = useState<{
    from: number;
    over: number;
    placement: 'above' | 'below';
  } | null>(null);
  const pointerReorderRef = useRef<PointerReorderState>(null);
  useActionMenuScrollLock(Boolean(trackMenu || queueMenu));

  useEffect(() => {
    const close = () => {
      setTrackMenu(null);
      setQueueMenu(null);
    };
    window.addEventListener('click', close);
    window.addEventListener('keydown', close);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', close);
    };
  }, []);

  if (!playlist) {
    return (
      <section className="view playlist-detail-view">
        <div className="playlist-detail-shell">
          <div className="playlist-empty">
            <strong>Playlist not found</strong>
            <span>It may have been deleted.</span>
          </div>
          <button className="pill" type="button" onClick={onBack}>
            Back to playlists
          </button>
        </div>
      </section>
    );
  }

  const items = playlistItems(playlist, tracks);
  const titleLengthClass =
    playlist.name.length > 58
      ? ' is-extra-long-title'
      : playlist.name.length > 38
        ? ' is-long-title'
        : '';
  const playlistTitleClass = `playlist-detail-title album-detail-title${titleLengthClass}${displayTitleUsesFallbackFont(playlist.name, customDisplayFont) ? ' uses-fallback-font' : ''}`;
  const playlistDuration = formatLongDuration(
    items.reduce((total, item) => total + Math.max(0, Number(item.durationSecs) || 0), 0)
  );
  const removeTrack = async (index: number) => {
    const next = items.filter((_, itemIndex) => itemIndex !== index);
    await savePlaylistItems(playlist, next);
    await onRefresh();
  };

  const reorderTrack = async (from: number, to: number) => {
    if (from < 0 || from >= items.length) return;
    let target = to;
    if (from < target) target -= 1;
    if (target === from || target < 0 || target > items.length) return;
    const next = items.slice();
    const [moved] = next.splice(from, 1);
    next.splice(target, 0, moved);
    await savePlaylistItems(playlist, next);
    await onRefresh();
  };

  const updatePointerReorderTarget = (clientX: number, clientY: number) => {
    const drag = pointerReorderRef.current;
    if (!drag) return;
    const row = document.elementFromPoint(clientX, clientY)?.closest('.playlist-track-row');
    if (!(row instanceof HTMLElement)) return;
    const index = Number(row.dataset.playlistTrack);
    if (!Number.isInteger(index)) return;
    const rect = row.getBoundingClientRect();
    const placement = clientY - rect.top < rect.height / 2 ? 'above' : 'below';
    pointerReorderRef.current = { ...drag, over: index, placement };
    setDragState({ from: drag.from, over: index, placement });
  };

  const startPointerReorder = (index: number, event: ReactPointerEvent<HTMLButtonElement>) => {
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    pointerReorderRef.current = {
      from: index,
      over: index,
      placement: 'above',
      pointerId: event.pointerId
    };
    setDragState({ from: index, over: index, placement: 'above' });
  };

  const movePointerReorder = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (pointerReorderRef.current?.pointerId !== event.pointerId) return;
    event.preventDefault();
    updatePointerReorderTarget(event.clientX, event.clientY);
  };

  const finishPointerReorder = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const drag = pointerReorderRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    pointerReorderRef.current = null;
    setDragState(null);
    const to = drag.placement === 'above' ? drag.over : drag.over + 1;
    reorderTrack(drag.from, to).catch(() => undefined);
  };

  const cancelPointerReorder = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (pointerReorderRef.current?.pointerId !== event.pointerId) return;
    pointerReorderRef.current = null;
    setDragState(null);
  };

  const openSettings = () => {
    if (!playlist) return;
    setSettingsName(playlist.name);
    setSettingsError('');
    setDeleteConfirmOpen(false);
    setSettingsOpen(true);
  };

  const closeSettings = () => {
    if (settingsBusy) return;
    setSettingsOpen(false);
    setSettingsError('');
    setDeleteConfirmOpen(false);
  };

  const saveSettings = async (event: FormEvent) => {
    event.preventDefault();
    if (!playlist || settingsBusy) return;
    const name = settingsName.trim();
    if (!name) {
      setSettingsError('Playlist name is required');
      return;
    }
    setSettingsBusy(true);
    setSettingsError('');
    try {
      await savePlaylistName(playlist, name);
      await onRefresh();
      setDeleteConfirmOpen(false);
      setSettingsOpen(false);
    } catch (error) {
      setSettingsError(error instanceof Error ? error.message : 'Could not update playlist');
    } finally {
      setSettingsBusy(false);
    }
  };

  const deletePlaylist = async () => {
    if (settingsBusy) return;
    setSettingsBusy(true);
    setSettingsError('');
    try {
      await endpoints.deletePlaylist(playlist.id);
      await onRefresh();
      setSettingsOpen(false);
      onBack();
    } catch (error) {
      setSettingsError(error instanceof Error ? error.message : 'Could not delete playlist');
      setSettingsBusy(false);
    }
  };

  const formatPlaylistDate = (value: number) => {
    const timestamp = value < 1_000_000_000_000 ? value * 1000 : value;
    if (!Number.isFinite(timestamp) || timestamp <= 0) return 'Unknown';
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: 'medium',
      timeStyle: 'short'
    }).format(new Date(timestamp));
  };

  const createdLabel = formatPlaylistDate(playlistCreatedAt(playlist));
  const updatedLabel = formatPlaylistDate(playlistUpdatedAt(playlist));

  const exportPlaylistCsv = () => {
    try {
      const blob = new Blob([`\uFEFF${playlistCsv(items)}`], { type: 'text/csv;charset=utf-8' });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = playlistCsvFilename(playlist.name);
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
      setSettingsError('');
    } catch {
      setSettingsError('Could not export playlist');
    }
  };

  const openTrackAlbum = (item: QueueItem) => {
    const albumId = item.qobuzTrack?.album_id || item.resolvedSource?.album_id || item.albumId;
    if (!albumId) return;
    if (item.qobuzTrack) onOpenQobuzAlbum(albumId);
    else onOpenAlbum(albumId);
  };

  return (
    <section className="view playlist-detail-view">
      <div className="playlist-detail-shell">
        <div className="playlist-detail">
          <div className="playlist-detail-header">
            <div className="playlist-detail-art">
              <PlaylistCover playlist={playlist} />
            </div>
            <div className="playlist-detail-copy">
              <div className="section-label">Playlist</div>
              <h2 className={playlistTitleClass}>{playlist.name}</h2>
              <span>
                {songCountLabel(items.length)}
                {playlistDuration ? ` · ${playlistDuration}` : ''}
              </span>
              <div className="playlist-detail-actions">
                <div
                  className="album-play-split"
                  role="group"
                  aria-label="Playlist playback actions"
                >
                  <button
                    className="album-play-main"
                    type="button"
                    onClick={() => playPlaylist(playlist, playItems, false, 0, tracks)}
                  >
                    <PlaybarPlayIcon />
                    <span>Play now</span>
                  </button>
                  <button
                    className="album-play-menu-trigger"
                    type="button"
                    aria-label="Playlist queue options"
                    title="Playlist queue options"
                    onClick={(event) => {
                      event.stopPropagation();
                      const rect = event.currentTarget.getBoundingClientRect();
                      setQueueMenu(actionMenuPosition(rect, { menuHeight: 84 }));
                    }}
                  >
                    <Icon path="m6 9 6 6 6-6" />
                  </button>
                </div>
                <button
                  className="pill"
                  type="button"
                  onClick={() => playPlaylist(playlist, playItems, true, 0, tracks)}
                >
                  <ShuffleIcon />
                  Shuffle
                </button>
                <button
                  className="playlist-settings-trigger"
                  type="button"
                  aria-label="Playlist settings"
                  title="Playlist settings"
                  onClick={openSettings}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <circle cx="12" cy="12" r="1.4" />
                    <circle cx="12" cy="5" r="1.4" />
                    <circle cx="12" cy="19" r="1.4" />
                  </svg>
                </button>
              </div>
            </div>
          </div>
          <ol className="playlist-track-list">
            {items.length ? (
              items.map((item, index) => {
                const dropClass = dragState?.over === index ? ` drop-${dragState.placement}` : '';
                return (
                  <li
                    className={`playlist-track-row${dragState?.from === index ? ' is-dragging' : ''}${dropClass}`}
                    data-playlist-track={index}
                    draggable
                    key={`${item.title}-${item.filename || item.qobuzTrack?.id || index}-${index}`}
                    onClick={(event) => {
                      if ((event.target as Element).closest('.btn-item-more, .playlist-track-grip'))
                        return;
                      playPlaylist(playlist, playItems, false, index, tracks);
                    }}
                    onDragStart={(event) => {
                      setDragState({ from: index, over: index, placement: 'above' });
                      event.dataTransfer.effectAllowed = 'move';
                      try {
                        event.dataTransfer.setData('text/plain', String(index));
                      } catch {}
                    }}
                    onDragOver={(event) => {
                      if (!dragState) return;
                      event.preventDefault();
                      const rect = event.currentTarget.getBoundingClientRect();
                      const placement =
                        event.clientY - rect.top < rect.height / 2 ? 'above' : 'below';
                      setDragState({ from: dragState.from, over: index, placement });
                    }}
                    onDragLeave={() =>
                      setDragState((current) =>
                        current?.over === index ? { ...current, over: -1 } : current
                      )
                    }
                    onDragEnd={() => setDragState(null)}
                    onDrop={(event) => {
                      event.preventDefault();
                      if (!dragState) return;
                      const to = dragState.placement === 'above' ? index : index + 1;
                      const from = dragState.from;
                      setDragState(null);
                      reorderTrack(from, to).catch(() => undefined);
                    }}
                  >
                    <span className="track-row-hover-surface" aria-hidden="true" />
                    <button
                      className="playlist-track-grip"
                      type="button"
                      title="Drag to reorder"
                      aria-label="Drag to reorder"
                      onPointerDown={(event) => {
                        startPointerReorder(index, event);
                      }}
                      onPointerMove={movePointerReorder}
                      onPointerUp={finishPointerReorder}
                      onPointerCancel={cancelPointerReorder}
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true">
                        <circle cx="9" cy="6" r="1.4" />
                        <circle cx="9" cy="12" r="1.4" />
                        <circle cx="9" cy="18" r="1.4" />
                        <circle cx="15" cy="6" r="1.4" />
                        <circle cx="15" cy="12" r="1.4" />
                        <circle cx="15" cy="18" r="1.4" />
                      </svg>
                    </button>
                    <span className="playlist-track-index-control">
                      <span className="playlist-track-index">{index + 1}</span>
                      <button
                        className="playlist-track-play"
                        type="button"
                        title="Play"
                        aria-label={`Play ${item.title || 'Untitled'}`}
                        onClick={(event) => {
                          event.stopPropagation();
                          playPlaylist(playlist, playItems, false, index, tracks);
                        }}
                      >
                        <PlaybarPlayIcon className="playlist-track-play-icon" />
                      </button>
                    </span>
                    <div className="playlist-track-art">
                      <PlaylistTrackArt item={item} />
                    </div>
                    <div className="playlist-track-text">
                      <strong>{item.title || 'Untitled'}</strong>
                      <span>{subtitleForItem(item)}</span>
                    </div>
                    <span className="playlist-track-duration">
                      {item.durationSecs ? formatTime(item.durationSecs) : ''}
                    </span>
                    <button
                      className="btn-item-more"
                      type="button"
                      title="More options"
                      aria-label="More options"
                      onClick={(event) => {
                        event.stopPropagation();
                        const rect = event.currentTarget.getBoundingClientRect();
                        setTrackMenu({ index, ...actionMenuPosition(rect, { menuHeight: 231 }) });
                      }}
                    >
                      <svg
                        viewBox="0 0 24 24"
                        width="16"
                        height="16"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="2"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      >
                        <circle cx="12" cy="12" r="1" />
                        <circle cx="12" cy="5" r="1" />
                        <circle cx="12" cy="19" r="1" />
                      </svg>
                    </button>
                  </li>
                );
              })
            ) : (
              <li className="now-playing-empty">Add songs from any three-dot menu.</li>
            )}
          </ol>
        </div>
      </div>
      {queueMenu ? (
        <Menu
          className="track-actions-menu track-actions-menu-wide is-open"
          ariaLabel="Playlist queue options"
          style={{ left: Math.max(12, queueMenu.x), top: queueMenu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              addItemsToQueue(queueItemsForPlayback(playlist, false, tracks), 'next');
              setQueueMenu(null);
            }}
          >
            <PlayNextIcon />
            <span>Add playlist next</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              addItemsToQueue(queueItemsForPlayback(playlist, false, tracks), 'end');
              setQueueMenu(null);
            }}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add playlist to queue</span>
          </button>
        </Menu>
      ) : null}
      {trackMenu && items[trackMenu.index] ? (
        <Menu
          className="track-actions-menu track-actions-menu-wide is-open"
          ariaLabel="Track options"
          style={{ left: Math.max(12, trackMenu.x), top: trackMenu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="track-action-item has-filled-icon"
            type="button"
            role="menuitem"
            onClick={() => {
              playPlaylist(playlist, playItems, false, trackMenu.index, tracks);
              setTrackMenu(null);
            }}
          >
            <PlaybarPlayIcon className="track-action-play-icon" />
            <span>Play from here</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              removeTrack(trackMenu.index).catch(() => undefined);
              setTrackMenu(null);
            }}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
            <span>Remove from playlist</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              addItemsToQueue([sourceTrack(items[trackMenu.index])], 'next');
              setTrackMenu(null);
            }}
          >
            <PlayNextIcon />
            <span>Add next</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              addItemsToQueue([sourceTrack(items[trackMenu.index])], 'end');
              setTrackMenu(null);
            }}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add to queue</span>
          </button>
          {items[trackMenu.index].albumId ||
          items[trackMenu.index].qobuzTrack?.album_id ||
          items[trackMenu.index].resolvedSource?.album_id ? (
            <button
              className="track-action-item"
              type="button"
              role="menuitem"
              onClick={() => {
                openTrackAlbum(items[trackMenu.index]);
                setTrackMenu(null);
              }}
            >
              <Icon path="M5 4h14v16H5zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM12 12h.01" />
              <span>Go to album</span>
            </button>
          ) : null}
          {items[trackMenu.index].artist ? (
            <button
              className="track-action-item"
              type="button"
              role="menuitem"
              onClick={() => {
                onOpenArtist(items[trackMenu.index].artist);
                setTrackMenu(null);
              }}
            >
              <Icon path="M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM4 20c1.8-4 4.5-6 8-6s6.2 2 8 6" />
              <span>Go to artist</span>
            </button>
          ) : null}
        </Menu>
      ) : null}
      {settingsOpen ? (
        <Modal
          open
          className="playlist-confirm-backdrop playlist-settings-backdrop is-open"
          ariaLabelledBy="playlist-settings-title"
          onClose={closeSettings}
        >
          <div className="playlist-confirm-panel app-modal-surface">
            <header className="playlist-confirm-head">
              <div>
                <div className="section-label">Playlist settings</div>
                <h2 id="playlist-settings-title">{playlist.name}</h2>
              </div>
              <button
                className="global-search-close"
                type="button"
                aria-label="Close"
                disabled={settingsBusy}
                onClick={closeSettings}
              >
                <Icon path="M18 6 6 18M6 6l12 12" />
              </button>
            </header>
            <form className="playlist-confirm-body playlist-settings-body" onSubmit={saveSettings}>
              <label className="zone-settings-field">
                <span>Name</span>
                <input
                  className="zone-settings-input"
                  type="text"
                  value={settingsName}
                  disabled={settingsBusy}
                  onChange={(event) => setSettingsName(event.currentTarget.value)}
                />
              </label>
              <dl className="playlist-settings-meta">
                <div>
                  <dt>Created</dt>
                  <dd>{createdLabel}</dd>
                </div>
                <div>
                  <dt>Last edited</dt>
                  <dd>{updatedLabel}</dd>
                </div>
              </dl>
              {settingsError ? <p className="playlist-settings-error">{settingsError}</p> : null}
              <div className="playlist-confirm-actions playlist-settings-actions">
                <div className="playlist-settings-actions-start">
                  <button
                    className="playlist-settings-delete"
                    type="button"
                    disabled={settingsBusy}
                    title={`Delete playlist. Its ${songCountLabel(items.length)} will stay in your library.`}
                    onClick={() => setDeleteConfirmOpen(true)}
                  >
                    <Icon path="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
                    Delete
                  </button>
                  <button
                    className="pill playlist-settings-export"
                    type="button"
                    aria-label="Export playlist as CSV"
                    title="Export playlist as CSV"
                    disabled={settingsBusy}
                    onClick={exportPlaylistCsv}
                  >
                    <Icon path="M12 3v12M7 8l5-5 5 5M5 21h14" />
                    CSV
                  </button>
                </div>
                <div className="playlist-settings-actions-end">
                  <button className="pill primary" type="submit" disabled={settingsBusy}>
                    Save
                  </button>
                </div>
              </div>
            </form>
          </div>
        </Modal>
      ) : null}
      <Modal
        open={settingsOpen && deleteConfirmOpen}
        className="playlist-confirm-backdrop is-open"
        ariaLabelledBy="playlist-delete-title"
        onClose={() => {
          if (!settingsBusy) setDeleteConfirmOpen(false);
        }}
      >
        <div className="playlist-confirm-panel app-modal-surface">
          <header className="playlist-confirm-head">
            <div>
              <div className="section-label">Delete playlist</div>
              <h2 id="playlist-delete-title">Delete “{playlist.name}”?</h2>
            </div>
          </header>
          <div className="playlist-confirm-body">
            <p>
              This cannot be undone. The playlist's {songCountLabel(items.length)} will remain in
              your library.
            </p>
            {settingsError ? <p className="playlist-settings-error">{settingsError}</p> : null}
            <div className="playlist-confirm-actions">
              <button
                className="pill"
                type="button"
                disabled={settingsBusy}
                onClick={() => setDeleteConfirmOpen(false)}
              >
                Cancel
              </button>
              <button
                className="playlist-settings-delete"
                type="button"
                disabled={settingsBusy}
                onClick={() => {
                  deletePlaylist().catch(() => undefined);
                }}
              >
                {settingsBusy ? 'Deleting…' : 'Delete playlist'}
              </button>
            </div>
          </div>
        </div>
      </Modal>
    </section>
  );
}
