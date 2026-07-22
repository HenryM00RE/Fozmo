import { longTitleClass, queueItemArt, resolveLocalAlbumId } from '../../../shared/lib/appSupport';
import {
  qobuzDisplayName,
  queueItemToSourceRef,
  sourceRefKey,
  sourceRefToQueueItem
} from '../../../shared/lib/queue';
import type {
  JsonRecord,
  LibraryAlbum,
  QueueItem,
  QueueState,
  SourceRef
} from '../../../shared/types';
import {
  boolValue,
  isAirPlayProtocol,
  numberValue,
  stringValue
} from '../../settings/settingsModel';
import type { PlaybackAlbumTarget } from './playbackChromeState';

const VOLATILE_ARTWORK_REQUEST_KEY = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;

export type PlaybackChromeTrackModel = {
  currentAlbum: string;
  currentAlbumTarget: PlaybackAlbumTarget | null;
  currentArtist: string;
  currentArt: string | null;
  currentQueueItem: QueueItem | null;
  currentTrackName: string;
  sourceProvider: string;
  trackTitleClass: string;
};

function rateStampLabel(rate: number, bits: number) {
  const khz = rate / 1000;
  const rateLabel = Number.isInteger(khz) ? String(khz) : khz.toFixed(1);
  return bits > 0 ? `${bits}/${rateLabel}kHz` : `${rateLabel}kHz`;
}

export function signalTriggerLabel(status: JsonRecord) {
  const playbackActive =
    status.state === 'Playing' || status.state === 'Paused' || status.state === 'Starting';
  const hasSignalSource = playbackActive && Boolean(status.file_name || status.current_source);
  if (!hasSignalSource) return 'Signal';

  // Browser zones stream a server-side chain; the stamp reflects what the
  // server recorded for the current stream. Lossy encodes get their codec
  // name (the delivered rate would be misleading in a stamp this small),
  // lossless delivery shows the source quality.
  const browserSignal =
    status.browser_stream_signal && typeof status.browser_stream_signal === 'object'
      ? (status.browser_stream_signal as JsonRecord)
      : null;
  if (browserSignal) {
    const variant = stringValue(browserSignal.variant);
    if (variant === 'opus' || variant === 'qobuz_opus') return 'Opus';
    if (variant === 'qobuz_lossy') return 'MP3';
    const rate = numberValue(browserSignal.source_rate);
    if (rate > 0) return rateStampLabel(rate, numberValue(browserSignal.source_bits));
    return 'Signal';
  }

  const sourceRate = numberValue(status.source_rate);
  if (sourceRate <= 0) return 'Signal';

  const outputMode = stringValue(status.active_output_mode ?? status.output_mode, 'Pcm');
  if (outputMode === 'Dsd64') return 'DSD64';
  if (outputMode === 'Dsd128') return 'DSD128';
  if (outputMode === 'Dsd256') return 'DSD256';
  const targetRate = numberValue(status.target_rate);
  const upsamplingEnabled = isAirPlayProtocol(status.zone_protocol)
    ? false
    : boolValue(status.upsampling_enabled, false);
  const resamplingActive =
    upsamplingEnabled && sourceRate > 0 && targetRate > 0 && sourceRate !== targetRate;
  const rate = resamplingActive ? targetRate : sourceRate;
  if (rate <= 0) return 'Signal';
  const bits = resamplingActive ? numberValue(status.target_bits) : numberValue(status.source_bits);
  return rateStampLabel(rate, bits);
}

export function playbackChromeTrackModel({
  pendingArtSrc,
  albums = [],
  playbackLoading,
  queue,
  status
}: {
  pendingArtSrc: string | null;
  albums?: LibraryAlbum[];
  playbackLoading: boolean;
  queue: QueueState;
  status: JsonRecord;
}): PlaybackChromeTrackModel {
  const statusIsActive =
    status.state === 'Playing' || status.state === 'Paused' || status.state === 'Starting';
  const queuedItem =
    (statusIsActive || playbackLoading) && queue.cursor >= 0 ? queue.items[queue.cursor] : null;
  const currentSource = sourceRefFromStatus(status);
  const sourceItem = currentSource ? sourceRefToQueueItem(currentSource) : null;
  const queueMatchesStatus = queuedItem
    ? queueItemMatchesStatus(queuedItem, status, currentSource)
    : false;
  const showPendingQueueItem = playbackLoading && Boolean(queuedItem) && !queueMatchesStatus;
  const matchedQueueItem =
    !showPendingQueueItem && !queueMatchesStatus
      ? queue.items.find((item) => queueItemMatchesStatus(item, status, currentSource)) || null
      : null;
  const currentQueueItem = showPendingQueueItem
    ? queuedItem
    : queueMatchesStatus
      ? queuedItem
      : matchedQueueItem;
  const metadataItem = currentQueueItem || sourceItem;
  const currentTrackName = showPendingQueueItem
    ? currentQueueItem?.title || liveTrackTitle(status) || 'Select a track'
    : liveTrackTitle(status) || metadataItem?.title || 'Select a track';
  const currentArtist = showPendingQueueItem
    ? currentQueueItem?.artist || stringValue(status.track_artist, '')
    : stringValue(status.track_artist, '') || metadataItem?.artist || '';
  const currentAlbum = showPendingQueueItem
    ? currentQueueItem?.album || stringValue(status.track_album, '')
    : stringValue(status.track_album, '') || metadataItem?.album || '';
  const currentAlbumTarget =
    albumTargetForQueueItem(metadataItem) ||
    localAlbumTargetFromMetadata(currentAlbum, currentArtist, metadataItem, albums);
  const coverVersion = Number(status.cover_version || 0);
  const statusZoneId = stringValue(status.active_zone_id, '');
  const serverArtKey = nowPlayingServerArtKey(currentSource);
  const serverArt =
    statusZoneId && serverArtKey
      ? volatileArtworkUrl(
          `/api/zones/${encodeURIComponent(statusZoneId)}/now-playing-art?source=${encodeURIComponent(serverArtKey)}`
        )
      : null;
  const metadataArt = queueItemArt(metadataItem);
  const currentArt =
    coverVersion > 0
      ? volatileArtworkUrl(
          `${statusZoneId ? `/api/zones/${encodeURIComponent(statusZoneId)}/cover` : '/api/cover'}?v=${coverVersion}`
        )
      : metadataArt || serverArt || pendingArtSrc;

  return {
    currentAlbum,
    currentAlbumTarget,
    currentArtist,
    currentArt,
    currentQueueItem: metadataItem,
    currentTrackName,
    sourceProvider: queueSourceProvider(metadataItem) || (status.file_name ? 'Local' : ''),
    trackTitleClass: longTitleClass(currentTrackName)
  };
}

function albumTargetForQueueItem(item: QueueItem | null): PlaybackAlbumTarget | null {
  if (!item) return null;
  const albumId =
    item.albumId ?? item.qobuzTrack?.album_id ?? item.resolvedSource?.album_id ?? null;
  if (albumId === null || albumId === undefined || albumId === '') return null;
  const resolvedKind = String(item.resolvedSource?.kind || '');
  const qobuz = Boolean(item.qobuzTrack) || resolvedKind.includes('qobuz');
  return { source: qobuz ? 'qobuz' : 'local', id: albumId };
}

function localAlbumTargetFromMetadata(
  album: string,
  artist: string,
  item: QueueItem | null,
  albums: LibraryAlbum[]
): PlaybackAlbumTarget | null {
  if (!album || isQobuzQueueItem(item)) return null;
  const albumId = resolveLocalAlbumId(
    { title: album, album_artist: item?.albumArtist || artist, artist },
    albums
  );
  return albumId === null || albumId === undefined || albumId === ''
    ? null
    : { source: 'local', id: albumId };
}

function isQobuzQueueItem(item: QueueItem | null) {
  if (!item) return false;
  return Boolean(item.qobuzTrack) || String(item.resolvedSource?.kind || '').includes('qobuz');
}

function volatileArtworkUrl(url: string) {
  return `${url}${url.includes('?') ? '&' : '?'}r=${encodeURIComponent(VOLATILE_ARTWORK_REQUEST_KEY)}`;
}

function sourceRefFromStatus(status: JsonRecord): SourceRef | null {
  const source = status.current_source;
  return source && typeof source === 'object' ? (source as SourceRef) : null;
}

function nowPlayingServerArtKey(source: SourceRef | null) {
  if (!source) return '';
  if (source.kind === 'qobuz_track' && source.image_url) return sourceRefKey(source);
  if (source.kind === 'local_track' && source.art_id) return sourceRefKey(source);
  return '';
}

function liveTrackTitle(status: JsonRecord) {
  return stringValue(status.track_title, '') || stringValue(status.file_name, '');
}

function normalizedText(value: unknown) {
  return String(value ?? '')
    .trim()
    .toLowerCase();
}

function queueItemFileName(item: QueueItem) {
  return item.filename || qobuzDisplayName(item.qobuzTrack || {});
}

function queueItemMatchesStatus(
  item: QueueItem,
  status: JsonRecord,
  currentSource: SourceRef | null
) {
  if (!status.file_name && !status.track_title && !currentSource) return true;

  const itemSourceKey = sourceRefKey(queueItemToSourceRef(item));
  const statusSourceKey = sourceRefKey(currentSource);
  if (itemSourceKey && statusSourceKey && itemSourceKey === statusSourceKey) return true;

  const itemFileName = normalizedText(queueItemFileName(item));
  const statusFileName = normalizedText(status.file_name);
  if (itemFileName && statusFileName && itemFileName === statusFileName) return true;

  const itemTitle = normalizedText(item.title);
  const itemArtist = normalizedText(item.artist);
  const itemAlbum = normalizedText(item.album);
  const statusTitle = normalizedText(status.track_title);
  const statusArtist = normalizedText(status.track_artist);
  const statusAlbum = normalizedText(status.track_album);
  return Boolean(
    itemTitle &&
      statusTitle &&
      itemTitle === statusTitle &&
      (!statusArtist || !itemArtist || statusArtist === itemArtist) &&
      (!statusAlbum || !itemAlbum || statusAlbum === itemAlbum)
  );
}

function queueSourceProvider(item: QueueItem | null) {
  if (!item) return '';
  const kind = String(item.resolvedSource?.kind || '');
  if (item.qobuzTrack || kind.includes('qobuz')) return 'Qobuz';
  if (item.ref || kind.includes('local') || item.filename) return 'Local';
  return '';
}
