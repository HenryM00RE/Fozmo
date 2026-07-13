import {
  type Dispatch,
  type SetStateAction,
  useCallback,
  useEffect,
  useRef,
  useState
} from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  qobuzAlbumToLibraryShape,
  qobuzTrackFromAlbumTrack,
  queueItemArt,
  safeArray,
  sourceTrack,
  warmImage
} from '../../../shared/lib/appSupport';
import {
  localTrackToQueueItem,
  normalizeQueueItem,
  normalizeQueueState,
  qobuzDisplayName,
  qobuzTrackToQueueItem,
  queueItemToSourceRef,
  queueKindForItems,
  resolvedPlaySourceToQueueItem,
  sourceRefKey,
  sourceRefToQueueItem
} from '../../../shared/lib/queue';
import type {
  JsonRecord,
  LibraryTrack,
  QobuzTrack,
  QueueItem,
  QueueState,
  ResolvedPlaySource,
  SourceRef
} from '../../../shared/types';
import { setNowPlayingQueueActions, updateNowPlayingQueue } from '../model/nowPlayingQueueStore';
import {
  clearTransportPending,
  setPendingPlaybackArt,
  setPendingPlaybackIntent as setPendingPlaybackControlIntent,
  setPlaybackControlActions,
  setPlaybackLoading,
  setTransportPending
} from '../model/playbackControlStore';
import { refreshPlaybackStatus } from '../model/playbackStore';
import {
  findQueueIndexByFileName,
  findQueueIndexBySourceRef,
  nextQueueItemForPrefetch,
  qobuzQueueForPlayback,
  queueSnapshot,
  shuffleUpcomingQueue,
  sourceRefsForBackendQueue,
  sourceRefsForPlayback
} from '../model/queueModel';

const PLAYBACK_INTENT_TTL_MS = 12_000;
const PREVIOUS_RESTART_THRESHOLD_SECS = 3;
const PREVIOUS_RESTART_SKIP_WINDOW_MS = 1_500;

type PendingPlaybackIntent = {
  id: number;
  artist: string;
  fileName: string;
  requestedAt: number;
  title: string;
};

function queueItemPlaybackFileName(item: QueueItem | null | undefined) {
  if (!item) return '';
  if (item.filename) return String(item.filename);
  if (item.qobuzTrack) return qobuzDisplayName(item.qobuzTrack);
  if (item.ref?.file_name) return String(item.ref.file_name);
  return '';
}

function normalizedPlaybackText(value: unknown) {
  return String(value ?? '')
    .trim()
    .toLowerCase();
}

function statusMatchesPendingIntent(status: JsonRecord, pendingIntent: PendingPlaybackIntent) {
  const fileName = normalizedPlaybackText(status.file_name);
  const trackTitle = normalizedPlaybackText(status.track_title);
  const trackArtist = normalizedPlaybackText(status.track_artist);
  const pendingFileName = normalizedPlaybackText(pendingIntent.fileName);
  const pendingTitle = normalizedPlaybackText(pendingIntent.title);
  const pendingArtist = normalizedPlaybackText(pendingIntent.artist);

  if (pendingFileName && fileName === pendingFileName) return true;
  if (pendingTitle && (trackTitle === pendingTitle || fileName === pendingTitle)) {
    return !pendingArtist || !trackArtist || trackArtist === pendingArtist;
  }
  return false;
}

function statusCurrentSource(status: JsonRecord): SourceRef | null {
  const source = status.current_source;
  return source && typeof source === 'object' ? (source as SourceRef) : null;
}

function findQueueIndexByStatus(state: QueueState, status: JsonRecord) {
  const currentSourceKey = sourceRefKey(statusCurrentSource(status));
  if (currentSourceKey) {
    const sourceIndex = state.items.findIndex(
      (item) => sourceRefKey(queueItemToSourceRef(item)) === currentSourceKey
    );
    if (sourceIndex >= 0) return sourceIndex;
  }
  const fileNameIndex = findQueueIndexByFileName(state, String(status.file_name || ''));
  if (fileNameIndex >= 0) return fileNameIndex;
  return state.items.findIndex((item) => queueItemMatchesStatusMetadata(item, status));
}

function playbackPositionSecs(status: JsonRecord) {
  const position = Number(status.position_secs);
  return Number.isFinite(position) ? Math.max(0, position) : 0;
}

function playbackRestartKey(status: JsonRecord) {
  return (
    sourceRefKey(statusCurrentSource(status)) ||
    [status.file_name || '', status.track_title || '', status.track_artist || ''].join('|')
  );
}

function playbackAlbumKey(status: JsonRecord, queue: QueueState) {
  const source = statusCurrentSource(status);
  const queueIndex = findQueueIndexByStatus(queue, status);
  const queueItem = queueIndex >= 0 ? queue.items[queueIndex] : null;
  const albumId = source?.album_id ?? queueItem?.albumId ?? queueItem?.qobuzTrack?.album_id;
  const isQobuz = String(source?.kind || '').includes('qobuz') || Boolean(queueItem?.qobuzTrack);
  if (albumId !== null && albumId !== undefined && String(albumId)) {
    return `${isQobuz ? 'qobuz' : 'local'}:${String(albumId)}`;
  }

  const album = normalizedPlaybackText(source?.album || status.track_album || queueItem?.album);
  if (!album) return '';
  const artist = normalizedPlaybackText(
    source?.album_artist ||
      source?.artist ||
      status.track_artist ||
      queueItem?.albumArtist ||
      queueItem?.artist
  );
  return `${isQobuz ? 'qobuz' : 'local'}:${artist}:${album}`;
}

function queueItemMatchesStatusMetadata(item: QueueItem, status: JsonRecord) {
  const itemTitle = normalizedPlaybackText(item.title);
  const statusTitle = normalizedPlaybackText(status.track_title);
  if (!itemTitle || !statusTitle || itemTitle !== statusTitle) return false;

  const itemArtist = normalizedPlaybackText(item.artist);
  const statusArtist = normalizedPlaybackText(status.track_artist);
  if (itemArtist && statusArtist && itemArtist !== statusArtist) return false;

  const itemAlbum = normalizedPlaybackText(item.album);
  const statusAlbum = normalizedPlaybackText(status.track_album);
  return !itemAlbum || !statusAlbum || itemAlbum === statusAlbum;
}

type UsePlaybackQueueParams = {
  activeZoneId: string;
  refreshRecentlyPlayed: () => Promise<void>;
  setNotice: (message: string) => void;
  setSignalOpen: Dispatch<SetStateAction<boolean>>;
  status: JsonRecord;
  tracks: LibraryTrack[];
};

export function usePlaybackQueue({
  activeZoneId,
  refreshRecentlyPlayed,
  setNotice,
  setSignalOpen,
  status,
  tracks
}: UsePlaybackQueueParams) {
  const [queue, setQueue] = useState<QueueState>({
    kind: null,
    cursor: -1,
    items: [],
    loopMode: 'off'
  });
  const latestQueueRef = useRef<QueueState>({ kind: null, cursor: -1, items: [], loopMode: 'off' });
  const refreshQueueRef = useRef<() => Promise<void>>(async () => undefined);
  const qobuzPrefetchRef = useRef<{ key: string | null; pending: boolean }>({
    key: null,
    pending: false
  });
  const previousPlaybackRef = useRef<{ state: string; fileName: string }>({
    state: '',
    fileName: ''
  });
  const latestPlaybackRef = useRef<{ state: string; fileName: string; sourceKey: string }>({
    state: '',
    fileName: '',
    sourceKey: ''
  });
  const previousRestartRef = useRef<{ trackKey: string; requestedAt: number } | null>(null);
  const playbackIntentSeqRef = useRef(0);
  const pendingPlaybackIntentRef = useRef<PendingPlaybackIntent | null>(null);
  const albumPlaybackRequestRef = useRef(0);
  const unknownPlaybackRefreshRef = useRef('');
  const recentlyPlayedAlbumKeyRef = useRef('');
  const queueRefreshRequestRef = useRef<{ id: number; controller: AbortController | null }>({
    id: 0,
    controller: null
  });

  const clearPendingPlaybackIntent = useCallback((intentId?: number) => {
    const pendingIntent = pendingPlaybackIntentRef.current;
    if (!pendingIntent || (intentId && pendingIntent.id !== intentId)) return;
    pendingPlaybackIntentRef.current = null;
    setPendingPlaybackControlIntent(null);
    setPlaybackLoading(false);
    setPendingPlaybackArt(null);
  }, []);

  const setPendingPlaybackIntent = useCallback(
    (intent: PendingPlaybackIntent | null) => {
      pendingPlaybackIntentRef.current = intent;
      setPendingPlaybackControlIntent(
        intent
          ? {
              artist: intent.artist,
              fileName: intent.fileName,
              title: intent.title
            }
          : null
      );
      setPlaybackLoading(Boolean(intent));
      if (!intent) return;
      window.setTimeout(() => {
        clearPendingPlaybackIntent(intent.id);
      }, PLAYBACK_INTENT_TTL_MS);
    },
    [clearPendingPlaybackIntent]
  );

  const persistQueue = useCallback(
    (next: QueueState) => {
      endpoints.saveNowPlayingQueue(activeZoneId, next).catch(() => undefined);
    },
    [activeZoneId]
  );

  const setAndPersistQueue = useCallback(
    (next: QueueState) => {
      setQueue(next);
      persistQueue(next);
    },
    [persistQueue]
  );

  const commitBackendQueue = useCallback(
    async (next: QueueState) => {
      const normalized = normalizeQueueState(next);
      const currentSourceKey =
        normalized.cursor >= 0
          ? sourceRefKey(queueItemToSourceRef(normalized.items[normalized.cursor]))
          : '';
      const expectedCurrent = currentSourceKey || latestPlaybackRef.current.fileName || null;
      await endpoints.zoneQueue(activeZoneId, {
        queue: sourceRefsForBackendQueue(normalized),
        ...(expectedCurrent ? { expected_current: expectedCurrent } : {})
      });
      setAndPersistQueue(normalized);
    },
    [activeZoneId, setAndPersistQueue]
  );

  const markManualPlaybackChange = useCallback(() => {
    qobuzPrefetchRef.current = { key: null, pending: false };
  }, []);

  const playQueueIndex = useCallback(
    async (index: number, inputQueue = queue) => {
      if (index < 0 || index >= inputQueue.items.length) return;
      const previousQueue = latestQueueRef.current;
      markManualPlaybackChange();
      const nextQueue = {
        ...inputQueue,
        cursor: index,
        kind: inputQueue.kind || queueKindForItems(inputQueue.items)
      };
      const item = sourceTrack(nextQueue.items[index]);
      const expectedFileName = queueItemPlaybackFileName(item);
      const expectedSourceKey = sourceRefKey(queueItemToSourceRef(item));
      const expectedCurrent = expectedSourceKey || expectedFileName || null;
      const pendingArtSrc = queueItemArt(item);
      const intentId = playbackIntentSeqRef.current + 1;
      playbackIntentSeqRef.current = intentId;
      if (expectedFileName) {
        setPendingPlaybackIntent({
          artist: String(item.artist || ''),
          id: intentId,
          fileName: expectedFileName,
          requestedAt: Date.now(),
          title: String(item.title || '')
        });
      }
      setQueue(nextQueue);
      try {
        let playbackRequest: Promise<unknown> | null = null;
        if (item.qobuzTrack) {
          playbackRequest = endpoints.qobuzPlayZone(
            activeZoneId,
            item.qobuzTrack,
            qobuzQueueForPlayback(nextQueue, index)
          );
          warmImage(pendingArtSrc);
          setPendingPlaybackArt(pendingArtSrc);
          await playbackRequest;
          await endpoints.zoneQueue(activeZoneId, {
            queue: sourceRefsForPlayback(nextQueue, index),
            ...(expectedCurrent ? { expected_current: expectedCurrent } : {})
          });
        } else if (item.ref) {
          const playlistContext =
            item.playlistContext || item.resolvedSource?.playlist_context || null;
          playbackRequest = endpoints.playZone(activeZoneId, {
            ...item.ref,
            ...(playlistContext ? { playlist_context: playlistContext } : {}),
            queue: sourceRefsForPlayback(nextQueue, index)
          });
          warmImage(pendingArtSrc);
          setPendingPlaybackArt(pendingArtSrc);
          await playbackRequest;
        }
        persistQueue(nextQueue);
      } catch (error) {
        clearPendingPlaybackIntent(intentId);
        setQueue(previousQueue);
        refreshQueueRef.current().catch(() => undefined);
        setNotice(error instanceof Error ? error.message : 'Playback failed');
      }
    },
    [
      activeZoneId,
      clearPendingPlaybackIntent,
      markManualPlaybackChange,
      persistQueue,
      queue,
      setNotice,
      setPendingPlaybackIntent
    ]
  );

  const playItems = useCallback(
    (items: QueueItem[], startIndex = 0) => {
      const normalized = items.filter(Boolean);
      if (!normalized.length) return;
      const nextQueue = normalizeQueueState({
        kind: queueKindForItems(normalized),
        cursor: -1,
        items: normalized,
        loopMode: queue.loopMode
      });
      playQueueIndex(startIndex, nextQueue).catch(() => undefined);
    },
    [playQueueIndex, queue.loopMode]
  );

  const playAlbum = useCallback(
    async (albumId: string | number, startIndex = 0, shuffle = false, versionId?: number) => {
      const requestId = albumPlaybackRequestRef.current + 1;
      albumPlaybackRequestRef.current = requestId;
      try {
        const plan = await endpoints.albumPlaySources(albumId, startIndex, shuffle, versionId);
        if (albumPlaybackRequestRef.current !== requestId) return;
        const items = safeArray<ResolvedPlaySource>(plan.sources)
          .map(resolvedPlaySourceToQueueItem)
          .filter(Boolean) as QueueItem[];
        playItems(items, 0);
      } catch (error) {
        if (albumPlaybackRequestRef.current !== requestId) return;
        setNotice(error instanceof Error ? error.message : 'Could not prepare album');
      }
    },
    [playItems, setNotice]
  );

  const playTrack = useCallback(
    (track: LibraryTrack) => {
      const item = localTrackToQueueItem(track);
      const ordered = [
        item,
        ...tracks.filter((candidate) => candidate !== track).map(localTrackToQueueItem)
      ];
      playItems(ordered, 0);
    },
    [playItems, tracks]
  );

  const playSingleTrack = useCallback(
    (track: LibraryTrack) => {
      playItems([localTrackToQueueItem(track)], 0);
    },
    [playItems]
  );

  const playQobuzTrack = useCallback(
    (track: QobuzTrack, related: QobuzTrack[] = []) => {
      const items = [track, ...related.filter((candidate) => candidate !== track)].map(
        qobuzTrackToQueueItem
      );
      playItems(items, 0);
    },
    [playItems]
  );

  const playQobuzAlbum = useCallback(
    async (albumId: string | number) => {
      try {
        const detail = qobuzAlbumToLibraryShape(await endpoints.qobuzAlbum(albumId));
        const tracks = safeArray<LibraryTrack>(detail.tracks)
          .map(qobuzTrackFromAlbumTrack)
          .filter(Boolean)
          .map((track) => qobuzTrackToQueueItem(track as QobuzTrack));
        if (tracks.length) playItems(tracks, 0);
        else setNotice('No streamable tracks found for this album');
      } catch (error) {
        setNotice(error instanceof Error ? error.message : 'Could not play this Qobuz album');
      }
    },
    [playItems, setNotice]
  );

  const playQobuzPlaylist = useCallback(
    async (playlistId: string | number) => {
      try {
        const detail = await endpoints.qobuzPlaylist(playlistId);
        const tracks = safeArray<QobuzTrack>(detail.tracks)
          .map((track) => ({ ...track, playlist_context: null }))
          .map(qobuzTrackToQueueItem);
        if (tracks.length) playItems(tracks, 0);
        else setNotice('No streamable tracks found for this Qobuz playlist');
      } catch (error) {
        setNotice(error instanceof Error ? error.message : 'Could not play this Qobuz playlist');
      }
    },
    [playItems, setNotice]
  );

  const playArtistRadio = useCallback(
    async (artistName: string) => {
      const artist = artistName.trim();
      if (!artist) return;
      markManualPlaybackChange();
      setPlaybackLoading(true);
      try {
        await endpoints.playArtistRadioZone(activeZoneId, {
          artist_name: artist,
          mode: 'auto'
        });
        await refreshQueueRef.current();
      } catch (error) {
        setNotice(error instanceof Error ? error.message : 'Could not start Artist Radio');
      } finally {
        setPlaybackLoading(false);
      }
    },
    [activeZoneId, markManualPlaybackChange, setNotice]
  );

  const addItemsToQueue = useCallback(
    (items: QueueItem[], placement: 'next' | 'end') => {
      const normalized = items.map(normalizeQueueItem).filter(Boolean) as QueueItem[];
      if (!normalized.length) return;
      const next = normalizeQueueState({
        ...queue,
        kind: queueKindForItems([...queue.items, ...normalized]),
        items: [
          ...queue.items.slice(
            0,
            placement === 'next' && queue.cursor >= 0 ? queue.cursor + 1 : queue.items.length
          ),
          ...normalized,
          ...queue.items.slice(
            placement === 'next' && queue.cursor >= 0 ? queue.cursor + 1 : queue.items.length
          )
        ]
      });
      commitBackendQueue(next).catch((error) => {
        setNotice(error instanceof Error ? error.message : 'Could not update queue');
      });
    },
    [commitBackendQueue, queue, setNotice]
  );

  const refreshQueue = useCallback(async () => {
    const requestId = queueRefreshRequestRef.current.id + 1;
    queueRefreshRequestRef.current.controller?.abort();
    const controller = new AbortController();
    queueRefreshRequestRef.current = { id: requestId, controller };
    try {
      const response = await endpoints.nowPlayingQueue(activeZoneId, controller.signal);
      if (queueRefreshRequestRef.current.id !== requestId) return;
      const saved = normalizeQueueState(response.state || null);
      const currentSourceKey = sourceRefKey(response.current_source);
      const queuedSources = safeArray<SourceRef>(response.queued_sources).filter(
        (source, index) =>
          index !== 0 || !currentSourceKey || sourceRefKey(source) !== currentSourceKey
      );
      const currentQueueIndex = currentSourceKey
        ? queuedSources.findIndex((source) => sourceRefKey(source) === currentSourceKey)
        : -1;
      const futureSources =
        currentQueueIndex >= 0 ? queuedSources.slice(currentQueueIndex + 1) : queuedSources;
      const sources = [
        ...(response.current_source ? [response.current_source] : []),
        ...futureSources
      ];
      const items = sources.map(sourceRefToQueueItem).filter(Boolean) as QueueItem[];
      const futureItems = futureSources.map(sourceRefToQueueItem).filter(Boolean) as QueueItem[];
      if (response.current_source) {
        const liveCurrent = sourceRefToQueueItem(response.current_source);
        const liveKey = liveCurrent ? queueItemPlaybackFileName(liveCurrent) : null;
        const sourceIndex = findQueueIndexBySourceRef(saved, response.current_source);
        const savedIndex =
          sourceIndex >= 0 ? sourceIndex : liveKey ? findQueueIndexByFileName(saved, liveKey) : -1;
        if (saved.items.length && savedIndex >= 0) {
          const mergedItems = saved.items.slice(0, savedIndex + 1).concat(futureItems);
          setQueue(
            normalizeQueueState({
              ...saved,
              kind: queueKindForItems(mergedItems),
              cursor: savedIndex,
              items: mergedItems
            })
          );
          return;
        }
        if (items.length) {
          setQueue(
            normalizeQueueState({
              kind: queueKindForItems(items),
              cursor: items.length ? 0 : -1,
              items,
              loopMode: saved.loopMode
            })
          );
          return;
        }
      }
      if (saved.items.length) {
        setQueue(saved);
        return;
      }
      if (items.length)
        setQueue(
          normalizeQueueState({
            kind: queueKindForItems(items),
            cursor: response.current_source ? 0 : -1,
            items,
            loopMode: saved.loopMode
          })
        );
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return;
      setQueue((current) => current);
    } finally {
      if (queueRefreshRequestRef.current.id === requestId) {
        queueRefreshRequestRef.current.controller = null;
      }
    }
  }, [activeZoneId]);

  refreshQueueRef.current = refreshQueue;

  const clearQueue = useCallback(() => {
    const items = queue.cursor >= 0 ? queue.items.slice(0, queue.cursor + 1) : [];
    commitBackendQueue(normalizeQueueState({ ...queue, items })).catch((error) => {
      setNotice(error instanceof Error ? error.message : 'Could not update queue');
    });
  }, [commitBackendQueue, queue, setNotice]);

  const shuffleQueue = useCallback(() => {
    commitBackendQueue(shuffleUpcomingQueue(queue)).catch((error) => {
      setNotice(error instanceof Error ? error.message : 'Could not update queue');
    });
  }, [commitBackendQueue, queue, setNotice]);

  const toggleLoop = useCallback(() => {
    const mode = queue.loopMode === 'off' ? 'loop' : 'off';
    const next: QueueState = { ...queue, loopMode: mode };
    endpoints
      .loopMode(activeZoneId, mode)
      .then(() => setAndPersistQueue(next))
      .catch((error) => {
        setNotice(error instanceof Error ? error.message : 'Could not update repeat');
      });
  }, [activeZoneId, queue, setAndPersistQueue, setNotice]);

  useEffect(() => {
    refreshQueue().catch(() => undefined);
    return () => {
      queueRefreshRequestRef.current.id += 1;
      queueRefreshRequestRef.current.controller?.abort();
      queueRefreshRequestRef.current.controller = null;
    };
  }, [refreshQueue]);

  useEffect(() => {
    const refreshWhenActive = () => {
      if (document.visibilityState !== 'hidden') {
        refreshQueueRef.current().catch(() => undefined);
      }
    };

    document.addEventListener('visibilitychange', refreshWhenActive);
    window.addEventListener('focus', refreshWhenActive);
    return () => {
      document.removeEventListener('visibilitychange', refreshWhenActive);
      window.removeEventListener('focus', refreshWhenActive);
    };
  }, []);

  useEffect(() => {
    latestPlaybackRef.current = {
      state: String(status.state || 'Stopped'),
      fileName: String(status.file_name || ''),
      sourceKey: sourceRefKey(statusCurrentSource(status))
    };
    if (status.state === 'Playing') {
      clearTransportPending('play');
      clearTransportPending('next');
      clearTransportPending('previous');
      if (status.transport_pending !== 'seeking') clearTransportPending('seek');
      clearTransportPending('auto-advance');
    } else if (status.state === 'Paused' || status.state === 'Stopped') {
      clearTransportPending();
    }
  }, [status.current_source, status.file_name, status.state, status.transport_pending]);

  useEffect(() => {
    latestQueueRef.current = queue;
  }, [queue]);

  useEffect(() => {
    if (status.state !== 'Playing' && status.state !== 'Paused') return;
    const albumKey = playbackAlbumKey(status, latestQueueRef.current);
    if (!albumKey || recentlyPlayedAlbumKeyRef.current === albumKey) return;
    recentlyPlayedAlbumKeyRef.current = albumKey;
    refreshRecentlyPlayed().catch(() => undefined);
  }, [
    refreshRecentlyPlayed,
    status.current_source,
    status.file_name,
    status.state,
    status.track_album,
    status.track_artist,
    status.track_title
  ]);

  useEffect(() => {
    const hasUpcoming = queue.cursor >= 0 && queue.cursor < queue.items.length - 1;
    const backendLoading =
      status.state === 'Starting' ||
      status.state === 'Transitioning' ||
      (status.transport_pending &&
        status.transport_pending !== 'none' &&
        status.transport_pending !== 'seeking');
    if (hasUpcoming && backendLoading) {
      setTransportPending({ kind: 'auto-advance', requestedAt: Date.now() });
    } else {
      clearTransportPending('auto-advance');
    }
  }, [queue.cursor, queue.items.length, status.state, status.transport_pending]);

  useEffect(() => {
    const fileName = String(status.file_name || '');
    if (status.state !== 'Playing') return;
    if (!fileName && !statusCurrentSource(status)) return;
    const pendingIntent = pendingPlaybackIntentRef.current;
    if (pendingIntent) {
      if (statusMatchesPendingIntent(status, pendingIntent)) {
        clearPendingPlaybackIntent(pendingIntent.id);
      } else if (Date.now() - pendingIntent.requestedAt < PLAYBACK_INTENT_TTL_MS) {
        return;
      } else {
        clearPendingPlaybackIntent(pendingIntent.id);
      }
    }
    const knownIndex = findQueueIndexByStatus(latestQueueRef.current, status);
    if (knownIndex < 0) {
      const refreshKey = `${activeZoneId}:${sourceRefKey(statusCurrentSource(status)) || fileName}`;
      if (unknownPlaybackRefreshRef.current !== refreshKey) {
        unknownPlaybackRefreshRef.current = refreshKey;
        refreshQueueRef.current().catch(() => undefined);
      }
      return;
    }
    unknownPlaybackRefreshRef.current = '';
    setQueue((current) => {
      const index = findQueueIndexByStatus(current, status);
      return index >= 0 && index !== current.cursor ? { ...current, cursor: index } : current;
    });
  }, [
    activeZoneId,
    clearPendingPlaybackIntent,
    queue.items,
    status.current_source,
    status.file_name,
    status.state,
    status.track_artist,
    status.track_title
  ]);

  useEffect(() => {
    const stateName = String(status.state || 'Stopped');
    const fileName = String(status.file_name || '');
    const previous = previousPlaybackRef.current;
    const pendingIntent = pendingPlaybackIntentRef.current;

    if (stateName === 'Playing') {
      if (pendingIntent && statusMatchesPendingIntent(status, pendingIntent)) {
        clearPendingPlaybackIntent(pendingIntent.id);
      }
    } else if (pendingIntent && Date.now() - pendingIntent.requestedAt >= PLAYBACK_INTENT_TTL_MS) {
      clearPendingPlaybackIntent(pendingIntent.id);
    }

    previousPlaybackRef.current = { state: stateName, fileName: fileName || previous.fileName };
    if (stateName === 'Playing' && fileName) {
      previousPlaybackRef.current.fileName = fileName;
    } else if (stateName !== 'Stopped') {
      previousPlaybackRef.current.fileName = fileName;
    }
  }, [
    clearPendingPlaybackIntent,
    status.file_name,
    status.state,
    status.track_artist,
    status.track_title
  ]);

  useEffect(() => {
    if (status.state !== 'Playing' && status.state !== 'Paused') return;
    const fileName = String(status.file_name || '');
    if (!fileName) return;
    const currentIndex = findQueueIndexByFileName(queue, fileName);
    if (currentIndex < 0) return;
    const currentItem = queue.items[currentIndex];
    const nextItem = nextQueueItemForPrefetch(queue, currentIndex);
    if (!currentItem?.qobuzTrack || !nextItem?.qobuzTrack) return;
    const nextTrackId = Number(nextItem.qobuzTrack.id ?? nextItem.qobuzTrack.track_id);
    if (!Number.isFinite(nextTrackId) || nextTrackId <= 0) return;
    const expectedCurrent = sourceRefKey(statusCurrentSource(status)) || fileName;
    const key = `${activeZoneId}:${expectedCurrent}:${nextTrackId}`;
    if (qobuzPrefetchRef.current.key === key || qobuzPrefetchRef.current.pending) return;

    qobuzPrefetchRef.current = { key, pending: true };
    endpoints
      .qobuzPrefetchZone(activeZoneId, nextItem.qobuzTrack, expectedCurrent)
      .catch(() => {
        if (qobuzPrefetchRef.current.key === key) {
          qobuzPrefetchRef.current.key = null;
        }
      })
      .finally(() => {
        if (qobuzPrefetchRef.current.key === key) {
          qobuzPrefetchRef.current.pending = false;
        }
      });
  }, [activeZoneId, queue, status.current_source, status.file_name, status.state]);

  useEffect(() => {
    updateNowPlayingQueue({
      kind: queue.kind,
      cursor: queue.cursor,
      loopMode: queue.loopMode,
      upcomingCount: Math.max(0, queue.items.length - Math.max(0, queue.cursor + 1)),
      items: queueSnapshot(queue),
      structuralKey: queue.items
        .map((item, index) => `${item.title}:${item.filename}:${index}`)
        .join('|'),
      preserveQueueScroll: false
    });
  }, [queue]);

  useEffect(() => {
    setNowPlayingQueueActions({
      jumpToIndex: (index) => {
        playQueueIndex(index).catch(() => undefined);
      },
      removeIndex: (index) => {
        if (queue.cursor >= 0 && index <= queue.cursor) return;
        const next = normalizeQueueState({
          ...queue,
          cursor: index < queue.cursor ? queue.cursor - 1 : queue.cursor,
          items: queue.items.filter((_, itemIndex) => itemIndex !== index)
        });
        commitBackendQueue(next).catch((error) => {
          setNotice(error instanceof Error ? error.message : 'Could not update queue');
        });
      },
      reorderQueue: (from, to) => {
        if (from < 0 || from >= queue.items.length) return;
        if (queue.cursor >= 0 && (from <= queue.cursor || to <= queue.cursor)) return;
        const items = queue.items.slice();
        const [moved] = items.splice(from, 1);
        const target = from < to ? to - 1 : to;
        items.splice(Math.max(0, Math.min(items.length, target)), 0, moved);
        let cursor = queue.cursor;
        if (from === queue.cursor) cursor = Math.max(0, Math.min(items.length - 1, target));
        else if (from < queue.cursor && target >= queue.cursor) cursor -= 1;
        else if (from > queue.cursor && target <= queue.cursor) cursor += 1;
        commitBackendQueue(normalizeQueueState({ ...queue, cursor, items })).catch((error) => {
          setNotice(error instanceof Error ? error.message : 'Could not update queue');
        });
      },
      requestSnapshot: () => undefined
    });
  }, [commitBackendQueue, playQueueIndex, queue, setNotice]);

  useEffect(() => {
    setPlaybackControlActions({
      previous: () => {
        const trackKey = playbackRestartKey(status);
        const recentRestart = previousRestartRef.current;
        const shouldSkipPrevious =
          recentRestart &&
          recentRestart.trackKey === trackKey &&
          Date.now() - recentRestart.requestedAt < PREVIOUS_RESTART_SKIP_WINDOW_MS;

        if (!shouldSkipPrevious && playbackPositionSecs(status) > PREVIOUS_RESTART_THRESHOLD_SECS) {
          previousRestartRef.current = { trackKey, requestedAt: Date.now() };
          setTransportPending({
            kind: 'previous',
            requestedAt: Date.now(),
            expectedPosition: 0,
            expectedTrackKey: trackKey
          });
          return endpoints
            .seekZone(activeZoneId, 0)
            .then(() => refreshPlaybackStatus({ force: true }))
            .catch((error) => {
              clearTransportPending('previous');
              setNotice(error instanceof Error ? error.message : 'Could not restart track');
            });
        }

        previousRestartRef.current = null;
        if (queue.cursor > 0) {
          setTransportPending({ kind: 'previous', requestedAt: Date.now() });
          return playQueueIndex(queue.cursor - 1).catch((error) => {
            clearTransportPending('previous');
            setNotice(error instanceof Error ? error.message : 'Could not skip back');
          });
        }
      },
      next: () => {
        setTransportPending({ kind: 'next', requestedAt: Date.now() });
        if (queue.cursor >= 0 && queue.cursor < queue.items.length - 1) {
          return playQueueIndex(queue.cursor + 1).catch((error) => {
            clearTransportPending('next');
            setNotice(error instanceof Error ? error.message : 'Could not skip track');
          });
        }
        markManualPlaybackChange();
        return endpoints
          .nextZone(activeZoneId)
          .then(() => refreshPlaybackStatus({ force: true }))
          .catch((error) => {
            clearTransportPending('next');
            setNotice(error instanceof Error ? error.message : 'Could not skip track');
          });
      },
      playPause: () => {
        if (status.state === 'Playing') {
          return endpoints
            .pauseZone(activeZoneId)
            .then(() => refreshPlaybackStatus({ force: true }))
            .catch((error) => {
              setNotice(error instanceof Error ? error.message : 'Could not pause playback');
            });
        } else if (status.state === 'Paused') {
          setTransportPending({ kind: 'play', requestedAt: Date.now() });
          return endpoints
            .resumeZone(activeZoneId)
            .then(() => refreshPlaybackStatus({ force: true }))
            .catch((error) => {
              clearTransportPending('play');
              setNotice(error instanceof Error ? error.message : 'Could not resume playback');
            });
        } else if (queue.items.length) {
          setTransportPending({ kind: 'play', requestedAt: Date.now() });
          return playQueueIndex(Math.max(0, queue.cursor)).catch((error) => {
            clearTransportPending('play');
            setNotice(error instanceof Error ? error.message : 'Could not start playback');
          });
        } else if (tracks.length) playTrack(tracks[0]);
      },
      seek: (seconds) => {
        setTransportPending({
          kind: 'seek',
          requestedAt: Date.now(),
          expectedPosition: seconds,
          expectedTrackKey: playbackRestartKey(status)
        });
        endpoints
          .seekZone(activeZoneId, seconds)
          .then(() => refreshPlaybackStatus({ force: true }))
          .catch((error) => {
            clearTransportPending('seek');
            setNotice(error instanceof Error ? error.message : 'Could not seek');
          });
      },
      shuffle: shuffleQueue,
      stop: () => {
        markManualPlaybackChange();
        return endpoints
          .stopZone(activeZoneId)
          .then(() => refreshPlaybackStatus({ force: true }))
          .catch((error) => {
            setNotice(error instanceof Error ? error.message : 'Could not stop playback');
          });
      },
      toggleLoop,
      toggleSignalPath: () => setSignalOpen((open) => !open)
    });
  }, [
    activeZoneId,
    markManualPlaybackChange,
    playQueueIndex,
    playTrack,
    queue,
    setNotice,
    setSignalOpen,
    shuffleQueue,
    status,
    toggleLoop,
    tracks
  ]);

  return {
    addItemsToQueue,
    clearQueue,
    playArtistRadio,
    playAlbum,
    playItems,
    playQobuzAlbum,
    playQobuzPlaylist,
    playQobuzTrack,
    playSingleTrack,
    playTrack,
    queue,
    shuffleQueue,
    toggleLoop
  };
}
