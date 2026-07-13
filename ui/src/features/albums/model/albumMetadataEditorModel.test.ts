import { describe, expect, it } from 'vitest';
import type { LibraryAlbum } from '../../../shared/types';
import {
  albumEditorHasChanges,
  albumEditorInitialYear,
  albumEditorMetadataHasChanges,
  albumEditorYearPayload
} from './albumMetadataEditorModel';

const album: LibraryAlbum = {
  id: 1,
  title: 'Vespertine',
  album_artist: 'Björk',
  year: 2019
};

describe('albumMetadataEditorModel year editing', () => {
  it('normalizes the initial album year', () => {
    expect(albumEditorInitialYear(album)).toBe('2019');
    expect(albumEditorInitialYear({ ...album, year: null })).toBe('');
  });

  it('detects release year edits', () => {
    expect(albumEditorHasChanges(album, [], [], 'Vespertine', 'Björk', '2019', null)).toBe(false);
    expect(albumEditorHasChanges(album, [], [], 'Vespertine', 'Björk', '2001', null)).toBe(true);
  });

  it('serializes blank years as clear requests', () => {
    expect(albumEditorYearPayload('2001')).toBe(2001);
    expect(albumEditorYearPayload('')).toBeNull();
  });

  it('keeps Qobuz-only changes off the metadata update path', () => {
    expect(albumEditorMetadataHasChanges(album, [], [], 'Vespertine', 'Björk', '2019')).toBe(false);
    expect(albumEditorHasChanges(album, [], [], 'Vespertine', 'Björk', '2019', 'qobuz-1')).toBe(
      true
    );
  });
});
