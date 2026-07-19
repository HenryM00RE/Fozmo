import { albumListenCount, positiveNumber, titleOf } from '../../../shared/lib/appSupport';
import { formatTime, stripFileExtension } from '../../../shared/lib/format';
import type { LibraryTrack } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { PlayingEqualizer } from '../../../shared/ui/PlayingEqualizer';
import { useLongPressSelection } from '../../../shared/ui/useLongPressSelection';
import {
  type PendingPlaybackIntentSnapshot,
  usePlaybackControlSnapshot
} from '../../playback/model/playbackControlStore';
import type { PlaybackStatus } from '../../playback/model/playbackStore';

function normalizedText(value: unknown) {
  return String(value ?? '')
    .trim()
    .toLowerCase();
}

function trackSourceRecord(track: LibraryTrack) {
  return track.qobuz_track && typeof track.qobuz_track === 'object'
    ? (track.qobuz_track as Record<string, unknown>)
    : track;
}

function trackMatchesPlaybackMetadata(
  track: LibraryTrack,
  title: string,
  artist: string,
  album: string
) {
  const source = trackSourceRecord(track);
  const trackTitle = normalizedText(
    titleOf(source, titleOf(track, stripFileExtension(track.file_name)))
  );
  if (!trackTitle || trackTitle !== title) return false;

  const trackArtist = normalizedText(source.artist || track.artist || track.album_artist);
  const trackAlbum = normalizedText(source.album || track.album);
  const artistMatches = !artist || !trackArtist || trackArtist === artist;
  const albumMatches = !album || !trackAlbum || trackAlbum === album;
  return artistMatches && albumMatches;
}

export function albumTrackPlaybackMatchContext({
  allTracks,
  isQobuz,
  playbackStatus,
  getPlaybackFilename
}: {
  allTracks: LibraryTrack[];
  isQobuz: boolean;
  playbackStatus: PlaybackStatus;
  getPlaybackFilename: (track: LibraryTrack) => string;
}) {
  const currentFileName = String(playbackStatus.file_name || '');
  const currentSource = playbackStatus.current_source;
  const currentSourceKind = String(currentSource?.kind || '');
  const currentTrackId = Number(currentSource?.track_id);
  const currentTrackTitle = normalizedText(playbackStatus.track_title || currentSource?.title);
  const currentTrackArtist = normalizedText(playbackStatus.track_artist || currentSource?.artist);
  const currentTrackAlbum = normalizedText(playbackStatus.track_album || currentSource?.album);
  const isPlaybackPlaying = playbackStatus.state === 'Playing';
  const currentSourceKindMatchesAlbum = isQobuz
    ? currentSourceKind === 'qobuz_track' || currentSourceKind === 'qobuz'
    : currentSourceKind === 'local_track' || currentSourceKind === 'local';
  const sourceTrackIdMatchCount =
    Number.isFinite(currentTrackId) && currentTrackId > 0 && currentSourceKindMatchesAlbum
      ? allTracks.filter((track) => Number(track.id ?? track.track_id) === currentTrackId).length
      : 0;
  const playbackFilenameMatchCount = currentFileName
    ? allTracks.filter((track) => getPlaybackFilename(track) === currentFileName).length
    : 0;
  const metadataMatchCount = currentTrackTitle
    ? allTracks.filter((track) =>
        trackMatchesPlaybackMetadata(
          track,
          currentTrackTitle,
          currentTrackArtist,
          currentTrackAlbum
        )
      ).length
    : 0;
  return {
    currentFileName,
    currentSourceKindMatchesAlbum,
    currentTrackId,
    currentTrackTitle,
    currentTrackArtist,
    currentTrackAlbum,
    isPlaybackPlaying,
    sourceTrackIdMatchCount,
    playbackFilenameMatchCount,
    metadataMatchCount
  };
}

export function albumTrackPlaybackState({
  track,
  playbackFilename,
  context
}: {
  track: LibraryTrack;
  playbackFilename: string;
  context: ReturnType<typeof albumTrackPlaybackMatchContext>;
}) {
  const trackId = Number(track.id ?? track.track_id);
  const activeBySource =
    Number.isFinite(context.currentTrackId) &&
    Number.isFinite(trackId) &&
    context.currentTrackId > 0 &&
    trackId > 0 &&
    context.currentSourceKindMatchesAlbum &&
    context.sourceTrackIdMatchCount === 1 &&
    context.currentTrackId === trackId;
  const activeByFileName =
    context.playbackFilenameMatchCount === 1 && playbackFilename === context.currentFileName;
  const activeByMetadata =
    context.metadataMatchCount === 1 &&
    trackMatchesPlaybackMetadata(
      track,
      context.currentTrackTitle,
      context.currentTrackArtist,
      context.currentTrackAlbum
    );
  const active = activeBySource || activeByFileName || activeByMetadata;
  return {
    active,
    playing: active && context.isPlaybackPlaying
  };
}

function pendingIntentMatchesTrack(
  track: LibraryTrack,
  title: string,
  playbackFilename: string,
  pendingIntent: PendingPlaybackIntentSnapshot | null
) {
  if (!pendingIntent) return false;
  const pendingFileName = normalizedText(pendingIntent.fileName);
  if (pendingFileName && normalizedText(playbackFilename) === pendingFileName) return true;

  const pendingTitle = normalizedText(pendingIntent.title);
  if (!pendingTitle || normalizedText(title) !== pendingTitle) return false;

  const pendingArtist = normalizedText(pendingIntent.artist);
  const trackArtist = normalizedText(track.artist || track.album_artist);
  return !pendingArtist || !trackArtist || trackArtist === pendingArtist;
}

export function AlbumTrackList({
  tracks,
  allTracks,
  isQobuz,
  playbackStatus,
  onPlay,
  onOpenMenu,
  selectedKeys,
  selectionActive,
  onToggleSelection,
  getSelectionKey,
  getPlaybackFilename
}: {
  tracks: LibraryTrack[];
  allTracks: LibraryTrack[];
  isQobuz: boolean;
  playbackStatus: PlaybackStatus;
  onPlay: (index: number) => void;
  onOpenMenu: (index: number, rect: DOMRect) => void;
  selectedKeys: Set<string>;
  selectionActive: boolean;
  onToggleSelection: (key: string) => void;
  getSelectionKey: (track: LibraryTrack, index: number) => string;
  getPlaybackFilename: (track: LibraryTrack) => string;
}) {
  const playbackControls = usePlaybackControlSnapshot();
  const playbackMatchContext = albumTrackPlaybackMatchContext({
    allTracks,
    isQobuz,
    playbackStatus,
    getPlaybackFilename
  });
  const longPressSelection = useLongPressSelection({
    onSelect: onToggleSelection,
    resolveSelection: (target, currentTarget) => {
      const row = target.closest<HTMLElement>('[data-album-track-selection-key]');
      if (!row || !currentTarget.contains(row)) return null;
      return row.dataset.albumTrackSelectionKey || null;
    }
  });
  return (
    <ul className="file-list song-list" {...longPressSelection}>
      {tracks.map((track, index) => {
        const allIndex = allTracks.indexOf(track);
        const playIndex = allIndex >= 0 ? allIndex : index;
        const trackNumber = positiveNumber(track.track_number) || index + 1;
        const title = titleOf(track, stripFileExtension(track.file_name));
        const plays = albumListenCount(track);
        const selectionKey = getSelectionKey(track, playIndex);
        const selected = selectionKey ? selectedKeys.has(selectionKey) : false;
        const playbackFilename = getPlaybackFilename(track);
        const { active, playing } = albumTrackPlaybackState({
          track,
          playbackFilename,
          context: playbackMatchContext
        });
        const loading =
          playbackControls.playbackLoading &&
          !playing &&
          pendingIntentMatchesTrack(
            track,
            title,
            playbackFilename,
            playbackControls.pendingPlaybackIntent
          );
        const playOrSelect = () => {
          if (selectionActive && selectionKey) onToggleSelection(selectionKey);
          else if (loading) return;
          else onPlay(playIndex);
        };
        return (
          <li
            className={`file-item album-track-item${isQobuz ? ' qobuz-track-item' : ''}${active ? ' active' : ''}${loading ? ' is-loading' : ''}${selectionActive ? ' is-selection-mode' : ''}${selected ? ' is-selected' : ''}`}
            key={String(track.id || track.track_id || track.file_name || `${title}-${index}`)}
            data-album-track-selection-key={selectionKey}
            data-filename={playbackFilename}
            onClick={playOrSelect}
          >
            <span className="track-row-hover-surface" aria-hidden="true" />
            <div className="album-track-index">
              <span className="track-number">{trackNumber}</span>
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
                className={`btn-item-play${playing ? ' is-playing' : ''}${loading ? ' is-loading' : ''}`}
                type="button"
                title={loading ? 'Loading' : 'Play'}
                aria-label={loading ? `Loading ${title}` : `Play ${title}`}
                aria-busy={loading ? 'true' : undefined}
                disabled={loading}
                onClick={(event) => {
                  event.stopPropagation();
                  if (selectionActive && selectionKey) onToggleSelection(selectionKey);
                  else if (loading) return;
                  else onPlay(playIndex);
                }}
              >
                {playing ? (
                  <PlayingEqualizer />
                ) : loading ? (
                  <span className="album-track-loading-spinner" aria-hidden="true" />
                ) : (
                  <PlaybarPlayIcon className="album-track-play-icon" />
                )}
              </button>
            </div>
            <div className="file-details">
              <span className="file-name" title={title}>
                {title}
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
                onOpenMenu(playIndex, event.currentTarget.getBoundingClientRect());
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
