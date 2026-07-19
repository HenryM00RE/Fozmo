import {
  albumArt,
  artFallback,
  playlistCoverItems,
  RECENTLY_PLAYED_COLLAPSED_COUNT,
  recentlyPlayedSelectionKey,
  titleOf
} from '../../../shared/lib/appSupport';
import type { JsonRecord, Playlist } from '../../../shared/types';
import { AlbumCoverPlayButton } from '../../../shared/ui/AlbumCoverPlayButton';
import { Icon } from '../../../shared/ui/Icon';
import { useLongPressSelection } from '../../../shared/ui/useLongPressSelection';

type RecentlyPlayedSectionProps = {
  expanded: boolean;
  loading: boolean;
  onExpandedChange: (expanded: boolean) => void;
  onOpenRecent: (item: JsonRecord) => void;
  onPlayRecent: (item: JsonRecord) => void;
  onToggleRecentSelection: (item: JsonRecord) => void;
  playlists: Playlist[];
  recent: JsonRecord[];
  selectedKeys: Set<string>;
  selectionActive: boolean;
};

export function RecentlyPlayedSection({
  expanded,
  loading,
  onExpandedChange,
  onOpenRecent,
  onPlayRecent,
  onToggleRecentSelection,
  playlists,
  recent,
  selectedKeys,
  selectionActive
}: RecentlyPlayedSectionProps) {
  if (!recent.length && !loading) return null;

  const displayRecent = expanded ? recent : recent.slice(0, RECENTLY_PLAYED_COLLAPSED_COUNT);
  const canExpand = recent.length > RECENTLY_PLAYED_COLLAPSED_COUNT;
  return (
    <section
      className={`library-section home-album-shelf-section recently-played-section${loading ? ' is-loading' : ''}`}
      id="recently-played-section"
      aria-busy={loading}
    >
      <div className="panel-heading">
        <div>
          <h2>
            {canExpand ? (
              <button
                className={`recently-played-toggle${expanded ? ' is-expanded' : ''}`}
                type="button"
                aria-expanded={expanded}
                aria-controls="recently-played-albums"
                onClick={() => onExpandedChange(!expanded)}
              >
                <span>Recently Played</span>
                <Icon path="m9 18 6-6-6-6" />
              </button>
            ) : (
              'Recently Played'
            )}
          </h2>
        </div>
      </div>
      <div
        id="recently-played-albums"
        className={`album-grid compact home-album-shelf-grid${expanded ? ' rp-expanded' : ''}`}
      >
        {loading ? (
          <RecentlyPlayedSkeleton />
        ) : (
          displayRecent.map((item, index) => (
            <RecentPlayedCard
              item={item}
              playlists={playlists}
              key={recentlyPlayedSelectionKey(item) || index}
              selected={selectedKeys.has(recentlyPlayedSelectionKey(item))}
              selectionActive={selectionActive}
              onOpen={onOpenRecent}
              onPlay={onPlayRecent}
              onToggleSelection={onToggleRecentSelection}
            />
          ))
        )}
      </div>
    </section>
  );
}

function RecentlyPlayedSkeleton() {
  return Array.from({ length: RECENTLY_PLAYED_COLLAPSED_COUNT }, (_, index) => (
    <span className="album-card recently-played-card recently-played-skeleton" key={index}>
      <span className="album-cover skeleton-shimmer" />
      <span className="album-card-text">
        <span className="album-title skeleton-shimmer">&nbsp;</span>
        <span className="album-subtitle skeleton-shimmer">&nbsp;</span>
      </span>
    </span>
  ));
}

function RecentPlayedCard({
  item,
  playlists,
  selected,
  selectionActive,
  onOpen,
  onPlay,
  onToggleSelection
}: {
  item: JsonRecord;
  playlists: Playlist[];
  selected: boolean;
  selectionActive: boolean;
  onOpen: (item: JsonRecord) => void;
  onPlay: (item: JsonRecord) => void;
  onToggleSelection: (item: JsonRecord) => void;
}) {
  const title = titleOf(item);
  const subtitle = String(item.album_artist || item.artist || 'Unknown artist');
  const playlist =
    item.recent_type === 'playlist'
      ? playlists.find((candidate) => candidate.id === item.playlist_id) || item
      : null;
  const art = item.recent_type === 'playlist' ? null : albumArt(item);
  const selectionKey = recentlyPlayedSelectionKey(item);
  const open = () => (selectionActive ? onToggleSelection(item) : onOpen(item));
  const longPressSelection = useLongPressSelection({
    onSelect: onToggleSelection,
    resolveSelection: () => item
  });
  return (
    <article
      {...longPressSelection}
      className={`album-card recently-played-card${selectionActive ? ' is-selection-mode' : ''}${selected ? ' is-selected' : ''}`}
      data-recent-key={selectionKey}
      onClick={open}
    >
      <div className="album-cover rp-cover">
        {playlist ? (
          <PlaylistCover playlist={playlist} />
        ) : art ? (
          <img alt="" src={art} loading="lazy" />
        ) : (
          artFallback()
        )}
        <span className="recent-selection-check" aria-hidden="true">
          <Icon path="M20 6 9 17l-5-5" />
        </span>
        <AlbumCoverPlayButton
          title={item.recent_type === 'playlist' ? 'Play playlist' : 'Play album'}
          ariaLabel={item.recent_type === 'playlist' ? 'Play playlist' : 'Play album'}
          onClick={(event) => {
            event.stopPropagation();
            if (selectionActive) onToggleSelection(item);
            else onPlay(item);
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
        <div className="album-subtitle">{subtitle}</div>
      </div>
    </article>
  );
}

function PlaylistCover({ playlist }: { playlist: Playlist | JsonRecord }) {
  const arts = playlistCoverItems(playlist);
  if (!arts.length) return <Icon path="M4 7h12M4 12h12M4 17h8M18 15v5l4-2.5L18 15Z" />;
  return (
    <div className={`playlist-cover-mosaic count-${arts.length}`}>
      {arts.map((src) => (
        <img alt="" src={src} loading="lazy" key={src} />
      ))}
    </div>
  );
}
