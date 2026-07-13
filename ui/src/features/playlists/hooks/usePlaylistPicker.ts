import { type Dispatch, type SetStateAction, useCallback, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { nextPlaylistName, safeArray } from '../../../shared/lib/appSupport';
import { normalizeQueueItem } from '../../../shared/lib/queue';
import type { Playlist, QueueItem } from '../../../shared/types';
import type { PlaylistPickerState } from '../components/PlaylistPicker';
import { createPlaylistId, playlistCreatedAt } from '../model/playlistModel';

type UsePlaylistPickerParams = {
  playlists: Playlist[];
  setPlaylists: Dispatch<SetStateAction<Playlist[]>>;
  setNotice: (message: string) => void;
};

export function usePlaylistPicker({ playlists, setPlaylists, setNotice }: UsePlaylistPickerParams) {
  const [playlistPicker, setPlaylistPicker] = useState<PlaylistPickerState>(null);

  const openPlaylistPickerForItems = useCallback(
    (items: QueueItem[], title = '', onAdded?: () => void) => {
      const normalized = items.map(normalizeQueueItem).filter(Boolean) as QueueItem[];
      if (!normalized.length) {
        setNotice('Could not add those tracks to a playlist');
        return;
      }
      setPlaylistPicker({
        items: normalized,
        title:
          title ||
          (normalized.length === 1 ? normalized[0].title || 'Track' : `${normalized.length} songs`),
        onAdded
      });
    },
    [setNotice]
  );

  const closePlaylistPicker = useCallback(() => {
    setPlaylistPicker(null);
  }, []);

  const saveItemsToPlaylist = useCallback(
    async (playlist: Playlist, items: QueueItem[]) => {
      const normalized = items.map(normalizeQueueItem).filter(Boolean) as QueueItem[];
      if (!normalized.length) return false;
      const addedAt = Date.now();
      const nextItems = [
        ...safeArray<QueueItem>(playlist.items).map(normalizeQueueItem).filter(Boolean),
        ...normalized.map((item) => ({ ...item, addedAt }))
      ] as QueueItem[];
      const saved = await endpoints.savePlaylist(playlist.id, {
        name: playlist.name,
        createdAt: playlistCreatedAt(playlist),
        updatedAt: Date.now(),
        items: nextItems
      });
      setPlaylists((current) => [
        saved,
        ...current.filter((candidate) => candidate.id !== saved.id)
      ]);
      setNotice(
        normalized.length === 1
          ? `Added to ${saved.name}`
          : `Added ${normalized.length} songs to ${saved.name}`
      );
      return true;
    },
    [setNotice, setPlaylists]
  );

  const createPlaylistWithItems = useCallback(
    async (name: string, items: QueueItem[]) => {
      const normalized = items.map(normalizeQueueItem).filter(Boolean) as QueueItem[];
      if (!normalized.length) return false;
      const now = Date.now();
      try {
        const saved = await endpoints.savePlaylist(createPlaylistId(), {
          name: String(name || nextPlaylistName(playlists)).trim() || nextPlaylistName(playlists),
          createdAt: now,
          updatedAt: now,
          items: normalized.map((item) => ({ ...item, addedAt: now }))
        });
        setPlaylists((current) => [
          saved,
          ...current.filter((candidate) => candidate.id !== saved.id)
        ]);
        setNotice(
          normalized.length === 1
            ? `Added to ${saved.name}`
            : `Added ${normalized.length} songs to ${saved.name}`
        );
        return true;
      } catch (error) {
        setNotice(error instanceof Error ? error.message : 'Unable to create playlist');
        return false;
      }
    },
    [playlists, setNotice, setPlaylists]
  );

  return {
    closePlaylistPicker,
    createPlaylistWithItems,
    openPlaylistPickerForItems,
    playlistPicker,
    saveItemsToPlaylist
  };
}
