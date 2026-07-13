import { memo, useEffect, useRef } from 'react';
import {
  albumArt,
  albumArtSrcSet,
  albumVersionLabel,
  artFallback,
  recentlyPlayedSelectionKey,
  titleOf
} from '../../../shared/lib/appSupport';
import type { LibraryAlbum } from '../../../shared/types';
import { AlbumCoverPlayButton } from '../../../shared/ui/AlbumCoverPlayButton';

const progressiveAlbumSkeletonCount = 24;

export function AlbumGrid({
  albums,
  compact = false,
  emptyLabel = 'No albums indexed yet.',
  showArtist = true,
  virtualized = false,
  totalCount,
  loadingMore = false,
  selectedKeys,
  selectionActive = false,
  selectionAlbumForAlbum,
  onOpen,
  onPlay,
  onOpenArtist,
  onLoadMore,
  onToggleSelection
}: {
  albums: LibraryAlbum[];
  compact?: boolean;
  emptyLabel?: string;
  showArtist?: boolean;
  virtualized?: boolean;
  totalCount?: number;
  loadingMore?: boolean;
  selectedKeys?: Set<string>;
  selectionActive?: boolean;
  selectionAlbumForAlbum?: (album: LibraryAlbum) => LibraryAlbum;
  onOpen: (album: LibraryAlbum) => void;
  onPlay?: (album: LibraryAlbum) => void;
  onOpenArtist?: (name: string) => void;
  onLoadMore?: () => void;
  onToggleSelection?: (album: LibraryAlbum) => void;
}) {
  const effectiveTotal = totalCount ?? albums.length;
  if (!effectiveTotal && !loadingMore) return <div className="file-limits">{emptyLabel}</div>;

  if (virtualized) {
    return (
      <ProgressiveAlbumGrid
        albums={albums}
        compact={compact}
        loadingMore={loadingMore}
        selectedKeys={selectedKeys}
        selectionActive={selectionActive}
        selectionAlbumForAlbum={selectionAlbumForAlbum}
        showArtist={showArtist}
        onOpen={onOpen}
        onPlay={onPlay}
        onOpenArtist={onOpenArtist}
        onLoadMore={onLoadMore}
        onToggleSelection={onToggleSelection}
      />
    );
  }

  return (
    <div className={`album-grid${compact ? ' compact' : ''}`}>
      {albums.map((album, index) => (
        <AlbumCardContainer
          album={album}
          index={index}
          key={albumCardKey(album, index)}
          selectedKeys={selectedKeys}
          selectionActive={selectionActive}
          selectionAlbumForAlbum={selectionAlbumForAlbum}
          showArtist={showArtist}
          onOpen={onOpen}
          onPlay={onPlay}
          onOpenArtist={onOpenArtist}
          onToggleSelection={onToggleSelection}
        />
      ))}
    </div>
  );
}

function ProgressiveAlbumGrid({
  albums,
  compact,
  loadingMore,
  selectedKeys,
  selectionActive,
  selectionAlbumForAlbum,
  showArtist,
  onOpen,
  onPlay,
  onOpenArtist,
  onLoadMore,
  onToggleSelection
}: {
  albums: LibraryAlbum[];
  compact: boolean;
  loadingMore: boolean;
  selectedKeys?: Set<string>;
  selectionActive: boolean;
  selectionAlbumForAlbum?: (album: LibraryAlbum) => LibraryAlbum;
  showArtist: boolean;
  onOpen: (album: LibraryAlbum) => void;
  onPlay?: (album: LibraryAlbum) => void;
  onOpenArtist?: (name: string) => void;
  onLoadMore?: () => void;
  onToggleSelection?: (album: LibraryAlbum) => void;
}) {
  const sentinelRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!onLoadMore || loadingMore) return undefined;
    const sentinel = sentinelRef.current;
    if (!sentinel) return undefined;
    const scrollParent = findScrollParent(sentinel);
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) onLoadMore();
      },
      {
        root: scrollParent instanceof Window ? null : scrollParent,
        rootMargin: '1800px 0px'
      }
    );
    observer.observe(sentinel);

    return () => {
      observer.disconnect();
    };
  }, [loadingMore, onLoadMore]);

  return (
    <div className={`album-grid album-grid-progressive${compact ? ' compact' : ''}`}>
      {albums.map((album, index) => (
        <AlbumCardContainer
          album={album}
          index={index}
          key={albumCardKey(album, index)}
          selectedKeys={selectedKeys}
          selectionActive={selectionActive}
          selectionAlbumForAlbum={selectionAlbumForAlbum}
          showArtist={showArtist}
          onOpen={onOpen}
          onPlay={onPlay}
          onOpenArtist={onOpenArtist}
          onToggleSelection={onToggleSelection}
        />
      ))}
      <div className="album-grid-sentinel" ref={sentinelRef} aria-hidden="true" />
      {loadingMore ? <AlbumGridSkeletonCards count={progressiveAlbumSkeletonCount} /> : null}
    </div>
  );
}

function AlbumGridSkeletonCards({ count }: { count: number }) {
  return Array.from({ length: count }, (_, index) => (
    <AlbumGridSkeletonCard key={`album-grid-skeleton-${index}`} />
  ));
}

function AlbumGridSkeletonCard() {
  return (
    <article
      className="album-card library-loading-card album-grid-progressive-skeleton"
      aria-hidden="true"
    >
      <div className="album-cover skeleton-shimmer" />
      <div className="album-card-text">
        <span className="library-loading-title skeleton-shimmer" />
        <span className="library-loading-meta skeleton-shimmer" />
      </div>
    </article>
  );
}

function AlbumCardContainer({
  album,
  index,
  selectedKeys,
  selectionActive,
  selectionAlbumForAlbum,
  showArtist,
  onOpen,
  onPlay,
  onOpenArtist,
  onToggleSelection
}: {
  album: LibraryAlbum;
  index: number;
  selectedKeys?: Set<string>;
  selectionActive: boolean;
  selectionAlbumForAlbum?: (album: LibraryAlbum) => LibraryAlbum;
  showArtist: boolean;
  onOpen: (album: LibraryAlbum) => void;
  onPlay?: (album: LibraryAlbum) => void;
  onOpenArtist?: (name: string) => void;
  onToggleSelection?: (album: LibraryAlbum) => void;
}) {
  const selectionAlbum = selectionAlbumForAlbum ? selectionAlbumForAlbum(album) : album;
  const selectionKey = recentlyPlayedSelectionKey(selectionAlbum);
  const selected = selectedKeys?.has(selectionKey) || false;
  return (
    <AlbumCard
      album={album}
      imagePriority={index < 12}
      selected={selected}
      selectionActive={selectionActive}
      selectionAlbum={selectionAlbum}
      showArtist={showArtist}
      onOpen={onOpen}
      onPlay={onPlay}
      onOpenArtist={onOpenArtist}
      onToggleSelection={onToggleSelection}
    />
  );
}

const AlbumCard = memo(function AlbumCard({
  album,
  imagePriority,
  selected,
  selectionActive,
  selectionAlbum,
  showArtist,
  onOpen,
  onPlay,
  onOpenArtist,
  onToggleSelection
}: {
  album: LibraryAlbum;
  imagePriority: boolean;
  selected: boolean;
  selectionActive: boolean;
  selectionAlbum: LibraryAlbum;
  showArtist: boolean;
  onOpen: (album: LibraryAlbum) => void;
  onPlay?: (album: LibraryAlbum) => void;
  onOpenArtist?: (name: string) => void;
  onToggleSelection?: (album: LibraryAlbum) => void;
}) {
  const art = albumArt(album, 256);
  const srcSet = albumArtSrcSet(album);
  const artist = String(album.album_artist || album.artist || '');
  const year = album.year ? String(album.year) : '';
  const version = albumVersionLabel(album);
  const details = [version, year].filter(Boolean).join(' · ');
  const title = titleOf(album);
  const openAlbum = () => onOpen(album);
  const openOrSelect = () => {
    if (selectionActive && onToggleSelection) onToggleSelection(selectionAlbum);
    else openAlbum();
  };

  return (
    <article
      className={`album-card${selectionActive ? ' is-selection-mode' : ''}${selected ? ' is-selected' : ''}`}
      role="button"
      tabIndex={0}
      aria-pressed={selectionActive ? selected : undefined}
      onClick={openOrSelect}
      onKeyDown={(event) => {
        if (event.target !== event.currentTarget) return;
        if (event.key !== 'Enter' && event.key !== ' ') return;
        event.preventDefault();
        openOrSelect();
      }}
      onContextMenu={(event) => {
        if (!onToggleSelection) return;
        event.preventDefault();
        onToggleSelection(selectionAlbum);
      }}
    >
      <div className="album-cover">
        {art ? (
          <img
            alt=""
            src={art}
            srcSet={srcSet}
            sizes="(max-width: 760px) 33vw, (max-width: 1200px) 20vw, 180px"
            width="256"
            height="256"
            loading={imagePriority ? 'eager' : 'lazy'}
            decoding="async"
            fetchPriority={imagePriority ? 'high' : 'low'}
          />
        ) : (
          artFallback()
        )}
        {onToggleSelection ? (
          <span className="recent-selection-check" aria-hidden="true">
            <svg viewBox="0 0 24 24">
              <path d="M20 6 9 17l-5-5" />
            </svg>
          </span>
        ) : null}
        {onPlay ? (
          <AlbumCoverPlayButton
            title="Play album"
            ariaLabel="Play album"
            onClick={(event) => {
              event.stopPropagation();
              if (selectionActive && onToggleSelection) onToggleSelection(selectionAlbum);
              else onPlay(album);
            }}
          />
        ) : null}
      </div>
      <div className="album-card-text">
        <div className="album-title" title={title}>
          {title}
        </div>
        <div className="album-subtitle">
          {showArtist && artist && onOpenArtist ? (
            <button
              className="artist-link album-card-artist"
              type="button"
              onClick={(event) => {
                event.stopPropagation();
                onOpenArtist(artist);
              }}
            >
              {artist}
            </button>
          ) : showArtist ? (
            artist || 'Unknown artist'
          ) : (
            ''
          )}
          {details ? `${showArtist && artist ? ' · ' : ''}${details}` : ''}
        </div>
      </div>
    </article>
  );
});

function albumCardKey(album: LibraryAlbum, index: number) {
  return String(album.id || album.qobuz_album_id || `${album.title}-${index}`);
}

function findScrollParent(element: HTMLElement): HTMLElement | Window {
  let parent = element.parentElement;
  while (parent) {
    const { overflowY } = window.getComputedStyle(parent);
    if (/(auto|scroll|overlay)/.test(overflowY)) {
      return parent;
    }
    parent = parent.parentElement;
  }
  return window;
}
