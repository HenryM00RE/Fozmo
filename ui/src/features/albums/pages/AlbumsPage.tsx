import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  idValue,
  normalizeQobuzAlbumId,
  resolveLocalAlbumId
} from '../../../shared/lib/appSupport';
import type {
  LibraryAlbum,
  LibraryBrowsePage,
  LibraryBrowseParams,
  LibraryFacetOption
} from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { SetupNotice } from '../../../shared/ui/SetupNotice';
import { AlbumGrid } from '../components/AlbumGrid';
import { loadFavoriteAlbumsCached, loadQobuzAlbumsCached } from '../model/albumData';
import { isQobuzFavoriteAlbum } from '../model/albumFavorites';

const albumSkeletonCount = 24;
const allAlbumsRequestLimit = 160;
const albumSortOptions = [
  { value: 'popularity', label: 'Popularity' },
  { value: 'name', label: 'Name' },
  { value: 'releaseDate', label: 'Release Date' }
];

type AlbumSortKey = 'popularity' | 'name' | 'releaseDate';
type AlbumSortDirection = 'desc' | 'asc';

export function AlbumsPage({
  albums,
  onOpen,
  onOpenQobuzAlbum,
  onPlay,
  onPlayQobuzAlbum,
  onOpenArtist,
  selectedAlbumKeys,
  albumSelectionActive,
  onToggleAlbumSelection,
  onOpenMusicFolders
}: {
  albums: LibraryAlbum[];
  onOpen: (id: string | number) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlay: (id: string | number) => void;
  onPlayQobuzAlbum: (id: string | number) => void;
  onOpenArtist: (name: string) => void;
  selectedAlbumKeys: Set<string>;
  albumSelectionActive: boolean;
  onToggleAlbumSelection: (album: LibraryAlbum) => void;
  onOpenMusicFolders: () => void;
}) {
  const [favorites, setFavorites] = useState<LibraryAlbum[]>([]);
  const [qobuzLoggedIn, setQobuzLoggedIn] = useState(false);
  const [qobuzAlbums, setQobuzAlbums] = useState<LibraryAlbum[]>([]);
  const [allAlbumsQuery, setAllAlbumsQuery] = useState('');
  const [allAlbumsSortKey, setAllAlbumsSortKey] = useState<AlbumSortKey>('popularity');
  const [allAlbumsSortDirection, setAllAlbumsSortDirection] = useState<AlbumSortDirection>('desc');
  const [allAlbumsSortOpen, setAllAlbumsSortOpen] = useState(false);
  const [allAlbumsGenre, setAllAlbumsGenre] = useState('');
  const [allAlbumsDecade, setAllAlbumsDecade] = useState('');
  const [allAlbumsQuality, setAllAlbumsQuality] = useState('');
  const [allAlbumsSource, setAllAlbumsSource] = useState('');
  const allAlbumsSortRootRef = useRef<HTMLDivElement | null>(null);
  const browseParams = useMemo<LibraryBrowseParams>(
    () => ({
      q: allAlbumsQuery,
      sort: allAlbumsSortKey,
      direction: allAlbumsSortDirection,
      genre: allAlbumsGenre || null,
      decade: allAlbumsDecade || null,
      quality: allAlbumsQuality || null,
      source: allAlbumsSource || null
    }),
    [
      allAlbumsDecade,
      allAlbumsGenre,
      allAlbumsQuality,
      allAlbumsQuery,
      allAlbumsSortDirection,
      allAlbumsSortKey,
      allAlbumsSource
    ]
  );
  const allAlbumsBrowse = useContinuousAlbumBrowse(browseParams, allAlbumsRequestLimit);
  const filteredAlbums = allAlbumsBrowse.items;
  const allAlbumsTotal = allAlbumsBrowse.total;
  const visibleAlbums = filteredAlbums;
  const allAlbumsShowingStart = allAlbumsTotal && visibleAlbums.length ? 1 : 0;
  const allAlbumsShowingEnd = Math.min(visibleAlbums.length, allAlbumsTotal);
  const allAlbumsIndicator = albumPageIndicator({
    query: allAlbumsQuery,
    showingEnd: allAlbumsShowingEnd,
    showingStart: allAlbumsShowingStart,
    totalCount: allAlbumsTotal
  });

  useEffect(() => {
    let active = true;
    loadFavoriteAlbumsCached()
      .then((nextFavorites) => {
        if (active) setFavorites(nextFavorites);
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    loadQobuzAlbumsCached()
      .then((result) => {
        if (!active) return;
        setQobuzLoggedIn(result.loggedIn);
        setQobuzAlbums(result.albums);
      })
      .catch(() => {
        if (!active) return;
        setQobuzLoggedIn(false);
        setQobuzAlbums([]);
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!allAlbumsSortOpen) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest('.app-select-menu')) return;
      if (!allAlbumsSortRootRef.current?.contains(target)) setAllAlbumsSortOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setAllAlbumsSortOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [allAlbumsSortOpen]);

  const openAlbum = useCallback(
    (album: LibraryAlbum) => {
      const albumId = album.id ?? album.album_id;
      if (albumId !== null && albumId !== undefined && albumId !== '')
        onOpen(albumId as string | number);
    },
    [onOpen]
  );

  const playAlbum = useCallback(
    (album: LibraryAlbum) => {
      const albumId = album.id ?? album.album_id;
      if (albumId !== null && albumId !== undefined && albumId !== '')
        onPlay(albumId as string | number);
    },
    [onPlay]
  );

  const openQobuzAlbum = useCallback(
    (album: LibraryAlbum) => {
      const albumId = normalizeQobuzAlbumId(album);
      if (albumId) onOpenQobuzAlbum(albumId);
    },
    [onOpenQobuzAlbum]
  );

  const playQobuzAlbum = useCallback(
    (album: LibraryAlbum) => {
      const albumId = normalizeQobuzAlbumId(album);
      if (albumId) onPlayQobuzAlbum(albumId);
    },
    [onPlayQobuzAlbum]
  );

  const openFavorite = useCallback(
    (album: LibraryAlbum) => {
      if (isQobuzFavoriteAlbum(album)) {
        const albumId = normalizeQobuzAlbumId(album);
        if (albumId) onOpenQobuzAlbum(albumId);
        return;
      }
      const albumId = resolveLocalAlbumId(album, albums) ?? album.id ?? album.album_id;
      if (albumId !== null && albumId !== undefined && albumId !== '')
        onOpen(albumId as string | number);
    },
    [albums, onOpen, onOpenQobuzAlbum]
  );

  const playFavorite = useCallback(
    (album: LibraryAlbum) => {
      if (isQobuzFavoriteAlbum(album)) {
        const albumId = normalizeQobuzAlbumId(album);
        if (albumId) onPlayQobuzAlbum(albumId);
        return;
      }
      const albumId = resolveLocalAlbumId(album, albums) ?? album.id ?? album.album_id;
      if (albumId !== null && albumId !== undefined && albumId !== '')
        onPlay(albumId as string | number);
    },
    [albums, onPlay, onPlayQobuzAlbum]
  );

  return (
    <section className="view albums-view">
      <div className="library-page-heading">
        <div>
          <h1>Albums</h1>
        </div>
      </div>

      {favorites.length ? (
        <section className="library-section favorite-albums-section">
          <div className="panel-heading">
            <div>
              <h2>Favorites</h2>
            </div>
          </div>
          <AlbumGrid
            albums={favorites}
            compact
            selectedKeys={selectedAlbumKeys}
            selectionActive={albumSelectionActive}
            selectionAlbumForAlbum={favoriteSelectionAlbum}
            onOpen={openFavorite}
            onPlay={playFavorite}
            onOpenArtist={onOpenArtist}
            onToggleSelection={onToggleAlbumSelection}
          />
        </section>
      ) : null}

      {qobuzLoggedIn ? (
        <section className="library-section qobuz-albums-section">
          <div className="panel-heading">
            <div>
              <h2>Qobuz Albums</h2>
            </div>
          </div>
          <AlbumGrid
            albums={qobuzAlbums}
            compact
            emptyLabel="No Qobuz albums saved yet."
            selectedKeys={selectedAlbumKeys}
            selectionActive={albumSelectionActive}
            selectionAlbumForAlbum={qobuzSelectionAlbum}
            onOpen={openQobuzAlbum}
            onPlay={playQobuzAlbum}
            onOpenArtist={onOpenArtist}
            onToggleSelection={onToggleAlbumSelection}
          />
        </section>
      ) : null}

      <section className="library-section">
        <div className="panel-heading">
          <div>
            <h2>All Albums</h2>
          </div>
        </div>
        <div className="albums-all-toolbar songs-toolbar">
          <div className="songs-search-cluster" ref={allAlbumsSortRootRef}>
            <label className="songs-search-field">
              <span className="sr-only">Search albums or artists</span>
              <Icon path="M10.5 18a7.5 7.5 0 1 1 5.3-12.8 7.5 7.5 0 0 1-5.3 12.8Zm5.3-2.2L21 21" />
              <input
                type="search"
                value={allAlbumsQuery}
                autoComplete="off"
                onChange={(event) => setAllAlbumsQuery(event.target.value)}
                placeholder="Search albums or artists"
              />
            </label>
            <button
              className={`songs-filter-button${allAlbumsSortOpen ? ' is-open' : ''}`}
              type="button"
              aria-label="Sort albums"
              aria-haspopup="dialog"
              aria-expanded={allAlbumsSortOpen}
              title="Sort albums"
              onClick={() => setAllAlbumsSortOpen((open) => !open)}
            >
              <Icon path="M4 21v-7m0-4V3m8 18v-9m0-4V3m8 18v-5m0-4V3M2 14h4M10 8h4m4 8h4" />
            </button>
            {allAlbumsSortOpen ? (
              <div className="songs-sort-popover" role="dialog" aria-label="Sort albums">
                <div className="songs-sort-row">
                  <span>Sort by</span>
                  <SelectMenu
                    ariaLabel="Sort albums by"
                    className="songs-sort-select"
                    menuMinWidth={170}
                    value={allAlbumsSortKey}
                    onChange={(value) => setAllAlbumsSortKey(value as AlbumSortKey)}
                    options={albumSortOptions}
                  />
                </div>
                <div className="songs-sort-direction" aria-label="Sort direction">
                  <button
                    className={`songs-sort-direction-button${allAlbumsSortDirection === 'desc' ? ' is-active' : ''}`}
                    type="button"
                    aria-pressed={allAlbumsSortDirection === 'desc'}
                    title={albumSortDirectionLabel(allAlbumsSortKey, 'desc')}
                    onClick={() => setAllAlbumsSortDirection('desc')}
                  >
                    <Icon path="m6 9 6 6 6-6" />
                    <span>{albumSortDirectionLabel(allAlbumsSortKey, 'desc')}</span>
                  </button>
                  <button
                    className={`songs-sort-direction-button${allAlbumsSortDirection === 'asc' ? ' is-active' : ''}`}
                    type="button"
                    aria-pressed={allAlbumsSortDirection === 'asc'}
                    title={albumSortDirectionLabel(allAlbumsSortKey, 'asc')}
                    onClick={() => setAllAlbumsSortDirection('asc')}
                  >
                    <Icon path="m18 15-6-6-6 6" />
                    <span>{albumSortDirectionLabel(allAlbumsSortKey, 'asc')}</span>
                  </button>
                </div>
              </div>
            ) : null}
          </div>
          <div className="songs-page-status" aria-live="polite">
            {allAlbumsIndicator}
          </div>
        </div>
        <div className="library-facet-bar" aria-label="Album filters">
          <FacetSelect
            label="Genre"
            value={allAlbumsGenre}
            options={allAlbumsBrowse.facets?.genres}
            onChange={setAllAlbumsGenre}
          />
          <FacetSelect
            label="Decade"
            value={allAlbumsDecade}
            options={allAlbumsBrowse.facets?.decades}
            onChange={setAllAlbumsDecade}
          />
          <FacetSelect
            label="Quality"
            value={allAlbumsQuality}
            options={allAlbumsBrowse.facets?.qualities}
            onChange={setAllAlbumsQuality}
          />
          <FacetSelect
            label="Source"
            value={allAlbumsSource}
            options={allAlbumsBrowse.facets?.sources}
            onChange={setAllAlbumsSource}
          />
        </div>
        {allAlbumsBrowse.loading && !allAlbumsBrowse.loaded ? (
          <AlbumGridSkeleton count={albumSkeletonCount} />
        ) : allAlbumsBrowse.error && !visibleAlbums.length ? (
          <div className="songs-empty-state library-empty-state">Albums are unavailable.</div>
        ) : !allAlbumsTotal &&
          !allAlbumsQuery.trim() &&
          !hasAlbumFacet(allAlbumsGenre, allAlbumsDecade, allAlbumsQuality, allAlbumsSource) ? (
          <SetupNotice
            actionLabel="Choose a music folder"
            message="Add a local music folder to populate your albums, songs and artists."
            onAction={onOpenMusicFolders}
          />
        ) : (
          <AlbumGrid
            albums={visibleAlbums}
            virtualized
            totalCount={allAlbumsTotal}
            loadingMore={allAlbumsBrowse.loadingMore}
            emptyLabel={
              allAlbumsQuery.trim() ||
              hasAlbumFacet(allAlbumsGenre, allAlbumsDecade, allAlbumsQuality, allAlbumsSource)
                ? 'No albums match those filters.'
                : 'No albums indexed yet.'
            }
            selectedKeys={selectedAlbumKeys}
            selectionActive={albumSelectionActive}
            selectionAlbumForAlbum={localSelectionAlbum}
            onOpen={openAlbum}
            onPlay={playAlbum}
            onOpenArtist={onOpenArtist}
            onLoadMore={allAlbumsBrowse.loadMore}
            onToggleSelection={onToggleAlbumSelection}
          />
        )}
      </section>
    </section>
  );
}

function useContinuousAlbumBrowse(baseParams: LibraryBrowseParams, chunkSize: number) {
  const [items, setItems] = useState<LibraryAlbum[]>([]);
  const [total, setTotal] = useState(0);
  const [facets, setFacets] = useState<LibraryBrowsePage<LibraryAlbum>['facets']>();
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nextOffset, setNextOffset] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const requestRef = useRef<AbortController | null>(null);
  const queryKey = useMemo(() => JSON.stringify(baseParams), [baseParams]);

  const fetchPage = useCallback(
    async (offset: number, mode: 'reset' | 'append') => {
      requestRef.current?.abort();
      const controller = new AbortController();
      requestRef.current = controller;
      if (mode === 'reset') {
        setLoading(true);
        setLoaded(false);
        setError(null);
      } else {
        setLoadingMore(true);
      }
      try {
        const page = await endpoints.browseAlbums(
          {
            ...baseParams,
            limit: chunkSize,
            offset,
            include_facets: mode === 'reset'
          },
          controller.signal
        );
        const nextTotal = Number(page.total || 0);
        const responseOffset = Number(page.offset || offset);
        setTotal(nextTotal);
        setHasMore(Boolean(page.has_more));
        setNextOffset(responseOffset + Math.max(page.items.length, chunkSize));
        if (mode === 'reset') {
          setItems(page.items);
          setFacets(page.facets);
        } else {
          setItems((current) => appendDedupeAlbums(current, page.items));
        }
        setLoaded(true);
      } catch (err) {
        if (controller.signal.aborted) return;
        setError(err instanceof Error ? err.message : 'Albums are unavailable.');
        setLoaded(true);
      } finally {
        if (requestRef.current === controller) requestRef.current = null;
        setLoading(false);
        setLoadingMore(false);
      }
    },
    [baseParams, chunkSize]
  );

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setItems([]);
      setTotal(0);
      setFacets(undefined);
      setNextOffset(0);
      setHasMore(false);
      void fetchPage(0, 'reset');
    }, 180);
    return () => {
      window.clearTimeout(timer);
      requestRef.current?.abort();
    };
  }, [fetchPage, queryKey]);

  const loadMore = useCallback(() => {
    if (!hasMore || loading || loadingMore) return;
    void fetchPage(nextOffset, 'append');
  }, [fetchPage, hasMore, loading, loadingMore, nextOffset]);

  return {
    error,
    facets,
    items,
    loaded,
    loading,
    loadingMore,
    loadMore,
    total
  };
}

function appendDedupeAlbums(current: LibraryAlbum[], next: LibraryAlbum[]) {
  const seen = new Set(current.map(albumIdentity));
  const out = [...current];
  next.forEach((album) => {
    const key = albumIdentity(album);
    if (seen.has(key)) return;
    seen.add(key);
    out.push(album);
  });
  return out;
}

function albumIdentity(album: LibraryAlbum) {
  return String(album.id ?? album.album_id ?? album.qobuz_album_id ?? album.title ?? '');
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

function AlbumGridSkeleton({ count }: { count: number }) {
  return (
    <div className="album-grid library-loading-grid" aria-label="Loading albums" aria-busy="true">
      {Array.from({ length: count }, (_, index) => (
        <article className="album-card library-loading-card" key={index}>
          <div className="album-cover skeleton-shimmer" />
          <div className="album-card-text">
            <span className="library-loading-title skeleton-shimmer" />
            <span className="library-loading-meta skeleton-shimmer" />
          </div>
        </article>
      ))}
    </div>
  );
}

function hasAlbumFacet(...values: string[]) {
  return values.some((value) => value.trim());
}

function localSelectionAlbum(album: LibraryAlbum): LibraryAlbum {
  const albumId = idValue(album.album_id, album.id);
  return {
    ...album,
    id: albumId || album.id,
    album_id: albumId || album.album_id,
    is_qobuz: false
  };
}

function qobuzSelectionAlbum(album: LibraryAlbum): LibraryAlbum {
  const albumId = normalizeQobuzAlbumId(album);
  const fallbackId = idValue(album.qobuz_album_id, album.qobuz_id, album.id);
  return {
    ...album,
    id: albumId || fallbackId || album.id,
    qobuz_album_id: albumId || fallbackId || album.qobuz_album_id,
    qobuz_id: albumId || fallbackId || album.qobuz_id,
    is_qobuz: true,
    provider: 'qobuz'
  };
}

function favoriteSelectionAlbum(album: LibraryAlbum): LibraryAlbum {
  return isQobuzFavoriteAlbum(album) ? qobuzSelectionAlbum(album) : localSelectionAlbum(album);
}

function albumSortDirectionLabel(sortKey: AlbumSortKey, direction: AlbumSortDirection) {
  if (sortKey === 'popularity') return direction === 'desc' ? 'Most popular' : 'Least popular';
  if (sortKey === 'releaseDate') return direction === 'desc' ? 'Newest first' : 'Oldest first';
  return direction === 'desc' ? 'Z to A' : 'A to Z';
}

function albumPageIndicator({
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
  const totalLabel = `${totalCount.toLocaleString()} album${totalCount === 1 ? '' : 's'}`;
  const visibleRange =
    showingStart === showingEnd
      ? showingStart.toLocaleString()
      : `${showingStart.toLocaleString()}-${showingEnd.toLocaleString()}`;
  if (query.trim()) {
    const matchLabel = `${totalCount.toLocaleString()} match${totalCount === 1 ? '' : 'es'}`;
    return totalCount ? `Showing ${visibleRange} of ${matchLabel}` : `Showing 0 of ${matchLabel}`;
  }
  if (!totalCount) return 'Showing 0 albums';
  return `Showing ${visibleRange} of ${totalLabel}`;
}
