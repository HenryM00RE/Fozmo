import type { Dispatch, SetStateAction } from 'react';
import { usePlaybackQueue } from '../../features/playback/hooks/usePlaybackQueue';
import { usePlaybackSnapshot } from '../../features/playback/model/playbackStore';
import { useSelectedPlaybackZone } from '../../features/playback/model/zoneSelection';
import { useGlobalSearchQueueActions } from '../../features/search/globalSearchModel';
import type { JsonRecord, LibraryAlbum, LibraryTrack, ZoneProfile } from '../../shared/types';
import { buildPlaybackRouteActions } from '../appComposition';

type UseAppPlaybackActionsParams = {
  albums: LibraryAlbum[];
  refreshRecentlyPlayed: () => Promise<void>;
  setNotice: (message: string) => void;
  setSignalOpen: Dispatch<SetStateAction<boolean>>;
  tracks: LibraryTrack[];
  zones: ZoneProfile[];
};

export function useAppPlaybackActions({
  albums,
  refreshRecentlyPlayed,
  setNotice,
  setSignalOpen,
  tracks,
  zones
}: UseAppPlaybackActionsParams) {
  const playback = usePlaybackSnapshot();
  const globalStatus = playback.status as JsonRecord;
  // The browser's own private zone arrives in `zones` like any other zone:
  // the server includes it only for the browser session that registered it.
  const playbackZones = zones;
  const { activeZoneId, selectZone, status } = useSelectedPlaybackZone(globalStatus, playbackZones);
  const playbackQueue = usePlaybackQueue({
    activeZoneId,
    refreshRecentlyPlayed,
    setNotice,
    setSignalOpen,
    status,
    tracks
  });
  const globalSearchQueueActions = useGlobalSearchQueueActions({
    addItemsToQueue: playbackQueue.addItemsToQueue,
    albums,
    setNotice
  });
  const routePlaybackActions = buildPlaybackRouteActions({
    addItemsToQueue: playbackQueue.addItemsToQueue,
    playAlbum: playbackQueue.playAlbum,
    playArtistRadio: playbackQueue.playArtistRadio,
    playItems: playbackQueue.playItems,
    playQobuzAlbum: playbackQueue.playQobuzAlbum,
    playQobuzPlaylist: playbackQueue.playQobuzPlaylist,
    playQobuzTrack: playbackQueue.playQobuzTrack,
    playSingleTrack: playbackQueue.playSingleTrack,
    playTrack: playbackQueue.playTrack
  });

  return {
    activeZoneId,
    playbackZones,
    selectZone,
    routePlaybackActions,
    status,
    ...playbackQueue,
    ...globalSearchQueueActions
  };
}
