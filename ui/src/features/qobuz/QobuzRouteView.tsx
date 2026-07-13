import type { CustomDisplayFontSettings } from '../../shared/lib/theme';
import type { RouteState } from '../../shared/types';
import type { AlbumTrackSelectionRouteState } from '../albums/model/albumModel';
import type { PlaybackRouteActions } from '../playback/model/playbackRouteActions';
import type { PlaybackStatus } from '../playback/model/playbackStore';
import { QobuzAlbumPage } from './pages/QobuzAlbumPage';
import { QobuzPlaylistPage } from './pages/QobuzPlaylistPage';

type QobuzRouteViewProps = {
  albumTrackSelection: AlbumTrackSelectionRouteState;
  navigate: (next: RouteState) => void;
  openArtistName: (rawName: unknown) => void;
  playbackActions: PlaybackRouteActions;
  playbackStatus: PlaybackStatus;
  remoteSurface?: boolean;
  route: RouteState;
  customDisplayFont: CustomDisplayFontSettings | null;
};

export function QobuzRouteView({
  albumTrackSelection,
  navigate,
  openArtistName,
  playbackActions,
  playbackStatus,
  remoteSurface = false,
  route,
  customDisplayFont
}: QobuzRouteViewProps) {
  if (route.view === 'qobuz-playlist') {
    return (
      <QobuzPlaylistPage
        id={route.id}
        onOpenArtist={openArtistName}
        onOpenQobuzAlbum={(id) => navigate({ view: 'qobuz-album', id })}
        playItems={playbackActions.playItems}
        addItemsToQueue={playbackActions.addItemsToQueue}
        customDisplayFont={customDisplayFont}
      />
    );
  }

  return (
    <QobuzAlbumPage
      id={route.id}
      albumHint={route.albumHint}
      onOpenArtist={openArtistName}
      onOpenLocalAlbum={(id) => navigate({ view: 'album', id })}
      onOpenQobuzAlbum={(id, albumHint) => navigate({ view: 'qobuz-album', id, albumHint })}
      playAlbum={playbackActions.playAlbum}
      playItems={playbackActions.playItems}
      addItemsToQueue={playbackActions.addItemsToQueue}
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
}
