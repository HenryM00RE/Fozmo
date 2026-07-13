import { useCallback, useRef, useState } from 'react';
import type { JsonRecord } from '../../../shared/types';
import { loadRecentlyPlayedShelves } from '../model/recentlyPlayedData';

function recentItemPresentationKey(item: JsonRecord) {
  return [
    item.recent_type,
    item.playlist_id,
    item.album_id,
    item.local_album_id,
    item.qobuz_album_id,
    item.id,
    item.title,
    item.album_artist,
    item.artist,
    item.art_id,
    item.image_url,
    item.cover_url,
    item.is_qobuz
  ]
    .map((value) => String(value ?? ''))
    .join('|');
}

function sameRecentShelf(current: JsonRecord[], next: JsonRecord[]) {
  return (
    current.length === next.length &&
    current.every(
      (item, index) => recentItemPresentationKey(item) === recentItemPresentationKey(next[index])
    )
  );
}

export function useRecentlyPlayedData() {
  const [recentAlbums, setRecentAlbums] = useState<JsonRecord[]>([]);
  const [recentPlaylists, setRecentPlaylists] = useState<JsonRecord[]>([]);
  const [recentlyPlayedLoading, setRecentlyPlayedLoading] = useState(true);
  const requestIdRef = useRef(0);
  const hasCompletedInitialLoadRef = useRef(false);

  const refreshRecentlyPlayed = useCallback(async () => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    if (!hasCompletedInitialLoadRef.current) setRecentlyPlayedLoading(true);
    try {
      const { recentAlbums: nextRecentAlbums, recentPlaylists: nextRecentPlaylists } =
        await loadRecentlyPlayedShelves(50);
      if (requestIdRef.current !== requestId) return;
      if (nextRecentAlbums) {
        setRecentAlbums((current) =>
          sameRecentShelf(current, nextRecentAlbums) ? current : nextRecentAlbums
        );
      }
      if (nextRecentPlaylists) {
        setRecentPlaylists((current) =>
          sameRecentShelf(current, nextRecentPlaylists) ? current : nextRecentPlaylists
        );
      }
    } finally {
      if (requestIdRef.current === requestId) {
        hasCompletedInitialLoadRef.current = true;
        setRecentlyPlayedLoading(false);
      }
    }
  }, []);

  return {
    recentAlbums,
    recentPlaylists,
    recentlyPlayedLoading,
    refreshRecentlyPlayed
  };
}
