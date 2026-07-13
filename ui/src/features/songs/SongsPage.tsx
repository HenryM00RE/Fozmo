import { useEffect, useMemo, useRef, useState } from 'react';
import { albumArt, albumListenCount, artistOf, titleOf } from '../../shared/lib/appSupport';
import { formatTime, stripFileExtension } from '../../shared/lib/format';
import { localTrackToQueueItem } from '../../shared/lib/queue';
import type {
  LibraryBrowseParams,
  LibraryFacetOption,
  LibraryTrack,
  QueueItem,
  ResolvedPlaySource
} from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import { Menu } from '../../shared/ui/Menu';
import { actionMenuPosition } from '../../shared/ui/menuPosition';
import { PlaybarPlayIcon } from '../../shared/ui/PlaybarPlayIcon';
import { PlayNextIcon } from '../../shared/ui/PlayNextIcon';
import { SelectMenu } from '../../shared/ui/SelectMenu';
import { SetupNotice } from '../../shared/ui/SetupNotice';
import { useActionMenuScrollLock } from '../../shared/ui/useActionMenuScrollLock';
import {
  type AlbumSelectionItem,
  albumTrackSelectionKeyForQueueItem
} from '../albums/model/albumModel';
import { usePagedLibraryBrowse } from '../library/hooks/usePagedLibraryBrowse';

const songsPerPage = 20;
const sortOptions = [
  { value: 'popularity', label: 'Popularity' },
  { value: 'name', label: 'Name' },
  { value: 'releaseDate', label: 'Release Date' }
];

type SongSortKey = 'popularity' | 'name' | 'releaseDate';
type SongSortDirection = 'desc' | 'asc';

export function SongsPage({
  addItemsToQueue,
  onOpenAlbum,
  onPlay,
  onSelectionItemsChange,
  onToggleSelection,
  openPlaylistPickerForItems,
  selectedTrackKeys,
  selectionActive,
  onOpenMusicFolders
}: {
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  onOpenAlbum: (id: string | number) => void;
  onPlay: (track: LibraryTrack) => void;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  onSelectionItemsChange: (items: AlbumSelectionItem[]) => void;
  onToggleSelection: (key: string) => void;
  selectedTrackKeys: Set<string>;
  selectionActive: boolean;
  onOpenMusicFolders: () => void;
}) {
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [sortKey, setSortKey] = useState<SongSortKey>('popularity');
  const [sortDirection, setSortDirection] = useState<SongSortDirection>('desc');
  const [sortOpen, setSortOpen] = useState(false);
  const [genre, setGenre] = useState('');
  const [decade, setDecade] = useState('');
  const [quality, setQuality] = useState('');
  const [trackMenu, setTrackMenu] = useState<{ index: number; x: number; y: number } | null>(null);
  const sortRootRef = useRef<HTMLDivElement | null>(null);
  useActionMenuScrollLock(Boolean(trackMenu));
  const browseParams = useMemo<LibraryBrowseParams>(
    () => ({
      q: query,
      limit: songsPerPage,
      offset: page * songsPerPage,
      sort: sortKey,
      direction: sortDirection,
      genre: genre || null,
      decade: decade || null,
      quality: quality || null
    }),
    [decade, genre, page, quality, query, sortDirection, sortKey]
  );
  const browse = usePagedLibraryBrowse('tracks', browseParams);
  const pageTracks = browse.page.items;
  const totalTracks = browse.page.total;
  const pageCount = Math.max(1, Math.ceil(totalTracks / songsPerPage));
  const currentPage = Math.min(page, pageCount - 1);
  const pageStart = currentPage * songsPerPage;
  const showingStart = totalTracks ? pageStart + 1 : 0;
  const showingEnd = Math.min(pageStart + pageTracks.length, totalTracks);
  const indicator = songPageIndicator({
    query,
    showingEnd,
    showingStart,
    totalCount: totalTracks
  });

  useEffect(() => {
    setPage(0);
  }, [decade, genre, quality, query, sortDirection, sortKey]);

  useEffect(() => {
    setPage((current) => Math.min(current, Math.max(0, pageCount - 1)));
  }, [pageCount]);

  const selectionItems = useMemo(
    () =>
      pageTracks
        .map((track, index) => {
          const item = localTrackToQueueItem(track);
          const key = songSelectionKey(track, pageStart + index);
          return key ? { key, item } : null;
        })
        .filter(Boolean) as AlbumSelectionItem[],
    [pageStart, pageTracks]
  );

  useEffect(() => {
    onSelectionItemsChange(selectionItems);
  }, [onSelectionItemsChange, selectionItems]);

  useEffect(() => {
    if (!sortOpen) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest('.app-select-menu')) return;
      if (!sortRootRef.current?.contains(target)) setSortOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setSortOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [sortOpen]);

  useEffect(() => {
    const closeMenu = () => setTrackMenu(null);
    window.addEventListener('click', closeMenu);
    window.addEventListener('keydown', closeMenu);
    return () => {
      window.removeEventListener('click', closeMenu);
      window.removeEventListener('keydown', closeMenu);
    };
  }, []);

  return (
    <section className="view songs-view">
      <div className="library-page-heading">
        <div>
          <div className="section-label">My library</div>
          <h1>Songs</h1>
        </div>
      </div>
      <div className="songs-toolbar">
        <div className="songs-search-cluster" ref={sortRootRef}>
          <label className="songs-search-field">
            <span className="sr-only">Search songs or artists</span>
            <Icon path="M10.5 18a7.5 7.5 0 1 1 5.3-12.8 7.5 7.5 0 0 1-5.3 12.8Zm5.3-2.2L21 21" />
            <input
              type="search"
              value={query}
              autoComplete="off"
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search songs or artists"
            />
          </label>
          <button
            className={`songs-filter-button${sortOpen ? ' is-open' : ''}`}
            type="button"
            aria-label="Sort songs"
            aria-haspopup="dialog"
            aria-expanded={sortOpen}
            title="Sort songs"
            onClick={() => setSortOpen((open) => !open)}
          >
            <Icon path="M4 21v-7m0-4V3m8 18v-9m0-4V3m8 18v-5m0-4V3M2 14h4M10 8h4m4 8h4" />
          </button>
          {sortOpen ? (
            <div className="songs-sort-popover" role="dialog" aria-label="Sort songs">
              <div className="songs-sort-row">
                <span>Sort by</span>
                <SelectMenu
                  ariaLabel="Sort songs by"
                  className="songs-sort-select"
                  menuMinWidth={170}
                  value={sortKey}
                  onChange={(value) => setSortKey(value as SongSortKey)}
                  options={sortOptions}
                />
              </div>
              <div className="songs-sort-direction" aria-label="Sort direction">
                <button
                  className={`songs-sort-direction-button${sortDirection === 'desc' ? ' is-active' : ''}`}
                  type="button"
                  aria-pressed={sortDirection === 'desc'}
                  title={sortDirectionLabel(sortKey, 'desc')}
                  onClick={() => setSortDirection('desc')}
                >
                  <Icon path="m6 9 6 6 6-6" />
                  <span>{sortDirectionLabel(sortKey, 'desc')}</span>
                </button>
                <button
                  className={`songs-sort-direction-button${sortDirection === 'asc' ? ' is-active' : ''}`}
                  type="button"
                  aria-pressed={sortDirection === 'asc'}
                  title={sortDirectionLabel(sortKey, 'asc')}
                  onClick={() => setSortDirection('asc')}
                >
                  <Icon path="m18 15-6-6-6 6" />
                  <span>{sortDirectionLabel(sortKey, 'asc')}</span>
                </button>
              </div>
            </div>
          ) : null}
        </div>
        <div className="songs-page-status" aria-live="polite">
          {indicator}
        </div>
        <nav className="songs-pagination" aria-label="Songs pages">
          <button
            className="songs-page-button"
            type="button"
            aria-label="Previous songs page"
            disabled={currentPage === 0}
            onClick={() => setPage((value) => Math.max(0, value - 1))}
          >
            <Icon path="m15 18-6-6 6-6" />
          </button>
          <span>
            {currentPage + 1} / {pageCount}
          </span>
          <button
            className="songs-page-button"
            type="button"
            aria-label="Next songs page"
            disabled={currentPage >= pageCount - 1}
            onClick={() => setPage((value) => Math.min(pageCount - 1, value + 1))}
          >
            <Icon path="m9 18 6-6-6-6" />
          </button>
        </nav>
      </div>
      <div className="library-facet-bar" aria-label="Song filters">
        <FacetSelect
          label="Genre"
          value={genre}
          options={browse.page.facets?.genres}
          onChange={setGenre}
        />
        <FacetSelect
          label="Decade"
          value={decade}
          options={browse.page.facets?.decades}
          onChange={setDecade}
        />
        <FacetSelect
          label="Quality"
          value={quality}
          options={browse.page.facets?.qualities}
          onChange={setQuality}
        />
      </div>
      <section className="playlist-panel album-track-panel songs-track-panel">
        {browse.loading && !browse.loaded ? (
          <TrackListSkeleton count={songsPerPage} />
        ) : browse.error && !pageTracks.length ? (
          <div className="songs-empty-state library-empty-state">Songs are unavailable.</div>
        ) : (
          <>
            <TrackList
              tracks={pageTracks}
              pageStart={pageStart}
              onPlay={(track) => onPlay(track)}
              onToggleSelection={onToggleSelection}
              selectedKeys={selectedTrackKeys}
              selectionActive={selectionActive}
              onOpenMenu={(index, rect) =>
                setTrackMenu({ index, ...actionMenuPosition(rect, { menuHeight: 193 }) })
              }
            />
            {!pageTracks.length ? (
              query.trim() || hasSongFacet(genre, decade, quality) ? (
                <div className="songs-empty-state">No songs match those filters.</div>
              ) : (
                <SetupNotice
                  actionLabel="Choose a music folder"
                  message="Add a local music folder to see your songs here."
                  onAction={onOpenMusicFolders}
                />
              )
            ) : null}
          </>
        )}
      </section>
      {trackMenu && pageTracks[trackMenu.index] ? (
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
              onPlay(pageTracks[trackMenu.index]);
              setTrackMenu(null);
            }}
          >
            <PlaybarPlayIcon className="track-action-play-icon" />
            <span>Play</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              const item = localTrackToQueueItem(pageTracks[trackMenu.index]);
              addItemsToQueue([item], 'next');
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
              const item = localTrackToQueueItem(pageTracks[trackMenu.index]);
              openPlaylistPickerForItems([item], item.title || 'Track');
              setTrackMenu(null);
            }}
          >
            <Icon path="M4 7h12M4 12h9M4 17h7M18 15v6M15 18h6" />
            <span>Add to playlist</span>
          </button>
          {hasAlbumRoute(pageTracks[trackMenu.index]) ? (
            <button
              className="track-action-item"
              type="button"
              role="menuitem"
              onClick={() => {
                const albumId = pageTracks[trackMenu.index].album_id;
                if (albumId !== null && albumId !== undefined && albumId !== '')
                  onOpenAlbum(albumId);
                setTrackMenu(null);
              }}
            >
              <Icon path="M5 4h14v16H5zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM12 12h.01" />
              <span>Go to album</span>
            </button>
          ) : null}
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              const item = localTrackToQueueItem(pageTracks[trackMenu.index]);
              addItemsToQueue([item], 'end');
              setTrackMenu(null);
            }}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add to queue</span>
          </button>
        </Menu>
      ) : null}
    </section>
  );
}

function FacetSelect({
  label,
  onChange,
  options,
  value
}: {
  label: string;
  onChange: (value: string) => void;
  options?: LibraryFacetOption[];
  value: string;
}) {
  const selectOptions = [
    { value: '', label: `All ${label.toLowerCase()}` },
    ...(options || []).map((option) => ({
      value: option.value,
      label: `${option.label} (${option.count})`
    }))
  ];
  return (
    <SelectMenu
      ariaLabel={`${label} filter`}
      className="library-facet-select"
      menuMinWidth={190}
      value={value}
      onChange={onChange}
      options={selectOptions}
    />
  );
}

function TrackListSkeleton({ count }: { count: number }) {
  return (
    <ul
      className="file-list song-list library-loading-list"
      aria-label="Loading songs"
      aria-busy="true"
    >
      {Array.from({ length: count }, (_, index) => (
        <li className="file-item album-track-item songs-track-row library-loading-row" key={index}>
          <span className="track-row-hover-surface" aria-hidden="true" />
          <span className="songs-track-art skeleton-shimmer" />
          <div className="file-details songs-track-details">
            <span className="library-loading-title skeleton-shimmer" />
            <span className="library-loading-meta skeleton-shimmer" />
          </div>
          <span className="song-meta-cell album-track-duration library-loading-small skeleton-shimmer" />
          <span className="album-track-play-count library-loading-small skeleton-shimmer" />
          <span className="btn-item-more library-loading-icon skeleton-shimmer" />
        </li>
      ))}
    </ul>
  );
}

function TrackList({
  onOpenMenu,
  onPlay,
  onToggleSelection,
  pageStart,
  selectedKeys,
  selectionActive,
  tracks
}: {
  tracks: LibraryTrack[];
  pageStart: number;
  onOpenMenu: (index: number, rect: DOMRect) => void;
  onPlay: (track: LibraryTrack, index: number) => void;
  onToggleSelection: (key: string) => void;
  selectedKeys: Set<string>;
  selectionActive: boolean;
}) {
  return (
    <ul className="file-list song-list">
      {tracks.map((track, index) => {
        const absoluteIndex = pageStart + index;
        const title = titleOf(track, stripFileExtension(track.file_name));
        const artist = artistOf(track) || 'Unknown artist';
        const album = String(track.album || 'Unknown album');
        const art = albumArt(
          (track.preferred_play_source as ResolvedPlaySource | null | undefined) || track
        );
        const plays = albumListenCount(track);
        const selectionKey = songSelectionKey(track, absoluteIndex);
        const selected = selectionKey ? selectedKeys.has(selectionKey) : false;
        const playOrSelect = () => {
          if (selectionActive && selectionKey) onToggleSelection(selectionKey);
          else onPlay(track, index);
        };
        return (
          <li
            className={`file-item album-track-item songs-track-row${selectionActive ? ' is-selection-mode' : ''}${selected ? ' is-selected' : ''}`}
            key={String(track.id || track.track_id || track.file_name || index)}
            data-album-track-selection-key={selectionKey}
            onClick={playOrSelect}
            onContextMenu={(event) => {
              event.preventDefault();
              event.stopPropagation();
              if (selectionKey) onToggleSelection(selectionKey);
            }}
          >
            <span className="track-row-hover-surface" aria-hidden="true" />
            <div className="album-track-index songs-track-art-cell">
              <span className={`songs-track-art${art ? ' has-cover' : ''}`} aria-hidden="true">
                {art ? (
                  <img alt="" src={art} loading="lazy" />
                ) : (
                  <Icon path="M9 18V5l12-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm12-2a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z" />
                )}
              </span>
              <button
                className="album-track-check"
                type="button"
                title={selected ? 'Deselect track' : 'Select track'}
                aria-label={selected ? 'Deselect track' : 'Select track'}
                aria-pressed={selected}
                onClick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  if (selectionKey) onToggleSelection(selectionKey);
                }}
              >
                <Icon path="M20 6 9 17l-5-5" />
              </button>
              <button
                className="btn-item-play"
                type="button"
                aria-label={`Play ${title}`}
                onClick={(event) => {
                  event.stopPropagation();
                  if (selectionActive && selectionKey) onToggleSelection(selectionKey);
                  else onPlay(track, index);
                }}
              >
                <svg
                  className="songs-track-play-icon"
                  viewBox="0 0 100 100"
                  aria-hidden="true"
                  shapeRendering="geometricPrecision"
                >
                  <path d="M 39 32 C 36.2 33.3 35 35.7 35 39 L 35 61 C 35 64.3 36.2 66.7 39 68 C 41.4 69.1 43.5 68.2 46.2 66.6 L 66.4 54.4 C 71.2 51.5 71.2 48.5 66.4 45.6 L 46.2 33.4 C 43.5 31.8 41.4 30.9 39 32 Z" />
                </svg>
              </button>
            </div>
            <div className="file-details songs-track-details">
              <span className="file-name" title={title}>
                {title}
              </span>
              <span className="file-subline" title={`${artist} / ${album}`}>
                <span>{artist}</span>
                <span className="file-subline-sep">/</span>
                <span>{album}</span>
              </span>
            </div>
            <span className="song-meta-cell album-track-duration">
              {formatTime(track.duration_secs)}
            </span>
            <span
              className={`album-track-play-count${plays === 0 ? ' is-empty' : ''}`}
              title={`${plays} listen${plays === 1 ? '' : 's'}`}
              aria-label={`${plays} listen${plays === 1 ? '' : 's'}`}
            >
              <span className="album-track-listen-icon" aria-hidden="true">
                ▶
              </span>
              <span className="album-track-listen-value">{plays}</span>
            </span>
            <button
              className="btn-item-more"
              type="button"
              title="More options"
              aria-label={`More options for ${title}`}
              onClick={(event) => {
                event.stopPropagation();
                onOpenMenu(index, event.currentTarget.getBoundingClientRect());
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
      })}
    </ul>
  );
}

function songSelectionKey(track: LibraryTrack, fallback: string | number) {
  return albumTrackSelectionKeyForQueueItem(localTrackToQueueItem(track), fallback);
}

function hasAlbumRoute(track: LibraryTrack) {
  const albumId = track.album_id;
  return albumId !== null && albumId !== undefined && albumId !== '';
}

function sortDirectionLabel(sortKey: SongSortKey, direction: SongSortDirection) {
  if (sortKey === 'popularity') return direction === 'desc' ? 'Most popular' : 'Least popular';
  if (sortKey === 'releaseDate') return direction === 'desc' ? 'Newest first' : 'Oldest first';
  return direction === 'desc' ? 'Z to A' : 'A to Z';
}

function hasSongFacet(...values: string[]) {
  return values.some((value) => value.trim());
}

function songPageIndicator({
  query,
  showingEnd,
  showingStart,
  totalCount
}: {
  query: string;
  showingEnd: number;
  showingStart: number;
  totalCount: number;
}) {
  const totalLabel = `${totalCount.toLocaleString()} song${totalCount === 1 ? '' : 's'}`;
  const visibleRange =
    showingStart === showingEnd
      ? showingStart.toLocaleString()
      : `${showingStart.toLocaleString()}-${showingEnd.toLocaleString()}`;
  if (query.trim()) {
    const matchLabel = `${totalCount.toLocaleString()} match${totalCount === 1 ? '' : 'es'}`;
    return totalCount ? `Showing ${visibleRange} of ${matchLabel}` : `Showing 0 of ${matchLabel}`;
  }
  if (!totalCount) return 'Showing 0 songs';
  return `Showing ${visibleRange} of ${totalLabel}`;
}
