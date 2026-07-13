import type {
  AlbumSelectionItem,
  AlbumTrackSelectionRouteState
} from '../features/albums/model/albumModel';
import type { HomeRouteState } from '../features/home/model/homeRouteState';
import type { LibraryRouteState } from '../features/library/model/libraryRouteState';
import type { PlaybackRouteActions } from '../features/playback/model/playbackRouteActions';
import type { PlaylistRouteState } from '../features/playlists/model/playlistModel';
import type { SettingsRouteState } from '../features/settings/model/settingsRouteState';
import type { ApplyProfilesResponse, ProfilesResponse } from '../features/settings/settingsModel';
import type { JsonRecord, LibraryTrack, Playlist, QueueItem, ZoneProfile } from '../shared/types';

type RefreshCore = () => Promise<void>;
type OpenPlaylistPickerForItems = (
  items: QueueItem[],
  title?: string,
  onAdded?: () => void
) => void;

export function buildPlaybackRouteActions(actions: PlaybackRouteActions) {
  return actions;
}

type BuildAlbumTrackSelectionRouteParams = {
  albumSelectionActive: boolean;
  albumSelectionKeys: Set<string>;
  openPlaylistPickerForItems: OpenPlaylistPickerForItems;
  registerAlbumSelectionItems: (items: AlbumSelectionItem[]) => void;
  toggleAlbumTrackSelection: (key: string) => void;
};

export function buildAlbumTrackSelectionRoute({
  albumSelectionActive,
  albumSelectionKeys,
  openPlaylistPickerForItems,
  registerAlbumSelectionItems,
  toggleAlbumTrackSelection
}: BuildAlbumTrackSelectionRouteParams): AlbumTrackSelectionRouteState {
  return {
    openPlaylistPickerForItems,
    onSelectionItemsChange: registerAlbumSelectionItems,
    onToggleSelection: toggleAlbumTrackSelection,
    selectedTrackKeys: albumSelectionKeys,
    selectionActive: albumSelectionActive
  };
}

type BuildHomeRouteParams = {
  openRecentlyPlayedItem: (item: JsonRecord) => Promise<void>;
  playRecentlyPlayedItem: (item: JsonRecord) => Promise<void>;
  recentlyPlayedLoading: boolean;
  recentlyPlayedItems: JsonRecord[];
  recentSelectionActive: boolean;
  recentSelectionKeys: Set<string>;
  toggleAlbumSelection: (album: JsonRecord) => void;
  toggleRecentSelection: (item: JsonRecord) => void;
};

export function buildHomeRoute({
  openRecentlyPlayedItem,
  playRecentlyPlayedItem,
  recentlyPlayedLoading,
  recentlyPlayedItems,
  recentSelectionActive,
  recentSelectionKeys,
  toggleAlbumSelection,
  toggleRecentSelection
}: BuildHomeRouteParams): HomeRouteState {
  return {
    openRecentItem: openRecentlyPlayedItem,
    playRecentItem: playRecentlyPlayedItem,
    recentlyPlayedLoading,
    recentlyPlayedItems,
    recentSelectionActive,
    recentSelectionKeys,
    toggleAlbumSelection,
    toggleRecentSelection
  };
}

type BuildPlaylistRouteParams = Pick<
  PlaylistRouteState,
  'addItemsToQueue' | 'createPlaylist' | 'playItems'
> & {
  playlists: Playlist[];
  refreshCore: RefreshCore;
  tracks: LibraryTrack[];
};

export function buildPlaylistRoute({
  addItemsToQueue,
  createPlaylist,
  playItems,
  playlists,
  refreshCore,
  tracks
}: BuildPlaylistRouteParams): PlaylistRouteState {
  return {
    addItemsToQueue,
    createPlaylist,
    onRefresh: refreshCore,
    playItems,
    playlists,
    tracks
  };
}

type BuildSettingsRouteParams = {
  activeProfileId: string;
  applyProfilesResponse: ApplyProfilesResponse;
  profiles: JsonRecord[];
  qobuzStatus: JsonRecord | null;
  refreshCore: RefreshCore;
  refreshProfileScopedData: RefreshCore;
  selectProfile: (profileId: string) => Promise<ProfilesResponse>;
  settingsStatus: JsonRecord;
  zones: ZoneProfile[];
};

export function buildSettingsRoute({
  activeProfileId,
  applyProfilesResponse,
  profiles,
  qobuzStatus,
  refreshCore,
  refreshProfileScopedData,
  selectProfile,
  settingsStatus,
  zones
}: BuildSettingsRouteParams): SettingsRouteState {
  return {
    activeProfileId,
    applyProfilesResponse,
    onRefresh: refreshCore,
    onProfileScopedRefresh: refreshProfileScopedData,
    profiles,
    qobuzStatus,
    selectProfile,
    status: settingsStatus,
    zones
  };
}

type BuildLibraryRouteParams = LibraryRouteState;

export function buildLibraryRoute(params: BuildLibraryRouteParams): LibraryRouteState {
  return params;
}
