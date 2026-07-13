import type { Dispatch, SetStateAction } from 'react';

export type ActiveSelectionType = 'album-tracks' | 'recently-played' | null;
export type QueuePlacement = 'next' | 'end';

export type SelectionToolbarState = {
  activeSelectionBusy: boolean;
  activeSelectionCount: number;
  activeSelectionType: ActiveSelectionType;
  addSelectedAlbumTracksToPlaylist: () => void;
  addSelectedRecentlyPlayedToPlaylist: () => Promise<void>;
  albumSelectionMenuOpen: boolean;
  clearAlbumTrackSelection: () => void;
  clearRecentSelection: () => void;
  playSelectedAlbumTracks: () => void;
  playSelectedRecentlyPlayed: () => Promise<void>;
  queueSelectedAlbumTracks: (placement: QueuePlacement) => void;
  queueSelectedRecentlyPlayed: (placement: QueuePlacement) => Promise<void>;
  recentSelectionMenuOpen: boolean;
  setAlbumSelectionMenuOpen: Dispatch<SetStateAction<boolean>>;
  setRecentSelectionMenuOpen: Dispatch<SetStateAction<boolean>>;
};
