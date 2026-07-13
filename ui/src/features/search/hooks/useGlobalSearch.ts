import { type SetStateAction, useCallback, useEffect, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  emptyGlobalSearchBucket,
  type GlobalSearchState,
  initialGlobalSearchState,
  safeArray
} from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum, LibraryTrack, QobuzTrack } from '../../../shared/types';

const MAX_RECENT_SEARCHES = 5;

function normalizeRecentSearches(searches: unknown) {
  if (!Array.isArray(searches)) return [];
  const normalized: string[] = [];
  searches.forEach((search) => {
    const trimmed = String(search).trim();
    if (!trimmed) return;
    if (normalized.some((item) => item.toLocaleLowerCase() === trimmed.toLocaleLowerCase())) return;
    normalized.push(trimmed);
  });
  return normalized.slice(0, MAX_RECENT_SEARCHES);
}

export function useGlobalSearch(activeProfileId: string, profiles: JsonRecord[]) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<GlobalSearchState>(() => initialGlobalSearchState());
  const [recentSearches, setRecentSearches] = useState<string[]>([]);
  const activeProfileIdRef = useRef(activeProfileId);

  useEffect(() => {
    activeProfileIdRef.current = activeProfileId;
  }, [activeProfileId]);

  const persistRecentSearches = useCallback((profileId: string, searches: string[]) => {
    if (!profileId) return;
    endpoints
      .updateProfileRecentSearches(profileId, searches)
      .then((response) => {
        if (activeProfileIdRef.current === profileId) {
          setRecentSearches(normalizeRecentSearches(response.searches));
        }
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    if (!activeProfileId) {
      setRecentSearches([]);
      return undefined;
    }
    const profile = profiles.find((item) => String(item.id || '') === activeProfileId);
    setRecentSearches(normalizeRecentSearches(profile?.recent_searches));

    let active = true;
    endpoints
      .profileRecentSearches(activeProfileId)
      .then((response) => {
        if (active && activeProfileIdRef.current === activeProfileId) {
          setRecentSearches(normalizeRecentSearches(response.searches));
        }
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [activeProfileId, profiles]);

  const rememberSearch = useCallback(
    (rawQuery: string) => {
      const trimmedQuery = rawQuery.trim();
      if (!trimmedQuery || !activeProfileId) return;
      setRecentSearches((current) => {
        const next = [
          trimmedQuery,
          ...current.filter((item) => item.toLocaleLowerCase() !== trimmedQuery.toLocaleLowerCase())
        ].slice(0, MAX_RECENT_SEARCHES);
        persistRecentSearches(activeProfileId, next);
        return next;
      });
    },
    [activeProfileId, persistRecentSearches]
  );

  const removeRecentSearch = useCallback(
    (rawQuery: string) => {
      if (!activeProfileId) return;
      setRecentSearches((current) => {
        const next = current.filter((item) => item !== rawQuery);
        persistRecentSearches(activeProfileId, next);
        return next;
      });
    },
    [activeProfileId, persistRecentSearches]
  );

  const setSearchOpen = useCallback((nextOpen: SetStateAction<boolean>) => {
    setOpen((currentOpen) => (typeof nextOpen === 'function' ? nextOpen(currentOpen) : nextOpen));
  }, []);

  useEffect(() => {
    document.body.classList.toggle('global-search-open', open);
    return () => document.body.classList.remove('global-search-open');
  }, [open]);

  useEffect(() => {
    if (!open) return undefined;
    const trimmedQuery = query.trim();
    if (!trimmedQuery) {
      setResults(initialGlobalSearchState());
      return undefined;
    }

    const controller = new AbortController();
    let active = true;
    setResults((current) => ({
      ...current,
      localLoading: true,
      qobuzLoading: true,
      localError: null,
      qobuzError: null
    }));

    const timer = window.setTimeout(() => {
      endpoints
        .librarySearch(trimmedQuery, controller.signal)
        .then((nextResults) => {
          if (!active) return;
          setResults((current) => ({
            ...current,
            local: {
              songs: safeArray<LibraryTrack>(nextResults.tracks || nextResults.songs),
              albums: safeArray<LibraryAlbum>(nextResults.albums),
              artists: safeArray<JsonRecord>(nextResults.artists)
            }
          }));
        })
        .catch(() => {
          if (!active || controller.signal.aborted) return;
          setResults((current) => ({
            ...current,
            localError: 'Library search unavailable.',
            local: emptyGlobalSearchBucket()
          }));
        })
        .finally(() => {
          if (!active) return;
          setResults((current) => ({ ...current, localLoading: false }));
        });

      Promise.allSettled([
        endpoints.qobuzSearch(trimmedQuery, controller.signal),
        endpoints.qobuzAlbumSearch(trimmedQuery, controller.signal),
        endpoints.qobuzArtistSearch(trimmedQuery, 10, controller.signal)
      ])
        .then(([songs, albumsResult, artistsResult]) => {
          if (!active) return;
          const qobuzSongs =
            songs.status === 'fulfilled'
              ? Array.isArray(songs.value)
                ? songs.value
                : safeArray<LibraryTrack>((songs.value as JsonRecord).tracks)
              : [];
          const qobuzAlbums =
            albumsResult.status === 'fulfilled'
              ? Array.isArray(albumsResult.value)
                ? albumsResult.value
                : safeArray<LibraryAlbum>((albumsResult.value as JsonRecord).albums)
              : [];
          const qobuzArtists =
            artistsResult.status === 'fulfilled'
              ? safeArray<JsonRecord>(artistsResult.value.artists)
              : [];
          setResults((current) => ({
            ...current,
            qobuz: {
              songs: qobuzSongs as QobuzTrack[],
              albums: qobuzAlbums,
              artists: qobuzArtists
            },
            qobuzError: [songs, albumsResult, artistsResult].every(
              (result) => result.status === 'rejected'
            )
              ? 'Qobuz search unavailable.'
              : null
          }));
        })
        .finally(() => {
          if (!active) return;
          setResults((current) => ({ ...current, qobuzLoading: false }));
        });
    }, 180);

    return () => {
      active = false;
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [open, query]);

  return {
    open,
    recentSearches,
    rememberSearch,
    removeRecentSearch,
    setOpen: setSearchOpen,
    query,
    setQuery,
    results
  };
}
