import { describe, expect, it } from 'vitest';
import { mobileTabForRoute } from './navigation';

describe('mobileTabForRoute', () => {
  it('uses search whenever the search overlay is open', () => {
    expect(mobileTabForRoute({ view: 'album', id: 12 }, true)).toBe('search');
  });

  it('keeps discovery pages under Discover', () => {
    expect(mobileTabForRoute({ view: 'discover' }, false)).toBe('discover');
    expect(mobileTabForRoute({ view: 'qobuz-playlist', id: 'abc' }, false)).toBe('discover');
  });

  it('keeps library-adjacent pages under Library', () => {
    expect(mobileTabForRoute({ view: 'library' }, false)).toBe('library');
    expect(mobileTabForRoute({ view: 'history' }, false)).toBe('library');
    expect(mobileTabForRoute({ view: 'playlists' }, false)).toBe('library');
    expect(mobileTabForRoute({ view: 'artist', id: 'Sade' }, false)).toBe('library');
    expect(mobileTabForRoute({ view: 'qobuz-album', id: '123' }, false)).toBe('library');
  });
});
