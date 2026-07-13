import { useCallback, useRef, useState } from 'react';
import { type QueryState, queryStateData, queryStateIsStale } from '../../../shared/lib/queryState';
import type { JsonRecord, LibraryAlbum, LibraryTrack } from '../../../shared/types';
import {
  type LibraryCollections,
  LibraryRefreshError,
  loadLibraryCollections
} from '../model/libraryData';

const LIBRARY_STALE_AFTER_MS = 60_000;

export function useLibraryData() {
  const [albums, setAlbums] = useState<LibraryAlbum[]>([]);
  const [tracks, setTracks] = useState<LibraryTrack[]>([]);
  const [artists, setArtists] = useState<JsonRecord[]>([]);
  const [queryState, setQueryState] = useState<QueryState<LibraryCollections>>({ status: 'idle' });
  const queryStateRef = useRef(queryState);

  const updateQueryState = useCallback((next: QueryState<LibraryCollections>) => {
    queryStateRef.current = next;
    setQueryState(next);
  }, []);

  const refreshLibraryData = useCallback(async () => {
    const previous = queryStateData(queryStateRef.current);
    updateQueryState({ status: 'loading', previous });
    try {
      const collections = await loadLibraryCollections();
      setAlbums(collections.albums);
      setTracks(collections.tracks);
      setArtists(collections.artists);
      updateQueryState({ status: 'success', data: collections, fetchedAt: Date.now() });
    } catch (error) {
      if (!(error instanceof LibraryRefreshError)) throw error;
      const merged: LibraryCollections = {
        albums: error.partial.albums ?? previous?.albums ?? [],
        tracks: error.partial.tracks ?? previous?.tracks ?? [],
        artists: error.partial.artists ?? previous?.artists ?? []
      };
      setAlbums(merged.albums);
      setTracks(merged.tracks);
      setArtists(merged.artists);
      updateQueryState({ status: 'error', error, previous: merged });
    }
  }, [updateQueryState]);

  return {
    albums,
    artists,
    libraryIsStale: queryStateIsStale(queryState, LIBRARY_STALE_AFTER_MS),
    libraryQueryState: queryState,
    refreshLibraryData,
    tracks
  };
}
