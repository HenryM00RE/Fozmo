import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';

function settledValue<T>(result: PromiseSettledResult<T>) {
  return result.status === 'fulfilled' ? result.value : undefined;
}

export type RecentlyPlayedShelves = {
  recentAlbums?: JsonRecord[];
  recentPlaylists?: JsonRecord[];
};

export async function loadRecentlyPlayedShelves(limit = 50): Promise<RecentlyPlayedShelves> {
  const [albumsResult, playlistsResult] = await Promise.allSettled([
    endpoints.recentAlbums(limit),
    endpoints.recentPlaylists(limit)
  ]);

  return {
    recentAlbums: settledValue(albumsResult),
    recentPlaylists: settledValue(playlistsResult)
  };
}
