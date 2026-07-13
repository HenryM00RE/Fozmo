import type { JsonRecord } from '../../../shared/types';

export type HistoryRange = '4w' | 'week' | 'month' | 'year' | 'all';
export type HistoryRankKind = 'artist' | 'song' | 'default';
export type HistoryTarget =
  | { type: 'artist'; artist: string }
  | { type: 'album'; album: string; artist: string; album_id: unknown; qobuz_album_id: unknown };

export const HISTORY_RANGES: Array<{ value: HistoryRange; label: string }> = [
  { value: '4w', label: '4 weeks' },
  { value: 'week', label: 'Week' },
  { value: 'month', label: 'Month' },
  { value: 'year', label: 'Year' },
  { value: 'all', label: 'All' }
];

export const HISTORY_RANK_VISIBLE_LIMIT = 6;

export function historyRankTarget(item: JsonRecord, kind: HistoryRankKind): HistoryTarget | null {
  if (kind === 'artist') {
    return { type: 'artist', artist: String(item.name || '') };
  }
  return {
    type: 'album',
    album: String(item.album || item.name || ''),
    artist: String(item.subtitle || item.artist || item.album_artist || ''),
    album_id: item.album_id ?? item.local_album_id ?? null,
    qobuz_album_id: item.qobuz_album_id ?? null
  };
}

export function historyRecentTrackTarget(entry: JsonRecord): HistoryTarget {
  const source = (entry.source || {}) as JsonRecord;
  return {
    type: 'album',
    album: String(entry.album || source.album || ''),
    artist: String(entry.artist || source.artist || ''),
    album_id: entry.album_id ?? source.album_id ?? null,
    qobuz_album_id:
      source.kind === 'qobuz_track' ? (source.album_id ?? null) : (entry.qobuz_album_id ?? null)
  };
}

export function normalizeRange(value: unknown): HistoryRange {
  return HISTORY_RANGES.some((option) => option.value === value) ? (value as HistoryRange) : '4w';
}

export function formatHistoryBucketLabel(bucket: JsonRecord) {
  const start = Number(bucket?.start_at || 0);
  const end = Number(bucket?.end_at || 0);
  if (!start || !end) return String(bucket?.label || '');
  const startDate = new Date(start * 1000);
  const endDate = new Date(Math.max(start, end - 1) * 1000);
  const sameYear = startDate.getFullYear() === endDate.getFullYear();
  const sameMonth = sameYear && startDate.getMonth() === endDate.getMonth();
  const month = new Intl.DateTimeFormat(undefined, { month: 'short' });
  const monthDay = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
  const monthDayYear = new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    year: 'numeric'
  });
  if (startDate.toDateString() === endDate.toDateString()) return monthDay.format(startDate);
  if (!sameYear) return `${monthDayYear.format(startDate)} - ${monthDayYear.format(endDate)}`;
  if (sameMonth) return `${month.format(startDate)} ${startDate.getDate()}-${endDate.getDate()}`;
  return `${monthDay.format(startDate)} - ${monthDay.format(endDate)}`;
}

export function formatListeningTime(seconds: unknown) {
  const totalMinutes = Math.max(0, Math.round(Number(seconds || 0) / 60));
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

export function historyBucketKey(bucket: JsonRecord, index: number) {
  return String(bucket.start_at || bucket.label || index);
}

export function historyRankKey(item: JsonRecord, index: number) {
  return `${String(item.id || item.album_id || item.name || item.title || 'rank')}:${index}`;
}

export function historyRecentKey(item: JsonRecord, index: number) {
  return `${String(item.played_at || item.id || item.title || 'recent')}:${index}`;
}
