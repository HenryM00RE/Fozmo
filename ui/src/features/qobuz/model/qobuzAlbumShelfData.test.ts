import { describe, expect, it } from 'vitest';
import {
  isQobuzAlbumShelfCacheEntry,
  normalizeQobuzAlbumPageResponse,
  qobuzAlbumPageFromCollection,
  qobuzAlbumPageFromPreview,
  qobuzAlbumShelfCacheKey
} from './qobuzAlbumShelfData';

describe('qobuzAlbumShelfData', () => {
  it('canonicalizes album shelf category aliases in cache keys', () => {
    expect(qobuzAlbumShelfCacheKey('qobuzissims', 28, 12, 64)).toBe(
      qobuzAlbumShelfCacheKey('standouts', 28, 12, 64)
    );
    expect(qobuzAlbumShelfCacheKey('new-releases', 12, 0, null)).toBe(
      qobuzAlbumShelfCacheKey('new', 12, 0, null)
    );
  });

  it('treats response arrays as already paged album responses', () => {
    const response = normalizeQobuzAlbumPageResponse(
      [{ id: 'album-13', title: 'Thirteen' }],
      'standouts',
      12,
      12
    );

    expect(response.albums.map((album) => album.id)).toEqual(['album-13']);
    expect(response.offset).toBe(12);
    expect(response.count).toBe(1);
  });

  it('slices local fallback collections separately from API responses', () => {
    const response = qobuzAlbumPageFromCollection(
      [{ id: 'album-1' }, { id: 'album-2' }, { id: 'album-3' }],
      2,
      2
    );

    expect(response.albums.map((album) => album.id)).toEqual(['album-3']);
    expect(response.total).toBe(3);
    expect(response.has_more).toBe(false);
  });

  it('treats home preview pages as unknown-total shelves', () => {
    const fullPreview = qobuzAlbumPageFromPreview(
      Array.from({ length: 12 }, (_, index) => ({ id: `album-${index + 1}` })),
      12,
      0
    );
    const shortPreview = qobuzAlbumPageFromPreview(
      Array.from({ length: 11 }, (_, index) => ({ id: `album-${index + 1}` })),
      12,
      0
    );

    expect(fullPreview.total).toBeNull();
    expect(fullPreview.has_more).toBe(true);
    expect(shortPreview.total).toBeNull();
    expect(shortPreview.has_more).toBe(false);
  });

  it('selects the requested category from home-style section wrappers', () => {
    const response = normalizeQobuzAlbumPageResponse(
      {
        sections: [
          { id: 'new-releases', albums: [{ id: 'new-1' }] },
          { id: 'qobuzissims', albums: [{ id: 'standout-1' }], limit: 12, offset: 0, total: 25 }
        ]
      },
      'standouts',
      12,
      0
    );

    expect(response.albums.map((album) => album.id)).toEqual(['standout-1']);
    expect(response.total).toBe(25);
    expect(response.has_more).toBe(true);
  });

  it('drops suspicious exact totals from standout responses', () => {
    const response = normalizeQobuzAlbumPageResponse(
      {
        albums: Array.from({ length: 12 }, (_, index) => ({ id: `standout-${index + 1}` })),
        limit: 12,
        offset: 0,
        count: 12,
        total: 12,
        has_more: false
      },
      'standouts',
      12,
      0
    );

    expect(response.total).toBeNull();
    expect(response.has_more).toBe(true);
  });

  it('honors explicit has_more false even when a page is full', () => {
    const response = normalizeQobuzAlbumPageResponse(
      {
        albums: [{ id: 'album-1' }, { id: 'album-2' }],
        limit: 2,
        offset: 2,
        count: 2,
        has_more: false
      },
      'popular',
      2,
      2
    );

    expect(response.has_more).toBe(false);
  });

  it('validates cached album shelf entries strictly', () => {
    expect(
      isQobuzAlbumShelfCacheEntry({
        loadedAt: Date.now(),
        response: {
          albums: [{ id: 'album-1' }],
          limit: 12,
          offset: 0,
          count: 1,
          total: null,
          has_more: true
        }
      })
    ).toBe(true);

    expect(
      isQobuzAlbumShelfCacheEntry({
        loadedAt: Date.now(),
        response: {
          albums: [{ id: 'album-1' }],
          limit: 12,
          offset: 0,
          count: 1,
          total: null
        }
      })
    ).toBe(false);
  });
});
