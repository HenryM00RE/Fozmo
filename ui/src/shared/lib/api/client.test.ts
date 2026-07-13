import { describe, expect, it } from 'vitest';
import { createApiClient, endpointContracts } from '../../generated/api-endpoints';
import { ApiError, asApiError, playbackSequenceClientForPath } from './client';

describe('playback request sequencing lanes', () => {
  it('does not let Qobuz prefetch supersede an active play request', () => {
    expect(playbackSequenceClientForPath('browser-a', '/api/zones/kef/qobuz/play')).toBe(
      'browser-a'
    );
    expect(playbackSequenceClientForPath('browser-a', '/api/zones/kef/qobuz/prefetch')).toBe(
      'browser-a:prefetch'
    );
    expect(playbackSequenceClientForPath('browser-a', '/api/qobuz/prefetch?next=1')).toBe(
      'browser-a:prefetch'
    );
  });
});

describe('API error categories', () => {
  it('keeps retryable transport failures distinct from HTTP validation failures', () => {
    expect(asApiError(new TypeError('fetch failed'))).toMatchObject({
      status: 0,
      category: 'retryable_network'
    });
    expect(new ApiError(413, 'too large').category).toBe('validation');
    expect(new ApiError(409, 'changed').category).toBe('conflict');
  });
});

describe('generated API client coverage', () => {
  it('emits one callable helper for every registered endpoint contract', () => {
    const client = createApiClient({ request: async () => undefined as never });
    expect(Object.keys(client)).toHaveLength(endpointContracts.length);
    expect(Object.values(client).every((helper) => typeof helper === 'function')).toBe(true);
  });
});
