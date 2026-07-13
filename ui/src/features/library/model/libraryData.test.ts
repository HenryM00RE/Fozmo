import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, endpoints } from '../../../shared/lib/api';
import { LibraryRefreshError, loadLibraryCollections } from './libraryData';

describe('loadLibraryCollections', () => {
  afterEach(() => vi.restoreAllMocks());

  it('returns all collections when every generated endpoint succeeds', async () => {
    vi.spyOn(endpoints, 'albums').mockResolvedValue([]);
    vi.spyOn(endpoints, 'tracks').mockResolvedValue([]);
    vi.spyOn(endpoints, 'artists').mockResolvedValue([]);

    await expect(loadLibraryCollections()).resolves.toEqual({
      albums: [],
      tracks: [],
      artists: []
    });
  });

  it('preserves successful collections and exposes failed request structure', async () => {
    vi.spyOn(endpoints, 'albums').mockResolvedValue([]);
    vi.spyOn(endpoints, 'tracks').mockRejectedValue(
      new ApiError(0, 'library refresh network failed')
    );
    vi.spyOn(endpoints, 'artists').mockResolvedValue([]);

    const error = await loadLibraryCollections().catch((reason: unknown) => reason);
    expect(error).toBeInstanceOf(LibraryRefreshError);
    expect(error).toMatchObject({
      category: 'retryable_network',
      partial: { albums: [], artists: [] }
    });
    expect((error as LibraryRefreshError).failures).toHaveLength(1);
  });
});
