import type { Dispatch, SetStateAction } from 'react';
import type { PlaybackChromeState } from '../features/playback/model/playbackChromeState';
import type { PlaylistChromeState } from '../features/playlists/model/playlistChromeState';
import type { SearchChromeState } from '../features/search/model/searchChromeState';
import { localTrackToQueueItem, qobuzTrackToQueueItem } from '../shared/lib/queue';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  QobuzTrack,
  QueueItem,
  QueueState,
  RouteState,
  ZoneProfile
} from '../shared/types';

type Navigate = (next: RouteState) => void;

type BuildPlaybackChromeParams = {
  activeZoneId: string;
  albums: LibraryAlbum[];
  clearQueue: () => void;
  navigate: Navigate;
  nowPlayingOpen: boolean;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string) => void;
  onSelectZone: (zoneId: string) => Promise<void>;
  queue: QueueState;
  setNowPlayingOpen: Dispatch<SetStateAction<boolean>>;
  setSignalOpen: Dispatch<SetStateAction<boolean>>;
  shuffleQueue: () => void;
  signalOpen: boolean;
  status: JsonRecord;
  toggleLoop: () => void;
  zones: ZoneProfile[];
};

export function buildPlaybackChrome({
  activeZoneId,
  albums,
  clearQueue,
  navigate,
  nowPlayingOpen,
  openPlaylistPickerForItems,
  onSelectZone,
  queue,
  setNowPlayingOpen,
  setSignalOpen,
  shuffleQueue,
  signalOpen,
  status,
  toggleLoop,
  zones
}: BuildPlaybackChromeParams): PlaybackChromeState {
  return {
    activeZoneId,
    albums,
    nowPlayingOpen,
    onAddToPlaylist: openPlaylistPickerForItems,
    onClearQueue: clearQueue,
    onOpenAlbum: (target) => {
      setNowPlayingOpen(false);
      navigate({ view: target.source === 'qobuz' ? 'qobuz-album' : 'album', id: target.id });
    },
    onSelectZone,
    onShuffleQueue: shuffleQueue,
    onToggleLoop: toggleLoop,
    queue,
    setNowPlayingOpen,
    setSignalOpen,
    signalOpen,
    status,
    zones
  };
}

type BuildPlaylistChromeParams = Omit<
  PlaylistChromeState,
  'onAddToPlaylist' | 'onClosePlaylistPicker' | 'onCreatePlaylist' | 'picker'
> & {
  closePlaylistPicker: () => void;
  createPlaylistWithItems: PlaylistChromeState['onCreatePlaylist'];
  playlistPicker: PlaylistChromeState['picker'];
  saveItemsToPlaylist: PlaylistChromeState['onAddToPlaylist'];
};

export function buildPlaylistChrome({
  closePlaylistPicker,
  createPlaylistWithItems,
  playlistPicker,
  playlists,
  saveItemsToPlaylist
}: BuildPlaylistChromeParams): PlaylistChromeState {
  return {
    onAddToPlaylist: saveItemsToPlaylist,
    onClosePlaylistPicker: closePlaylistPicker,
    onCreatePlaylist: createPlaylistWithItems,
    picker: playlistPicker,
    playlists
  };
}

type BuildSearchChromeParams = {
  albums: LibraryAlbum[];
  globalSearch: SearchChromeState['globalSearch'];
  openPlaylistPickerForItems: (
    items: import('../shared/types').QueueItem[],
    title?: string
  ) => void;
  navigate: Navigate;
  openArtistName: (rawName: unknown) => void;
  playQobuzTrack: (track: QobuzTrack) => void;
  playSingleTrack: (track: LibraryTrack) => void;
  queueGlobalSearchAlbum: SearchChromeState['onQueueAlbum'];
  queueGlobalSearchTrack: SearchChromeState['onQueueTrack'];
};

export function buildSearchChrome({
  albums,
  globalSearch,
  openPlaylistPickerForItems,
  navigate,
  openArtistName,
  playQobuzTrack,
  playSingleTrack,
  queueGlobalSearchAlbum,
  queueGlobalSearchTrack
}: BuildSearchChromeParams): SearchChromeState {
  return {
    albums,
    globalSearch,
    onAddTrackToPlaylist: (track, source) => {
      const item =
        source === 'qobuz'
          ? qobuzTrackToQueueItem(track as QobuzTrack)
          : localTrackToQueueItem(track as LibraryTrack);
      openPlaylistPickerForItems([item], item.title || 'Track');
    },
    onOpenAlbum: (id: string | number) => navigate({ view: 'album', id }),
    onOpenArtist: openArtistName,
    onOpenQobuzAlbum: (id: string | number) => navigate({ view: 'qobuz-album', id }),
    onPlayQobuzTrack: playQobuzTrack,
    onPlayTrack: playSingleTrack,
    onQueueAlbum: queueGlobalSearchAlbum,
    onQueueTrack: queueGlobalSearchTrack
  };
}
