import type { JsonRecord, Playlist, RouteState } from '../../shared/types';
import { HomePage } from './HomePage';
import type { HomeRouteState } from './model/homeRouteState';

type HomeRouteViewProps = {
  homeRoute: HomeRouteState;
  loading: boolean;
  navigate: (next: RouteState) => void;
  playlists: Playlist[];
  playQobuzAlbum: (id: string | number) => void;
  qobuzHome: JsonRecord | null;
  qobuzConnected: boolean;
  setNotice: (message: string) => void;
};

export function HomeRouteView({
  homeRoute,
  loading,
  navigate,
  playlists,
  playQobuzAlbum,
  qobuzHome,
  qobuzConnected,
  setNotice
}: HomeRouteViewProps) {
  return (
    <HomePage
      loading={loading}
      recent={homeRoute.recentlyPlayedItems}
      recentLoading={homeRoute.recentlyPlayedLoading}
      playlists={playlists}
      qobuzHome={qobuzHome}
      qobuzConnected={qobuzConnected}
      onOpenServices={() => navigate({ view: 'settings', id: 'qobuz' })}
      selectedKeys={homeRoute.recentSelectionKeys}
      selectionActive={homeRoute.recentSelectionActive}
      onOpenRecent={(item) => {
        homeRoute
          .openRecentItem(item)
          .catch((error) =>
            setNotice(error instanceof Error ? error.message : 'Failed to load item')
          );
      }}
      onPlayRecent={(item) => {
        homeRoute
          .playRecentItem(item)
          .catch((error) =>
            setNotice(error instanceof Error ? error.message : 'Could not play this item')
          );
      }}
      onToggleRecentSelection={homeRoute.toggleRecentSelection}
      onToggleQobuzAlbumSelection={homeRoute.toggleAlbumSelection}
      onOpenQobuzAlbum={(id) => navigate({ view: 'qobuz-album', id })}
      onPlayQobuzAlbum={playQobuzAlbum}
    />
  );
}
