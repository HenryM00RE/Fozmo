import { albumArt, versionArt } from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum, QueueItem } from '../../../shared/types';

export type AlbumSelectionItem = {
  key: string;
  item: QueueItem;
};

export type AlbumTrackSelectionRouteState = {
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  onSelectionItemsChange: (items: AlbumSelectionItem[]) => void;
  onToggleSelection: (key: string) => void;
  selectedTrackKeys: Set<string>;
  selectionActive: boolean;
};

export function albumArtworkForViewingVersion(
  album: LibraryAlbum | null | undefined,
  viewingVersion: JsonRecord | null | undefined
) {
  const albumArtwork = albumArt(album);
  if (viewingVersion?.provider !== 'qobuz') return albumArtwork;
  return versionArt(viewingVersion) || albumArtwork;
}

export function albumTrackSelectionKeyForQueueItem(
  item: QueueItem | null,
  fallback: string | number = ''
) {
  if (!item) return '';
  if (item.resolvedSource) {
    const source = item.resolvedSource;
    return `resolved:${source.kind || 'source'}:${source.track_id || source.file_name || item.filename || fallback}`;
  }
  if (item.qobuzTrack?.id != null) return `qobuz:${item.qobuzTrack.id}`;
  if (item.qobuzTrack?.track_id != null) return `qobuz:${item.qobuzTrack.track_id}`;
  if (item.ref?.track_id != null) return `local-id:${item.ref.track_id}`;
  if (item.ref?.file_name) return `local-file:${item.ref.file_name}`;
  if (item.filename) return `file:${item.filename}`;
  return `track:${item.title || 'untitled'}:${fallback}`;
}
