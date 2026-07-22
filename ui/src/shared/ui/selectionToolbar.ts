import type { Dispatch, SetStateAction } from 'react';

export type ActiveSelectionType = 'album-tracks' | 'playlists' | 'recently-played' | null;
export type QueuePlacement = 'next' | 'end';

export type SelectionToolbarState = {
  activeSelectionBusy: boolean;
  activeSelectionCount: number;
  activeSelectionType: ActiveSelectionType;
  addSelectedAlbumTracksToPlaylist: () => void;
  addSelectedRecentlyPlayedToPlaylist: () => Promise<void>;
  albumSelectionMenuOpen: boolean;
  clearAlbumTrackSelection: () => void;
  clearPlaylistSelection: () => void;
  clearRecentSelection: () => void;
  playSelectedAlbumTracks: () => void;
  playSelectedPlaylists: () => void;
  playSelectedRecentlyPlayed: () => Promise<void>;
  queueSelectedAlbumTracks: (placement: QueuePlacement) => void;
  queueSelectedPlaylists: (placement: QueuePlacement) => void;
  queueSelectedRecentlyPlayed: (placement: QueuePlacement) => Promise<void>;
  playlistSelectionMenuOpen: boolean;
  recentSelectionMenuOpen: boolean;
  setAlbumSelectionMenuOpen: Dispatch<SetStateAction<boolean>>;
  setPlaylistSelectionMenuOpen: Dispatch<SetStateAction<boolean>>;
  setRecentSelectionMenuOpen: Dispatch<SetStateAction<boolean>>;
};
