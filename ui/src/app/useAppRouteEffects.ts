import { type Dispatch, type SetStateAction, useEffect } from 'react';
import type { RouteState } from '../shared/types';
import type { ToolbarAction } from '../shared/ui/toolbar';

type UseAppRouteEffectsParams = {
  albumSelectionActive: boolean;
  clearAlbumTrackSelection: () => void;
  clearPlaylistSelection: () => void;
  clearRecentSelection: () => void;
  playlistSelectionActive: boolean;
  recentSelectionActive: boolean;
  route: RouteState;
  setToolbarAction: Dispatch<SetStateAction<ToolbarAction | null>>;
};

export function useAppRouteEffects({
  albumSelectionActive,
  clearAlbumTrackSelection,
  clearPlaylistSelection,
  clearRecentSelection,
  playlistSelectionActive,
  recentSelectionActive,
  route,
  setToolbarAction
}: UseAppRouteEffectsParams) {
  useEffect(() => {
    if (route.view !== 'home' && route.view !== 'albums' && recentSelectionActive)
      clearRecentSelection();
    if (
      route.view !== 'album' &&
      route.view !== 'qobuz-album' &&
      route.view !== 'songs' &&
      albumSelectionActive
    )
      clearAlbumTrackSelection();
    if (route.view !== 'playlists' && playlistSelectionActive) clearPlaylistSelection();
  }, [
    albumSelectionActive,
    clearAlbumTrackSelection,
    clearPlaylistSelection,
    clearRecentSelection,
    playlistSelectionActive,
    recentSelectionActive,
    route.view
  ]);

  useEffect(() => {
    if (route.view !== 'settings') setToolbarAction(null);
  }, [route.view, setToolbarAction]);
}
