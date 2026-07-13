import { useEffect, useMemo, useState } from 'react';
import {
  descriptionParagraphs,
  idValue,
  plainDescription,
  titleOf
} from '../../../shared/lib/appSupport';
import { displayTitleUsesFallbackFont } from '../../../shared/lib/displayTitle';
import { formatTime } from '../../../shared/lib/format';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { JsonRecord, Playlist, QueueItem } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Menu } from '../../../shared/ui/Menu';
import { actionMenuPosition } from '../../../shared/ui/menuPosition';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { PlayNextIcon } from '../../../shared/ui/PlayNextIcon';
import { ShuffleIcon } from '../../../shared/ui/ShuffleIcon';
import { useActionMenuScrollLock } from '../../../shared/ui/useActionMenuScrollLock';
import { AlbumDescriptionModal } from '../../albums/components/AlbumDescriptionModal';
import { PlaylistCover } from '../../playlists/components/PlaylistCover';
import { PlaylistTrackArt } from '../../playlists/components/PlaylistTrackArt';
import { songCountLabel, subtitleForItem } from '../../playlists/model/playlistModel';
import { QobuzPlaylistArtwork } from '../components/QobuzPlaylistArtwork';
import {
  loadQobuzPlaylistDetail,
  qobuzPlaylistImage,
  qobuzPlaylistQueueItems
} from '../model/qobuzPlaylistData';

function shuffledItems(items: QueueItem[]) {
  return [...items].sort(() => Math.random() - 0.5);
}

type QueueMenuState = {
  x: number;
  y: number;
} | null;

type TrackMenuState = {
  index: number;
  x: number;
  y: number;
} | null;

export function QobuzPlaylistPage({
  id,
  onOpenArtist,
  onOpenQobuzAlbum,
  playItems,
  addItemsToQueue,
  customDisplayFont
}: {
  id?: string | number | null;
  onOpenArtist: (name: string) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  customDisplayFont: CustomDisplayFontSettings | null;
}) {
  const [detail, setDetail] = useState<JsonRecord | null>(null);
  const [error, setError] = useState('');
  const [queueMenu, setQueueMenu] = useState<QueueMenuState>(null);
  const [trackMenu, setTrackMenu] = useState<TrackMenuState>(null);
  const [descriptionOpen, setDescriptionOpen] = useState(false);
  const playlist = (detail?.playlist as JsonRecord | undefined) || null;
  const tracks = useMemo(() => qobuzPlaylistQueueItems(detail), [detail]);
  const artwork = qobuzPlaylistImage(detail);
  const title = titleOf(playlist, 'Qobuz playlist');
  const titleLengthClass =
    title.length > 58 ? ' is-extra-long-title' : title.length > 38 ? ' is-long-title' : '';
  const playlistTitleClass = `playlist-detail-title album-detail-title${titleLengthClass}${displayTitleUsesFallbackFont(title, customDisplayFont) ? ' uses-fallback-font' : ''}`;
  const owner = String(playlist?.owner || '').trim();
  const rawDescription =
    playlist?.description ||
    detail?.description ||
    playlist?.about ||
    detail?.about ||
    playlist?.description_html ||
    detail?.description_html;
  const description = plainDescription(rawDescription);
  const descriptionBlocks = descriptionParagraphs(rawDescription);
  const trackCount =
    tracks.length || Number(playlist?.tracks_count ?? playlist?.track_count ?? 0) || 0;
  const coverPlaylist = useMemo<Playlist>(
    () => ({
      id: String(id ?? playlist?.id ?? 'qobuz-playlist'),
      name: title,
      items: tracks
    }),
    [id, playlist?.id, title, tracks]
  );

  useActionMenuScrollLock(Boolean(queueMenu || trackMenu || descriptionOpen));

  useEffect(() => {
    const close = () => {
      setQueueMenu(null);
      setTrackMenu(null);
    };
    window.addEventListener('click', close);
    window.addEventListener('keydown', close);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', close);
    };
  }, []);

  useEffect(() => {
    if (id === null || id === undefined) return;
    let cancelled = false;
    setDetail(null);
    setError('');
    loadQobuzPlaylistDetail(id)
      .then((next) => {
        if (!cancelled) setDetail(next);
      })
      .catch((err) => {
        if (!cancelled)
          setError(err instanceof Error ? err.message : 'Could not load this Qobuz playlist');
      });
    return () => {
      cancelled = true;
    };
  }, [id]);

  const play = (startIndex = 0, shuffle = false) => {
    const items = shuffle ? shuffledItems(tracks) : tracks;
    if (items.length) playItems(items, shuffle ? 0 : startIndex);
  };

  const queue = (placement: 'next' | 'end') => {
    if (tracks.length) addItemsToQueue(tracks, placement);
  };

  const queueTrack = (index: number, placement: 'next' | 'end') => {
    const item = tracks[index];
    if (item) addItemsToQueue([item], placement);
  };

  const openTrackAlbum = (item: QueueItem) => {
    const albumId = idValue(item.qobuzTrack?.album_id || item.albumId);
    if (albumId) onOpenQobuzAlbum(albumId);
  };

  return (
    <section className="view playlist-detail-view qobuz-playlist-detail-view">
      <div className="playlist-detail-shell">
        <div className="playlist-detail">
          <div className="playlist-detail-header">
            <div className="playlist-detail-art">
              {artwork ? (
                <QobuzPlaylistArtwork src={artwork} />
              ) : (
                <PlaylistCover playlist={coverPlaylist} />
              )}
            </div>
            <div className="playlist-detail-copy">
              <div className="section-label">Qobuz playlist</div>
              <h2 className={playlistTitleClass}>{title}</h2>
              {description ? (
                <button
                  className="album-detail-about qobuz-playlist-about qobuz-playlist-description"
                  type="button"
                  aria-label="Read full description"
                  onClick={() => setDescriptionOpen(true)}
                >
                  <div className="album-detail-about-text">{description}</div>
                  <span className="album-detail-about-more" aria-hidden="true">
                    More
                  </span>
                </button>
              ) : null}
              <div className="playlist-detail-actions">
                <div
                  className="album-play-split"
                  role="group"
                  aria-label="Qobuz playlist playback actions"
                >
                  <button
                    className="album-play-main"
                    type="button"
                    disabled={!tracks.length}
                    onClick={() => play()}
                  >
                    <PlaybarPlayIcon />
                    <span>Play now</span>
                  </button>
                  <button
                    className="album-play-menu-trigger"
                    type="button"
                    disabled={!tracks.length}
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
                  disabled={!tracks.length}
                  onClick={() => play(0, true)}
                >
                  <ShuffleIcon />
                  Shuffle
                </button>
              </div>
            </div>
          </div>
          {error ? (
            <div className="playlist-empty">
              <strong>{error}</strong>
            </div>
          ) : null}
          {!detail && !error ? (
            <div className="playlist-empty">
              <strong>Loading playlist</strong>
            </div>
          ) : null}
          {detail && !tracks.length && !error ? (
            <div className="playlist-empty">
              <strong>No streamable tracks found</strong>
            </div>
          ) : null}
          {tracks.length ? (
            <ol className="playlist-track-list qobuz-playlist-track-list">
              {tracks.map((item, index) => (
                <li
                  className="playlist-track-row"
                  data-playlist-track={index}
                  key={`${item.qobuzTrack?.id || item.title}-${index}`}
                  onClick={(event) => {
                    if ((event.target as Element).closest('.btn-item-more')) return;
                    play(index);
                  }}
                >
                  <span className="track-row-hover-surface" aria-hidden="true" />
                  <span
                    className="playlist-track-grip qobuz-playlist-track-spacer"
                    aria-hidden="true"
                  />
                  <span className="playlist-track-index-control">
                    <span className="playlist-track-index">{index + 1}</span>
                    <button
                      className="playlist-track-play"
                      type="button"
                      title="Play"
                      aria-label={`Play ${item.title || 'Untitled'}`}
                      onClick={(event) => {
                        event.stopPropagation();
                        play(index);
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
                      setTrackMenu({ index, ...actionMenuPosition(rect, { menuHeight: 193 }) });
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
              ))}
            </ol>
          ) : null}
        </div>
      </div>
      {queueMenu ? (
        <Menu
          className="track-actions-menu track-actions-menu-wide is-open"
          ariaLabel="Qobuz playlist queue options"
          style={{ left: Math.max(12, queueMenu.x), top: queueMenu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              queue('next');
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
              queue('end');
              setQueueMenu(null);
            }}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add playlist to queue</span>
          </button>
        </Menu>
      ) : null}
      {trackMenu && tracks[trackMenu.index] ? (
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
              play(trackMenu.index);
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
              queueTrack(trackMenu.index, 'next');
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
              queueTrack(trackMenu.index, 'end');
              setTrackMenu(null);
            }}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add to queue</span>
          </button>
          {idValue(
            tracks[trackMenu.index].qobuzTrack?.album_id || tracks[trackMenu.index].albumId
          ) ? (
            <button
              className="track-action-item"
              type="button"
              role="menuitem"
              onClick={() => {
                openTrackAlbum(tracks[trackMenu.index]);
                setTrackMenu(null);
              }}
            >
              <Icon path="M5 4h14v16H5zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM12 12h.01" />
              <span>Go to album</span>
            </button>
          ) : null}
          {tracks[trackMenu.index].artist ? (
            <button
              className="track-action-item"
              type="button"
              role="menuitem"
              onClick={() => {
                onOpenArtist(tracks[trackMenu.index].artist);
                setTrackMenu(null);
              }}
            >
              <Icon path="M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM4 20c1.8-4 4.5-6 8-6s6.2 2 8 6" />
              <span>Go to artist</span>
            </button>
          ) : null}
        </Menu>
      ) : null}
      {descriptionOpen ? (
        <AlbumDescriptionModal
          title={title}
          artist={owner || 'Qobuz'}
          year={trackCount ? songCountLabel(trackCount) : undefined}
          paragraphs={descriptionBlocks.length ? descriptionBlocks : [description]}
          onClose={() => setDescriptionOpen(false)}
        />
      ) : null}
    </section>
  );
}
