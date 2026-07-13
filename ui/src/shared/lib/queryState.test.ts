import { describe, expect, it } from 'vitest';
import { ApiError } from './api';
import { type QueryState, queryStateData, queryStateIsStale } from './queryState';

describe('QueryState', () => {
  it('retains previous data while refreshing and after an error', () => {
    const previous = { value: 42 };
    expect(queryStateData({ status: 'loading', previous })).toEqual(previous);
    expect(
      queryStateData({ status: 'error', error: new ApiError(503, 'offline'), previous })
    ).toEqual(previous);
  });

  it('reports age-based and error-based staleness', () => {
    const success: QueryState<string> = { status: 'success', data: 'fresh', fetchedAt: 1_000 };
    expect(queryStateIsStale(success, 500, 1_499)).toBe(false);
    expect(queryStateIsStale(success, 500, 1_501)).toBe(true);
    expect(
      queryStateIsStale(
        { status: 'error', error: new ApiError(0, 'offline'), previous: 'cached' },
        500,
        1_000
      )
    ).toBe(true);
  });
});
