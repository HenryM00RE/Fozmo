import { describe, expect, it } from 'vitest';
import type { LibraryAlbum } from '../../../shared/types';
import { localAlbumForDiscographyAlbum } from './ArtistDetailPage';

describe('localAlbumForDiscographyAlbum', () => {
  it('prefers a local album linked to any Qobuz sibling version', () => {
    const localAlbum = {
      id: 7,
      title: 'Vespertine',
      album_artist: 'Bjork',
      qobuz_album_id: 'vespertine-hires'
    } as LibraryAlbum;
    const remoteAlbum = {
      id: 'vespertine-cd',
      title: 'Vespertine',
      artist: 'Bjork',
      qobuz_album_versions: [{ id: 'vespertine-hires' }]
    } as LibraryAlbum;

    expect(localAlbumForDiscographyAlbum(remoteAlbum, [localAlbum])).toBe(localAlbum);
  });

  it('falls back to the same discography album group when ids are not linked', () => {
    const localAlbum = {
      id: 42,
      title: "Tomorrow's Modern Boxes",
      album_artist: 'Thom Yorke'
    } as LibraryAlbum;
    const remoteAlbum = {
      id: 'qobuz-tmb',
      title: "Tomorrow's Modern Boxes",
      artist: 'Thom Yorke'
    } as LibraryAlbum;

    expect(localAlbumForDiscographyAlbum(remoteAlbum, [localAlbum])).toBe(localAlbum);
  });
});
