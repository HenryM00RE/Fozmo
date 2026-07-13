import type {
  LibraryTrack,
  QobuzTrack,
  QueueItem,
  QueueKind,
  QueueState,
  ResolvedPlaySource,
  SourceRef
} from '../types';

function text(value: unknown, fallback = '') {
  const out = String(value ?? '').trim();
  return out || fallback;
}

function number(value: unknown, fallback = 0) {
  const out = Number(value);
  return Number.isFinite(out) ? out : fallback;
}

function playlistContext(value: unknown) {
  if (!value || typeof value !== 'object') return null;
  const playlistId = (value as { playlist_id?: unknown }).playlist_id;
  return typeof playlistId === 'string' && playlistId.trim()
    ? { playlist_id: playlistId.trim() }
    : null;
}

function queueItemPlaylistContext(item: QueueItem) {
  return (
    playlistContext(item.playlistContext) ||
    playlistContext(item.resolvedSource?.playlist_context) ||
    playlistContext(item.qobuzTrack?.playlist_context)
  );
}

export function qobuzDisplayName(track: QobuzTrack) {
  const artist = text(track.artist);
  const title = text(track.title, `qobuz:${track.id ?? track.track_id ?? ''}`);
  return artist ? `${artist} - ${title}` : title;
}

export function localTrackToQueueItem(track: LibraryTrack): QueueItem {
  const trackId = number(track.id ?? track.track_id, 0);
  const fileName = text(track.file_name ?? track.name, trackId ? String(trackId) : '');
  const localItem: QueueItem = {
    title: text(track.title ?? track.file_name ?? track.name, 'Untitled'),
    artist: text(track.artist),
    album: text(track.album),
    albumArtist: text(track.album_artist ?? track.artist),
    albumId: (track.album_id as string | number | null | undefined) ?? null,
    artId: (track.art_id as string | number | null | undefined) ?? null,
    imageUrl: (track.image_url as string | null | undefined) ?? null,
    durationSecs: number(track.duration_secs),
    filename: fileName,
    ref: trackId ? { track_id: trackId } : { file_name: fileName }
  };
  const preferred = track.preferred_play_source as ResolvedPlaySource | null | undefined;
  const preferredItem = preferred ? resolvedPlaySourceToQueueItem(preferred) : null;
  if (!preferredItem) return localItem;
  return {
    ...localItem,
    ...preferredItem,
    albumId: preferredItem.albumId ?? localItem.albumId,
    artId: preferredItem.artId ?? localItem.artId,
    imageUrl: preferredItem.imageUrl ?? localItem.imageUrl
  };
}

export function qobuzTrackToQueueItem(track: QobuzTrack): QueueItem {
  const normalized: QobuzTrack = {
    ...track,
    id: number(track.id ?? track.track_id),
    title: text(track.title, 'Untitled'),
    artist: text(track.artist),
    album: text(track.album),
    album_id: (track.album_id as string | number | null | undefined) ?? null,
    image_url: (track.image_url as string | null | undefined) ?? null,
    duration: number(track.duration ?? track.duration_secs)
  };
  return {
    title: text(normalized.title, 'Untitled'),
    artist: text(normalized.artist),
    album: text(normalized.album),
    albumId: normalized.album_id,
    imageUrl: normalized.image_url,
    durationSecs: number(normalized.duration ?? normalized.duration_secs),
    filename: qobuzDisplayName(normalized),
    qobuzTrack: normalized,
    playlistContext: playlistContext(normalized.playlist_context)
  };
}

export function sourceRefToQueueItem(source: SourceRef): QueueItem | null {
  if (!source) return null;
  if (source.kind === 'local_track' || source.kind === 'local') {
    return {
      title: text(source.title, source.track_id ? `Track ${source.track_id}` : 'Untitled'),
      artist: text(source.artist),
      album: text(source.album),
      albumArtist: text(source.album_artist ?? source.artist),
      albumId: source.album_id ?? null,
      artId: source.art_id ?? null,
      imageUrl: null,
      durationSecs: number(source.duration_secs),
      filename: text(source.file_name, source.track_id ? String(source.track_id) : ''),
      ref: source.track_id
        ? { track_id: number(source.track_id) }
        : { file_name: source.file_name ?? null },
      resolvedSource: source,
      radio: Boolean(source.radio),
      playlistContext: playlistContext(source.playlist_context)
    };
  }
  if (source.kind === 'qobuz_track' || source.kind === 'qobuz') {
    return {
      ...qobuzTrackToQueueItem({
        id: source.track_id,
        title: source.title ?? undefined,
        artist: source.artist ?? undefined,
        album: source.album ?? undefined,
        album_id: source.album_id ?? null,
        image_url: source.image_url ?? null,
        duration_secs: source.duration_secs ?? undefined,
        format_id: source.format_id ?? null,
        radio: Boolean(source.radio),
        playlist_context: playlistContext(source.playlist_context)
      }),
      resolvedSource: source
    };
  }
  return null;
}

export function resolvedPlaySourceToQueueItem(source: ResolvedPlaySource): QueueItem | null {
  if (!source) return null;
  if (source.kind === 'local') {
    return sourceRefToQueueItem({
      kind: 'local_track',
      track_id: number(source.track_id),
      file_name: source.file_name ?? null,
      title: source.title ?? null,
      artist: source.artist ?? null,
      album: source.album ?? null,
      album_artist: source.album_artist ?? source.artist ?? null,
      album_id: source.album_id ?? null,
      art_id: source.art_id ?? null,
      duration_secs: source.duration_secs ?? null
    });
  }
  if (source.kind === 'qobuz') {
    return sourceRefToQueueItem({
      kind: 'qobuz_track',
      track_id: number(source.track_id),
      title: source.title ?? null,
      artist: source.artist ?? null,
      album: source.album ?? null,
      album_id: source.album_id ?? null,
      image_url: source.image_url ?? null,
      duration_secs: source.duration_secs ?? null,
      format_id: source.format_id ?? null
    });
  }
  return sourceRefToQueueItem(source as SourceRef);
}

export function queueItemToSourceRef(item: QueueItem): SourceRef | null {
  const context = queueItemPlaylistContext(item);
  if (item.resolvedSource) {
    return context ? { ...item.resolvedSource, playlist_context: context } : item.resolvedSource;
  }
  if (item.qobuzTrack) {
    const trackId = number(item.qobuzTrack.id ?? item.qobuzTrack.track_id);
    if (!trackId) return null;
    return {
      kind: 'qobuz_track',
      track_id: trackId,
      title: text(item.qobuzTrack.title ?? item.title, 'Untitled'),
      artist: text(item.qobuzTrack.artist ?? item.artist),
      album: text(item.qobuzTrack.album ?? item.album),
      album_id: item.qobuzTrack.album_id ?? item.albumId ?? null,
      image_url: item.qobuzTrack.image_url ?? item.imageUrl ?? null,
      duration_secs: number(
        item.qobuzTrack.duration_secs ?? item.qobuzTrack.duration ?? item.durationSecs
      ),
      format_id: item.qobuzTrack.format_id ?? null,
      radio: Boolean(item.qobuzTrack.radio),
      playlist_context: context
    };
  }
  const trackId = number(item.ref?.track_id);
  const fileName = item.ref?.file_name || item.filename || null;
  if (!trackId && !fileName) return null;
  if (!trackId) return { file_name: fileName } as SourceRef;
  return {
    kind: 'local_track',
    track_id: trackId,
    file_name: fileName,
    title: text(item.title, trackId ? `Track ${trackId}` : 'Untitled'),
    artist: text(item.artist),
    album: text(item.album),
    album_artist: text(item.albumArtist ?? item.artist),
    album_id: item.albumId ?? null,
    art_id: item.artId ?? null,
    duration_secs: number(item.durationSecs),
    radio: Boolean(item.radio),
    playlist_context: context
  };
}

export function itemKey(item: QueueItem) {
  if (item.qobuzTrack?.id || item.qobuzTrack?.track_id)
    return `qobuz:${item.qobuzTrack.id ?? item.qobuzTrack.track_id}`;
  if (item.ref?.track_id) return `local:${item.ref.track_id}`;
  if (item.ref?.file_name) return `file:${item.ref.file_name}`;
  return item.filename || `${item.title}:${item.artist}:${item.album}`;
}

export function sourceRefKey(source?: SourceRef | null) {
  const item = source ? sourceRefToQueueItem(source) : null;
  return item ? itemKey(item) : '';
}

export function queueKindForItems(items: QueueItem[]): QueueKind {
  const kinds = new Set(
    items.map((item) => (item.qobuzTrack ? 'qobuz' : item.ref ? 'local' : null)).filter(Boolean)
  );
  if (kinds.size === 0) return null;
  if (kinds.size === 1) return Array.from(kinds)[0] as QueueKind;
  return 'mixed';
}

export function normalizeQueueItem(item: unknown): QueueItem | null {
  if (!item || typeof item !== 'object') return null;
  const raw = item as QueueItem;
  if (raw.resolvedSource) return sourceRefToQueueItem(raw.resolvedSource) || raw;
  if (raw.qobuzTrack) return { ...qobuzTrackToQueueItem(raw.qobuzTrack), ...raw };
  return {
    ...raw,
    title: text(raw.title, 'Untitled'),
    artist: text(raw.artist),
    album: text(raw.album),
    durationSecs: number(raw.durationSecs),
    imageUrl: raw.imageUrl || null,
    filename: raw.filename || raw.ref?.file_name || null,
    radio: Boolean(raw.radio),
    playlistContext: playlistContext(raw.playlistContext)
  };
}

export function normalizeQueueState(state?: Partial<QueueState> | null): QueueState {
  const items = Array.isArray(state?.items)
    ? state.items.map(normalizeQueueItem).filter((item): item is QueueItem => Boolean(item))
    : [];
  const rawLoopMode = (state as { loopMode?: unknown } | null | undefined)?.loopMode;
  return {
    kind: state?.kind || queueKindForItems(items),
    cursor:
      typeof state?.cursor === 'number'
        ? Math.min(Math.max(-1, state.cursor), items.length - 1)
        : -1,
    items,
    loopMode: rawLoopMode === 'one' || rawLoopMode === 'loop' ? 'loop' : 'off'
  };
}
