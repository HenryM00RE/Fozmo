import type { PlaybackChromeState } from '../features/playback/model/playbackChromeState';
import { PlaybackChromeView } from '../features/playback/PlaybackChromeView';
import type { PlaylistChromeState } from '../features/playlists/model/playlistChromeState';
import { PlaylistChromeView } from '../features/playlists/PlaylistChromeView';
import type { SearchChromeState } from '../features/search/model/searchChromeState';
import { SearchChromeView } from '../features/search/SearchChromeView';

type AppChromeProps = {
  playbackChrome: PlaybackChromeState;
  playlistChrome: PlaylistChromeState;
  searchChrome: SearchChromeState;
};

export function AppChrome({ playbackChrome, playlistChrome, searchChrome }: AppChromeProps) {
  return (
    <>
      <PlaybackChromeView
        playbackChrome={playbackChrome}
        onOpenArtist={searchChrome.onOpenArtist}
      />
      <SearchChromeView searchChrome={searchChrome} />
      <PlaylistChromeView playlistChrome={playlistChrome} />
    </>
  );
}
