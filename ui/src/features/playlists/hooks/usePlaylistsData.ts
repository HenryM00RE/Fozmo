import { useCallback, useState } from 'react';
import type { Playlist } from '../../../shared/types';
import { loadPlaylists } from '../model/playlistModel';

export function usePlaylistsData() {
  const [playlists, setPlaylists] = useState<Playlist[]>([]);

  const refreshPlaylists = useCallback(async () => {
    setPlaylists(await loadPlaylists());
  }, []);

  return {
    playlists,
    refreshPlaylists,
    setPlaylists
  };
}
