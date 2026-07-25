// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { PLAYBACK_CLIENT_HEADER, PLAYBACK_SEQUENCE_HEADER } from '../identity';
import { requestHeaders } from './requestContext';

describe('request playback sequencing', () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear()
    });
  });

  it('marks Apple Music play requests as ordered playback intents', () => {
    const first = requestHeaders('/api/apple-music/play', {});
    const second = requestHeaders('/api/apple-music/play', {});

    expect(first[PLAYBACK_CLIENT_HEADER]).toBeTruthy();
    expect(first[PLAYBACK_SEQUENCE_HEADER]).toBe('1');
    expect(second[PLAYBACK_CLIENT_HEADER]).toBe(first[PLAYBACK_CLIENT_HEADER]);
    expect(second[PLAYBACK_SEQUENCE_HEADER]).toBe('2');
  });
});
