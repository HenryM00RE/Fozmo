import { useCallback, useState } from 'react';
import { storageKey } from '../../../shared/identity';
import { endpoints } from '../../../shared/lib/api';
import { nextPlaylistName } from '../../../shared/lib/appSupport';
import type { Playlist, RouteState } from '../../../shared/types';

const SIDEBAR_PLAYLISTS_OPEN_KEY = storageKey('SidebarPlaylistsOpen');

type UseSidebarPlaylistsParams = {
  navigate: (next: RouteState) => void;
  playlists: Playlist[];
  refreshCore: () => Promise<void>;
};

export function useSidebarPlaylists({
  navigate,
  playlists,
  refreshCore
}: UseSidebarPlaylistsParams) {
  const [sidebarPlaylistsOpen, setSidebarPlaylistsOpen] = useState(() => {
    try {
      return localStorage.getItem(SIDEBAR_PLAYLISTS_OPEN_KEY) !== '0';
    } catch {
      return true;
    }
  });

  const toggleSidebarPlaylists = useCallback(() => {
    setSidebarPlaylistsOpen((open) => {
      const next = !open;
      try {
        localStorage.setItem(SIDEBAR_PLAYLISTS_OPEN_KEY, next ? '1' : '0');
      } catch {
        // The visible state still toggles even when storage is unavailable.
      }
      return next;
    });
  }, []);

  const createSidebarPlaylist = useCallback(async () => {
    setSidebarPlaylistsOpen(true);
    try {
      localStorage.setItem(SIDEBAR_PLAYLISTS_OPEN_KEY, '1');
    } catch {
      // Ignore storage failures; the section is open in this session.
    }
    const id = crypto.randomUUID();
    await endpoints.savePlaylist(id, { name: nextPlaylistName(playlists), items: [] });
    await refreshCore();
    navigate({ view: 'playlist', id });
  }, [navigate, playlists, refreshCore]);

  return {
    createSidebarPlaylist,
    sidebarPlaylistsOpen,
    toggleSidebarPlaylists
  };
}
