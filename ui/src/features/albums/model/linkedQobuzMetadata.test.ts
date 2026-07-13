import { describe, expect, it } from 'vitest';
import type { JsonRecord, LibraryTrack } from '../../../shared/types';
import { localTracksWithLinkedQobuzMetadata } from './linkedQobuzMetadata';

const localTrack: LibraryTrack = {
  id: 7,
  title: 'Clocks',
  composer: 'Incomplete local composer',
  credits: [{ name: 'Local Credit', roles: ['Performer'] }]
};

function detail(status = 'linked', confidence = 95): JsonRecord {
  return {
    qobuz_track_links: [{ local_track_id: 7, qobuz_track_id: '42', status, confidence }],
    canonical_tracks: [
      {
        qobuz_track_id: '42',
        composer: 'Chris Martin, Guy Berryman, Jonny Buckland, Will Champion',
        isrc: 'GBAYE0200771',
        copyright: '2002 Parlophone Records Ltd',
        credits: [
          { name: 'Chris Martin', roles: ['Composer', 'Lyricist', 'Piano'] },
          { name: 'Ken Nelson', roles: ['Producer'] }
        ]
      }
    ]
  };
}

describe('linked Qobuz track metadata', () => {
  it('uses Qobuz credits for a high-confidence linked local track', () => {
    const [track] = localTracksWithLinkedQobuzMetadata(detail(), [localTrack]);

    expect(track.composer).toContain('Chris Martin');
    expect(track.isrc).toBe('GBAYE0200771');
    expect(track.credits).toEqual([
      { name: 'Chris Martin', roles: ['Composer', 'Lyricist', 'Piano'] },
      { name: 'Ken Nelson', roles: ['Producer'] }
    ]);
  });

  it('does not use Qobuz credits for an unlinked or low-confidence track', () => {
    expect(localTracksWithLinkedQobuzMetadata(detail('unlinked'), [localTrack])[0]).toBe(
      localTrack
    );
    expect(localTracksWithLinkedQobuzMetadata(detail('linked', 79), [localTrack])[0]).toBe(
      localTrack
    );
  });

  it('keeps local credit fields when Qobuz has no value for them', () => {
    const emptyDetail = detail();
    emptyDetail.canonical_tracks = [{ qobuz_track_id: '42', credits: [] }];

    const [track] = localTracksWithLinkedQobuzMetadata(emptyDetail, [localTrack]);
    expect(track.composer).toBe('Incomplete local composer');
    expect(track.credits).toEqual(localTrack.credits);
  });
});
