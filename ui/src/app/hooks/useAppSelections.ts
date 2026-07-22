import { useCallback } from 'react';
import { useAlbumSelection } from '../../features/albums/hooks/useAlbumSelection';
import { useRecentlyPlayedSelection } from '../../features/home/hooks/useRecentlyPlayedSelection';
import { usePlaylistSelection } from '../../features/playlists/hooks/usePlaylistSelection';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  Playlist,
  QueueItem,
  RouteState
} from '../../shared/types';
import {
  buildAlbumTrackSelectionRoute,
  buildHomeRoute,
  buildSelectionToolbar
} from '../appComposition';

type UseAppSelectionsParams = {
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  albums: LibraryAlbum[];
  navigate: (next: RouteState) => void;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  playAlbum: (albumId: string | number, startIndex?: number, shuffle?: boolean) => Promise<void>;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  playlists: Playlist[];
  recentAlbums: JsonRecord[];
  recentlyPlayedLoading: boolean;
  recentPlaylists: JsonRecord[];
  setNotice: (message: string) => void;
  tracks: LibraryTrack[];
};

export function useAppSelections({
  addItemsToQueue,
  albums,
  navigate,
  openPlaylistPickerForItems,
  playAlbum,
  playItems,
  playlists,
  recentAlbums,
  recentlyPlayedLoading,
  recentPlaylists,
  setNotice,
  tracks
}: UseAppSelectionsParams) {
  const {
    addSelectionToPlaylist: addSelectedAlbumTracksToPlaylist,
    clearSelection: clearAlbumTrackSelection,
    playSelection: playSelectedAlbumTracks,
    queueSelection: queueSelectedAlbumTracks,
    registerSelectionItems: registerAlbumSelectionItems,
    selectionActive: albumSelectionActive,
    selectionBusy: albumSelectionBusy,
    selectionKeys: albumSelectionKeys,
    selectionMenuOpen: albumSelectionMenuOpen,
    setSelectionMenuOpen: setAlbumSelectionMenuOpen,
    toggleSelection: toggleAlbumSelection
  } = useAlbumSelection({
    addItemsToQueue,
    openPlaylistPickerForItems,
    playItems
  });

  const {
    clearSelection: clearPlaylistSelection,
    playSelection: playSelectedPlaylists,
    queueSelection: queueSelectedPlaylists,
    selectionActive: playlistSelectionActive,
    selectionKeys: playlistSelectionKeys,
    selectionMenuOpen: playlistSelectionMenuOpen,
    setSelectionMenuOpen: setPlaylistSelectionMenuOpen,
    toggleSelection: togglePlaylistSelection
  } = usePlaylistSelection({
    addItemsToQueue,
    playlists,
    playItems,
    tracks
  });

  const clearOtherSelectionsForRecent = useCallback(() => {
    clearAlbumTrackSelection();
    clearPlaylistSelection();
  }, [clearAlbumTrackSelection, clearPlaylistSelection]);

  const {
    addSelectionToPlaylist: addSelectedRecentlyPlayedToPlaylist,
    clearSelection: clearRecentSelection,
    openItem: openRecentlyPlayedItem,
    playItem: playRecentlyPlayedItem,
    playSelection: playSelectedRecentlyPlayed,
    queueSelection: queueSelectedRecentlyPlayed,
    recentlyPlayedItems,
    selectionActive: recentSelectionActive,
    selectionBusy: recentSelectionBusy,
    selectionKeys: recentSelectionKeys,
    selectionMenuOpen: recentSelectionMenuOpen,
    setSelectionMenuOpen: setRecentSelectionMenuOpen,
    toggleSelection: toggleRecentSelection
  } = useRecentlyPlayedSelection({
    addItemsToQueue,
    albums,
    navigate,
    onSelectionStart: clearOtherSelectionsForRecent,
    openPlaylistPickerForItems,
    playAlbum,
    playItems,
    playlists,
    recentAlbums,
    recentPlaylists,
    setNotice
  });

  const toggleAlbumTrackSelection = useCallback(
    (key: string) => {
      if (!key) return;
      clearPlaylistSelection();
      clearRecentSelection();
      toggleAlbumSelection(key);
    },
    [clearPlaylistSelection, clearRecentSelection, toggleAlbumSelection]
  );

  const togglePlaylistCardSelection = useCallback(
    (playlistId: string) => {
      if (!playlistId) return;
      clearAlbumTrackSelection();
      clearRecentSelection();
      togglePlaylistSelection(playlistId);
    },
    [clearAlbumTrackSelection, clearRecentSelection, togglePlaylistSelection]
  );

  const selectionToolbar = buildSelectionToolbar({
    addSelectedAlbumTracksToPlaylist,
    addSelectedRecentlyPlayedToPlaylist,
    albumSelectionActive,
    albumSelectionBusy,
    albumSelectionKeys,
    albumSelectionMenuOpen,
    clearAlbumTrackSelection,
    clearPlaylistSelection,
    clearRecentSelection,
    playSelectedAlbumTracks,
    playSelectedPlaylists,
    playSelectedRecentlyPlayed,
    playlistSelectionActive,
    playlistSelectionKeys,
    playlistSelectionMenuOpen,
    queueSelectedAlbumTracks,
    queueSelectedPlaylists,
    queueSelectedRecentlyPlayed,
    recentSelectionActive,
    recentSelectionBusy,
    recentSelectionKeys,
    recentSelectionMenuOpen,
    setAlbumSelectionMenuOpen,
    setPlaylistSelectionMenuOpen,
    setRecentSelectionMenuOpen
  });

  return {
    activeSelectionType: selectionToolbar.activeSelectionType,
    albumSelectionActive,
    albumTrackSelection: buildAlbumTrackSelectionRoute({
      albumSelectionActive,
      albumSelectionKeys,
      openPlaylistPickerForItems,
      registerAlbumSelectionItems,
      toggleAlbumTrackSelection
    }),
    clearAlbumTrackSelection,
    clearPlaylistSelection,
    clearRecentSelection,
    homeRoute: buildHomeRoute({
      openRecentlyPlayedItem,
      playRecentlyPlayedItem,
      recentlyPlayedLoading,
      recentlyPlayedItems,
      recentSelectionActive,
      recentSelectionKeys,
      toggleAlbumSelection: toggleRecentSelection,
      toggleRecentSelection
    }),
    playlistSelectionActive,
    playlistSelectionRoute: {
      onToggleSelection: togglePlaylistCardSelection,
      selectedPlaylistIds: playlistSelectionKeys,
      selectionActive: playlistSelectionActive
    },
    recentSelectionActive,
    selectionToolbar
  };
}
