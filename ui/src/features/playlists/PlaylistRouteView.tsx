import type { CustomDisplayFontSettings } from '../../shared/lib/theme';
import type { RouteState } from '../../shared/types';
import type { PlaylistRouteState, PlaylistSelectionRouteState } from './model/playlistModel';
import { PlaylistDetailPage } from './pages/PlaylistDetailPage';
import { PlaylistsPage } from './pages/PlaylistsPage';

type PlaylistRouteViewProps = {
  navigate: (next: RouteState) => void;
  openArtistName: (rawName: unknown) => void;
  playlistRoute: PlaylistRouteState;
  playlistSelection: PlaylistSelectionRouteState;
  route: RouteState;
  customDisplayFont: CustomDisplayFontSettings | null;
};

export function PlaylistRouteView({
  navigate,
  openArtistName,
  playlistRoute,
  playlistSelection,
  route,
  customDisplayFont
}: PlaylistRouteViewProps) {
  if (route.view === 'playlists') {
    return (
      <PlaylistsPage
        playlists={playlistRoute.playlists}
        selectedPlaylistIds={playlistSelection.selectedPlaylistIds}
        selectionActive={playlistSelection.selectionActive}
        onToggleSelection={playlistSelection.onToggleSelection}
        onCreatePlaylist={playlistRoute.createPlaylist}
        onOpen={(id) => navigate({ view: 'playlist', id })}
        onRefresh={playlistRoute.onRefresh}
        playItems={playlistRoute.playItems}
        tracks={playlistRoute.tracks}
      />
    );
  }

  return (
    <PlaylistDetailPage
      id={String(route.id || '')}
      playlists={playlistRoute.playlists}
      onBack={() => navigate({ view: 'playlists' })}
      onRefresh={playlistRoute.onRefresh}
      playItems={playlistRoute.playItems}
      addItemsToQueue={playlistRoute.addItemsToQueue}
      tracks={playlistRoute.tracks}
      onOpenAlbum={(id) => navigate({ view: 'album', id })}
      onOpenQobuzAlbum={(id) => navigate({ view: 'qobuz-album', id })}
      onOpenArtist={openArtistName}
      customDisplayFont={customDisplayFont}
    />
  );
}
