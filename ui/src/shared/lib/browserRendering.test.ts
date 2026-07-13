import { describe, expect, it } from 'vitest';
import { isSafariUserAgent } from './browserRendering';

describe('Safari rendering detection', () => {
  it('recognises desktop Safari', () => {
    expect(
      isSafariUserAgent(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.5 Safari/605.1.15',
        'Apple Computer, Inc.'
      )
    ).toBe(true);
  });

  it('does not classify Chrome on macOS as Safari', () => {
    expect(
      isSafariUserAgent(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/137.0.0.0 Safari/537.36',
        'Google Inc.'
      )
    ).toBe(false);
  });

  it('does not classify Chrome on iOS as Safari', () => {
    expect(
      isSafariUserAgent(
        'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 CriOS/137.0 Mobile/15E148 Safari/604.1',
        'Apple Computer, Inc.'
      )
    ).toBe(false);
  });
});
