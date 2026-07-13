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
});
