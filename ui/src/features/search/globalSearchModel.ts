import { useCallback } from 'react';
import { endpoints } from '../../shared/lib/api';
import {
  artistOf,
  compactMeta,
  type GlobalSearchPlacement,
  type GlobalSearchSource,
  type GlobalSearchState,
  idValue,
  normalizeQobuzAlbumId,
  normalizeSearchText,
  qobuzAlbumToLibraryShape,
  qobuzTrackFromAlbumTrack,
  resolveLocalAlbumId,
  safeArray,
  titleOf,
  wordBoundaryContains
} from '../../shared/lib/appSupport';
import { formatTime } from '../../shared/lib/format';
import {
  localTrackToQueueItem,
  qobuzTrackToQueueItem,
  resolvedPlaySourceToQueueItem
} from '../../shared/lib/queue';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  QobuzTrack,
  QueueItem,
  ResolvedPlaySource
} from '../../shared/types';

export type GlobalSearchMenuAction = {
  id: string;
  label: string;
  path?: string;
  icon?: 'play-next';
  filled?: boolean;
  run: () => void | Promise<void>;
};

export type GlobalSearchRowModel = {
  id: string;
  kind: 'song' | 'album' | 'artist';
  kindLabel: string;
  source: GlobalSearchSource | 'mixed';
  title: string;
  subtitle: string;
  titleBadge?: string;
  sourceLabel: string;
  sourceIcon?: boolean;
  actionLabel: string;
  hideAction?: boolean;
  imageUrl?: string | null;
  artId?: string | number | null;
  score: number;
  matchReason: string;
  searchMeta: GlobalSearchRowSearchMeta;
  actions?: GlobalSearchMenuAction[];
  run: () => void | Promise<void>;
};

export type GlobalSearchView = {
  allRows: GlobalSearchRowModel[];
  hasMore: boolean;
  hasQuery: boolean;
  isLoading: boolean;
  rows: GlobalSearchRowModel[];
  status: string;
  topResult: GlobalSearchRowModel | null;
  total: number;
};

type BuildGlobalSearchRowsParams = {
  albums: LibraryAlbum[];
  onAddTrackToPlaylist: (track: LibraryTrack | QobuzTrack, source: GlobalSearchSource) => void;
  onClose: () => void;
  onOpenAlbum: (id: string | number) => void;
  onOpenArtist: (name: string) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlayQobuz: (track: QobuzTrack) => void;
  onPlayTrack: (track: LibraryTrack) => void;
  onQueueAlbum: (
    album: LibraryAlbum,
    source: GlobalSearchSource,
    placement: GlobalSearchPlacement
  ) => void | Promise<void>;
  onQueueTrack: (
    track: LibraryTrack | QobuzTrack,
    source: GlobalSearchSource,
    placement: GlobalSearchPlacement
  ) => void;
  query: string;
  results: GlobalSearchState;
};

type BuildGlobalSearchViewParams = BuildGlobalSearchRowsParams & {
  showAll: boolean;
};

const MIXED_RESULT_LIMIT = 12;
const DIVERSITY_WINDOW = 8;

const kindLabels: Record<GlobalSearchRowModel['kind'], string> = {
  song: 'Song',
  album: 'Album',
  artist: 'Artist'
};

type ScoreResult = {
  score: number;
  matchReason: string;
};

type SearchFieldKind = 'none' | 'exact' | 'prefix' | 'word' | 'contains' | 'tokens';

type SearchFieldScore = {
  kind: SearchFieldKind;
  score: number;
};

type GlobalSearchRowSearchMeta = {
  album: SearchFieldScore;
  albumTitle: string;
  artist: SearchFieldScore;
  dedupeKey: string;
  directPrimary: boolean;
  exactPrimary: boolean;
  hasMatch: boolean;
  metadata: SearchFieldScore;
  primaryArtist: string;
  primaryScore: number;
  sourceIndex: number;
  title: SearchFieldScore;
  weakQobuzArtist: boolean;
};

type RowSearchFields = {
  album?: string;
  artist?: string;
  dedupeKey: string;
  metadata?: string;
  primaryArtist?: string;
  title: string;
  weakQobuzArtist?: boolean;
};

type SearchIntent = {
  key: string;
  kind: 'artist' | 'album' | 'song' | null;
  strongSongOrAlbum: boolean;
};

function scoreField(value: string | null | undefined, query: string): SearchFieldScore {
  const q = normalizeSearchText(query);
  const normalized = normalizeSearchText(value);
  if (!q || !normalized) return { kind: 'none', score: 0 };
  const queryTokens = q.split(' ').filter(Boolean);

  if (normalized === q) return { kind: 'exact', score: 120 };
  if (normalized.startsWith(`${q} `) || normalized.startsWith(q))
    return { kind: 'prefix', score: 104 };
  if (wordBoundaryContains(normalized, q)) return { kind: 'word', score: 88 };
  if (normalized.includes(q)) return { kind: 'contains', score: 70 };
  if (queryTokens.length > 1 && queryTokens.every((token) => normalized.includes(token)))
    return { kind: 'tokens', score: 48 };
  return { kind: 'none', score: 0 };
}

function buildSearchMeta(
  row: Omit<GlobalSearchRowModel, 'score' | 'matchReason' | 'searchMeta'>,
  fields: RowSearchFields,
  query: string,
  sourceIndex: number
): GlobalSearchRowSearchMeta {
  const title = scoreField(fields.title, query);
  const artist = scoreField(fields.artist, query);
  const album = scoreField(fields.album, query);
  const metadata = scoreField(fields.metadata, query);
  const primaryScore =
    row.kind === 'artist'
      ? title.score
      : row.kind === 'album'
        ? Math.max(title.score, artist.score)
        : Math.max(title.score, artist.score, album.score);
  const exactPrimary =
    row.kind === 'artist'
      ? title.kind === 'exact'
      : row.kind === 'album'
        ? title.kind === 'exact' || artist.kind === 'exact'
        : title.kind === 'exact' || artist.kind === 'exact' || album.kind === 'exact';

  return {
    album,
    albumTitle: normalizeSearchText(fields.album),
    artist,
    dedupeKey: fields.dedupeKey,
    directPrimary: primaryScore >= 70,
    exactPrimary,
    hasMatch: Boolean(title.score || artist.score || album.score || metadata.score),
    metadata,
    primaryArtist: normalizeSearchText(fields.primaryArtist || fields.artist),
    primaryScore,
    sourceIndex,
    title,
    weakQobuzArtist: Boolean(fields.weakQobuzArtist)
  };
}

function scoreGlobalSearchRow(row: GlobalSearchRowModel): ScoreResult {
  const meta = row.searchMeta;
  const sourceBonus = row.source === 'local' ? 16 : row.source === 'mixed' ? 18 : 0;
  const artBonus = row.imageUrl || row.artId ? 2 : 0;
  const providerOrderBonus =
    row.source === 'qobuz'
      ? Math.max(0, 5 - Math.min(meta.sourceIndex, 5))
      : Math.max(0, 8 - Math.min(meta.sourceIndex, 8));
  const kindBonus =
    row.kind === 'artist' && meta.title.score >= 88
      ? 8
      : row.kind === 'album' && meta.title.score >= 88
        ? 4
        : 0;
  const weightedPrimary =
    row.kind === 'artist'
      ? meta.title.score * 1.08
      : row.kind === 'album'
        ? Math.max(meta.title.score, meta.artist.score * 0.76, meta.metadata.score * 0.35)
        : Math.max(
            meta.title.score,
            meta.artist.score * 0.84,
            meta.album.score * 0.74,
            scoreFieldWeight(meta.metadata, 0.35)
          );
  const score = Math.round(
    weightedPrimary + sourceBonus + artBonus + providerOrderBonus + kindBonus
  );
  return {
    score,
    matchReason: matchReasonForRow(row)
  };
}

function scoreFieldWeight(field: SearchFieldScore, weight: number) {
  return field.score * weight;
}

function matchReasonForRow(row: GlobalSearchRowModel) {
  const meta = row.searchMeta;
  const fields: Array<[string, SearchFieldScore]> = [
    [row.kind === 'artist' ? 'Artist' : 'Title', meta.title],
    ['Artist', meta.artist],
    ['Album', meta.album],
    ['Metadata', meta.metadata]
  ];
  const [label, match] = fields.sort((a, b) => b[1].score - a[1].score)[0];
  if (!match.score) return '';
  if (match.kind === 'exact') return `Exact ${label.toLowerCase()} match`;
  if (match.kind === 'prefix') return `${label} starts with search`;
  if (match.kind === 'word' || match.kind === 'contains') return `${label} match`;
  return 'Related match';
}

function resultSort(a: GlobalSearchRowModel, b: GlobalSearchRowModel) {
  const sourceRank = (row: GlobalSearchRowModel) =>
    row.source === 'local' ? 0 : row.source === 'mixed' ? 1 : 2;
  return (
    b.score - a.score ||
    sourceRank(a) - sourceRank(b) ||
    a.kind.localeCompare(b.kind) ||
    a.title.localeCompare(b.title)
  );
}

function diverseRows(rows: GlobalSearchRowModel[]) {
  const remaining = [...rows].sort(resultSort);
  const selected: GlobalSearchRowModel[] = [];

  while (remaining.length) {
    let index = 0;
    if (selected.length < DIVERSITY_WINDOW) {
      const top = remaining[0];
      const sameKindCount = selected.filter((row) => row.kind === top.kind).length;
      const kindCount = new Set(selected.map((row) => row.kind)).size;
      const allowedGap = sameKindCount >= 3 ? 18 : 14;
      if ((selected.length >= 2 && sameKindCount >= 2 && kindCount < 3) || sameKindCount >= 3) {
        const alternateIndex = remaining.findIndex(
          (row) =>
            row.kind !== top.kind &&
            row.searchMeta.directPrimary &&
            top.score - row.score <= allowedGap
        );
        if (alternateIndex > 0) index = alternateIndex;
      }
    }
    selected.push(remaining.splice(index, 1)[0]);
  }

  return selected;
}

function topResultForRows(rows: GlobalSearchRowModel[]) {
  const [first, second] = rows;
  if (!first) return null;
  if (!first.searchMeta.directPrimary) return null;
  const secondScore = second?.score || 0;
  if (first.score >= 104 || (first.score >= 88 && first.score - secondScore >= 16)) return first;
  return null;
}

function finalizeGlobalSearchRows(rows: GlobalSearchRowModel[]) {
  const initiallyScored = rows
    .filter((row) => row.searchMeta.hasMatch)
    .map((row) => ({ ...row, ...scoreGlobalSearchRow(row) }));
  const intent = detectSearchIntent(initiallyScored);
  const intentScored = initiallyScored.map((row) => applySearchIntent(row, intent));
  return diverseRows(dedupeRows(intentScored).sort(resultSort));
}

function detectSearchIntent(rows: GlobalSearchRowModel[]): SearchIntent {
  const strongSongOrAlbum = rows.some(
    (row) => row.kind !== 'artist' && row.searchMeta.title.score >= 96
  );
  const exactLocalArtist = rows.find(
    (row) =>
      row.kind === 'artist' && row.source !== 'qobuz' && row.searchMeta.title.kind === 'exact'
  );
  if (exactLocalArtist) {
    return { kind: 'artist', key: normalizeSearchText(exactLocalArtist.title), strongSongOrAlbum };
  }

  const exactAlbum = rows.find(
    (row) => row.kind === 'album' && row.source !== 'qobuz' && row.searchMeta.title.kind === 'exact'
  );
  if (exactAlbum) {
    return { kind: 'album', key: normalizeSearchText(exactAlbum.title), strongSongOrAlbum };
  }

  const strongSong = rows.find((row) => row.kind === 'song' && row.searchMeta.title.score >= 96);
  if (strongSong) {
    return { kind: 'song', key: normalizeSearchText(strongSong.title), strongSongOrAlbum };
  }

  const exactCatalogArtist = rows.find(
    (row) => row.kind === 'artist' && row.searchMeta.title.kind === 'exact'
  );
  if (exactCatalogArtist && !strongSongOrAlbum) {
    return {
      kind: 'artist',
      key: normalizeSearchText(exactCatalogArtist.title),
      strongSongOrAlbum
    };
  }

  const exactCatalogAlbum = rows.find(
    (row) => row.kind === 'album' && row.searchMeta.title.kind === 'exact'
  );
  if (exactCatalogAlbum) {
    return { kind: 'album', key: normalizeSearchText(exactCatalogAlbum.title), strongSongOrAlbum };
  }

  return { kind: null, key: '', strongSongOrAlbum };
}

function applySearchIntent(row: GlobalSearchRowModel, intent: SearchIntent): GlobalSearchRowModel {
  let score = row.score;
  if (row.kind === 'artist' && row.searchMeta.weakQobuzArtist && intent.strongSongOrAlbum)
    score -= 76;

  if (intent.kind === 'artist') {
    if (normalizeSearchText(row.title) === intent.key && row.kind === 'artist') score += 46;
    if (row.searchMeta.primaryArtist === intent.key) score += row.kind === 'artist' ? 28 : 56;
    if (
      row.kind !== 'artist' &&
      row.searchMeta.title.kind === 'exact' &&
      row.searchMeta.primaryArtist !== intent.key
    )
      score -= 22;
    if (
      row.kind === 'artist' &&
      row.source === 'qobuz' &&
      normalizeSearchText(row.title) !== intent.key
    )
      score -= 28;
  }

  if (intent.kind === 'album') {
    if (row.kind === 'album' && normalizeSearchText(row.title) === intent.key) score += 48;
    if (row.kind === 'song' && row.searchMeta.albumTitle === intent.key) score += 46;
    if (row.kind === 'album' && row.searchMeta.title.score >= 96) score += 18;
  }

  if (intent.kind === 'song' && row.kind === 'song' && row.searchMeta.title.score >= 96) {
    score += 42;
  }

  return {
    ...row,
    score,
    matchReason: row.matchReason || 'Provider match'
  };
}

function dedupeRows(rows: GlobalSearchRowModel[]) {
  const byKey = new Map<string, GlobalSearchRowModel>();
  rows.forEach((row) => {
    const key = row.searchMeta.dedupeKey;
    const existing = byKey.get(key);
    if (!existing || preferRow(row, existing)) byKey.set(key, row);
  });
  return Array.from(byKey.values());
}

function preferRow(candidate: GlobalSearchRowModel, current: GlobalSearchRowModel) {
  const sourceRank = (row: GlobalSearchRowModel) =>
    row.source === 'local' ? 0 : row.source === 'mixed' ? 1 : 2;
  return (
    candidate.score > current.score ||
    (candidate.score === current.score && sourceRank(candidate) < sourceRank(current)) ||
    (candidate.score === current.score &&
      sourceRank(candidate) === sourceRank(current) &&
      Boolean(candidate.imageUrl || candidate.artId) &&
      !(current.imageUrl || current.artId))
  );
}

export function globalSearchStatus(query: string, results: GlobalSearchState, total: number) {
  const isLoading = results.localLoading || results.qobuzLoading;
  if (!query.trim()) return '';
  if (isLoading) return '';
  if (total) return '';
  if (results.localError && results.qobuzError) return 'Search unavailable.';
  return 'No matching records.';
}

export function buildGlobalSearchView(params: BuildGlobalSearchViewParams): GlobalSearchView {
  const allRows = buildGlobalSearchRows(params);
  const total = allRows.length;
  const isLoading = params.results.localLoading || params.results.qobuzLoading;
  const topResult = topResultForRows(allRows);
  const feedRows = topResult ? allRows.filter((row) => row.id !== topResult.id) : allRows;
  const rows = params.showAll ? feedRows : feedRows.slice(0, MIXED_RESULT_LIMIT);

  return {
    allRows,
    hasMore: feedRows.length > rows.length,
    hasQuery: Boolean(params.query.trim()),
    isLoading,
    rows,
    status: globalSearchStatus(params.query, params.results, total),
    topResult,
    total
  };
}

export function buildGlobalSearchRows({
  albums,
  onAddTrackToPlaylist,
  onClose,
  onOpenAlbum,
  onOpenArtist,
  onOpenQobuzAlbum,
  onPlayQobuz,
  onPlayTrack,
  onQueueAlbum,
  onQueueTrack,
  query,
  results
}: BuildGlobalSearchRowsParams) {
  const decorateResult = (
    row: Omit<GlobalSearchRowModel, 'score' | 'matchReason' | 'searchMeta'>,
    sourceIndex: number,
    fields: RowSearchFields
  ): GlobalSearchRowModel => {
    const searchMeta = buildSearchMeta(row, fields, query, sourceIndex);
    const rowWithMeta = { ...row, score: 0, matchReason: '', searchMeta };
    return { ...rowWithMeta, ...scoreGlobalSearchRow(rowWithMeta) };
  };

  const makeSongResult = (
    track: LibraryTrack | QobuzTrack,
    source: GlobalSearchSource,
    sourceIndex: number
  ): GlobalSearchRowModel => {
    const isQobuz = source === 'qobuz';
    const title = titleOf(track);
    const artistName = artistOf(track);
    const albumName = String((track as JsonRecord).album || '');
    const albumId = isQobuz ? (track as QobuzTrack).album_id : resolveLocalAlbumId(track, albums);
    const run = () => {
      onClose();
      if (isQobuz) onPlayQobuz(track as QobuzTrack);
      else onPlayTrack(track as LibraryTrack);
    };
    const actions: GlobalSearchMenuAction[] = [
      { id: 'play', label: 'Play', path: 'M8 5v14l11-7Z', filled: true, run },
      {
        id: 'add-next',
        label: 'Add next',
        icon: 'play-next',
        run: () => onQueueTrack(track, source, 'next')
      },
      {
        id: 'add-to-playlist',
        label: 'Add to playlist',
        path: 'M4 7h12M4 12h9M4 17h7M18 15v6M15 18h6',
        run: () => {
          onClose();
          onAddTrackToPlaylist(track, source);
        }
      }
    ];
    if (albumId) {
      actions.push({
        id: 'go-to-album',
        label: 'Go to album',
        path: 'M5 4h14v16H5zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM12 12h.01',
        run: () => {
          onClose();
          if (isQobuz) onOpenQobuzAlbum(albumId);
          else onOpenAlbum(albumId);
        }
      });
    }
    if (artistName) {
      actions.push({
        id: 'go-to-artist',
        label: 'Go to artist',
        path: 'M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM4 20c1.8-4 4.5-6 8-6s6.2 2 8 6',
        run: () => {
          onClose();
          onOpenArtist(artistName);
        }
      });
    }
    actions.push({
      id: 'add-to-queue',
      label: 'Add to queue',
      path: 'M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8',
      run: () => onQueueTrack(track, source, 'end')
    });
    return decorateResult(
      {
        id: `${source}:song:${String(track.id ?? track.track_id ?? track.file_name ?? title)}`,
        kind: 'song',
        kindLabel: kindLabels.song,
        source,
        title,
        subtitle: compactMeta([
          artistName,
          albumName,
          formatTime(
            Number((track as JsonRecord).duration || (track as JsonRecord).duration_secs || 0)
          )
        ]),
        sourceLabel: isQobuz ? 'Qobuz' : 'Library',
        sourceIcon: isQobuz,
        actionLabel: 'Play',
        imageUrl: (track as JsonRecord).image_url as string | null | undefined,
        artId: (track as JsonRecord).art_id as string | number | null | undefined,
        actions,
        run
      },
      sourceIndex,
      {
        album: albumName,
        artist: artistName,
        dedupeKey: `song:${normalizeSearchText(title)}:${normalizeSearchText(artistName)}:${normalizeSearchText(albumName)}`,
        metadata: compactMeta([
          (track as JsonRecord).album_artist,
          (track as JsonRecord).composer,
          (track as JsonRecord).genre,
          (track as JsonRecord).file_name,
          (track as JsonRecord).performers_raw
        ]),
        primaryArtist: artistName,
        title
      }
    );
  };

  const makeAlbumResult = (
    album: LibraryAlbum,
    source: GlobalSearchSource,
    sourceIndex: number
  ): GlobalSearchRowModel => {
    const isQobuz = source === 'qobuz';
    const albumId = isQobuz ? normalizeQobuzAlbumId(album) : idValue(album.id);
    const title = album.title || 'Unknown album';
    const artistName = String(album.album_artist || album.artist || '');
    const year = String(album.year || '');
    const trackCount = String(
      (album as JsonRecord).track_count || (album as JsonRecord).tracks_count || ''
    );
    const run = () => {
      onClose();
      if (isQobuz) onOpenQobuzAlbum(albumId);
      else onOpenAlbum(albumId);
    };
    return decorateResult(
      {
        id: `${source}:album:${String(albumId || album.title)}`,
        kind: 'album',
        kindLabel: kindLabels.album,
        source,
        title,
        subtitle: compactMeta([
          artistName,
          album.year,
          (album as JsonRecord).track_count || (album as JsonRecord).tracks_count
            ? `${(album as JsonRecord).track_count || (album as JsonRecord).tracks_count} songs`
            : null
        ]),
        sourceLabel: isQobuz ? 'Qobuz' : 'Library',
        sourceIcon: isQobuz,
        actionLabel: 'Open',
        imageUrl: album.image_url || album.cover_url,
        artId: album.art_id,
        actions: [
          { id: 'open', label: 'Open', path: 'M5 12h14m-6-6 6 6-6 6', run },
          {
            id: 'add-next',
            label: 'Add album next',
            icon: 'play-next',
            run: () => onQueueAlbum(album, source, 'next')
          },
          {
            id: 'add-to-queue',
            label: 'Add album to queue',
            path: 'M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8',
            run: () => onQueueAlbum(album, source, 'end')
          }
        ],
        run
      },
      sourceIndex,
      {
        artist: artistName,
        dedupeKey: `album:${normalizeSearchText(title)}:${normalizeSearchText(artistName)}:${year}:${trackCount}`,
        metadata: compactMeta([
          (album as JsonRecord).genre,
          (album as JsonRecord).label,
          (album as JsonRecord).version
        ]),
        primaryArtist: artistName,
        title
      }
    );
  };

  const makeArtistResult = (
    artist: JsonRecord,
    source: GlobalSearchSource | 'mixed',
    sourceIndex: number
  ): GlobalSearchRowModel => {
    const name = String(artist.name || 'Unknown artist');
    const inLibrary = Boolean(artist.in_library);
    const isQobuz = source === 'qobuz';
    const albumCount = artist.album_count || artist.albums_count;
    const trackCount = artist.track_count;
    const weakQobuzArtist =
      isQobuz && !inLibrary && !artist.image_url && !albumCount && !trackCount;
    return decorateResult(
      {
        id: `artist:${normalizeSearchText(name) || name}`,
        kind: 'artist',
        kindLabel: kindLabels.artist,
        source,
        title: name,
        subtitle: compactMeta([
          albumCount ? `${albumCount} albums` : null,
          trackCount ? `${trackCount} songs` : null
        ]),
        titleBadge: inLibrary ? 'In Library' : '',
        sourceLabel: source === 'mixed' ? '' : isQobuz ? 'Qobuz' : 'Library',
        sourceIcon: isQobuz,
        actionLabel: 'Open',
        hideAction: true,
        imageUrl: artist.image_url as string | null | undefined,
        run: () => {
          onClose();
          onOpenArtist(name);
        }
      },
      sourceIndex,
      {
        dedupeKey: `artist:${normalizeSearchText(name)}`,
        metadata: compactMeta([artist.genre, albumCount, trackCount]),
        primaryArtist: name,
        title: name,
        weakQobuzArtist
      }
    );
  };

  const localByName = new Map<string, JsonRecord>();
  results.local.artists.forEach((artist) => {
    const key = normalizeSearchText(artist.name);
    if (key && !localByName.has(key)) localByName.set(key, artist);
  });

  const seenQobuz = new Set<string>();
  const usedLocal = new Set<string>();
  const mergedArtists: GlobalSearchRowModel[] = [];
  results.qobuz.artists.forEach((artist, index) => {
    const key = normalizeSearchText(artist.name);
    if (key && seenQobuz.has(key)) return;
    if (key) seenQobuz.add(key);
    const localArtist = key ? localByName.get(key) : null;
    if (key && localArtist) usedLocal.add(key);
    mergedArtists.push(
      makeArtistResult(
        localArtist ? { ...localArtist, ...artist, in_library: true } : artist,
        localArtist ? 'mixed' : 'qobuz',
        index
      )
    );
  });
  results.local.artists.forEach((artist, index) => {
    const key = normalizeSearchText(artist.name);
    if (key && usedLocal.has(key)) return;
    mergedArtists.push(makeArtistResult({ ...artist, in_library: true }, 'local', index));
  });

  const rows = [
    ...results.local.songs.map((track, index) => makeSongResult(track, 'local', index)),
    ...results.qobuz.songs.map((track, index) =>
      makeSongResult(track as QobuzTrack, 'qobuz', index)
    ),
    ...results.local.albums.map((album, index) => makeAlbumResult(album, 'local', index)),
    ...results.qobuz.albums.map((album, index) => makeAlbumResult(album, 'qobuz', index)),
    ...mergedArtists
  ];

  return finalizeGlobalSearchRows(rows);
}

type UseGlobalSearchQueueActionsParams = {
  addItemsToQueue: (items: QueueItem[], placement: GlobalSearchPlacement) => void;
  albums: LibraryAlbum[];
  setNotice: (message: string) => void;
};

export function useGlobalSearchQueueActions({
  addItemsToQueue,
  albums,
  setNotice
}: UseGlobalSearchQueueActionsParams) {
  const queueGlobalSearchTrack = useCallback(
    (
      track: LibraryTrack | QobuzTrack,
      source: GlobalSearchSource,
      placement: GlobalSearchPlacement
    ) => {
      addItemsToQueue(
        [
          source === 'qobuz'
            ? qobuzTrackToQueueItem(track as QobuzTrack)
            : localTrackToQueueItem(track as LibraryTrack)
        ],
        placement
      );
    },
    [addItemsToQueue]
  );

  const queueGlobalSearchAlbum = useCallback(
    async (album: LibraryAlbum, source: GlobalSearchSource, placement: GlobalSearchPlacement) => {
      try {
        let items: QueueItem[] = [];
        if (source === 'qobuz') {
          const albumId = normalizeQobuzAlbumId(album);
          if (!albumId) throw new Error('No album link available');
          const detail = qobuzAlbumToLibraryShape(await endpoints.qobuzAlbum(albumId));
          items = safeArray<LibraryTrack>(detail.tracks)
            .map(qobuzTrackFromAlbumTrack)
            .filter(Boolean)
            .map((track) => qobuzTrackToQueueItem(track as QobuzTrack));
        } else {
          const albumId = resolveLocalAlbumId(album, albums);
          if (albumId === null || albumId === undefined || albumId === '')
            throw new Error('No album link available');
          const plan = await endpoints.albumPlaySources(albumId);
          items = safeArray<ResolvedPlaySource>(plan.sources)
            .map(resolvedPlaySourceToQueueItem)
            .filter(Boolean) as QueueItem[];
        }
        if (!items.length) {
          setNotice('No playable tracks found for this album');
          return;
        }
        addItemsToQueue(items, placement);
      } catch (error) {
        setNotice(error instanceof Error ? error.message : 'Could not add album to queue');
      }
    },
    [addItemsToQueue, albums, setNotice]
  );

  return {
    queueGlobalSearchAlbum,
    queueGlobalSearchTrack
  };
}
