import { ApiError, asApiError, endpoints } from '../../../shared/lib/api';
import type { JsonRecord, LibraryAlbum, LibraryTrack } from '../../../shared/types';

export type LibraryCollections = {
  albums: LibraryAlbum[];
  tracks: LibraryTrack[];
  artists: JsonRecord[];
};

export class LibraryRefreshError extends ApiError {
  partial: Partial<LibraryCollections>;
  failures: ApiError[];

  constructor(partial: Partial<LibraryCollections>, failures: ApiError[]) {
    super(
      failures[0]?.status ?? 0,
      failures.length === 1
        ? failures[0].message
        : `${failures.length} library requests failed: ${failures.map((error) => error.message).join('; ')}`,
      failures.some((error) => error.category === 'retryable_network')
        ? 'retryable_network'
        : (failures[0]?.category ?? 'internal')
    );
    this.name = 'LibraryRefreshError';
    this.partial = partial;
    this.failures = failures;
  }
}

export async function loadLibraryCollections(): Promise<LibraryCollections> {
  const [albumsResult, tracksResult, artistsResult] = await Promise.allSettled([
    endpoints.albums(),
    endpoints.tracks(),
    endpoints.artists()
  ]);

  const failures: ApiError[] = [];
  const partial: Partial<LibraryCollections> = {
    ...(albumsResult.status === 'fulfilled' ? { albums: albumsResult.value } : {}),
    ...(tracksResult.status === 'fulfilled' ? { tracks: tracksResult.value } : {}),
    ...(artistsResult.status === 'fulfilled' ? { artists: artistsResult.value } : {})
  };
  for (const result of [albumsResult, tracksResult, artistsResult]) {
    if (result.status === 'rejected') failures.push(asApiError(result.reason));
  }

  if (failures.length > 0) throw new LibraryRefreshError(partial, failures);
  return partial as LibraryCollections;
}
