import { describe, expect, it } from 'vitest';
import { albumListenCount } from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryTrack } from '../../../shared/types';
import { appleMusicAlbumToLibraryDetail, appleMusicSourceFromAlbumTrack } from './appleMusicAlbum';

describe('Apple Music album adapter', () => {
  it('maps catalog albums and tracks into the shared album detail contract', () => {
    const detail = appleMusicAlbumToLibraryDetail({
      album_id: '1440880938',
      storefront: 'nz',
      title: 'Homogenic',
      artist: 'Björk',
      release_date: '1997-09-22T00:00:00Z',
      artwork_url: 'https://example.test/homogenic.jpg',
      editorial_notes_standard: 'Apple Music editorial notes for Homogenic.',
      audio_variants: ['lossless'],
      tracks: [
        {
          song_id: '1440880976',
          storefront: 'nz',
          title: 'Jóga',
          artist: 'Björk',
          duration_secs: 312,
          track_number: 2,
          disc_number: 1,
          audio_variants: ['lossless']
        }
      ]
    });

    expect(detail.quality_label).toBe('Lossless');
    expect(detail.album).toMatchObject({
      id: '1440880938',
      provider: 'apple_music',
      title: 'Homogenic',
      album_artist: 'Björk',
      year: 1997,
      track_count: 1,
      description: 'Apple Music editorial notes for Homogenic.'
    });
    expect(detail.versions).toEqual([
      expect.objectContaining({
        provider: 'apple_music',
        source_label: 'Apple Music',
        format: 'Apple Music',
        is_primary: true
      })
    ]);

    const track = (detail.tracks as LibraryTrack[])[0];
    expect(track).toMatchObject({
      song_id: '1440880976',
      title: 'Jóga',
      album: 'Homogenic',
      album_id: '1440880938',
      image_url: 'https://example.test/homogenic.jpg',
      format: 'Apple Music'
    });
    expect(appleMusicSourceFromAlbumTrack(track)).toMatchObject({
      kind: 'apple_music_track',
      song_id: '1440880976',
      storefront: 'nz',
      album: 'Homogenic',
      album_id: '1440880938'
    });
  });

  it('carries the play history the server attaches to catalog tracks', () => {
    const detail = appleMusicAlbumToLibraryDetail({
      album_id: '1440857780',
      storefront: 'nz',
      title: 'Post',
      artist: 'Björk',
      tracks: [
        {
          song_id: '1440857781',
          storefront: 'nz',
          title: 'Hyperballad',
          artist: 'Björk',
          play_count: 4,
          last_played_at: 1785126167,
          listened_secs: 1200
        },
        {
          song_id: '1440857782',
          storefront: 'nz',
          title: 'The Modern Things',
          artist: 'Björk',
          play_count: 0,
          last_played_at: null,
          listened_secs: 0
        }
      ]
    });

    const tracks = detail.tracks as LibraryTrack[];
    expect(albumListenCount(tracks[0] as JsonRecord)).toBe(4);
    expect(tracks[0]).toMatchObject({ last_played_at: 1785126167, listened_secs: 1200 });
    expect(albumListenCount(tracks[1] as JsonRecord)).toBe(0);
  });

  it('drops catalog tracks that do not have a playable song id', () => {
    const detail = appleMusicAlbumToLibraryDetail({
      album_id: 'album-1',
      storefront: 'nz',
      title: 'Test Album',
      artist: 'Test Artist',
      tracks: [{ title: 'Unavailable item' }]
    } as JsonRecord);

    expect(detail.tracks).toEqual([]);
    expect(detail.quality_label).toBe('Apple Music');
  });

  it('groups all advertised Apple quality variants behind one Apple Music version', () => {
    const detail = appleMusicAlbumToLibraryDetail({
      album_id: 'album-1',
      storefront: 'nz',
      title: 'Test Album',
      artist: 'Test Artist',
      // MusicKit album payloads have historically included the enum's leading
      // dot while track payloads use the bare protocol value.
      audio_variants: ['.highResolutionLossless'],
      tracks: []
    });

    expect(detail.quality_label).toBe('Hi-Res Lossless');
    expect(detail.versions).toHaveLength(1);
    expect(detail.versions).toEqual([
      expect.objectContaining({
        provider: 'apple_music',
        source_label: 'Apple Music',
        format: 'Apple Music'
      })
    ]);
  });

  it('reads the hi-res tier from the current MusicKit spelling too', () => {
    const detail = appleMusicAlbumToLibraryDetail({
      album_id: 'album-1',
      storefront: 'nz',
      title: 'Test Album',
      artist: 'Test Artist',
      audio_variants: ['lossless'],
      // `hiResLossless` is MusicKit's own enum case.
      tracks: [{ song_id: 'song-1', title: 'Track', audio_variants: ['hiResLossless'] }]
    });

    expect(detail.quality_label).toBe('Hi-Res Lossless');
  });

  it('reports the verified playback format instead of the advertised tier', () => {
    const detail = appleMusicAlbumToLibraryDetail({
      album_id: 'album-1',
      storefront: 'nz',
      title: 'Test Album',
      artist: 'Test Artist',
      audio_variants: ['.highResolutionLossless'],
      verified_format: { codec: 'ALAC', sample_rate: 96000, bit_depth: 24 },
      tracks: []
    });

    expect(detail.quality_label).toBe('ALAC 96.0kHz 24bit');
    expect(detail.versions).toEqual([
      expect.objectContaining({
        provider: 'apple_music',
        source_label: 'Apple Music',
        format: 'ALAC',
        sample_rate: 96000,
        bit_depth: 24
      })
    ]);
  });
});
