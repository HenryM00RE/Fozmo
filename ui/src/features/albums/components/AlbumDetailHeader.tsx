import { artFallback, formatAlbumQualityStamp } from '../../../shared/lib/appSupport';
import { displayTitleUsesFallbackFont } from '../../../shared/lib/displayTitle';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { LibraryTrack } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { QobuzSourceIcon } from '../../../shared/ui/QobuzSourceIcon';
import { ShuffleIcon } from '../../../shared/ui/ShuffleIcon';

export function AlbumDetailHeader({
  albumDate,
  art,
  artist,
  description,
  favoriteBusy,
  isFavorite,
  showQobuzStamp,
  onOpenArtist,
  onOpenDescription,
  onOpenArtwork,
  onOpenQueueMenu,
  onPlay,
  onShuffle,
  onToggleFavorite,
  title,
  titleClass,
  tracks,
  customDisplayFont
}: {
  albumDate: string;
  art: string | null;
  artist: string;
  description: string;
  favoriteBusy: boolean;
  isFavorite: boolean;
  showQobuzStamp: boolean;
  onOpenArtist: (artist: string) => void;
  onOpenDescription: () => void;
  onOpenArtwork: () => void;
  onOpenQueueMenu: (rect: DOMRect) => void;
  onPlay: () => void;
  onShuffle: () => void;
  onToggleFavorite: () => void;
  title: string;
  titleClass: string;
  tracks: LibraryTrack[];
  customDisplayFont: CustomDisplayFontSettings | null;
}) {
  const fallbackTitleClass = displayTitleUsesFallbackFont(title, customDisplayFont)
    ? ' uses-fallback-font'
    : '';
  return (
    <div className="album-detail-header">
      <button
        className="album-cover album-detail-cover-button"
        type="button"
        aria-label={`Open ${title} artwork`}
        onClick={onOpenArtwork}
      >
        {art ? <img alt="" src={art} /> : artFallback()}
      </button>
      <div className="album-detail-info">
        {albumDate ? <span className="album-detail-date section-label">{albumDate}</span> : null}
        <h1 className={`album-detail-title${titleClass}${fallbackTitleClass}`}>{title}</h1>
        <div className="album-detail-artist">
          {artist ? (
            <button className="artist-link" type="button" onClick={() => onOpenArtist(artist)}>
              {artist}
            </button>
          ) : (
            'Unknown artist'
          )}
        </div>
        <div className="album-detail-meta-row">
          {albumDate ? <span className="section-label">{albumDate}</span> : null}
          <span className="album-quality-stamp is-mobile-quality">
            {showQobuzStamp ? <QobuzSourceIcon /> : null}
            <span>{formatAlbumQualityStamp(tracks)}</span>
          </span>
        </div>
        {description ? (
          <button
            className="album-detail-about"
            type="button"
            aria-label="Read full description"
            onClick={onOpenDescription}
          >
            <div className="album-detail-about-text">{description}</div>
            <span className="album-detail-about-more" aria-hidden="true">
              More
            </span>
          </button>
        ) : null}
        <div className="album-actions">
          <div className="album-play-split" role="group" aria-label="Album playback actions">
            <button className="album-play-main" type="button" onClick={onPlay}>
              <PlaybarPlayIcon />
              <span>Play now</span>
            </button>
            <button
              className="album-play-menu-trigger"
              type="button"
              aria-label="Album queue options"
              title="Album queue options"
              onClick={(event) => {
                event.stopPropagation();
                const rect = event.currentTarget.getBoundingClientRect();
                onOpenQueueMenu(rect);
              }}
            >
              <Icon path="m6 9 6 6 6-6" />
            </button>
          </div>
          <button className="pill" type="button" title="Shuffle play" onClick={onShuffle}>
            <ShuffleIcon />
            Shuffle
          </button>
          <button
            className={`pill album-favorite${isFavorite ? ' is-favorited' : ''}`}
            type="button"
            aria-pressed={isFavorite}
            title={isFavorite ? 'Favorited' : 'Favorite'}
            disabled={favoriteBusy}
            onClick={onToggleFavorite}
          >
            <svg
              className="heart-outline"
              viewBox="0 0 24 24"
              aria-hidden="true"
              width="14"
              height="14"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
            </svg>
            <svg
              className="heart-filled"
              viewBox="0 0 24 24"
              aria-hidden="true"
              width="14"
              height="14"
              fill="currentColor"
            >
              <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
            </svg>
          </button>
          <span className="album-quality-stamp is-action-quality">
            {showQobuzStamp ? <QobuzSourceIcon /> : null}
            <span>{formatAlbumQualityStamp(tracks)}</span>
          </span>
        </div>
      </div>
    </div>
  );
}
