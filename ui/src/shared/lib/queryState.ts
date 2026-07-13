import type { ApiError } from './api';

export type QueryState<T> =
  | { status: 'idle' }
  | { status: 'loading'; previous?: T }
  | { status: 'success'; data: T; fetchedAt: number }
  | { status: 'error'; error: ApiError; previous?: T };

export function queryStateData<T>(state: QueryState<T>): T | undefined {
  if (state.status === 'success') return state.data;
  if (state.status === 'loading' || state.status === 'error') return state.previous;
  return undefined;
}

export function queryStateIsStale<T>(
  state: QueryState<T>,
  staleAfterMs: number,
  now = Date.now()
): boolean {
  if (state.status === 'error') return state.previous !== undefined;
  return state.status === 'success' && now - state.fetchedAt > staleAfterMs;
}
