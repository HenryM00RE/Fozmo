// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { createElement } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { JsonRecord } from '../../../shared/types';
import { AlbumVersionsPanel, albumVersionHierarchyRank } from './AlbumVersionsPanel';

afterEach(cleanup);

describe('album version hierarchy', () => {
  it('orders grouped Apple Music after local and Qobuz versions', () => {
    const versions = [
      { provider: 'apple_music' },
      { provider: 'local', sample_rate: 44_100, bit_depth: 16 },
      { provider: 'qobuz', sample_rate: 192_000, bit_depth: 24 },
      { provider: 'local', sample_rate: 96_000, bit_depth: 24 },
      { provider: 'qobuz', sample_rate: 44_100, bit_depth: 16 }
    ] as JsonRecord[];

    expect(
      versions.sort(
        (left, right) => albumVersionHierarchyRank(left) - albumVersionHierarchyRank(right)
      )
    ).toEqual([
      expect.objectContaining({ provider: 'local', sample_rate: 96_000 }),
      expect.objectContaining({ provider: 'qobuz', sample_rate: 192_000 }),
      expect.objectContaining({ provider: 'local', sample_rate: 44_100 }),
      expect.objectContaining({ provider: 'qobuz', sample_rate: 44_100 }),
      expect.objectContaining({ provider: 'apple_music' })
    ]);
  });

  it('shows one Lossless row without Apple management controls or authorization banners', () => {
    render(
      createElement(AlbumVersionsPanel, {
        versions: [
          {
            id: 4,
            provider: 'apple_music',
            source_label: 'Apple Music',
            title: 'In Rainbows',
            artist: 'Radiohead',
            year: 2007,
            track_count: 10,
            is_primary: false
          }
        ],
        fallbackAlbum: null,
        fallbackTracks: [],
        viewingVersionId: 'another-version',
        onSetPrimary: vi.fn()
      })
    );

    expect(screen.getAllByText('Lossless')).toHaveLength(2);
    expect(screen.queryByLabelText('Apple Music version options')).not.toBeInTheDocument();
    expect(
      screen.queryByText('Authorize Apple Music before using the catalog.')
    ).not.toBeInTheDocument();
  });
});
