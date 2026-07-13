import type { CustomDisplayFontSettings } from '../../shared/lib/theme';
import type { RouteState } from '../../shared/types';
import type { AlbumTrackSelectionRouteState } from '../albums/model/albumModel';
import { AlbumDetailPage } from '../albums/pages/AlbumDetailPage';
import { AlbumsPage } from '../albums/pages/AlbumsPage';
import { ArtistDetailPage } from '../artists/pages/ArtistDetailPage';
import { ArtistsPage } from '../artists/pages/ArtistsPage';
import { HistoryPage } from '../history/HistoryPage';
import type { HomeRouteState } from '../home/model/homeRouteState';
import type { PlaybackRouteActions } from '../playback/model/playbackRouteActions';
import type { PlaybackStatus } from '../playback/model/playbackStore';
import { SongsPage } from '../songs/SongsPage';
import type { LibraryRouteState } from './model/libraryRouteState';

type LibraryRouteViewProps = {
  albumSelection: HomeRouteState;
  albumTrackSelection: AlbumTrackSelectionRouteState;
  libraryRoute: LibraryRouteState;
  navigate: (next: RouteState) => void;
  openArtistName: (rawName: unknown) => void;
  playbackActions: PlaybackRouteActions;
  playbackStatus: PlaybackStatus;
  remoteSurface?: boolean;
  route: RouteState;
  setNotice: (message: string) => void;
  customDisplayFont: CustomDisplayFontSettings | null;
};

export function LibraryRouteView({
  albumSelection,
  albumTrackSelection,
  libraryRoute,
  navigate,
  openArtistName,
  playbackActions,
  playbackStatus,
  remoteSurface = false,
  route,
  setNotice,
  customDisplayFont
}: LibraryRouteViewProps) {
  const {
    addItemsToQueue,
    playAlbum,
    playArtistRadio,
    playQobuzAlbum,
    playQobuzTrack,
    playSingleTrack,
    playTrack
  } = playbackActions;

  switch (route.view) {
    case 'history':
      return (
        <HistoryPage
          stats={libraryRoute.historyStats}
          statsLoading={libraryRoute.historyStatsLoading}
          recent={libraryRoute.recentHistory}
          recentLoading={libraryRoute.recentHistoryLoading}
          albums={libraryRoute.albums}
          onOpenAlbum={(id) => navigate({ view: 'album', id })}
          onOpenQobuzAlbum={(id) => navigate({ view: 'qobuz-album', id })}
          onOpenArtist={openArtistName}
          onNotice={setNotice}
        />
      );
    case 'albums':
      return (
        <AlbumsPage
          albums={libraryRoute.albums}
          onOpen={(id) => navigate({ view: 'album', id })}
          onOpenQobuzAlbum={(id) => navigate({ view: 'qobuz-album', id })}
          onPlay={(id) => playAlbum(id)}
          onPlayQobuzAlbum={playQobuzAlbum}
          onOpenArtist={openArtistName}
          selectedAlbumKeys={albumSelection.recentSelectionKeys}
          albumSelectionActive={albumSelection.recentSelectionActive}
          onToggleAlbumSelection={albumSelection.toggleAlbumSelection}
          onOpenMusicFolders={() => navigate({ view: 'settings', id: 'general' })}
        />
      );
    case 'album':
      return (
        <AlbumDetailPage
          id={route.id}
          playAlbum={playAlbum}
          onOpenArtist={openArtistName}
          onOpenQobuzAlbum={(id, albumHint) => navigate({ view: 'qobuz-album', id, albumHint })}
          addItemsToQueue={addItemsToQueue}
          playbackStatus={playbackStatus}
          selectedTrackKeys={albumTrackSelection.selectedTrackKeys}
          selectionActive={albumTrackSelection.selectionActive}
          onSelectionItemsChange={albumTrackSelection.onSelectionItemsChange}
          onToggleSelection={albumTrackSelection.onToggleSelection}
          openPlaylistPickerForItems={albumTrackSelection.openPlaylistPickerForItems}
          remoteSurface={remoteSurface}
          customDisplayFont={customDisplayFont}
        />
      );
    case 'songs':
      return (
        <SongsPage
          addItemsToQueue={addItemsToQueue}
          onOpenAlbum={(id) => navigate({ view: 'album', id })}
          onPlay={playSingleTrack}
          openPlaylistPickerForItems={albumTrackSelection.openPlaylistPickerForItems}
          selectedTrackKeys={albumTrackSelection.selectedTrackKeys}
          selectionActive={albumTrackSelection.selectionActive}
          onSelectionItemsChange={albumTrackSelection.onSelectionItemsChange}
          onToggleSelection={albumTrackSelection.onToggleSelection}
          onOpenMusicFolders={() => navigate({ view: 'settings', id: 'general' })}
        />
      );
    case 'artists':
      return (
        <ArtistsPage
          onOpen={openArtistName}
          onOpenMusicFolders={() => navigate({ view: 'settings', id: 'general' })}
        />
      );
    case 'artist':
      return (
        <ArtistDetailPage
          name={String(route.id || '')}
          albums={libraryRoute.albums}
          tracks={libraryRoute.tracks}
          onOpenAlbum={(id) => navigate({ view: 'album', id })}
          onOpenQobuzAlbum={(id, albumHint) => navigate({ view: 'qobuz-album', id, albumHint })}
          onOpenArtist={openArtistName}
          onPlayAlbum={(id) => playAlbum(id)}
          onPlayArtistRadio={playArtistRadio}
          onPlayQobuzAlbum={playQobuzAlbum}
          onPlayTrack={playTrack}
          onPlayQobuzTrack={playQobuzTrack}
          customDisplayFont={customDisplayFont}
        />
      );
    default:
      return null;
  }
}
