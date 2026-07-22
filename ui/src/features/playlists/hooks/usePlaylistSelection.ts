import { useCallback, useEffect, useMemo, useState } from 'react';
import type { LibraryTrack, Playlist, QueueItem } from '../../../shared/types';
import { queueItemsForPlayback } from '../model/playlistModel';

type QueuePlacement = 'next' | 'end';

type UsePlaylistSelectionParams = {
  addItemsToQueue: (items: QueueItem[], placement: QueuePlacement) => void;
  playlists: Playlist[];
  playItems: (items: QueueItem[], startIndex?: number) => void;
  tracks: LibraryTrack[];
};

export function usePlaylistSelection({
  addItemsToQueue,
  playlists,
  playItems,
  tracks
}: UsePlaylistSelectionParams) {
  const [selectionKeys, setSelectionKeys] = useState<Set<string>>(() => new Set());
  const [selectionMenuOpen, setSelectionMenuOpen] = useState(false);

  const selectedItems = useMemo(
    () =>
      playlists
        .filter((playlist) => selectionKeys.has(playlist.id))
        .flatMap((playlist) => queueItemsForPlayback(playlist, false, tracks)),
    [playlists, selectionKeys, tracks]
  );

  const selectionActive = selectionKeys.size > 0;

  const clearSelection = useCallback(() => {
    setSelectionKeys(new Set());
    setSelectionMenuOpen(false);
  }, []);

  useEffect(() => {
    setSelectionKeys((current) => {
      if (!current.size) return current;
      const liveKeys = new Set(playlists.map((playlist) => playlist.id));
      const next = new Set(Array.from(current).filter((key) => liveKeys.has(key)));
      return next.size === current.size ? current : next;
    });
  }, [playlists]);

  const toggleSelection = useCallback((playlistId: string) => {
    if (!playlistId) return;
    setSelectionKeys((current) => {
      const next = new Set(current);
      if (next.has(playlistId)) next.delete(playlistId);
      else next.add(playlistId);
      return next;
    });
  }, []);

  const playSelection = useCallback(() => {
    if (selectedItems.length) playItems(selectedItems, 0);
    clearSelection();
  }, [clearSelection, playItems, selectedItems]);

  const queueSelection = useCallback(
    (placement: QueuePlacement) => {
      if (selectedItems.length) addItemsToQueue(selectedItems, placement);
      clearSelection();
    },
    [addItemsToQueue, clearSelection, selectedItems]
  );

  return {
    clearSelection,
    playSelection,
    queueSelection,
    selectionActive,
    selectionKeys,
    selectionMenuOpen,
    setSelectionMenuOpen,
    toggleSelection
  };
}
