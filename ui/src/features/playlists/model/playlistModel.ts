import { endpoints } from '../../../shared/lib/api';
import { sourceTrack } from '../../../shared/lib/appSupport';
import { normalizeQueueItem } from '../../../shared/lib/queue';
import type { LibraryTrack, Playlist, QueueItem } from '../../../shared/types';
import type { PlaybackRouteActions } from '../../playback/model/playbackRouteActions';

type PlayItems = (items: QueueItem[], startIndex?: number) => void;

function trackId(value: unknown) {
  const id = Number(value);
  return Number.isFinite(id) && id > 0 ? id : 0;
}

function albumId(value: unknown) {
  return value === null || value === undefined || value === '' ? null : value;
}

function localTrackLookup(tracks?: LibraryTrack[]) {
  if (!tracks?.length) return new Map<number, LibraryTrack>();
  return new Map(
    tracks
      .map((track) => [trackId(track.id ?? track.track_id), track] as const)
      .filter(([id]) => id > 0)
  );
}

export function enrichPlaylistItemAlbum(
  item: QueueItem,
  tracksById: Map<number, LibraryTrack>
): QueueItem {
  if (albumId(item.albumId) || item.qobuzTrack) return item;
  const id = trackId(item.ref?.track_id ?? item.resolvedSource?.track_id);
  const track = id ? tracksById.get(id) : null;
  const resolvedAlbumId = albumId(track?.album_id);
  if (!resolvedAlbumId) return item;
  return {
    ...item,
    albumId: resolvedAlbumId as string | number,
    album: item.album || track?.album || '',
    albumArtist: item.albumArtist || track?.album_artist || track?.artist || item.artist,
    artId: item.artId ?? track?.art_id ?? null,
    imageUrl: item.imageUrl || track?.image_url || null
  };
}

export function playlistItems(playlist: Playlist, tracks?: LibraryTrack[]) {
  const tracksById = localTrackLookup(tracks);
  return (playlist.items || [])
    .map(normalizeQueueItem)
    .filter(Boolean)
    .map((item) => enrichPlaylistItemAlbum(item as QueueItem, tracksById)) as QueueItem[];
}

export function playlistCreatedAt(playlist: Playlist) {
  return Number(playlist.createdAt || playlist.created_at || Date.now());
}

export function playlistUpdatedAt(playlist: Playlist) {
  return Number(playlist.updatedAt || playlist.updated_at || playlistCreatedAt(playlist));
}

export function mostRecentPlaylists(playlists: Playlist[], limit = 5) {
  return playlists
    .map((playlist, index) => ({
      index,
      playlist,
      updatedAt: Number(
        playlist.updatedAt ?? playlist.updated_at ?? playlist.createdAt ?? playlist.created_at ?? 0
      )
    }))
    .sort((left, right) => {
      const leftUpdatedAt = Number.isFinite(left.updatedAt) ? left.updatedAt : 0;
      const rightUpdatedAt = Number.isFinite(right.updatedAt) ? right.updatedAt : 0;
      return rightUpdatedAt - leftUpdatedAt || left.index - right.index;
    })
    .slice(0, Math.max(0, limit))
    .map(({ playlist }) => playlist);
}

export async function savePlaylistItems(playlist: Playlist, items: QueueItem[]) {
  return endpoints.savePlaylist(playlist.id, {
    name: playlist.name,
    createdAt: playlistCreatedAt(playlist),
    updatedAt: Date.now(),
    items
  });
}

export async function savePlaylistName(playlist: Playlist, name: string) {
  return endpoints.savePlaylist(playlist.id, {
    name,
    createdAt: playlistCreatedAt(playlist),
    updatedAt: Date.now(),
    items: playlistItems(playlist)
  });
}

export function shuffledItems(items: QueueItem[]) {
  const copy = items.slice();
  for (let index = copy.length - 1; index > 0; index -= 1) {
    const swapIndex = Math.floor(Math.random() * (index + 1));
    [copy[index], copy[swapIndex]] = [copy[swapIndex], copy[index]];
  }
  return copy;
}

function withPlaylistContext(item: QueueItem, playlist: Playlist): QueueItem {
  const playlistContext = { playlist_id: playlist.id };
  return {
    ...item,
    playlistContext,
    qobuzTrack: item.qobuzTrack
      ? { ...item.qobuzTrack, playlist_context: playlistContext }
      : item.qobuzTrack,
    resolvedSource: item.resolvedSource
      ? { ...item.resolvedSource, playlist_context: playlistContext }
      : item.resolvedSource
  };
}

export function queueItemsForPlayback(
  playlist: Playlist,
  shuffle = false,
  tracks?: LibraryTrack[]
) {
  const items = playlistItems(playlist, tracks).map((item) =>
    withPlaylistContext(sourceTrack(item), playlist)
  );
  return shuffle ? shuffledItems(items) : items;
}

export function playPlaylist(
  playlist: Playlist,
  playItems: PlayItems,
  shuffle = false,
  startIndex = 0,
  tracks?: LibraryTrack[]
) {
  const items = queueItemsForPlayback(playlist, shuffle, tracks);
  if (!items.length) return;
  endpoints.recordPlaylist(playlist.id).catch(() => undefined);
  playItems(items, shuffle ? 0 : startIndex);
}

export function songCountLabel(count: number) {
  return `${count} song${count === 1 ? '' : 's'}`;
}

function csvCell(value: unknown) {
  let text = String(value ?? '');
  if (/^[=+\-@]/.test(text)) text = `'${text}`;
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

export function playlistCsv(items: QueueItem[]) {
  const header = [
    'Position',
    'Title',
    'Artist',
    'Album',
    'Album Artist',
    'Duration (seconds)',
    'Source',
    'Filename'
  ];
  const rows = items.map((item, index) => {
    const sourceKind = String(item.resolvedSource?.kind || '').toLowerCase();
    const source = item.qobuzTrack || sourceKind.includes('qobuz') ? 'Qobuz' : 'Local';
    const filename = item.filename || item.resolvedSource?.file_name || item.ref?.file_name || '';
    return [
      index + 1,
      item.title,
      item.artist,
      item.album,
      item.albumArtist || item.resolvedSource?.album_artist || '',
      Math.max(0, Number(item.durationSecs) || 0),
      source,
      filename
    ];
  });
  return [header, ...rows].map((row) => row.map(csvCell).join(',')).join('\r\n');
}

export function playlistCsvFilename(name: string) {
  const invalidCharacters = '<>:"/\\|?*';
  const stem = Array.from(name.trim(), (character) =>
    invalidCharacters.includes(character) || character.charCodeAt(0) < 32 ? '-' : character
  )
    .join('')
    .replace(/[. ]+$/g, '')
    .slice(0, 120);
  return `${stem || 'playlist'}.csv`;
}

export function subtitleForItem(item: QueueItem) {
  return [item.artist, item.album].filter(Boolean).join(' · ');
}

export function createPlaylistId() {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function')
    return crypto.randomUUID();
  return `playlist-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function loadPlaylists() {
  return endpoints.playlists();
}

export type PlaylistRouteState = Pick<PlaybackRouteActions, 'addItemsToQueue' | 'playItems'> & {
  createPlaylist: (name: string) => Promise<Playlist>;
  onRefresh: () => Promise<void>;
  playlists: Playlist[];
  tracks: LibraryTrack[];
};

export type PlaylistSelectionRouteState = {
  onToggleSelection: (playlistId: string) => void;
  selectedPlaylistIds: Set<string>;
  selectionActive: boolean;
};
