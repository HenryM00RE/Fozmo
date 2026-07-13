import { useCallback, useMemo, useState } from 'react';
import type { QueueItem } from '../../../shared/types';
import type { AlbumSelectionItem } from '../model/albumModel';

type QueuePlacement = 'next' | 'end';

type UseAlbumSelectionParams = {
  addItemsToQueue: (items: QueueItem[], placement: QueuePlacement) => void;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  playItems: (items: QueueItem[], startIndex?: number) => void;
};

export function useAlbumSelection({
  addItemsToQueue,
  openPlaylistPickerForItems,
  playItems
}: UseAlbumSelectionParams) {
  const [selectionItems, setSelectionItems] = useState<AlbumSelectionItem[]>([]);
  const [selectionKeys, setSelectionKeys] = useState<Set<string>>(() => new Set());
  const [selectionBusy, setSelectionBusy] = useState(false);
  const [selectionMenuOpen, setSelectionMenuOpen] = useState(false);

  const selectedTrackItems = useMemo(
    () => selectionItems.filter((entry) => selectionKeys.has(entry.key)).map((entry) => entry.item),
    [selectionItems, selectionKeys]
  );

  const selectionActive = selectionKeys.size > 0;

  const clearSelection = useCallback(() => {
    setSelectionKeys(new Set());
    setSelectionBusy(false);
    setSelectionMenuOpen(false);
  }, []);

  const registerSelectionItems = useCallback((items: AlbumSelectionItem[]) => {
    setSelectionItems((current) =>
      current.length === items.length &&
      current.every((entry, index) => entry.key === items[index]?.key)
        ? current
        : items
    );
    setSelectionKeys((current) => {
      if (!current.size) return current;
      const liveKeys = new Set(items.map((item) => item.key));
      const next = new Set(Array.from(current).filter((key) => liveKeys.has(key)));
      return next.size === current.size ? current : next;
    });
  }, []);

  const toggleSelection = useCallback((key: string) => {
    if (!key) return;
    setSelectionKeys((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const playSelection = useCallback(() => {
    if (selectionBusy) return;
    if (!selectedTrackItems.length) {
      clearSelection();
      return;
    }
    playItems(selectedTrackItems, 0);
    clearSelection();
  }, [clearSelection, playItems, selectedTrackItems, selectionBusy]);

  const queueSelection = useCallback(
    (placement: QueuePlacement) => {
      if (selectionBusy) return;
      if (!selectedTrackItems.length) {
        clearSelection();
        return;
      }
      addItemsToQueue(selectedTrackItems, placement);
      clearSelection();
    },
    [addItemsToQueue, clearSelection, selectedTrackItems, selectionBusy]
  );

  const addSelectionToPlaylist = useCallback(() => {
    if (selectionBusy) return;
    if (!selectedTrackItems.length) {
      clearSelection();
      return;
    }
    openPlaylistPickerForItems(
      selectedTrackItems,
      `${selectedTrackItems.length} selected track${selectedTrackItems.length === 1 ? '' : 's'}`,
      clearSelection
    );
    setSelectionMenuOpen(false);
  }, [clearSelection, openPlaylistPickerForItems, selectedTrackItems, selectionBusy]);

  return {
    addSelectionToPlaylist,
    clearSelection,
    playSelection,
    queueSelection,
    registerSelectionItems,
    selectedTrackItems,
    selectionActive,
    selectionBusy,
    selectionKeys,
    selectionMenuOpen,
    setSelectionMenuOpen,
    toggleSelection
  };
}
