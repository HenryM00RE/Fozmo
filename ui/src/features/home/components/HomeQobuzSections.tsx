import { useCallback, useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  albumArt,
  artFallback,
  idValue,
  recentlyPlayedSelectionKey,
  safeArray,
  titleOf
} from '../../../shared/lib/appSupport';
import type {
  JsonRecord,
  Playlist,
  QobuzAlbumPageResponse,
  QobuzFeaturedPlaylistsResponse,
  QueueItem
} from '../../../shared/types';
import { AlbumCoverPlayButton } from '../../../shared/ui/AlbumCoverPlayButton';
import { Icon } from '../../../shared/ui/Icon';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { PlaylistCover } from '../../playlists/components/PlaylistCover';
import { QobuzPlaylistArtwork } from '../../qobuz/components/QobuzPlaylistArtwork';
import {
  loadQobuzAlbumShelfCached,
  normalizeQobuzAlbumPageResponse,
  qobuzAlbumPageFromPreview,
  qobuzAlbumShelfCacheKey,
  readQobuzAlbumShelfCache
} from '../../qobuz/model/qobuzAlbumShelfData';
import {
  loadQobuzFeaturedPlaylistsCached,
  loadQobuzPlaylistDetailCached,
  qobuzFeaturedPlaylistsFallbackResponse,
  qobuzPlaylistImage,
  qobuzPlaylistQueueItems,
  qobuzPlaylistShelfCacheKey,
  readQobuzPlaylistDetailCache,
  readQobuzPlaylistShelfCache
} from '../../qobuz/model/qobuzPlaylistData';

type HomeQobuzSectionsProps = {
  onOpenArtist: (name: string) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlayQobuzAlbum: (id: string | number) => void;
  onToggleQobuzAlbumSelection: (album: JsonRecord) => void;
  qobuzHome: JsonRecord | null;
  selectedKeys: Set<string>;
  selectionActive: boolean;
};

type HomeQobuzPlaylistsProps = {
  onOpenQobuzPlaylist: (id: string | number) => void;
  onPlayQobuzPlaylist: (id: string | number) => void;
  qobuzHome: JsonRecord | null;
};

type QobuzCategory = {
  id: string;
  label: string;
  sectionIds: string[];
};

type QobuzGenreOption = {
  value: string;
  label: string;
  genreId?: number;
};

type QobuzPlaylistCategoryOption = {
  value: string;
  label: string;
};

type QobuzPlaylistShelfState = {
  key: string;
  response: QobuzFeaturedPlaylistsResponse;
};

type QobuzAlbumShelfState = {
  key: string;
  response: QobuzAlbumPageResponse;
};

const QOBUZ_CATEGORIES: QobuzCategory[] = [
  { id: 'acclaimed', label: 'Acclaimed', sectionIds: ['press-awards'] },
  { id: 'new', label: 'New', sectionIds: ['new-releases', 'album-of-the-week'] },
  { id: 'standouts', label: 'Standouts', sectionIds: ['qobuzissims'] },
  { id: 'popular', label: 'Popular', sectionIds: ['most-streamed'] }
];

const defaultQobuzCategoryId = 'acclaimed';
const qobuzCollapsedAlbumsCount = 12;
const qobuzExpandedAlbumsCount = 28;
const qobuzCollapsedPlaylistsCount = 12;
const qobuzExpandedPlaylistsCount = 28;
// Keep the collapsed shelf aligned with Recently Played at every breakpoint.
const newOnQobuzCollapsedCount = 6;
const allQobuzGenresValue = 'all';
const allQobuzPlaylistCategoriesValue = 'all';
const defaultQobuzPlaylistCategoryValue = allQobuzPlaylistCategoriesValue;
const newQobuzExcludedGenreValues = new Set(['vocal jazz']);
const emptyAlbums: JsonRecord[] = [];
const emptyPlaylists: JsonRecord[] = [];
const defaultQobuzPlaylistCategories: QobuzPlaylistCategoryOption[] = [
  { value: allQobuzPlaylistCategoriesValue, label: 'All categories' }
];

export function HomeQobuzSections({
  onOpenArtist,
  onOpenQobuzAlbum,
  onPlayQobuzAlbum,
  onToggleQobuzAlbumSelection,
  qobuzHome,
  selectedKeys,
  selectionActive
}: HomeQobuzSectionsProps) {
  void onOpenArtist;
  const homeCategories = useMemo(() => visibleHomeQobuzSections(qobuzHome), [qobuzHome]);
  const [activeCategoryId, setActiveCategoryId] = useState(defaultQobuzCategoryId);
  const [genreFilter, setGenreFilter] = useState(allQobuzGenresValue);
  const [expanded, setExpanded] = useState(false);
  const [page, setPage] = useState(0);
  const [remoteAlbumShelf, setRemoteAlbumShelf] = useState<QobuzAlbumShelfState | null>(null);
  const [remoteGenreOptions, setRemoteGenreOptions] = useState<QobuzGenreOption[]>([
    { value: allQobuzGenresValue, label: 'All genres' }
  ]);
  const [loading, setLoading] = useState(false);
  const [albumLoadError, setAlbumLoadError] = useState(false);
  const [albumPaginationMeta, setAlbumPaginationMeta] = useState<{
    hasMore: boolean;
    key: string;
    total: number | null;
  } | null>(null);
  const activeCategory =
    QOBUZ_CATEGORIES.find((category) => category.id === activeCategoryId) || QOBUZ_CATEGORIES[0];
  const activeHomeCategory = homeCategories.find((category) => category.id === activeCategory.id);
  const albumRequestLimit = expanded ? qobuzExpandedAlbumsCount : qobuzCollapsedAlbumsCount;
  const albumRequestOffset = page * albumRequestLimit;
  const activeAlbums = activeHomeCategory?.albums || emptyAlbums;
  const fallbackGenreOptions = useMemo(
    () =>
      qobuzGenreOptions(
        activeAlbums,
        activeCategory?.id === 'new' ? newQobuzExcludedGenreValues : undefined
      ),
    [activeAlbums, activeCategory?.id]
  );
  const genreOptions = remoteGenreOptions.length > 1 ? remoteGenreOptions : fallbackGenreOptions;
  const selectedGenre =
    genreOptions.find((option) => option.value === genreFilter) || genreOptions[0];
  const categoryOptions = useMemo(
    () => QOBUZ_CATEGORIES.map((category) => ({ value: category.id, label: category.label })),
    []
  );
  const selectedCategory =
    categoryOptions.find((option) => option.value === activeCategory?.id) || categoryOptions[0];
  const genreFilterActive = genreFilter !== allQobuzGenresValue;
  const needsAlbumFetch = expanded || genreFilterActive || page > 0;
  const albumRequestGenreId = selectedGenre?.genreId ?? null;
  const albumPaginationKey = qobuzAlbumShelfCacheKey(
    activeCategory.id,
    albumRequestLimit,
    0,
    albumRequestGenreId
  );
  const albumRequestKey = qobuzAlbumShelfCacheKey(
    activeCategory.id,
    albumRequestLimit,
    albumRequestOffset,
    albumRequestGenreId
  );
  const expandedAlbumRequestKey = qobuzAlbumShelfCacheKey(
    activeCategory.id,
    qobuzExpandedAlbumsCount,
    0,
    albumRequestGenreId
  );
  const expandedCachedAlbums = useMemo(
    () => (!expanded ? readQobuzAlbumShelfCache(expandedAlbumRequestKey) : null),
    [expanded, expandedAlbumRequestKey]
  );
  const cachedAlbumResponse = useMemo(
    () =>
      readQobuzAlbumShelfCache(albumRequestKey) ||
      (expandedCachedAlbums &&
      albumRequestOffset + albumRequestLimit <= expandedCachedAlbums.albums.length
        ? qobuzAlbumPageFromCachedResponse(
            expandedCachedAlbums,
            albumRequestLimit,
            albumRequestOffset
          )
        : null),
    [albumRequestKey, albumRequestLimit, albumRequestOffset, expandedCachedAlbums]
  );
  const remoteAlbumResponse =
    remoteAlbumShelf?.key === albumRequestKey ? remoteAlbumShelf.response : null;
  const localFilteredAlbums = useMemo(
    () => filterQobuzAlbumsByGenre(activeAlbums, genreFilter),
    [activeAlbums, genreFilter]
  );
  const fallbackAlbumResponse = useMemo(
    () =>
      !needsAlbumFetch
        ? qobuzAlbumPageFromPreview(localFilteredAlbums, albumRequestLimit, 0)
        : null,
    [albumRequestLimit, localFilteredAlbums, needsAlbumFetch]
  );
  const albumResponse = remoteAlbumResponse ?? cachedAlbumResponse ?? fallbackAlbumResponse;
  const pageAlbums = albumResponse?.albums ?? emptyAlbums;
  const albumsLoading =
    loading ||
    (needsAlbumFetch &&
      remoteAlbumResponse === null &&
      !cachedAlbumResponse &&
      !fallbackAlbumResponse);
  const albumSkeletonCount = albumsLoading && !pageAlbums.length ? albumRequestLimit : 0;
  const activePaginationMeta =
    albumPaginationMeta?.key === albumPaginationKey ? albumPaginationMeta : null;
  const albumTotal =
    remoteAlbumResponse?.total ?? cachedAlbumResponse?.total ?? activePaginationMeta?.total ?? null;
  const albumExactPageCount =
    albumTotal !== null
      ? Math.max(1, Math.ceil(albumTotal / Math.max(1, albumRequestLimit)))
      : null;
  const albumHasMore =
    albumExactPageCount !== null
      ? page + 1 < albumExactPageCount
      : albumsLoading
        ? true
        : Boolean(
            remoteAlbumResponse?.has_more ??
              cachedAlbumResponse?.has_more ??
              fallbackAlbumResponse?.has_more ??
              activePaginationMeta?.hasMore
          );
  const pageCount = albumExactPageCount ?? page + 1 + (albumHasMore ? 1 : 0);
  const genreLabel = selectedGenre?.value === allQobuzGenresValue ? '' : selectedGenre?.label || '';

  const resetAlbumPagination = useCallback(() => {
    setPage(0);
    setAlbumLoadError(false);
    setLoading(false);
    setRemoteAlbumShelf(null);
    setAlbumPaginationMeta(null);
  }, []);

  const handleCategoryChange = useCallback(
    (nextCategoryId: string) => {
      resetAlbumPagination();
      setActiveCategoryId(nextCategoryId);
    },
    [resetAlbumPagination]
  );

  const handleGenreChange = useCallback(
    (nextGenreFilter: string) => {
      resetAlbumPagination();
      setGenreFilter(nextGenreFilter);
    },
    [resetAlbumPagination]
  );

  useEffect(() => {
    const controller = new AbortController();
    endpoints
      .qobuzGenres(controller.signal)
      .then((genres) => {
        if (!controller.signal.aborted) setRemoteGenreOptions(qobuzPlaylistGenreOptions(genres));
      })
      .catch((error) => {
        if (!controller.signal.aborted) console.warn('qobuz: album genre options failed', error);
      });

    return () => controller.abort();
  }, []);

  useEffect(() => {
    if (!genreOptions.some((option) => option.value === genreFilter)) {
      setGenreFilter(allQobuzGenresValue);
    }
  }, [genreFilter, genreOptions]);

  useEffect(() => {
    setPage(0);
    setAlbumLoadError(false);
    setLoading(false);
    setRemoteAlbumShelf(null);
    setAlbumPaginationMeta(null);
  }, [activeCategoryId, genreFilter]);

  useEffect(() => {
    const metaResponse = remoteAlbumResponse ?? cachedAlbumResponse;
    if (!metaResponse) return;
    setAlbumPaginationMeta({
      hasMore: metaResponse.has_more,
      key: albumPaginationKey,
      total: metaResponse.total
    });
  }, [albumPaginationKey, cachedAlbumResponse, remoteAlbumResponse]);

  useEffect(() => {
    const controller = new AbortController();
    const cached = readQobuzAlbumShelfCache(albumRequestKey);
    if (cached) setRemoteAlbumShelf({ key: albumRequestKey, response: cached });
    setLoading(!cached);
    setAlbumLoadError(false);
    loadQobuzAlbumShelfCached(
      activeCategory.id,
      albumRequestLimit,
      albumRequestOffset,
      albumRequestGenreId,
      controller.signal
    )
      .then((next) => {
        if (!controller.signal.aborted)
          setRemoteAlbumShelf({ key: albumRequestKey, response: next });
      })
      .catch((error) => {
        if (!controller.signal.aborted) {
          console.warn('qobuz: album shelf fetch failed', error);
          setAlbumLoadError(true);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });

    return () => controller.abort();
  }, [
    activeCategory.id,
    albumRequestGenreId,
    albumRequestKey,
    albumRequestLimit,
    albumRequestOffset
  ]);

  useEffect(() => {
    if (albumExactPageCount !== null && page >= albumExactPageCount) {
      setPage(Math.max(0, albumExactPageCount - 1));
    }
  }, [albumExactPageCount, page]);

  const requestAlbumPage = useCallback(
    (targetPage: number) => {
      const nextPage = Math.max(0, targetPage);
      if (nextPage === page) return;
      setAlbumLoadError(false);
      setPage(nextPage);
    },
    [page]
  );

  const toggleExpanded = useCallback(() => {
    setAlbumLoadError(false);
    setLoading(false);
    const currentLimit = expanded ? qobuzExpandedAlbumsCount : qobuzCollapsedAlbumsCount;
    const nextExpanded = !expanded;
    const nextLimit = nextExpanded ? qobuzExpandedAlbumsCount : qobuzCollapsedAlbumsCount;
    const currentOffset = page * currentLimit;
    setPage(Math.floor(currentOffset / nextLimit));
    setExpanded(nextExpanded);
  }, [expanded, page]);

  if (!homeCategories.length && !loading && !pageAlbums.length && albumLoadError) {
    return null;
  }

  if (!homeCategories.length && !pageAlbums.length && !loading) {
    return null;
  }

  return (
    <section className="library-section home-qobuz-section">
      <div className="home-qobuz-shell">
        <header className="home-qobuz-header">
          <div className="home-qobuz-header-row home-qobuz-playlists-header-row">
            <div className="home-qobuz-playlist-title-row">
              <div className="home-qobuz-heading">
                <h2>
                  <button
                    className={`recently-played-toggle home-qobuz-playlist-expand-button${expanded ? ' is-expanded' : ''}`}
                    type="button"
                    aria-label={expanded ? 'Collapse From Qobuz' : 'Expand From Qobuz'}
                    aria-expanded={expanded}
                    title={expanded ? 'Collapse From Qobuz' : 'Expand From Qobuz'}
                    onClick={toggleExpanded}
                  >
                    <span>From Qobuz</span>
                    <Icon path="m9 18 6-6-6-6" />
                  </button>
                </h2>
              </div>
            </div>
            <div className="home-qobuz-playlist-controls-row">
              <SelectMenu
                ariaLabel="Choose Qobuz album category"
                className="home-qobuz-playlist-category-select"
                menuClassName="home-qobuz-playlist-category-menu"
                menuMinWidth={180}
                value={selectedCategory?.value || activeCategory.id}
                onChange={handleCategoryChange}
                options={categoryOptions}
              />
              <SelectMenu
                ariaLabel="Filter Qobuz albums by genre"
                className={`home-qobuz-genre-select home-qobuz-playlist-genre-select${genreFilterActive ? ' has-active-filter' : ''}`}
                menuClassName="home-qobuz-playlist-category-menu"
                menuMinWidth={158}
                triggerIconPath="M4 21v-7m0-4V3m8 18v-9m0-4V3m8 18v-5m0-4V3M2 14h4M10 8h4m4 8h4"
                value={selectedGenre?.value || allQobuzGenresValue}
                onChange={handleGenreChange}
                options={genreOptions}
              />
              <nav
                className="songs-pagination home-qobuz-playlist-pagination"
                aria-label={`${activeCategory.label} Qobuz pages`}
              >
                <button
                  className="songs-page-button"
                  type="button"
                  aria-label="Previous Qobuz page"
                  disabled={page === 0}
                  onClick={() => requestAlbumPage(page - 1)}
                >
                  <Icon path="m15 18-6-6 6-6" />
                </button>
                <span>
                  {page + 1} / {pageCount}
                </span>
                <button
                  className="songs-page-button"
                  type="button"
                  aria-label="Next Qobuz page"
                  disabled={!albumHasMore}
                  onClick={() => requestAlbumPage(page + 1)}
                >
                  <Icon path="m9 18 6-6-6-6" />
                </button>
              </nav>
            </div>
          </div>
        </header>
        <div
          className={`home-qobuz-grid home-qobuz-album-grid${expanded ? ' is-expanded' : ''}${albumsLoading ? ' is-loading' : ''}`}
          role="tabpanel"
          aria-label={activeCategory.label}
          aria-busy={albumsLoading}
        >
          {pageAlbums.map((album) => (
            <HomeQobuzAlbum
              album={album}
              selected={selectedKeys.has(recentlyPlayedSelectionKey(qobuzSelectionAlbum(album)))}
              selectionActive={selectionActive}
              onOpen={onOpenQobuzAlbum}
              onPlay={onPlayQobuzAlbum}
              onToggleSelection={onToggleQobuzAlbumSelection}
              key={String(album.id || album.title)}
            />
          ))}
          {Array.from({ length: albumSkeletonCount }, (_, index) => (
            <span
              className="album-card home-qobuz-card home-qobuz-album-skeleton"
              aria-hidden="true"
              key={`qobuz-album-skeleton-${index}`}
            >
              <span className="album-cover home-qobuz-card-cover skeleton-shimmer" />
              <span className="album-card-text">
                <span className="album-title skeleton-shimmer">&nbsp;</span>
                <span className="album-subtitle skeleton-shimmer">&nbsp;</span>
              </span>
            </span>
          ))}
        </div>
        {albumLoadError ? (
          <div className="home-qobuz-empty-state home-qobuz-playlist-page-error">
            Could not load that Qobuz page.
          </div>
        ) : null}
        {!albumsLoading && !pageAlbums.length ? (
          <div className="home-qobuz-empty-state">
            {qobuzEmptyLabel(activeCategory.label, genreLabel)}
          </div>
        ) : null}
      </div>
    </section>
  );
}

export function hasVisibleHomeQobuzSections(qobuzHome: JsonRecord | null) {
  return visibleHomeQobuzSections(qobuzHome).length > 0;
}

export function HomeQobuzPlaylists({
  onOpenQobuzPlaylist,
  onPlayQobuzPlaylist,
  qobuzHome
}: HomeQobuzPlaylistsProps) {
  const homePlaylists = useMemo(() => qobuzEditorialPlaylists(qobuzHome), [qobuzHome]);
  const [playlistCategory, setPlaylistCategory] = useState(defaultQobuzPlaylistCategoryValue);
  const [playlistGenre, setPlaylistGenre] = useState(allQobuzGenresValue);
  const [categoryOptions, setCategoryOptions] = useState<QobuzPlaylistCategoryOption[]>(
    defaultQobuzPlaylistCategories
  );
  const [genreOptions, setGenreOptions] = useState<QobuzGenreOption[]>([
    { value: allQobuzGenresValue, label: 'All genres' }
  ]);
  const [remotePlaylistShelf, setRemotePlaylistShelf] = useState<QobuzPlaylistShelfState | null>(
    null
  );
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [playlistPage, setPlaylistPage] = useState(0);
  const [playlistLoadError, setPlaylistLoadError] = useState(false);
  const [playlistPaginationMeta, setPlaylistPaginationMeta] = useState<{
    hasMore: boolean;
    key: string;
    total: number | null;
  } | null>(null);
  const selectedGenre =
    genreOptions.find((option) => option.value === playlistGenre) || genreOptions[0];
  const selectedCategory =
    categoryOptions.find((option) => option.value === playlistCategory) || categoryOptions[0];
  const categoryActive = playlistCategory !== allQobuzPlaylistCategoriesValue;
  const genreFilterActive = playlistGenre !== allQobuzGenresValue;
  const needsPlaylistFetch = expanded || categoryActive || genreFilterActive || playlistPage > 0;
  const playlistRequestLimit = expanded
    ? qobuzExpandedPlaylistsCount
    : qobuzCollapsedPlaylistsCount;
  const playlistRequestOffset = playlistPage * playlistRequestLimit;
  const playlistRequestGenreId = selectedGenre?.genreId ?? null;
  const playlistRequestTag =
    playlistCategory === allQobuzPlaylistCategoriesValue ? null : playlistCategory;
  const playlistPaginationKey = qobuzPlaylistShelfCacheKey(
    playlistRequestLimit,
    0,
    playlistRequestGenreId,
    playlistRequestTag
  );
  const playlistRequestKey = qobuzPlaylistShelfCacheKey(
    playlistRequestLimit,
    playlistRequestOffset,
    playlistRequestGenreId,
    playlistRequestTag
  );
  const expandedPlaylistRequestKey = qobuzPlaylistShelfCacheKey(
    qobuzExpandedPlaylistsCount,
    0,
    playlistRequestGenreId,
    playlistRequestTag
  );
  const expandedCachedPlaylists = useMemo(
    () =>
      !expanded && !categoryActive && !genreFilterActive
        ? readQobuzPlaylistShelfCache(expandedPlaylistRequestKey)
        : null,
    [categoryActive, expanded, expandedPlaylistRequestKey, genreFilterActive]
  );
  const cachedPlaylistResponse = useMemo(
    () =>
      readQobuzPlaylistShelfCache(playlistRequestKey) ||
      (expandedCachedPlaylists &&
      playlistRequestOffset + playlistRequestLimit <= expandedCachedPlaylists.playlists.length
        ? qobuzPlaylistPageFromCachedResponse(
            expandedCachedPlaylists,
            playlistRequestLimit,
            playlistRequestOffset
          )
        : null),
    [expandedCachedPlaylists, playlistRequestKey, playlistRequestLimit, playlistRequestOffset]
  );
  const remotePlaylistResponse =
    remotePlaylistShelf?.key === playlistRequestKey ? remotePlaylistShelf.response : null;
  const fallbackPlaylistResponse =
    playlistPage === 0
      ? qobuzFeaturedPlaylistsFallbackResponse(homePlaylists, playlistRequestLimit, 0)
      : null;
  const playlistResponse = needsPlaylistFetch
    ? (remotePlaylistResponse ?? cachedPlaylistResponse ?? fallbackPlaylistResponse)
    : (cachedPlaylistResponse ?? fallbackPlaylistResponse);
  const playlists = playlistResponse?.playlists ?? emptyPlaylists;
  const visiblePlaylists = expanded
    ? playlists.slice(0, qobuzExpandedPlaylistsCount)
    : playlists.slice(0, qobuzCollapsedPlaylistsCount);
  const playlistsLoading =
    loading ||
    (needsPlaylistFetch &&
      remotePlaylistResponse === null &&
      !cachedPlaylistResponse &&
      !fallbackPlaylistResponse);
  const playlistSkeletonCount =
    playlistsLoading && !visiblePlaylists.length
      ? Math.max(0, playlistRequestLimit - visiblePlaylists.length)
      : 0;
  const activePaginationMeta =
    playlistPaginationMeta?.key === playlistPaginationKey ? playlistPaginationMeta : null;
  const playlistTotal = playlistResponse?.total ?? activePaginationMeta?.total ?? null;
  const playlistExactPageCount =
    playlistTotal !== null
      ? Math.max(1, Math.ceil(playlistTotal / Math.max(1, playlistRequestLimit)))
      : null;
  const playlistHasMore =
    playlistExactPageCount !== null
      ? playlistPage + 1 < playlistExactPageCount
      : playlistsLoading
        ? true
        : Boolean(playlistResponse?.has_more ?? activePaginationMeta?.hasMore);
  const playlistPageCount = playlistExactPageCount ?? playlistPage + 1 + (playlistHasMore ? 1 : 0);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      endpoints.qobuzPlaylistTags(controller.signal),
      endpoints.qobuzGenres(controller.signal)
    ])
      .then(([tags, genres]) => {
        if (controller.signal.aborted) return;
        const nextCategories = qobuzPlaylistCategoryOptions(tags);
        const nextGenres = qobuzPlaylistGenreOptions(genres);
        setCategoryOptions(nextCategories);
        setGenreOptions(nextGenres);
      })
      .catch((error) => {
        if (!controller.signal.aborted)
          console.warn('qobuz: playlist filter options failed', error);
      });

    return () => controller.abort();
  }, []);

  useEffect(() => {
    if (!categoryOptions.some((option) => option.value === playlistCategory)) {
      setPlaylistCategory(defaultQobuzPlaylistCategoryValue);
    }
  }, [categoryOptions, playlistCategory]);

  useEffect(() => {
    if (!genreOptions.some((option) => option.value === playlistGenre)) {
      setPlaylistGenre(allQobuzGenresValue);
    }
  }, [genreOptions, playlistGenre]);

  useEffect(() => {
    setPlaylistLoadError(false);
    setLoading(false);
    setPlaylistPage(0);
    setPlaylistPaginationMeta(null);
  }, [playlistCategory, playlistGenre]);

  useEffect(() => {
    if (!playlistResponse) return;
    setPlaylistPaginationMeta({
      hasMore: playlistResponse.has_more,
      key: playlistPaginationKey,
      total: playlistResponse.total
    });
  }, [playlistPaginationKey, playlistResponse]);

  useEffect(() => {
    if (!needsPlaylistFetch) {
      setLoading(false);
      return undefined;
    }

    const controller = new AbortController();
    const cached = readQobuzPlaylistShelfCache(playlistRequestKey);
    if (cached) setRemotePlaylistShelf({ key: playlistRequestKey, response: cached });
    setLoading(!cached);
    setPlaylistLoadError(false);
    loadQobuzFeaturedPlaylistsCached(
      playlistRequestLimit,
      playlistRequestOffset,
      playlistRequestGenreId,
      playlistRequestTag,
      controller.signal
    )
      .then((next) => {
        if (!controller.signal.aborted)
          setRemotePlaylistShelf({ key: playlistRequestKey, response: next });
      })
      .catch((error) => {
        if (!controller.signal.aborted) {
          console.warn('qobuz: playlist filter fetch failed', error);
          setPlaylistLoadError(true);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });

    return () => controller.abort();
  }, [
    needsPlaylistFetch,
    playlistRequestGenreId,
    playlistRequestKey,
    playlistRequestLimit,
    playlistRequestOffset,
    playlistRequestTag
  ]);

  const requestPlaylistPage = useCallback(
    (targetPage: number) => {
      const nextPage = Math.max(0, targetPage);
      if (nextPage === playlistPage) return;
      setPlaylistLoadError(false);
      setPlaylistPage(nextPage);
    },
    [playlistPage]
  );

  const togglePlaylistExpanded = useCallback(() => {
    setPlaylistLoadError(false);
    setLoading(false);
    const currentLimit = expanded ? qobuzExpandedPlaylistsCount : qobuzCollapsedPlaylistsCount;
    const nextExpanded = !expanded;
    const nextLimit = nextExpanded ? qobuzExpandedPlaylistsCount : qobuzCollapsedPlaylistsCount;
    const currentOffset = playlistPage * currentLimit;
    setPlaylistPage(Math.floor(currentOffset / nextLimit));
    setExpanded(nextExpanded);
  }, [expanded, playlistPage]);

  if (!homePlaylists.length && !needsPlaylistFetch) return null;

  return (
    <section className="library-section home-qobuz-section home-qobuz-playlists-section">
      <div className="home-qobuz-shell">
        <header className="home-qobuz-header">
          <div className="home-qobuz-header-row home-qobuz-playlists-header-row">
            <div className="home-qobuz-playlist-title-row">
              <div className="home-qobuz-heading">
                <h2>
                  <button
                    className={`recently-played-toggle home-qobuz-playlist-expand-button${expanded ? ' is-expanded' : ''}`}
                    type="button"
                    aria-label={expanded ? 'Collapse Qobuz playlists' : 'Expand Qobuz playlists'}
                    aria-expanded={expanded}
                    title={expanded ? 'Collapse Qobuz playlists' : 'Expand Qobuz playlists'}
                    onClick={togglePlaylistExpanded}
                  >
                    <span>Qobuz playlists</span>
                    <Icon path="m9 18 6-6-6-6" />
                  </button>
                </h2>
              </div>
            </div>
            <div className="home-qobuz-playlist-controls-row">
              <SelectMenu
                ariaLabel="Choose Qobuz playlist category"
                className="home-qobuz-playlist-category-select"
                menuClassName="home-qobuz-playlist-category-menu"
                menuMinWidth={180}
                value={selectedCategory?.value || defaultQobuzPlaylistCategoryValue}
                onChange={setPlaylistCategory}
                options={categoryOptions}
              />
              <SelectMenu
                ariaLabel="Filter Qobuz playlists by genre"
                className={`home-qobuz-genre-select home-qobuz-playlist-genre-select${genreFilterActive ? ' has-active-filter' : ''}`}
                menuClassName="home-qobuz-playlist-category-menu"
                menuMinWidth={158}
                triggerIconPath="M4 21v-7m0-4V3m8 18v-9m0-4V3m8 18v-5m0-4V3M2 14h4M10 8h4m4 8h4"
                value={selectedGenre?.value || allQobuzGenresValue}
                onChange={setPlaylistGenre}
                options={genreOptions}
              />
              <nav
                className="songs-pagination home-qobuz-playlist-pagination"
                aria-label="Qobuz playlist pages"
              >
                <button
                  className="songs-page-button"
                  type="button"
                  aria-label="Previous Qobuz playlist page"
                  disabled={playlistPage === 0}
                  onClick={() => requestPlaylistPage(playlistPage - 1)}
                >
                  <Icon path="m15 18-6-6 6-6" />
                </button>
                <span>
                  {playlistPage + 1} / {playlistPageCount}
                </span>
                <button
                  className="songs-page-button"
                  type="button"
                  aria-label="Next Qobuz playlist page"
                  disabled={!playlistHasMore}
                  onClick={() => requestPlaylistPage(playlistPage + 1)}
                >
                  <Icon path="m9 18 6-6-6-6" />
                </button>
              </nav>
            </div>
          </div>
        </header>
        <div
          className={`home-qobuz-grid home-qobuz-playlist-grid${expanded ? ' is-expanded' : ''}${playlistsLoading ? ' is-loading' : ''}`}
          aria-busy={playlistsLoading}
        >
          {visiblePlaylists.map((playlist) => (
            <HomeQobuzPlaylistCard
              playlist={playlist}
              onOpen={onOpenQobuzPlaylist}
              onPlay={onPlayQobuzPlaylist}
              key={String(playlist.id || playlist.title)}
            />
          ))}
          {Array.from({ length: playlistSkeletonCount }, (_, index) => (
            <span
              className="album-card home-qobuz-card home-qobuz-playlist-card home-qobuz-playlist-skeleton"
              aria-hidden="true"
              key={`qobuz-playlist-skeleton-${index}`}
            >
              <span className="album-cover home-qobuz-card-cover skeleton-shimmer" />
              <span className="album-card-text">
                <span className="album-title skeleton-shimmer">&nbsp;</span>
                <span className="album-subtitle skeleton-shimmer">&nbsp;</span>
              </span>
            </span>
          ))}
        </div>
        {playlistLoadError ? (
          <div className="home-qobuz-empty-state home-qobuz-playlist-page-error">
            Could not load that Qobuz playlist page.
          </div>
        ) : null}
        {needsPlaylistFetch && !playlistsLoading && !visiblePlaylists.length ? (
          <div className="home-qobuz-empty-state">No Qobuz playlists found.</div>
        ) : null}
      </div>
    </section>
  );
}

export function QobuzHomeSkeleton() {
  return (
    <div className="home-qobuz-shell">
      <header className="home-qobuz-header">
        <h2 className="skeleton-shimmer">&nbsp;</h2>
        <div className="segmented home-qobuz-tabs">
          {QOBUZ_CATEGORIES.map((category) => (
            <span className="skeleton-shimmer" key={category.id}>
              &nbsp;
            </span>
          ))}
        </div>
      </header>
      <div className="home-qobuz-grid">
        {Array.from({ length: 12 }, (_, index) => (
          <span className="album-card home-qobuz-card" key={index}>
            <span className="album-cover skeleton-shimmer" />
            <span className="album-card-text">
              <span className="album-title skeleton-shimmer">&nbsp;</span>
              <span className="album-subtitle skeleton-shimmer">&nbsp;</span>
            </span>
          </span>
        ))}
      </div>
    </div>
  );
}

export function NewOnQobuzSection({
  expanded,
  loading,
  onExpandedChange,
  onOpenQobuzAlbum,
  onPlayQobuzAlbum,
  onToggleQobuzAlbumSelection,
  qobuzHome,
  selectedKeys,
  selectionActive
}: {
  expanded: boolean;
  loading: boolean;
  onExpandedChange: (expanded: boolean) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlayQobuzAlbum: (id: string | number) => void;
  onToggleQobuzAlbumSelection: (album: JsonRecord) => void;
  qobuzHome: JsonRecord | null;
  selectedKeys: Set<string>;
  selectionActive: boolean;
}) {
  const albums = useMemo(() => qobuzNewReleaseAlbums(qobuzHome), [qobuzHome]);
  const expandedRequestKey = qobuzAlbumShelfCacheKey('new', qobuzExpandedAlbumsCount, 0, null);
  const [expandedShelf, setExpandedShelf] = useState<QobuzAlbumShelfState | null>(null);
  const [expandedLoading, setExpandedLoading] = useState(false);
  const expandedResponse =
    expandedShelf?.key === expandedRequestKey ? expandedShelf.response : null;

  useEffect(() => {
    if (!albums.length) {
      setExpandedLoading(false);
      return;
    }

    const controller = new AbortController();
    setExpandedLoading(expandedShelf?.key !== expandedRequestKey);
    endpoints
      .qobuzHomeSection('new', null, qobuzExpandedAlbumsCount, 0, controller.signal)
      .then((next) => normalizeQobuzAlbumPageResponse(next, 'new', qobuzExpandedAlbumsCount, 0))
      .then((next) => {
        if (!controller.signal.aborted)
          setExpandedShelf({ key: expandedRequestKey, response: next });
      })
      .catch((error) => {
        if (!controller.signal.aborted) console.warn('qobuz: new releases fetch failed', error);
      })
      .finally(() => {
        if (!controller.signal.aborted) setExpandedLoading(false);
      });

    return () => controller.abort();
  }, [albums.length, expandedRequestKey, qobuzHome]);

  if (!albums.length && !loading) return null;

  const fetchedAlbums = expandedResponse?.albums?.length ? expandedResponse.albums : albums;
  const canExpand = albums.length > 0;
  const displayAlbums = expanded ? fetchedAlbums : fetchedAlbums.slice(0, newOnQobuzCollapsedCount);
  return (
    <section
      className={`library-section home-album-shelf-section home-qobuz-section new-on-qobuz-section${loading ? ' is-loading' : ''}`}
      aria-busy={loading || expandedLoading}
    >
      <div className="panel-heading">
        <div>
          <h2>
            {canExpand ? (
              <button
                className={`recently-played-toggle${expanded ? ' is-expanded' : ''}`}
                type="button"
                aria-expanded={expanded}
                aria-controls="new-on-qobuz-albums"
                onClick={() => onExpandedChange(!expanded)}
              >
                <span>New on Qobuz</span>
                <Icon path="m9 18 6-6-6-6" />
              </button>
            ) : (
              'New on Qobuz'
            )}
          </h2>
        </div>
      </div>
      <div
        id="new-on-qobuz-albums"
        className={`home-qobuz-grid home-album-shelf-grid new-on-qobuz-grid${expanded ? ' is-expanded' : ''}`}
      >
        {loading ? (
          <NewOnQobuzSkeleton />
        ) : (
          displayAlbums.map((album) => (
            <HomeQobuzAlbum
              album={album}
              selected={selectedKeys.has(recentlyPlayedSelectionKey(qobuzSelectionAlbum(album)))}
              selectionActive={selectionActive}
              onOpen={onOpenQobuzAlbum}
              onPlay={onPlayQobuzAlbum}
              onToggleSelection={onToggleQobuzAlbumSelection}
              key={String(album.id || album.title)}
            />
          ))
        )}
      </div>
    </section>
  );
}

export function qobuzNewReleaseAlbums(qobuzHome: JsonRecord | null) {
  return (
    visibleHomeQobuzSections(qobuzHome).find((category) => category.id === 'new')?.albums ||
    emptyAlbums
  );
}

export function qobuzEditorialPlaylists(qobuzHome: JsonRecord | null) {
  const sections = safeArray<JsonRecord>(qobuzHome?.sections);
  const section = sections.find(
    (candidate) =>
      String(candidate.id || '')
        .trim()
        .toLowerCase() === 'editorial-playlists' ||
      String(candidate.item_type || '')
        .trim()
        .toLowerCase() === 'playlist'
  );
  if (!section) return emptyPlaylists;
  return safeArray<JsonRecord>(section?.playlists).filter((playlist) => idValue(playlist.id));
}

function qobuzPlaylistPageFromCachedResponse(
  response: QobuzFeaturedPlaylistsResponse,
  limit: number,
  offset: number
): QobuzFeaturedPlaylistsResponse {
  const playlists = response.playlists.slice(offset, offset + limit);
  const total = response.total ?? null;
  const count = playlists.length;
  const hasMore = total === null ? response.has_more || count >= limit : offset + count < total;
  return {
    playlists,
    limit,
    offset,
    count,
    total,
    has_more: hasMore
  };
}

function qobuzAlbumPageFromCachedResponse(
  response: QobuzAlbumPageResponse,
  limit: number,
  offset: number
): QobuzAlbumPageResponse {
  const albums = response.albums.slice(offset, offset + limit);
  const total = response.total ?? null;
  const count = albums.length;
  const hasMore = total === null ? response.has_more || count >= limit : offset + count < total;
  return {
    albums,
    limit,
    offset,
    count,
    total,
    has_more: hasMore
  };
}

export function qobuzPlaylistCategoryOptions(tags: JsonRecord[]) {
  const labels = new Map<string, QobuzPlaylistCategoryOption>();
  defaultQobuzPlaylistCategories.forEach((option) =>
    labels.set(qobuzPlaylistCategoryLabelKey(option.label), option)
  );
  tags.forEach((tag) => {
    const value = String(tag.id || tag.slug || '').trim();
    const label = String(tag.label || tag.name || '').trim();
    const labelKey = qobuzPlaylistCategoryLabelKey(label);
    if (value && label && !labels.has(labelKey)) labels.set(labelKey, { value, label });
  });
  return Array.from(labels.values());
}

function qobuzPlaylistCategoryLabelKey(label: string) {
  return label
    .trim()
    .replace(/\u2026/g, '...')
    .replace(/\.{3}$/g, '')
    .toLocaleLowerCase();
}

export function qobuzPlaylistGenreOptions(genres: JsonRecord[]) {
  const labels = new Map<string, QobuzGenreOption>();
  safeArray<JsonRecord>(genres).forEach((genre) => {
    const genreId = numberValue(genre.id, genre.genre_id);
    const label = String(genre.label || genre.name || '').trim();
    if (genreId && label) labels.set(`id:${genreId}`, { value: `id:${genreId}`, label, genreId });
  });
  return [
    { value: allQobuzGenresValue, label: 'All genres' },
    ...Array.from(labels.values()).sort((a, b) =>
      a.label.localeCompare(b.label, undefined, { sensitivity: 'base' })
    )
  ];
}

function NewOnQobuzSkeleton() {
  return Array.from({ length: newOnQobuzCollapsedCount }, (_, index) => (
    <span className="album-card home-qobuz-card recently-played-skeleton" key={index}>
      <span className="album-cover home-qobuz-card-cover skeleton-shimmer" />
      <span className="album-card-text">
        <span className="album-title skeleton-shimmer">&nbsp;</span>
        <span className="album-subtitle skeleton-shimmer">&nbsp;</span>
      </span>
    </span>
  ));
}

function visibleHomeQobuzSections(qobuzHome: JsonRecord | null) {
  const sections = safeArray<JsonRecord>(qobuzHome?.sections);
  return QOBUZ_CATEGORIES.map((category) => {
    const section = category.sectionIds
      .map((sectionId) =>
        sections.find(
          (candidate) =>
            String(candidate.id || '')
              .trim()
              .toLowerCase() === sectionId && safeArray(candidate.albums).length > 0
        )
      )
      .find(Boolean);
    if (!section) return null;
    return {
      ...category,
      albums: safeArray<JsonRecord>(section.albums)
    };
  }).filter(Boolean) as Array<QobuzCategory & { albums: JsonRecord[] }>;
}

export function qobuzGenreOptions(
  albums: JsonRecord[],
  excludedValues = new Set<string>()
): QobuzGenreOption[] {
  const labels = new Map<string, QobuzGenreOption>();
  albums.forEach((album) => {
    qobuzAlbumGenreEntries(album).forEach((entry) => {
      const value = qobuzGenreOptionValue(entry);
      if (value && !excludedValues.has(qobuzGenreValue(entry.label)) && !labels.has(value)) {
        labels.set(value, { value, label: entry.label, genreId: entry.genreId });
      }
    });
  });
  return [
    { value: allQobuzGenresValue, label: 'All genres' },
    ...Array.from(labels.values()).sort((a, b) =>
      a.label.localeCompare(b.label, undefined, { sensitivity: 'base' })
    )
  ];
}

export function filterQobuzAlbumsByGenre(albums: JsonRecord[], genreFilter: string) {
  if (!genreFilter || genreFilter === allQobuzGenresValue) return albums;
  return albums.filter((album) =>
    qobuzAlbumGenreEntries(album).some((entry) => qobuzGenreOptionValue(entry) === genreFilter)
  );
}

function qobuzEmptyLabel(categoryLabel: string, genreLabel: string) {
  const subject = [categoryLabel, genreLabel].filter(Boolean).join(' ');
  return `No ${subject} albums found.`;
}

function qobuzAlbumGenreEntries(album: JsonRecord) {
  const genreId = numberValue(album.genre_id);
  return normalizeQobuzGenreEntries(album.genre, genreId)
    .concat(normalizeQobuzGenreEntries(album.genres))
    .concat(normalizeQobuzGenreEntries(album.genres_list));
}

function normalizeQobuzGenreEntries(
  value: unknown,
  fallbackGenreId?: number
): Array<{ label: string; genreId?: number }> {
  if (Array.isArray(value)) return value.flatMap((item) => normalizeQobuzGenreEntries(item));
  if (value && typeof value === 'object') {
    const record = value as JsonRecord;
    const genreId = numberValue(record.id, record.genre_id) ?? fallbackGenreId;
    return normalizeQobuzGenreEntries(record.name, genreId);
  }
  return String(value || '')
    .split(/[;,]+/)
    .map((part) => part.trim())
    .filter(Boolean)
    .map((label, _, all) => ({ label, genreId: all.length === 1 ? fallbackGenreId : undefined }));
}

function qobuzGenreOptionValue(entry: { label: string; genreId?: number }) {
  return entry.genreId ? `id:${entry.genreId}` : qobuzGenreValue(entry.label);
}

function qobuzGenreValue(label: string) {
  return label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, ' ')
    .trim();
}

function numberValue(...values: unknown[]) {
  for (const value of values) {
    const number = typeof value === 'number' ? value : Number(String(value ?? '').trim());
    if (Number.isFinite(number) && number > 0) return number;
  }
  return undefined;
}

function HomeQobuzAlbum({
  album,
  selected,
  selectionActive,
  onOpen,
  onPlay,
  onToggleSelection
}: {
  album: JsonRecord;
  selected: boolean;
  selectionActive: boolean;
  onOpen: (id: string | number) => void;
  onPlay: (id: string | number) => void;
  onToggleSelection: (album: JsonRecord) => void;
}) {
  const artist = String(album.artist || album.album_artist || 'Unknown artist');
  const art = albumArt(album);
  const albumId = idValue(album.id, album.qobuz_album_id);
  const title = titleOf(album);
  const selectionAlbum = qobuzSelectionAlbum(album);
  const open = () => (selectionActive ? onToggleSelection(selectionAlbum) : onOpen(albumId));
  return (
    <article
      className={`album-card home-qobuz-card${selectionActive ? ' is-selection-mode' : ''}${selected ? ' is-selected' : ''}`}
      onClick={open}
      onContextMenu={(event) => {
        event.preventDefault();
        onToggleSelection(selectionAlbum);
      }}
    >
      <div className="album-cover home-qobuz-card-cover">
        {art ? <img alt="" src={art} loading="lazy" /> : artFallback()}
        <span className="recent-selection-check" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M20 6 9 17l-5-5" />
          </svg>
        </span>
        <AlbumCoverPlayButton
          title="Play album"
          ariaLabel="Play album"
          onClick={(event) => {
            event.stopPropagation();
            if (selectionActive) onToggleSelection(selectionAlbum);
            else onPlay(albumId);
          }}
        />
      </div>
      <div className="album-card-text">
        <button
          className="album-title album-link"
          type="button"
          title={title}
          aria-pressed={selectionActive ? selected : undefined}
          onClick={(event) => {
            event.stopPropagation();
            open();
          }}
        >
          {title}
        </button>
        <div className="album-subtitle">{artist}</div>
      </div>
    </article>
  );
}

function HomeQobuzPlaylistCard({
  playlist,
  onOpen,
  onPlay
}: {
  playlist: JsonRecord;
  onOpen: (id: string | number) => void;
  onPlay: (id: string | number) => void;
}) {
  const playlistId = idValue(playlist.id);
  const title = titleOf(playlist, 'Untitled playlist');
  const summaryArt = qobuzPlaylistImage(playlist) || albumArt(playlist);
  const subtitle = qobuzPlaylistSubtitle(playlist);
  const cachedDetail = useMemo(() => readQobuzPlaylistDetailCache(playlistId), [playlistId]);
  const [detail, setDetail] = useState<JsonRecord | null>(cachedDetail);
  const [coverItems, setCoverItems] = useState<QueueItem[]>(() =>
    cachedDetail ? qobuzPlaylistQueueItems(cachedDetail) : []
  );
  const art = qobuzPlaylistImage(detail) || summaryArt;
  const coverPlaylist = useMemo<Playlist>(
    () => ({
      id: String(playlistId || playlist.id || title),
      name: title,
      items: coverItems
    }),
    [coverItems, playlist.id, playlistId, title]
  );

  useEffect(() => {
    if (!playlistId) {
      setCoverItems([]);
      return undefined;
    }

    const controller = new AbortController();
    const cached = readQobuzPlaylistDetailCache(playlistId);
    setDetail(cached);
    setCoverItems(cached ? qobuzPlaylistQueueItems(cached) : []);
    loadQobuzPlaylistDetailCached(playlistId, controller.signal)
      .then((nextDetail) => {
        if (!controller.signal.aborted) {
          setDetail(nextDetail);
          setCoverItems(qobuzPlaylistQueueItems(nextDetail));
        }
      })
      .catch((error) => {
        if (!controller.signal.aborted) console.warn('qobuz: playlist cover fetch failed', error);
      });

    return () => controller.abort();
  }, [playlistId]);

  const open = () => onOpen(playlistId);
  return (
    <article className="album-card home-qobuz-card home-qobuz-playlist-card" onClick={open}>
      <div className="album-cover home-qobuz-card-cover">
        {art ? (
          <QobuzPlaylistArtwork src={art} />
        ) : coverItems.length ? (
          <PlaylistCover playlist={coverPlaylist} />
        ) : (
          artFallback()
        )}
        <AlbumCoverPlayButton
          title="Play playlist"
          ariaLabel="Play playlist"
          onClick={(event) => {
            event.stopPropagation();
            onPlay(playlistId);
          }}
        />
      </div>
      <div className="album-card-text">
        <button
          className="album-title album-link"
          type="button"
          title={title}
          onClick={(event) => {
            event.stopPropagation();
            open();
          }}
        >
          {title}
        </button>
        <div className="album-subtitle">{subtitle}</div>
      </div>
    </article>
  );
}

function qobuzPlaylistSubtitle(playlist: JsonRecord) {
  const owner = String(playlist.owner || '').trim();
  const count = Number(playlist.tracks_count ?? playlist.track_count ?? 0) || 0;
  const countLabel = count ? `${count} song${count === 1 ? '' : 's'}` : '';
  return [owner, countLabel].filter(Boolean).join(' / ') || 'Qobuz playlist';
}

function qobuzSelectionAlbum(album: JsonRecord) {
  const albumId = idValue(album.id, album.qobuz_album_id);
  return {
    ...album,
    id: albumId || album.id,
    qobuz_album_id: albumId || album.qobuz_album_id,
    is_qobuz: true
  };
}
