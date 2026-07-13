import { endpoints } from '../../../shared/lib/api';
import {
  discographyAlbumGroupKey,
  idValue,
  normalizeQobuzAlbumId,
  qobuzAlbumToLibraryShape,
  safeArray
} from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum } from '../../../shared/types';

function settledValue<T>(result: PromiseSettledResult<T>) {
  return result.status === 'fulfilled' ? result.value : undefined;
}

export type QobuzOverviewData = {
  qobuzStatus?: JsonRecord;
  qobuzHome?: JsonRecord;
};

export type QobuzAlbumDetailResult = {
  detail: JsonRecord | null;
  kind: 'local' | 'qobuz';
};

type CachedPromise<T> = {
  loadedAt: number;
  promise: Promise<T>;
};

const QOBUZ_ALBUM_DETAIL_CACHE_MS = 5 * 60_000;
const QOBUZ_ARTIST_CORE_CACHE_MS = 5 * 60_000;

const qobuzAlbumDetailCache = new Map<string, CachedPromise<JsonRecord>>();
const qobuzArtistCoreCache = new Map<string, CachedPromise<JsonRecord>>();
const qobuzTrackAlbumIdCache = new Map<string, Promise<string>>();

export function loadQobuzHomeAlbumOfTheWeek() {
  return endpoints.qobuzHomeAlbumOfTheWeek();
}

async function resolveQobuzAlbumRouteId(id: string | number) {
  const albumId = normalizeQobuzAlbumId(id);
  if (albumId.startsWith('qobuz:track:')) {
    const trackId = albumId.replace('qobuz:track:', '');
    const cached = qobuzTrackAlbumIdCache.get(trackId);
    if (cached) return cached;
    const promise = endpoints
      .qobuzTrack(trackId)
      .then((track) => normalizeQobuzAlbumId(track.album_id || ''))
      .catch((error) => {
        qobuzTrackAlbumIdCache.delete(trackId);
        throw error;
      });
    qobuzTrackAlbumIdCache.set(trackId, promise);
    return promise;
  }
  return albumId;
}

function isCachedPromiseFresh<T>(cached: CachedPromise<T> | undefined, ttlMs: number) {
  return Boolean(cached && Date.now() - cached.loadedAt < ttlMs);
}

function cachedQobuzAlbum(albumId: string | number) {
  const key = normalizeQobuzAlbumId(albumId) || String(albumId || '').trim();
  const cached = qobuzAlbumDetailCache.get(key);
  if (isCachedPromiseFresh(cached, QOBUZ_ALBUM_DETAIL_CACHE_MS)) {
    return cached!.promise;
  }
  const promise = endpoints.qobuzAlbum(key).catch((error) => {
    qobuzAlbumDetailCache.delete(key);
    throw error;
  });
  qobuzAlbumDetailCache.set(key, { loadedAt: Date.now(), promise });
  return promise;
}

function cachedQobuzArtistCore(artistId: string | number) {
  const key = String(artistId || '').trim();
  const cached = qobuzArtistCoreCache.get(key);
  if (isCachedPromiseFresh(cached, QOBUZ_ARTIST_CORE_CACHE_MS)) {
    return cached!.promise;
  }
  const promise = endpoints.qobuzArtistCore(key).catch((error) => {
    qobuzArtistCoreCache.delete(key);
    throw error;
  });
  qobuzArtistCoreCache.set(key, { loadedAt: Date.now(), promise });
  return promise;
}

export async function loadQobuzAlbumDetail(
  id: string | number,
  albumHint?: LibraryAlbum | null
): Promise<QobuzAlbumDetailResult> {
  let albumId = '';
  try {
    albumId = await resolveQobuzAlbumRouteId(id);
  } catch {
    albumId = normalizeQobuzAlbumId(id);
  }
  const linkedPromise = endpoints.albumByQobuzId(albumId || id).catch(() => null);

  try {
    const qobuzDetail = await cachedQobuzAlbum(albumId || id);
    const detailWithHint = await applyQobuzAlbumHint(qobuzDetail, albumHint);
    const linked = await linkedLibraryDetailForQobuzSet(detailWithHint, linkedPromise);
    const merged = await mergeLinkedLibraryDetail(
      qobuzAlbumToLibraryShape(detailWithHint),
      linked,
      detailWithHint
    );
    return {
      detail: merged,
      kind: 'qobuz'
    };
  } catch {
    // If the Qobuz catalog request is unavailable, a linked local album can
    // still render as a fallback. Qobuz routes prefer catalog detail so track
    // taps carry Qobuz ids and can resolve a fresh stream URL at play time.
  }

  try {
    const linked = await linkedPromise;
    if (linked?.album) return { detail: linked, kind: 'local' };
  } catch {
    return { detail: null, kind: 'qobuz' };
  }
  return { detail: null, kind: 'qobuz' };
}

async function linkedLibraryDetailForQobuzSet(
  qobuzDetail: JsonRecord,
  initialLinkedPromise: Promise<JsonRecord | null>
) {
  const initial = await initialLinkedPromise.catch(() => null);
  if (initial?.album) return initial;

  for (const candidateId of qobuzAlbumSetIds(qobuzDetail)) {
    const linked = await endpoints.albumByQobuzId(candidateId).catch(() => null);
    if (linked?.album) return linked;
  }
  return null;
}

function qobuzAlbumSetIds(detail: JsonRecord) {
  const album = (detail.album || detail) as JsonRecord;
  const ids = [
    normalizeQobuzAlbumId(album),
    ...safeArray<LibraryAlbum>(album.qobuz_album_versions || detail.qobuz_album_versions).map(
      (candidate) => normalizeQobuzAlbumId(candidate)
    )
  ];
  return Array.from(new Set(ids.filter(Boolean)));
}

async function mergeLinkedLibraryDetail(
  qobuzDetail: JsonRecord,
  linked: JsonRecord | null | undefined,
  qobuzSourceDetail?: JsonRecord
) {
  if (!linked?.album) return qobuzDetail;
  const linkedAlbum = linked.album as JsonRecord;
  const linkedAlbumId = idValue(linkedAlbum.id);
  const localVersions = safeArray<JsonRecord>(linked.versions)
    .filter((version) => version.provider === 'local')
    .map((version) => ({
      ...version,
      is_primary: false,
      open_local_album_id: linkedAlbumId || version.album_id || linkedAlbum.id
    }));
  const linkedQobuzVersions = await linkedCanonicalQobuzVersions(linked, qobuzSourceDetail);
  if (!localVersions.length && !linkedQobuzVersions.length) return qobuzDetail;
  const existingVersions = safeArray<JsonRecord>(qobuzDetail.versions);
  const versions = pruneCatalogRowsCoveredByTiers(
    mergeAlbumVersionRows([...localVersions, ...existingVersions], linkedQobuzVersions)
  );
  return {
    ...qobuzDetail,
    linked_album: linkedAlbum,
    versions,
    album:
      qobuzDetail.album && typeof qobuzDetail.album === 'object'
        ? {
            ...(qobuzDetail.album as JsonRecord),
            linked_album_id: linkedAlbumId || linkedAlbum.id
          }
        : qobuzDetail.album
  };
}

async function linkedCanonicalQobuzVersions(linked: JsonRecord, qobuzSourceDetail?: JsonRecord) {
  const linkedAlbum = linked.album as JsonRecord | undefined;
  const canonicalAlbum = linked.canonical_album as JsonRecord | undefined;
  const linkedQobuzId = normalizeQobuzAlbumId(
    idValue(canonicalAlbum?.qobuz_album_id, linkedAlbum?.qobuz_album_id, linkedAlbum?.qobuz_id)
  );
  if (!linkedQobuzId) return [];

  try {
    let linkedDetail = await cachedQobuzAlbum(linkedQobuzId);
    const sourceVersions = qobuzSourceDetail ? qobuzAlbumSetVersions(qobuzSourceDetail) : [];
    if (sourceVersions.length) linkedDetail = applyQobuzAlbumVersions(linkedDetail, sourceVersions);
    const linkedShape = qobuzAlbumToLibraryShape(linkedDetail);
    return safeArray<JsonRecord>(linkedShape.versions).map((version) => ({
      ...version,
      is_primary: false,
      open_album_id: idValue(version.open_album_id) || linkedQobuzId
    }));
  } catch {
    return safeArray<JsonRecord>(linked.versions)
      .filter((version) => version.provider === 'qobuz')
      .map((version) => ({
        ...version,
        is_primary: false,
        open_album_id: normalizeQobuzAlbumId(idValue(version.provider_id, version.open_album_id))
      }));
  }
}

function qobuzAlbumSetVersions(detail: JsonRecord) {
  const album = (detail.album || detail) as JsonRecord;
  const versions = safeArray<LibraryAlbum>(
    album.qobuz_album_versions || detail.qobuz_album_versions
  );
  const currentId = normalizeQobuzAlbumId(album);
  if (!versions.some((version) => normalizeQobuzAlbumId(version) === currentId)) {
    return [album as LibraryAlbum, ...versions];
  }
  return versions;
}

function versionRowIdentity(version: JsonRecord) {
  const rawId = idValue(version.id);
  const qobuzId = normalizeQobuzAlbumId(
    idValue(version.open_album_id, version.qobuz_album_id, version.provider_id, rawId)
  );
  if (version.provider === 'qobuz' && qobuzId)
    return `qobuz:${versionIdentityTier(version)}:${qobuzId}`;
  if (version.provider === 'local') return `local:${idValue(version.id, version.provider_id)}`;
  if (rawId) return `id:${rawId}`;
  return `${version.provider || ''}:${version.title || ''}:${version.version || ''}`;
}

function versionIdentityTier(version: JsonRecord) {
  const tier = String(version.tier || '')
    .trim()
    .toLowerCase();
  if (tier) return tier;
  const label = String(version.source_label || version.version || '').toLowerCase();
  const sampleRate = Number(version.sample_rate) || 0;
  const bitDepth = Number(version.bit_depth) || 0;
  if (label.includes('hi-res') || label.includes('hi res') || bitDepth >= 24 || sampleRate > 48_000)
    return 'hires';
  if (
    label.includes('cd') ||
    (bitDepth > 0 && bitDepth <= 16 && sampleRate > 0 && sampleRate <= 48_000)
  )
    return 'cd';
  return 'album';
}

function mergeAlbumVersionRows(current: JsonRecord[], additions: JsonRecord[]) {
  const seen = new Set(current.map(versionRowIdentity));
  const next = [...current];
  additions.forEach((version) => {
    const key = versionRowIdentity(version);
    if (seen.has(key)) return;
    seen.add(key);
    next.push(version);
  });
  return next;
}

function qobuzVersionAlbumId(version: JsonRecord) {
  if (version.provider !== 'qobuz') return '';
  return normalizeQobuzAlbumId(
    idValue(version.open_album_id, version.qobuz_album_id, version.provider_id, version.id)
  );
}

function pruneCatalogRowsCoveredByTiers(versions: JsonRecord[]) {
  const tieredIds = new Set(
    versions
      .filter(
        (version) => version.provider === 'qobuz' && versionIdentityTier(version) !== 'catalog'
      )
      .map(qobuzVersionAlbumId)
      .filter(Boolean)
  );
  if (!tieredIds.size) return versions;
  return versions.filter((version) => {
    if (version.provider !== 'qobuz' || versionIdentityTier(version) !== 'catalog') return true;
    return !tieredIds.has(qobuzVersionAlbumId(version));
  });
}

async function applyQobuzAlbumHint(detail: JsonRecord, albumHint?: LibraryAlbum | null) {
  let hinted = applyQobuzAlbumVersion(detail, albumHint);
  hinted = applyQobuzAlbumVersions(hinted, hintedAlbumVersions(detail, albumHint));
  if (hasQobuzAlbumVersions(hinted)) return enrichQobuzAlbumDescription(hinted);
  const artistId = idValue((hinted.album as JsonRecord | undefined)?.artist_id, hinted.artist_id);
  if (!artistId) return enrichQobuzAlbumDescription(hinted);
  try {
    const core = await cachedQobuzArtistCore(artistId);
    const albums = safeArray<LibraryAlbum>(core.albums);
    const match = albums.find(
      (album) =>
        normalizeQobuzAlbumId(album) ===
        normalizeQobuzAlbumId((hinted.album || hinted) as JsonRecord)
    );
    hinted = applyQobuzAlbumVersion(hinted, match);
    return enrichQobuzAlbumDescription(
      applyQobuzAlbumVersions(hinted, siblingAlbumVersions(hinted, albums))
    );
  } catch {
    return enrichQobuzAlbumDescription(hinted);
  }
}

function hasQobuzAlbumVersions(detail: JsonRecord) {
  const album = (detail.album || detail) as JsonRecord;
  return safeArray(album.qobuz_album_versions || detail.qobuz_album_versions).length > 1;
}

function hintedAlbumVersions(detail: JsonRecord, albumHint?: LibraryAlbum | null) {
  const versions = safeArray<LibraryAlbum>(albumHint?.qobuz_album_versions);
  const currentId = normalizeQobuzAlbumId((detail.album || detail) as JsonRecord);
  if (!versions.length || !currentId) return [];
  return versions.some((album) => normalizeQobuzAlbumId(album) === currentId) ? versions : [];
}

function siblingAlbumVersions(detail: JsonRecord, albums: LibraryAlbum[]) {
  const album = (detail.album || detail) as JsonRecord;
  const groupKey = discographyAlbumGroupKey(album);
  if (!groupKey) return [];
  return albums.filter((candidate) => discographyAlbumGroupKey(candidate) === groupKey);
}

function applyQobuzAlbumVersion(detail: JsonRecord, albumHint?: LibraryAlbum | null) {
  if (!albumHint?.version) return detail;
  const album = (detail.album || detail) as JsonRecord;
  if (album.version) return detail;
  if (normalizeQobuzAlbumId(albumHint) !== normalizeQobuzAlbumId(album)) return detail;
  if (detail.album && typeof detail.album === 'object') {
    return {
      ...detail,
      album: {
        ...(detail.album as JsonRecord),
        version: albumHint.version
      }
    };
  }
  return {
    ...detail,
    version: albumHint.version
  };
}

function applyQobuzAlbumVersions(detail: JsonRecord, albumVersions: LibraryAlbum[]) {
  if (albumVersions.length < 2) return detail;
  if (detail.album && typeof detail.album === 'object') {
    return {
      ...detail,
      qobuz_album_versions: albumVersions,
      album: {
        ...(detail.album as JsonRecord),
        qobuz_album_versions: albumVersions
      }
    };
  }
  return {
    ...detail,
    qobuz_album_versions: albumVersions
  };
}

async function enrichQobuzAlbumDescription(detail: JsonRecord) {
  if (albumDescription(detail)) return detail;
  const album = (detail.album || detail) as JsonRecord;
  const currentId = normalizeQobuzAlbumId(album);
  const candidates = safeArray<LibraryAlbum>(
    album.qobuz_album_versions || detail.qobuz_album_versions
  )
    .filter(
      (candidate) =>
        normalizeQobuzAlbumId(candidate) && normalizeQobuzAlbumId(candidate) !== currentId
    )
    .sort((a, b) => descriptionCandidateRank(a) - descriptionCandidateRank(b));
  for (const candidate of candidates.slice(0, 3)) {
    const candidateDescription = albumDescription(candidate);
    if (candidateDescription) return applyQobuzAlbumDescription(detail, candidateDescription);
    try {
      const candidateDetail = await cachedQobuzAlbum(normalizeQobuzAlbumId(candidate));
      const description = albumDescription(candidateDetail);
      if (description) return applyQobuzAlbumDescription(detail, description);
    } catch {
      // Keep the opened edition usable even if a sibling description lookup fails.
    }
  }
  return detail;
}

function albumDescription(detail: JsonRecord | null | undefined) {
  const album = (detail?.album || detail) as JsonRecord | null | undefined;
  return String(album?.description || detail?.description || '').trim();
}

function applyQobuzAlbumDescription(detail: JsonRecord, description: string) {
  if (!description || albumDescription(detail)) return detail;
  if (detail.album && typeof detail.album === 'object') {
    return {
      ...detail,
      album: {
        ...(detail.album as JsonRecord),
        description
      }
    };
  }
  return {
    ...detail,
    description
  };
}

function descriptionCandidateRank(album: LibraryAlbum) {
  const version = String(album.version || '').toLowerCase();
  const title = String(album.title || '').toLowerCase();
  const trackCount = Number(album.tracks_count || album.track_count || 0);
  return (
    (version.includes('expanded') || title.includes('expanded') ? 1000 : 0) +
    (Number.isFinite(trackCount) ? trackCount : 0)
  );
}

export async function loadQobuzOverviewData(): Promise<QobuzOverviewData> {
  const [statusResult, homeResult] = await Promise.allSettled([
    endpoints.qobuzStatus(),
    endpoints.qobuzHome()
  ]);

  return {
    qobuzStatus: settledValue(statusResult),
    qobuzHome: settledValue(homeResult)
  };
}

export function richerQobuzHome(current: JsonRecord | null, candidate: JsonRecord) {
  const candidateSectionCount = safeArray(candidate.sections).length;
  if (!candidateSectionCount) return current;
  return safeArray(current?.sections).length > candidateSectionCount ? current : candidate;
}
