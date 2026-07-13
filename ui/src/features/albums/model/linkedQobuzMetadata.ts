import type { JsonRecord, LibraryTrack } from '../../../shared/types';

function records(value: unknown): JsonRecord[] {
  return Array.isArray(value) ? (value as JsonRecord[]) : [];
}

function idValue(value: unknown) {
  if (value === null || value === undefined) return '';
  return String(value);
}

function nonEmptyText(value: unknown) {
  return typeof value === 'string' && value.trim() ? value : undefined;
}

export function localTracksWithLinkedQobuzMetadata(
  detail: JsonRecord | null | undefined,
  tracks: LibraryTrack[]
) {
  const canonicalTracks = records(detail?.canonical_tracks);
  const links = records(detail?.qobuz_track_links).filter(
    (link) => String(link.status || '').toLowerCase() === 'linked' && Number(link.confidence) >= 80
  );
  if (!tracks.length || !canonicalTracks.length || !links.length) return tracks;

  const canonicalByQobuzId = new Map(
    canonicalTracks
      .map((track) => [idValue(track.qobuz_track_id), track] as const)
      .filter(([trackId]) => trackId !== '')
  );
  const qobuzIdByLocalId = new Map(
    links
      .map((link) => [idValue(link.local_track_id), idValue(link.qobuz_track_id)] as const)
      .filter(([localId, qobuzId]) => localId !== '' && qobuzId !== '')
  );

  let changed = false;
  const next = tracks.map((track) => {
    const canonical = canonicalByQobuzId.get(qobuzIdByLocalId.get(idValue(track.id)) || '');
    if (!canonical) return track;

    const qobuzCredits = records(canonical.credits);
    changed = true;
    return {
      ...track,
      play_count: Math.max(Number(track.play_count) || 0, Number(canonical.play_count) || 0),
      listened_secs: Math.max(
        Number(track.listened_secs) || 0,
        Number(canonical.listened_secs) || 0
      ),
      last_played_at:
        Math.max(Number(track.last_played_at) || 0, Number(canonical.last_played_at) || 0) ||
        track.last_played_at,
      credits: qobuzCredits.length ? qobuzCredits : track.credits,
      composer: nonEmptyText(canonical.composer) ?? track.composer,
      work: nonEmptyText(canonical.work) ?? track.work,
      isrc: nonEmptyText(canonical.isrc) ?? track.isrc,
      copyright: nonEmptyText(canonical.copyright) ?? track.copyright,
      performers_raw: nonEmptyText(canonical.performers_raw) ?? track.performers_raw
    };
  });
  return changed ? next : tracks;
}
