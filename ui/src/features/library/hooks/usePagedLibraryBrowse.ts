import { useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryBrowseKind,
  LibraryBrowsePage,
  LibraryBrowseParams,
  LibraryTrack
} from '../../../shared/types';

type BrowseItemFor<K extends LibraryBrowseKind> = K extends 'albums'
  ? LibraryAlbum
  : K extends 'tracks'
    ? LibraryTrack
    : JsonRecord;

const browseDelayMs = 180;

export function usePagedLibraryBrowse<K extends LibraryBrowseKind>(
  kind: K,
  params: LibraryBrowseParams
) {
  type Item = BrowseItemFor<K>;
  const [page, setPage] = useState<LibraryBrowsePage<Item>>(() => emptyPage<Item>(params));
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const queryKey = useMemo(() => JSON.stringify(params), [params]);

  useEffect(() => {
    const controller = new AbortController();
    let active = true;
    setLoading(true);
    setError(null);

    const timer = window.setTimeout(() => {
      browseRequest<Item>(kind, params, controller.signal)
        .then((nextPage) => {
          if (!active) return;
          setPage(normalizePage(nextPage, params));
          setLoaded(true);
        })
        .catch((err) => {
          if (!active || controller.signal.aborted) return;
          setError(err instanceof Error ? err.message : 'Library browse unavailable.');
          setLoaded(true);
          setPage((current) => normalizePage(current, params));
        })
        .finally(() => {
          if (active) setLoading(false);
        });
    }, browseDelayMs);

    return () => {
      active = false;
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [kind, params, queryKey]);

  return {
    error,
    loaded,
    loading,
    page
  };
}

function browseRequest<T>(
  kind: LibraryBrowseKind,
  params: LibraryBrowseParams,
  signal: AbortSignal
): Promise<LibraryBrowsePage<T>> {
  if (kind === 'albums') {
    return endpoints.browseAlbums(params, signal) as Promise<LibraryBrowsePage<T>>;
  }
  if (kind === 'tracks') {
    return endpoints.browseTracks(params, signal) as Promise<LibraryBrowsePage<T>>;
  }
  return endpoints.browseArtists(params, signal) as Promise<LibraryBrowsePage<T>>;
}

function emptyPage<T>(params: LibraryBrowseParams): LibraryBrowsePage<T> {
  return {
    items: [],
    total: 0,
    limit: params.limit || 0,
    offset: params.offset || 0,
    has_more: false,
    facets: {}
  };
}

function normalizePage<T>(
  page: LibraryBrowsePage<T>,
  params: LibraryBrowseParams
): LibraryBrowsePage<T> {
  return {
    ...page,
    items: Array.isArray(page.items) ? page.items : [],
    total: Number(page.total || 0),
    limit: Number(page.limit || params.limit || 0),
    offset: Number(page.offset || params.offset || 0),
    has_more: Boolean(page.has_more),
    facets: page.facets || {}
  };
}
