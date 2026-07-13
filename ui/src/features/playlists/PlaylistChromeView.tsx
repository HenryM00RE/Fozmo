import { PlaylistPicker } from './components/PlaylistPicker';
import type { PlaylistChromeState } from './model/playlistChromeState';

type PlaylistChromeViewProps = {
  playlistChrome: PlaylistChromeState;
};

export function PlaylistChromeView({ playlistChrome }: PlaylistChromeViewProps) {
  const { onAddToPlaylist, onClosePlaylistPicker, onCreatePlaylist, picker, playlists } =
    playlistChrome;

  if (!picker) return null;

  return (
    <PlaylistPicker
      picker={picker}
      playlists={playlists}
      onClose={onClosePlaylistPicker}
      onAddToPlaylist={onAddToPlaylist}
      onCreatePlaylist={onCreatePlaylist}
    />
  );
}
