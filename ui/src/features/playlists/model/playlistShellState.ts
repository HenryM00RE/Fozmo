import type { Playlist } from '../../../shared/types';

export type PlaylistShellState = {
  createSidebarPlaylist: () => Promise<void>;
  playlists: Playlist[];
  sidebarPlaylistsOpen: boolean;
  toggleSidebarPlaylists: () => void;
};
