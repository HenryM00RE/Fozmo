import { useCallback, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  mergeRecentlyPlayed,
  normalizeQobuzAlbumId,
  qobuzAlbumToLibraryShape,
  qobuzTrackFromAlbumTrack,
  recentlyPlayedSelectionKey,
  resolveLocalAlbumId,
  safeArray
} from '../../../shared/lib/appSupport';
import {
  normalizeQueueItem,
  qobuzTrackToQueueItem,
  resolvedPlaySourceToQueueItem
} from '../../../shared/lib/queue';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  Playlist,
  QobuzTrack,
  QueueItem,
  ResolvedPlaySource,
  RouteState
} from '../../../shared/types';

type QueuePlacement = 'next' | 'end';

type UseRecentlyPlayedSelectionParams = {
  albums: LibraryAlbum[];
  playlists: Playlist[];
  recentAlbums: JsonRecord[];
  recentPlaylists: JsonRecord[];
  navigate: (next: RouteState) => void;
  playAlbum: (albumId: string | number, startIndex?: number, shuffle?: boolean) => Promise<void>;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  addItemsToQueue: (items: QueueItem[], placement: QueuePlacement) => void;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  setNotice: (message: string) => void;
  onSelectionStart: () => void;
};

export function useRecentlyPlayedSelection({
  albums,
  playlists,
  recentAlbums,
  recentPlaylists,
  navigate,
  playAlbum,
  playItems,
  addItemsToQueue,
  openPlaylistPickerForItems,
  setNotice,
  onSelectionStart
}: UseRecentlyPlayedSelectionParams) {
  const [selectionKeys, setSelectionKeys] = useState<Set<string>>(() => new Set());
  const [selectedItemsByKey, setSelectedItemsByKey] = useState<Record<string, JsonRecord>>({});
  const [selectionBusy, setSelectionBusy] = useState(false);
  const [selectionMenuOpen, setSelectionMenuOpen] = useState(false);

  const recentlyPlayedItems = useMemo(
    () => mergeRecentlyPlayed(recentAlbums, recentPlaylists, playlists),
    [playlists, recentAlbums, recentPlaylists]
  );

  const selectedItems = useMemo(() => {
    const recentByKey = new Map(
      recentlyPlayedItems.map((item) => [recentlyPlayedSelectionKey(item), item])
    );
    return Array.from(selectionKeys)
      .map((key) => selectedItemsByKey[key] || recentByKey.get(key))
      .filter(Boolean) as JsonRecord[];
  }, [recentlyPlayedItems, selectedItemsByKey, selectionKeys]);

  const selectionActive = selectionKeys.size > 0;

  const clearSelection = useCallback(() => {
    setSelectionKeys(new Set());
    setSelectedItemsByKey({});
    setSelectionBusy(false);
    setSelectionMenuOpen(false);
  }, []);

  const toggleSelection = useCallback(
    (item: JsonRecord) => {
      const key = recentlyPlayedSelectionKey(item);
      if (!key) return;
      onSelectionStart();
      setSelectionKeys((current) => {
        const next = new Set(current);
        if (next.has(key)) {
          next.delete(key);
          setSelectedItemsByKey((items) => {
            const { [key]: _removed, ...rest } = items;
            return rest;
          });
        } else {
          next.add(key);
          setSelectedItemsByKey((items) => ({ ...items, [key]: item }));
        }
        return next;
      });
    },
    [onSelectionStart]
  );

  const resolveItemQueueItems = useCallback(
    async (item: JsonRecord) => {
      if (item.recent_type === 'playlist') {
        const playlist = playlists.find((candidate) => candidate.id === item.playlist_id) || item;
        return safeArray<QueueItem>(playlist.items)
          .map(normalizeQueueItem)
          .filter(Boolean) as QueueItem[];
      }

      if (item.is_qobuz) {
        let albumId = normalizeQobuzAlbumId(item);
        if (albumId.startsWith('qobuz:track:')) {
          const trackId = albumId.replace('qobuz:track:', '');
          const track = await endpoints.qobuzTrack(trackId);
          albumId = String(track.album_id || '');
        }
        if (!albumId) return [];
        const qobuzDetail = await endpoints.qobuzAlbum(albumId);
        const detail = qobuzAlbumToLibraryShape(qobuzDetail);
        return safeArray<LibraryTrack>(detail.tracks)
          .map(qobuzTrackFromAlbumTrack)
          .filter(Boolean)
          .map((track) => qobuzTrackToQueueItem(track as QobuzTrack));
      }

      const albumId = resolveLocalAlbumId(item, albums);
      if (albumId === null || albumId === undefined || albumId === '') return [];
      const plan = await endpoints.albumPlaySources(albumId);
      return safeArray<ResolvedPlaySource>(plan.sources)
        .map(resolvedPlaySourceToQueueItem)
        .filter(Boolean) as QueueItem[];
    },
    [albums, playlists]
  );

  const resolveSelectionQueueItems = useCallback(async () => {
    const items: QueueItem[] = [];
    for (const item of selectedItems) {
      const resolved = await resolveItemQueueItems(item);
      items.push(...resolved);
    }
    return items;
  }, [resolveItemQueueItems, selectedItems]);

  const playSelection = useCallback(async () => {
    if (selectionBusy) return;
    if (!selectedItems.length) {
      clearSelection();
      return;
    }
    setSelectionBusy(true);
    try {
      const items = await resolveSelectionQueueItems();
      if (!items.length) {
        setNotice('No playable tracks found for the selection');
        return;
      }
      playItems(items, 0);
      clearSelection();
    } catch (error) {
      setNotice(error instanceof Error ? error.message : 'Could not play the selected items');
    } finally {
      setSelectionBusy(false);
    }
  }, [
    clearSelection,
    playItems,
    resolveSelectionQueueItems,
    selectedItems.length,
    selectionBusy,
    setNotice
  ]);

  const queueSelection = useCallback(
    async (placement: QueuePlacement) => {
      if (selectionBusy) return;
      if (!selectedItems.length) {
        clearSelection();
        return;
      }
      setSelectionBusy(true);
      try {
        const items = await resolveSelectionQueueItems();
        if (!items.length) {
          setNotice('No playable tracks found for the selection');
          return;
        }
        addItemsToQueue(items, placement);
        clearSelection();
      } catch (error) {
        setNotice(error instanceof Error ? error.message : 'Could not add the selected items');
      } finally {
        setSelectionBusy(false);
      }
    },
    [
      addItemsToQueue,
      clearSelection,
      resolveSelectionQueueItems,
      selectedItems.length,
      selectionBusy,
      setNotice
    ]
  );

  const addSelectionToPlaylist = useCallback(async () => {
    if (selectionBusy) return;
    if (!selectedItems.length) {
      clearSelection();
      return;
    }
    setSelectionBusy(true);
    try {
      const items = await resolveSelectionQueueItems();
      if (!items.length) {
        setNotice('No playable tracks found for the selection');
        return;
      }
      const title =
        selectedItems.length === 1
          ? String(selectedItems[0].title || 'Selected album')
          : `${selectedItems.length} selected items`;
      openPlaylistPickerForItems(items, title, clearSelection);
      setSelectionMenuOpen(false);
    } catch (error) {
      setNotice(
        error instanceof Error ? error.message : 'Could not add the selected items to a playlist'
      );
    } finally {
      setSelectionBusy(false);
    }
  }, [
    clearSelection,
    openPlaylistPickerForItems,
    resolveSelectionQueueItems,
    selectedItems,
    selectionBusy,
    setNotice
  ]);

  const openItem = useCallback(
    async (item: JsonRecord) => {
      if (item.recent_type === 'playlist') {
        const playlistId = String(item.playlist_id || '');
        if (playlistId) navigate({ view: 'playlist', id: playlistId });
        return;
      }
      if (item.is_qobuz) {
        let albumId = normalizeQobuzAlbumId(item);
        if (albumId.startsWith('qobuz:track:')) {
          const track = await endpoints.qobuzTrack(albumId.replace('qobuz:track:', ''));
          albumId = String(track.album_id || '');
        }
        if (albumId) navigate({ view: 'qobuz-album', id: albumId });
        else setNotice('Could not resolve the Qobuz album for this play');
        return;
      }
      const albumId = resolveLocalAlbumId(item, albums);
      if (albumId === null || albumId === undefined || albumId === '') {
        setNotice('Could not resolve this recently played album');
        return;
      }
      navigate({ view: 'album', id: albumId });
    },
    [albums, navigate, setNotice]
  );

  const playItem = useCallback(
    async (item: JsonRecord) => {
      if (item.recent_type === 'playlist') {
        const playlist = playlists.find((candidate) => candidate.id === item.playlist_id) || item;
        playItems(
          safeArray<QueueItem>(playlist.items)
            .map(normalizeQueueItem)
            .filter(Boolean) as QueueItem[],
          0
        );
        return;
      }
      if (item.is_qobuz) {
        const items = await resolveItemQueueItems(item);
        playItems(items, 0);
        return;
      }
      const albumId = resolveLocalAlbumId(item, albums);
      if (albumId !== null && albumId !== undefined && albumId !== '') playAlbum(albumId);
    },
    [albums, playAlbum, playItems, playlists, resolveItemQueueItems]
  );

  return {
    addSelectionToPlaylist,
    clearSelection,
    openItem,
    playItem,
    playSelection,
    queueSelection,
    recentlyPlayedItems,
    selectionActive,
    selectionBusy,
    selectionKeys,
    selectionMenuOpen,
    setSelectionMenuOpen,
    toggleSelection
  };
}
