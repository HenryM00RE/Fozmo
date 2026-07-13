import type { PlaylistShellState } from '../features/playlists/model/playlistShellState';
import type { ProfileShellState } from '../features/settings/model/profileShellState';
import type { ApplyProfilesResponse, ProfilesResponse } from '../features/settings/settingsModel';
import type { JsonRecord } from '../shared/types';
import type { SelectionToolbarState } from '../shared/ui/selectionToolbar';

type RefreshCore = () => Promise<void>;

type BuildPlaylistShellParams = PlaylistShellState;

export function buildPlaylistShell(params: BuildPlaylistShellParams): PlaylistShellState {
  return params;
}

type BuildProfileShellParams = {
  activeProfileId: string;
  applyProfilesResponse: ApplyProfilesResponse;
  profiles: JsonRecord[];
  refreshCore: RefreshCore;
  refreshProfileScopedData: RefreshCore;
  selectProfile: (profileId: string) => Promise<ProfilesResponse>;
};

export function buildProfileShell(params: BuildProfileShellParams): ProfileShellState {
  return params;
}

type BuildSelectionToolbarParams = Omit<
  SelectionToolbarState,
  'activeSelectionBusy' | 'activeSelectionCount' | 'activeSelectionType'
> & {
  albumSelectionActive: boolean;
  albumSelectionBusy: boolean;
  albumSelectionKeys: Set<string>;
  recentSelectionActive: boolean;
  recentSelectionBusy: boolean;
  recentSelectionKeys: Set<string>;
};

export function buildSelectionToolbar({
  addSelectedAlbumTracksToPlaylist,
  addSelectedRecentlyPlayedToPlaylist,
  albumSelectionActive,
  albumSelectionBusy,
  albumSelectionKeys,
  albumSelectionMenuOpen,
  clearAlbumTrackSelection,
  clearRecentSelection,
  playSelectedAlbumTracks,
  playSelectedRecentlyPlayed,
  queueSelectedAlbumTracks,
  queueSelectedRecentlyPlayed,
  recentSelectionActive,
  recentSelectionBusy,
  recentSelectionKeys,
  recentSelectionMenuOpen,
  setAlbumSelectionMenuOpen,
  setRecentSelectionMenuOpen
}: BuildSelectionToolbarParams): SelectionToolbarState {
  const activeSelectionType = albumSelectionActive
    ? 'album-tracks'
    : recentSelectionActive
      ? 'recently-played'
      : null;
  return {
    activeSelectionBusy: albumSelectionActive ? albumSelectionBusy : recentSelectionBusy,
    activeSelectionCount: albumSelectionActive ? albumSelectionKeys.size : recentSelectionKeys.size,
    activeSelectionType,
    addSelectedAlbumTracksToPlaylist,
    addSelectedRecentlyPlayedToPlaylist,
    albumSelectionMenuOpen,
    clearAlbumTrackSelection,
    clearRecentSelection,
    playSelectedAlbumTracks,
    playSelectedRecentlyPlayed,
    queueSelectedAlbumTracks,
    queueSelectedRecentlyPlayed,
    recentSelectionMenuOpen,
    setAlbumSelectionMenuOpen,
    setRecentSelectionMenuOpen
  };
}
