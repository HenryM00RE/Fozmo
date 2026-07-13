import { describe, expect, it } from 'vitest';
import { albumArtworkForViewingVersion } from './albumModel';

describe('album artwork for a selected version', () => {
  it('uses the selected Qobuz version cover', () => {
    expect(
      albumArtworkForViewingVersion(
        { image_url: 'https://example.test/local.jpg' },
        { provider: 'qobuz', image_url: 'https://static.qobuz.com/qobuz.jpg' }
      )
    ).toBe('https://static.qobuz.com/qobuz.jpg');
  });

  it('keeps album artwork for a local version', () => {
    expect(
      albumArtworkForViewingVersion(
        { image_url: 'https://example.test/local.jpg' },
        { provider: 'local', image_url: 'https://example.test/version.jpg' }
      )
    ).toBe('https://example.test/local.jpg');
  });
});
