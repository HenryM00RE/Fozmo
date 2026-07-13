import type { AlbumTrackSelectionRouteState } from '../features/albums/model/albumModel';
import { DiscoverPage } from '../features/discover/DiscoverPage';
import { HomeRouteView } from '../features/home/HomeRouteView';
import type { HomeRouteState } from '../features/home/model/homeRouteState';
import { LibraryRouteView } from '../features/library/LibraryRouteView';
import type { LibraryRouteState } from '../features/library/model/libraryRouteState';
import type { PlaybackRouteActions } from '../features/playback/model/playbackRouteActions';
import type { PlaybackStatus } from '../features/playback/model/playbackStore';
import type { PlaylistRouteState } from '../features/playlists/model/playlistModel';
import { PlaylistRouteView } from '../features/playlists/PlaylistRouteView';
import { QobuzRouteView } from '../features/qobuz/QobuzRouteView';
import type { SettingsRouteState } from '../features/settings/model/settingsRouteState';
import { SettingsRouteView } from '../features/settings/SettingsRouteView';
import { capabilityEnabled } from '../shared/lib/capabilities';
import type { CustomDisplayFontSettings } from '../shared/lib/theme';
import type { JsonRecord, RouteState } from '../shared/types';
import { MobileLibraryPage } from './mobile/MobileLibraryPage';

type AppRoutesProps = {
  route: RouteState;
  loading: boolean;
  qobuzHome: JsonRecord | null;
  navigate: (next: RouteState) => void;
  openArtistName: (rawName: unknown) => void;
  setNotice: (message: string) => void;
  playbackActions: PlaybackRouteActions;
  playbackStatus: PlaybackStatus;
  albumTrackSelection: AlbumTrackSelectionRouteState;
  homeRoute: HomeRouteState;
  libraryRoute: LibraryRouteState;
  playlistRoute: PlaylistRouteState;
  settingsRoute: SettingsRouteState;
  customDisplayFont: CustomDisplayFontSettings | null;
};

export function AppRoutes({
  route,
  loading,
  qobuzHome,
  navigate,
  openArtistName,
  setNotice,
  playbackActions,
  playbackStatus,
  albumTrackSelection,
  homeRoute,
  libraryRoute,
  playlistRoute,
  settingsRoute,
  customDisplayFont
}: AppRoutesProps) {
  const remoteSurface = settingsRoute.status.surface === 'remote';
  const qobuzConnected = Boolean(
    settingsRoute.qobuzStatus?.logged_in || settingsRoute.qobuzStatus?.authenticated
  );
  switch (route.view) {
    case 'library':
      return <MobileLibraryPage onNavigate={navigate} />;
    case 'discover':
      if (!capabilityEnabled(settingsRoute.status, 'qobuz')) {
        return (
          <HomeRouteView
            homeRoute={homeRoute}
            loading={loading}
            navigate={navigate}
            playlists={playlistRoute.playlists}
            playQobuzAlbum={playbackActions.playQobuzAlbum}
            qobuzHome={null}
            qobuzConnected={qobuzConnected}
            setNotice={setNotice}
          />
        );
      }
      return (
        <DiscoverPage
          loading={loading}
          qobuzHome={qobuzHome}
          qobuzConnected={qobuzConnected}
          onOpenServices={() => navigate({ view: 'settings', id: 'qobuz' })}
          selectedKeys={homeRoute.recentSelectionKeys}
          selectionActive={homeRoute.recentSelectionActive}
          onOpenQobuzAlbum={(id) => navigate({ view: 'qobuz-album', id })}
          onOpenQobuzPlaylist={(id) => navigate({ view: 'qobuz-playlist', id })}
          onPlayQobuzAlbum={playbackActions.playQobuzAlbum}
          onPlayQobuzPlaylist={playbackActions.playQobuzPlaylist}
          onToggleQobuzAlbumSelection={homeRoute.toggleAlbumSelection}
          onOpenArtist={openArtistName}
        />
      );
    case 'history':
    case 'albums':
    case 'album':
    case 'songs':
    case 'artists':
    case 'artist':
      return (
        <LibraryRouteView
          route={route}
          navigate={navigate}
          openArtistName={openArtistName}
          setNotice={setNotice}
          playbackActions={playbackActions}
          playbackStatus={playbackStatus}
          albumTrackSelection={albumTrackSelection}
          albumSelection={homeRoute}
          libraryRoute={libraryRoute}
          remoteSurface={remoteSurface}
          customDisplayFont={customDisplayFont}
        />
      );
    case 'qobuz-album':
    case 'qobuz-playlist':
      if (!capabilityEnabled(settingsRoute.status, 'qobuz')) {
        return (
          <HomeRouteView
            homeRoute={homeRoute}
            loading={loading}
            navigate={navigate}
            playlists={playlistRoute.playlists}
            playQobuzAlbum={playbackActions.playQobuzAlbum}
            qobuzHome={null}
            qobuzConnected={qobuzConnected}
            setNotice={setNotice}
          />
        );
      }
      return (
        <QobuzRouteView
          route={route}
          navigate={navigate}
          openArtistName={openArtistName}
          playbackActions={playbackActions}
          playbackStatus={playbackStatus}
          albumTrackSelection={albumTrackSelection}
          remoteSurface={remoteSurface}
          customDisplayFont={customDisplayFont}
        />
      );
    case 'playlists':
    case 'playlist':
      return (
        <PlaylistRouteView
          route={route}
          navigate={navigate}
          openArtistName={openArtistName}
          playlistRoute={playlistRoute}
          customDisplayFont={customDisplayFont}
        />
      );
    case 'settings':
      return <SettingsRouteView route={route} settingsRoute={settingsRoute} />;
    default:
      return (
        <HomeRouteView
          homeRoute={homeRoute}
          loading={loading}
          navigate={navigate}
          playlists={playlistRoute.playlists}
          playQobuzAlbum={playbackActions.playQobuzAlbum}
          qobuzHome={qobuzHome}
          qobuzConnected={qobuzConnected}
          setNotice={setNotice}
        />
      );
  }
}
