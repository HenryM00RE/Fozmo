import { describe, expect, it } from 'vitest';
import { normalizeRecentSearches } from './useGlobalSearch';

describe('normalizeRecentSearches', () => {
  it('keeps the ten newest unique searches', () => {
    const searches = [
      '  First  ',
      'SECOND',
      'first',
      'Third',
      'Fourth',
      'Fifth',
      'Sixth',
      'Seventh',
      'Eighth',
      'Ninth',
      'Tenth',
      'Eleventh'
    ];

    expect(normalizeRecentSearches(searches)).toEqual([
      'First',
      'SECOND',
      'Third',
      'Fourth',
      'Fifth',
      'Sixth',
      'Seventh',
      'Eighth',
      'Ninth',
      'Tenth'
    ]);
  });
});
