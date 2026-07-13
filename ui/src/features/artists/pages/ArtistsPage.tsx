import { useEffect, useMemo, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord, LibraryBrowseParams } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { SetupNotice } from '../../../shared/ui/SetupNotice';
import { usePagedLibraryBrowse } from '../../library/hooks/usePagedLibraryBrowse';

export function ArtistsPage({
  onOpen,
  onOpenMusicFolders
}: {
  onOpen: (name: string) => void;
  onOpenMusicFolders: () => void;
}) {
  const [artistImages, setArtistImages] = useState<Record<string, ArtistImageResult>>({});
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [sortKey, setSortKey] = useState<ArtistSortKey>('popularity');
  const [sortDirection, setSortDirection] = useState<ArtistSortDirection>('desc');
  const [sortOpen, setSortOpen] = useState(false);
  const artistImagesRef = useRef(artistImages);
  const profileImageWarmStartedRef = useRef(false);
  const sortRootRef = useRef<HTMLDivElement | null>(null);
  const browseParams = useMemo<LibraryBrowseParams>(
    () => ({
      q: query,
      limit: artistsPerPage,
      offset: page * artistsPerPage,
      sort: sortKey,
      direction: sortDirection
    }),
    [page, query, sortDirection, sortKey]
  );
  const browse = usePagedLibraryBrowse('artists', browseParams);
  const artistItems = useMemo(() => buildArtistItems(browse.page.items), [browse.page.items]);
  const pageCount = Math.max(1, Math.ceil(browse.page.total / artistsPerPage));
  const currentPage = Math.min(page, pageCount - 1);
  const pageStart = currentPage * artistsPerPage;
  const pageArtists = artistItems;
  const showingStart = browse.page.total ? pageStart + 1 : 0;
  const showingEnd = Math.min(pageStart + pageArtists.length, browse.page.total);
  const indicator = artistPageIndicator({
    query,
    showingEnd,
    showingStart,
    totalCount: browse.page.total
  });

  useEffect(() => {
    setPage(0);
  }, [query, sortDirection, sortKey]);

  useEffect(() => {
    setPage((current) => Math.min(current, Math.max(0, pageCount - 1)));
  }, [pageCount]);

  useEffect(() => {
    artistImagesRef.current = artistImages;
  }, [artistImages]);

  useEffect(() => {
    if (query.trim() || profileImageWarmStartedRef.current) return undefined;
    profileImageWarmStartedRef.current = true;
    const controller = new AbortController();
    endpoints
      .warmArtistProfileImageCache(artistsPerPage, controller.signal)
      .then((result) => {
        if (controller.signal.aborted) return;
        const warmedImages = artistImagesFromWarmResult(result);
        if (!Object.keys(warmedImages).length) return;
        setArtistImages((current) => {
          const next = { ...current, ...warmedImages };
          artistImagesRef.current = next;
          return next;
        });
      })
      .catch(() => undefined);

    return () => {
      controller.abort();
    };
  }, [query]);

  useEffect(() => {
    const controller = new AbortController();
    let active = true;

    const loadVisibleArtistImages = async () => {
      const uniqueArtists = Array.from(
        new Map(
          pageArtists
            .filter((artist) => !artist.imageUrl && artist.imageKey)
            .map((artist) => [artist.imageKey, artist])
        ).values()
      );

      for (const artist of uniqueArtists) {
        if (!active || controller.signal.aborted) break;
        if (artistImagesRef.current[artist.imageKey]) continue;
        let resolved: ArtistImageResult;
        try {
          resolved = await qobuzArtistImage(artist.name, controller.signal);
        } catch {
          if (controller.signal.aborted) break;
          resolved = { imageUrl: '' };
        }
        if (!active || controller.signal.aborted) break;
        setArtistImages((current) => {
          const next = { ...current, [artist.imageKey]: resolved };
          artistImagesRef.current = next;
          return next;
        });
      }
    };

    loadVisibleArtistImages().catch(() => undefined);

    return () => {
      active = false;
      controller.abort();
    };
  }, [pageArtists]);

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

  return (
    <section className="view artists-view">
      <div className="library-page-heading">
        <div>
          <div className="section-label">My library</div>
          <h1>Artists</h1>
        </div>
      </div>
      <div className="songs-toolbar artists-toolbar">
        <div className="songs-search-cluster" ref={sortRootRef}>
          <label className="songs-search-field">
            <span className="sr-only">Search artists</span>
            <Icon path="M10.5 18a7.5 7.5 0 1 1 5.3-12.8 7.5 7.5 0 0 1-5.3 12.8Zm5.3-2.2L21 21" />
            <input
              type="search"
              value={query}
              autoComplete="off"
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search artists"
            />
          </label>
          <button
            className={`songs-filter-button${sortOpen ? ' is-open' : ''}`}
            type="button"
            aria-label="Sort artists"
            aria-haspopup="dialog"
            aria-expanded={sortOpen}
            title="Sort artists"
            onClick={() => setSortOpen((open) => !open)}
          >
            <Icon path="M4 21v-7m0-4V3m8 18v-9m0-4V3m8 18v-5m0-4V3M2 14h4M10 8h4m4 8h4" />
          </button>
          {sortOpen ? (
            <div className="songs-sort-popover" role="dialog" aria-label="Sort artists">
              <div className="songs-sort-row">
                <span>Sort by</span>
                <SelectMenu
                  ariaLabel="Sort artists by"
                  className="songs-sort-select"
                  menuMinWidth={170}
                  value={sortKey}
                  onChange={(value) => setSortKey(value as ArtistSortKey)}
                  options={artistSortOptions}
                />
              </div>
              <div className="songs-sort-direction" aria-label="Sort direction">
                <button
                  className={`songs-sort-direction-button${sortDirection === 'desc' ? ' is-active' : ''}`}
                  type="button"
                  aria-pressed={sortDirection === 'desc'}
                  title={artistSortDirectionLabel(sortKey, 'desc')}
                  onClick={() => setSortDirection('desc')}
                >
                  <Icon path="m6 9 6 6 6-6" />
                  <span>{artistSortDirectionLabel(sortKey, 'desc')}</span>
                </button>
                <button
                  className={`songs-sort-direction-button${sortDirection === 'asc' ? ' is-active' : ''}`}
                  type="button"
                  aria-pressed={sortDirection === 'asc'}
                  title={artistSortDirectionLabel(sortKey, 'asc')}
                  onClick={() => setSortDirection('asc')}
                >
                  <Icon path="m18 15-6-6-6 6" />
                  <span>{artistSortDirectionLabel(sortKey, 'asc')}</span>
                </button>
              </div>
            </div>
          ) : null}
        </div>
        <div className="songs-page-status" aria-live="polite">
          {indicator}
        </div>
        <nav className="songs-pagination" aria-label="Artist pages">
          <button
            className="songs-page-button"
            type="button"
            aria-label="Previous artist page"
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
            aria-label="Next artist page"
            disabled={currentPage >= pageCount - 1}
            onClick={() => setPage((value) => Math.min(pageCount - 1, value + 1))}
          >
            <Icon path="m9 18 6-6-6-6" />
          </button>
        </nav>
      </div>
      {browse.loading && !browse.loaded ? (
        <ArtistGridSkeleton count={artistsPerPage} />
      ) : browse.error && !pageArtists.length ? (
        <div className="songs-empty-state artists-empty-state library-empty-state">
          Artists are unavailable.
        </div>
      ) : (
        <>
          <div className="artist-list">
            {pageArtists.map((artist) => (
              <button
                className="artist-card"
                type="button"
                key={artist.key}
                onClick={() => onOpen(artist.name)}
              >
                <div
                  className={`artist-avatar${artistImageUrl(artist, artistImages) ? ' has-image' : ''}`}
                >
                  {artistImageUrl(artist, artistImages) ? (
                    <img alt="" src={artistImageUrl(artist, artistImages)} loading="lazy" />
                  ) : (
                    <span>{artist.name.slice(0, 1).toUpperCase()}</span>
                  )}
                </div>
                <strong title={artist.name}>{artist.name}</strong>
              </button>
            ))}
          </div>
          {!pageArtists.length ? (
            query.trim() ? (
              <div className="songs-empty-state artists-empty-state">
                No artists match that search.
              </div>
            ) : (
              <SetupNotice
                actionLabel="Choose a music folder"
                message="Add a local music folder to see your artists here."
                onAction={onOpenMusicFolders}
              />
            )
          ) : null}
        </>
      )}
    </section>
  );
}

const artistsPerPage = 24;
const artistSortOptions = [
  { value: 'popularity', label: 'Popularity' },
  { value: 'name', label: 'Name' },
  { value: 'albums', label: 'Albums' },
  { value: 'songs', label: 'Songs' }
];

type ArtistSortKey = 'popularity' | 'name' | 'albums' | 'songs';
type ArtistSortDirection = 'desc' | 'asc';

type ArtistGridItem = {
  key: string;
  imageKey: string;
  imageUrl: string;
  name: string;
  albumCount: number;
  trackCount: number;
  playCount: number;
  listenedSecs: number;
};

type ArtistImageResult = {
  imageUrl: string;
  localImageUrl?: string;
};

function ArtistGridSkeleton({ count }: { count: number }) {
  return (
    <div className="artist-list library-loading-grid" aria-label="Loading artists" aria-busy="true">
      {Array.from({ length: count }, (_, index) => (
        <div className="artist-card library-loading-card" key={index}>
          <div className="artist-avatar skeleton-shimmer" />
          <span className="library-loading-title skeleton-shimmer" />
        </div>
      ))}
    </div>
  );
}

function buildArtistItems(artists: JsonRecord[]) {
  const byName = new Map<string, ArtistGridItem>();
  artists.forEach((artist, index) => {
    const name = artistName(artist);
    const key = artistItemKey(name, index);
    const existing = byName.get(key);
    const albumCount = numberField(artist.album_count);
    const trackCount = numberField(artist.track_count);
    const playCount = numberField(artist.play_count);
    const listenedSecs = numberField(artist.listened_secs);
    byName.set(key, {
      key,
      imageKey: key,
      imageUrl: artistImageField(artist),
      name,
      albumCount: Math.max(existing?.albumCount || 0, albumCount),
      trackCount: Math.max(existing?.trackCount || 0, trackCount),
      playCount: Math.max(existing?.playCount || 0, playCount),
      listenedSecs: Math.max(existing?.listenedSecs || 0, listenedSecs)
    });
  });
  return Array.from(byName.values());
}

function artistName(artist: JsonRecord) {
  return (
    String(artist.name || artist.artist || artist.title || 'Unknown Artist').trim() ||
    'Unknown Artist'
  );
}

function numberField(value: unknown) {
  const number = Number(value || 0);
  return Number.isFinite(number) ? number : 0;
}

function artistImageField(artist: JsonRecord) {
  return typeof artist.image_url === 'string' ? artist.image_url : '';
}

function artistLocalImageField(artist: JsonRecord) {
  return typeof artist.local_image_url === 'string' ? artist.local_image_url : '';
}

function normalizeArtistSearch(value: unknown) {
  return String(value || '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, ' ')
    .trim();
}

function artistItemKey(name: string, fallback: string | number = name) {
  return normalizeArtistSearch(name) || String(fallback).toLocaleLowerCase();
}

function artistImageUrl(artist: ArtistGridItem, artistImages: Record<string, ArtistImageResult>) {
  const resolved = artistImages[artist.imageKey];
  return resolved?.localImageUrl || artist.imageUrl || resolved?.imageUrl || '';
}

function artistSortDirectionLabel(sortKey: ArtistSortKey, direction: ArtistSortDirection) {
  if (sortKey === 'popularity') return direction === 'desc' ? 'Most popular' : 'Least popular';
  if (sortKey === 'albums') return direction === 'desc' ? 'Most albums' : 'Fewest albums';
  if (sortKey === 'songs') return direction === 'desc' ? 'Most songs' : 'Fewest songs';
  return direction === 'desc' ? 'Z to A' : 'A to Z';
}

function artistPageIndicator({
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
  const totalLabel = `${totalCount.toLocaleString()} artist${totalCount === 1 ? '' : 's'}`;
  const visibleRange =
    showingStart === showingEnd
      ? showingStart.toLocaleString()
      : `${showingStart.toLocaleString()}-${showingEnd.toLocaleString()}`;
  if (query.trim()) {
    const matchLabel = `${totalCount.toLocaleString()} match${totalCount === 1 ? '' : 'es'}`;
    return totalCount ? `Showing ${visibleRange} of ${matchLabel}` : `Showing 0 of ${matchLabel}`;
  }
  if (!totalCount) return 'Showing 0 artists';
  return `Showing ${visibleRange} of ${totalLabel}`;
}

async function qobuzArtistImage(name: string, signal: AbortSignal): Promise<ArtistImageResult> {
  const key = artistItemKey(name);
  if (!key) return { imageUrl: '' };
  const cached = qobuzArtistImageCache.get(key);
  if (cached) return cached;

  const result = await endpoints.qobuzArtistImage(name, signal);
  const imageUrl = artistImageField(result || {});
  const localImageUrl = artistLocalImageField(result || {});
  const resolved = { imageUrl, ...(localImageUrl ? { localImageUrl } : {}) };
  qobuzArtistImageCache.set(key, resolved);
  return resolved;
}

function artistImagesFromWarmResult(result: JsonRecord) {
  const images = Array.isArray(result.images) ? result.images : [];
  const warmedImages: Record<string, ArtistImageResult> = {};
  images.forEach((item) => {
    if (!item || typeof item !== 'object') return;
    const record = item as JsonRecord;
    const name = artistName(record);
    const key = artistItemKey(name);
    const imageUrl = artistImageField(record);
    const localImageUrl = artistLocalImageField(record);
    if (!key || (!imageUrl && !localImageUrl)) return;
    const resolved = { imageUrl, ...(localImageUrl ? { localImageUrl } : {}) };
    qobuzArtistImageCache.set(key, resolved);
    warmedImages[key] = resolved;
  });
  return warmedImages;
}

const qobuzArtistImageCache = new Map<string, ArtistImageResult>();
