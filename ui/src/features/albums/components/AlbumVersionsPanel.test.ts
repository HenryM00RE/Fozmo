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

  const appleVersion = (extra: JsonRecord = {}) =>
    ({
      id: 4,
      provider: 'apple_music',
      source_label: 'Apple Music',
      title: 'In Rainbows',
      artist: 'Radiohead',
      year: 2007,
      track_count: 10,
      is_primary: false,
      ...extra
    }) as JsonRecord;

  const renderVersions = (versions: JsonRecord[]) =>
    render(
      createElement(AlbumVersionsPanel, {
        versions,
        fallbackAlbum: null,
        fallbackTracks: [],
        viewingVersionId: 'another-version',
        onSetPrimary: vi.fn()
      })
    );

  it('names the source in the kicker and the advertised tier in the quality column', () => {
    renderVersions([appleVersion({ audio_variants: ['.highResolutionLossless'] })]);

    // The kicker says where the version came from, not how good it is.
    expect(screen.getByText('Apple Music')).toBeInTheDocument();
    expect(screen.getByText('Hi-Res Lossless')).toBeInTheDocument();
    expect(screen.queryByLabelText('Apple Music version options')).not.toBeInTheDocument();
    expect(
      screen.queryByText('Authorize Apple Music before using the catalog.')
    ).not.toBeInTheDocument();
  });

  it('replaces the advertised tier once playback has verified the real format', () => {
    renderVersions([
      appleVersion({
        audio_variants: ['.highResolutionLossless'],
        format: 'ALAC',
        sample_rate: 96_000,
        bit_depth: 24
      })
    ]);

    expect(screen.getByText('ALAC 96.0kHz 24bit')).toBeInTheDocument();
    expect(screen.queryByText('Hi-Res Lossless')).not.toBeInTheDocument();
    // The kicker still names the source.
    expect(screen.getByText('Apple Music')).toBeInTheDocument();
  });
});
