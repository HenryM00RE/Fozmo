import { useEffect, useMemo, useRef, useState } from 'react';
import { normalizeQobuzAlbumId, resolveLocalAlbumId, safeArray } from '../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum } from '../../shared/types';
import { HistoryAllModal } from './components/HistoryAllModal';
import { HistoryOverview } from './components/HistoryOverview';
import { HistoryRankPanel } from './components/HistoryRankPanel';
import { HistoryRecentTracks } from './components/HistoryRecentTracks';
import { loadHistoryStats } from './model/historyData';
import {
  HISTORY_RANGES,
  type HistoryRange,
  type HistoryRankKind,
  type HistoryTarget,
  normalizeRange
} from './model/historyModel';

type HistoryPageProps = {
  stats: JsonRecord | null;
  statsLoading: boolean;
  recent: JsonRecord[];
  recentLoading: boolean;
  albums: LibraryAlbum[];
  onOpenAlbum: (id: string | number) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onOpenArtist: (name: string) => void;
  onNotice?: (message: string) => void;
};

type HistoryAllState = {
  title: string;
  items: JsonRecord[];
  kind: HistoryRankKind | 'recent';
} | null;

export function HistoryPage({
  stats: initialStats,
  statsLoading,
  recent,
  recentLoading,
  albums,
  onOpenAlbum,
  onOpenQobuzAlbum,
  onOpenArtist,
  onNotice
}: HistoryPageProps) {
  const initialRange = normalizeRange(initialStats?.range);
  const [range, setRange] = useState<HistoryRange>(initialRange);
  const [stats, setStats] = useState<JsonRecord | null>(initialStats);
  const [error, setError] = useState(false);
  const [rangeLoading, setRangeLoading] = useState(false);
  const [allModal, setAllModal] = useState<HistoryAllState>(null);
  const lastLoadedRangeRef = useRef<HistoryRange | ''>(initialStats ? initialRange : '');

  useEffect(() => {
    setStats(initialStats);
    setError(false);
    const nextRange = normalizeRange(initialStats?.range);
    setRange(nextRange);
    if (initialStats) lastLoadedRangeRef.current = nextRange;
  }, [initialStats]);

  useEffect(() => {
    if (lastLoadedRangeRef.current === range) return;
    let active = true;
    setRangeLoading(true);
    loadHistoryStats(range)
      .then((nextStats) => {
        if (!active) return;
        setStats(nextStats);
        setError(false);
        const responseRange = normalizeRange(nextStats?.range || range);
        lastLoadedRangeRef.current = responseRange;
        if (nextStats?.range) {
          if (responseRange !== range) setRange(responseRange);
        }
      })
      .catch(() => {
        if (active) setError(true);
      })
      .finally(() => {
        if (active) setRangeLoading(false);
      });
    return () => {
      active = false;
    };
  }, [range]);

  const recentTracks = useMemo(() => {
    const statRecent = safeArray<JsonRecord>(stats?.recent_tracks);
    return statRecent.length ? statRecent : recent;
  }, [recent, stats]);
  const topArtists = safeArray<JsonRecord>(stats?.top_artists);
  const topReleases = safeArray<JsonRecord>(stats?.top_albums || stats?.albums);
  const topSongs = safeArray<JsonRecord>(stats?.top_songs || stats?.top_tracks || stats?.tracks);
  const historyLoading = statsLoading || rangeLoading || (!stats && recentLoading);
  const showSkeleton = historyLoading;

  const openTarget = (target: HistoryTarget | null) => {
    if (!target) return;
    if (target.type === 'artist') {
      if (target.artist) onOpenArtist(target.artist);
      return;
    }

    if (target.qobuz_album_id) {
      const qobuzAlbumId = normalizeQobuzAlbumId(target.qobuz_album_id);
      if (qobuzAlbumId) onOpenQobuzAlbum(qobuzAlbumId);
      return;
    }

    const albumId =
      target.album_id ??
      resolveLocalAlbumId(
        {
          title: target.album,
          album_artist: target.artist,
          artist: target.artist
        },
        albums
      );

    if (albumId === null || albumId === undefined || albumId === '') {
      onNotice?.('No album link available for this history item');
      return;
    }
    onOpenAlbum(albumId as string | number);
  };

  return (
    <section
      className={`view history-view${historyLoading ? ' is-loading' : ''}`}
      aria-busy={historyLoading}
    >
      <div className="library-page-heading">
        <div>
          <h1>History</h1>
        </div>
        <div className="segmented history-range" role="group" aria-label="History range">
          {HISTORY_RANGES.map((option) => (
            <button
              type="button"
              className={option.value === range ? 'on is-active' : undefined}
              aria-pressed={option.value === range}
              key={option.value}
              onClick={() => setRange(option.value)}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>

      <section className="history-dashboard">
        {error ? (
          <div className="history-empty">Listening history is unavailable.</div>
        ) : showSkeleton ? (
          <HistoryLoadingSkeleton />
        ) : (
          <div className="profile-data-refresh-surface">
            <HistoryOverview stats={stats} />
            <section className="history-rank-grid">
              <HistoryRankPanel
                title="Top artists"
                items={topArtists}
                kind="artist"
                onOpenAll={() =>
                  setAllModal({ title: 'Top artists', items: topArtists, kind: 'artist' })
                }
                onOpenTarget={openTarget}
              />
              <HistoryRankPanel
                title="Top releases"
                items={topReleases}
                onOpenAll={() =>
                  setAllModal({ title: 'Top releases', items: topReleases, kind: 'default' })
                }
                onOpenTarget={openTarget}
              />
              <HistoryRankPanel
                title="Top songs"
                items={topSongs}
                kind="song"
                onOpenAll={() => setAllModal({ title: 'Top songs', items: topSongs, kind: 'song' })}
                onOpenTarget={openTarget}
              />
            </section>
            <HistoryRecentTracks
              recentTracks={recentTracks}
              onOpenAll={() =>
                setAllModal({ title: 'Recent tracks', items: recentTracks, kind: 'recent' })
              }
              onOpenTarget={openTarget}
            />
          </div>
        )}
      </section>
      {allModal ? (
        <HistoryAllModal
          title={allModal.title}
          items={allModal.items}
          kind={allModal.kind}
          onClose={() => setAllModal(null)}
          onOpenTarget={openTarget}
        />
      ) : null}
    </section>
  );
}

function HistoryLoadingSkeleton() {
  return (
    <div className="history-loading-skeleton">
      <section className="history-overview" aria-hidden="true">
        <span className="history-skeleton-total skeleton-shimmer" />
        <span className="history-skeleton-chart skeleton-shimmer" />
        <span className="history-skeleton-chart skeleton-shimmer" />
      </section>
      <section className="history-rank-grid" aria-hidden="true">
        {Array.from({ length: 3 }, (_, panelIndex) => (
          <div className="history-rank-panel history-skeleton-panel" key={panelIndex}>
            <span className="history-skeleton-title skeleton-shimmer" />
            {Array.from({ length: 5 }, (_, rowIndex) => (
              <HistorySkeletonRow key={rowIndex} />
            ))}
          </div>
        ))}
      </section>
      <section className="history-recent-panel history-skeleton-panel" aria-hidden="true">
        <span className="history-skeleton-title skeleton-shimmer" />
        {Array.from({ length: 6 }, (_, rowIndex) => (
          <HistorySkeletonRow key={rowIndex} />
        ))}
      </section>
    </div>
  );
}

function HistorySkeletonRow() {
  return (
    <span className="history-skeleton-row">
      <span className="history-skeleton-art skeleton-shimmer" />
      <span className="history-skeleton-copy">
        <span className="history-skeleton-line is-primary skeleton-shimmer" />
        <span className="history-skeleton-line is-secondary skeleton-shimmer" />
      </span>
      <span className="history-skeleton-meta skeleton-shimmer" />
    </span>
  );
}
