import { describe, expect, it } from 'vitest';
import { routeFromHash, routeToHash } from './route';

describe('route hash helpers', () => {
  it('serializes and parses the mobile library route', () => {
    expect(routeToHash({ view: 'library' })).toBe('#/library');
    expect(routeFromHash('#/library')).toEqual({ view: 'library', id: null });
  });

  it('maps legacy library hash routes', () => {
    expect(routeFromHash('#/library-view')).toEqual({ view: 'library', id: null });
  });

  it('round-trips a routed Apple Music album without changing local album URLs', () => {
    expect(routeToHash({ view: 'album', id: 866 })).toBe('#/album/866');
    const route = {
      view: 'album',
      id: '1109714933',
      provider: 'apple_music',
      storefront: 'nz'
    } as const;
    expect(routeToHash(route)).toBe('#/album/1109714933?provider=apple_music&storefront=nz');
    expect(routeFromHash('#/album/1109714933?provider=apple_music&storefront=nz')).toEqual(route);
  });
});
