import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { JsonRecord, LibraryAlbum } from '../../../shared/types';

const apiMock = vi.hoisted(() => ({
  endpoints: {
    albumByQobuzId: vi.fn(),
    qobuzAlbum: vi.fn(),
    qobuzArtistCore: vi.fn(),
    qobuzTrack: vi.fn()
  }
}));

vi.mock('../../../shared/lib/api', () => apiMock);

import { loadQobuzAlbumDetail } from './qobuzData';

function qobuzAlbum(id: string, overrides: Partial<LibraryAlbum> = {}) {
  return {
    id,
    title: 'New Moon Daughter',
    artist: 'Cassandra Wilson',
    artist_id: 42,
    year: 1995,
    tracks_count: 12,
    maximum_sampling_rate: 44.1,
    maximum_bit_depth: 16,
    hires: false,
    ...overrides
  } as LibraryAlbum;
}

function qobuzDetail(album: LibraryAlbum) {
  return {
    album,
    tracks: Array.from({ length: Number(album.tracks_count || 12) }, (_, index) => ({
      id: Number(`${String(album.id).replace(/\D/g, '').slice(-4) || 10}${index + 1}`),
      title: `Track ${index + 1}`,
      artist: album.artist,
      album: album.title,
      album_id: album.id,
      duration: 180,
      track_number: index + 1,
      disc_number: 1,
      maximum_sampling_rate: album.maximum_sampling_rate,
      maximum_bit_depth: album.maximum_bit_depth
    }))
  } as JsonRecord;
}

describe('loadQobuzAlbumDetail versions', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('uses sibling Qobuz ids to merge a linked local album and canonical Qobuz tiers', async () => {
    const opened1995 = qobuzAlbum('nmd-1995');
    const linked2013 = qobuzAlbum('nmd-2013', {
      year: 2013,
      maximum_sampling_rate: 192,
      maximum_bit_depth: 24,
      hires: true
    });
    const alternate1995 = qobuzAlbum('nmd-1995-alt', { tracks_count: 13 });
    const linkedLocalDetail = {
      album: {
        id: 7,
        title: 'New Moon Daughter',
        album_artist: 'Cassandra Wilson',
        qobuz_album_id: 'nmd-2013'
      },
      versions: [
        {
          id: 11,
          provider: 'local',
          provider_id: 'local:/music/new-moon-daughter',
          source_label: 'Library 16/44.1',
          title: 'New Moon Daughter',
          artist: 'Cassandra Wilson',
          track_count: 12,
          sample_rate: 44100,
          bit_depth: 16,
          format: 'FLAC'
        }
      ]
    } as JsonRecord;

    apiMock.endpoints.qobuzAlbum.mockImplementation((id: string | number) => {
      if (id === 'nmd-1995') return Promise.resolve(qobuzDetail(opened1995));
      if (id === 'nmd-2013') return Promise.resolve(qobuzDetail(linked2013));
      return Promise.reject(new Error(`unexpected qobuz album ${id}`));
    });
    apiMock.endpoints.qobuzArtistCore.mockResolvedValue({
      albums: [linked2013, opened1995, alternate1995]
    });
    apiMock.endpoints.albumByQobuzId.mockImplementation((id: string | number) => {
      if (id === 'nmd-2013') return Promise.resolve(linkedLocalDetail);
      return Promise.reject(new Error('not linked'));
    });

    const result = await loadQobuzAlbumDetail('nmd-1995');
    const detail = result.detail as JsonRecord;
    const versions = detail.versions as JsonRecord[];
    const versionIds = versions.map((version) => String(version.id));

    expect(result.kind).toBe('qobuz');
    expect((detail.linked_album as JsonRecord).id).toBe(7);
    expect(versionIds).toContain('11');
    expect(versionIds).toContain('qobuz:cd:nmd-1995');
    expect(versionIds).toContain('qobuz:hires:nmd-2013');
    expect(versionIds).toContain('qobuz:cd:nmd-2013');
    expect(versionIds).toContain('qobuz:album:nmd-1995-alt');
    expect(versionIds).not.toContain('qobuz:album:nmd-2013');
    expect(apiMock.endpoints.albumByQobuzId).toHaveBeenCalledWith('nmd-2013');
  });
});
