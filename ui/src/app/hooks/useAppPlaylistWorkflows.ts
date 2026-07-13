import type { Dispatch, SetStateAction } from 'react';
import { usePlaylistPicker } from '../../features/playlists/hooks/usePlaylistPicker';
import { useSidebarPlaylists } from '../../features/playlists/hooks/useSidebarPlaylists';
import { createPlaylistId } from '../../features/playlists/model/playlistModel';
import { endpoints } from '../../shared/lib/api';
import type { LibraryTrack, Playlist, QueueItem, RouteState } from '../../shared/types';
import { buildPlaylistChrome, buildPlaylistRoute, buildPlaylistShell } from '../appComposition';

type UseAppPlaylistWorkflowsParams = {
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  navigate: (next: RouteState) => void;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  playlists: Playlist[];
  refreshCore: () => Promise<void>;
  setNotice: (message: string) => void;
  setPlaylists: Dispatch<SetStateAction<Playlist[]>>;
  tracks: LibraryTrack[];
};

export function useAppPlaylistWorkflows({
  addItemsToQueue,
  navigate,
  playItems,
  playlists,
  refreshCore,
  setNotice,
  setPlaylists,
  tracks
}: UseAppPlaylistWorkflowsParams) {
  const {
    closePlaylistPicker,
    createPlaylistWithItems,
    openPlaylistPickerForItems,
    playlistPicker,
    saveItemsToPlaylist
  } = usePlaylistPicker({ playlists, setPlaylists, setNotice });
  const { createSidebarPlaylist, sidebarPlaylistsOpen, toggleSidebarPlaylists } =
    useSidebarPlaylists({ navigate, playlists, refreshCore });
  const createRoutePlaylist = async (name: string) => {
    const now = Date.now();
    try {
      const saved = await endpoints.savePlaylist(createPlaylistId(), {
        name: name.trim(),
        createdAt: now,
        updatedAt: now,
        items: []
      });
      setPlaylists((current) => [
        saved,
        ...current.filter((candidate) => candidate.id !== saved.id)
      ]);
      setNotice(`Created ${saved.name}`);
      return saved;
    } catch (error) {
      setNotice(error instanceof Error ? error.message : 'Unable to create playlist');
      throw error;
    }
  };

  return {
    openPlaylistPickerForItems,
    playlistChrome: buildPlaylistChrome({
      closePlaylistPicker,
      createPlaylistWithItems,
      playlistPicker,
      playlists,
      saveItemsToPlaylist
    }),
    playlistRoute: buildPlaylistRoute({
      addItemsToQueue,
      createPlaylist: createRoutePlaylist,
      playItems,
      playlists,
      refreshCore,
      tracks
    }),
    playlistShell: buildPlaylistShell({
      createSidebarPlaylist,
      playlists,
      sidebarPlaylistsOpen,
      toggleSidebarPlaylists
    })
  };
}
