import { endpoints } from '../../../shared/lib/api';
import { formatTime } from '../../../shared/lib/format';
import {
  itemKey,
  qobuzDisplayName,
  queueItemToSourceRef,
  sourceRefKey
} from '../../../shared/lib/queue';
import type { QobuzTrack, QueueState, SourceRef } from '../../../shared/types';
import type { NowPlayingQueueItemSnapshot } from './nowPlayingQueueStore';

export function queueSnapshot(state: QueueState): Omit<NowPlayingQueueItemSnapshot, never>[] {
  return state.items.map((item, index) => ({
    index,
    key: `${itemKey(item)}:${index}`,
    title: item.title || 'Untitled',
    subtitle: [item.artist, item.album].filter(Boolean).join(' - '),
    featureHtml: '',
    durationLabel: item.durationSecs ? formatTime(item.durationSecs) : '',
    artSrc: item.imageUrl || endpoints.artUrl(item.artId) || null,
    isCurrent: index === state.cursor,
    isPast: state.cursor >= 0 && index < state.cursor,
    removable: state.cursor < 0 || index > state.cursor
  }));
}

export function findQueueIndexByFileName(state: QueueState, fileName?: string | null) {
  if (!fileName) return -1;
  return state.items.findIndex(
    (item) => item.filename === fileName || qobuzDisplayName(item.qobuzTrack || {}) === fileName
  );
}

export function findQueueIndexBySourceRef(state: QueueState, source?: SourceRef | null) {
  const key = sourceRefKey(source);
  if (!key) return -1;
  return state.items.findIndex((item) => sourceRefKey(queueItemToSourceRef(item)) === key);
}

export function sourceRefsForPlayback(state: QueueState, startIndex: number) {
  return state.items
    .slice(startIndex + 1)
    .map(queueItemToSourceRef)
    .filter(Boolean) as SourceRef[];
}

export function sourceRefsForBackendQueue(state: QueueState) {
  const start = state.cursor >= 0 ? state.cursor + 1 : 0;
  return state.items.slice(start).map(queueItemToSourceRef).filter(Boolean) as SourceRef[];
}

export function shuffleUpcomingQueue(
  state: QueueState,
  random: () => number = Math.random
): QueueState {
  const fixedCount = state.cursor >= 0 ? state.cursor + 1 : 0;
  const upcoming = state.items.slice(fixedCount);
  for (let i = upcoming.length - 1; i > 0; i -= 1) {
    const j = Math.floor(random() * (i + 1));
    [upcoming[i], upcoming[j]] = [upcoming[j], upcoming[i]];
  }
  return {
    ...state,
    items: [...state.items.slice(0, fixedCount), ...upcoming]
  };
}

export function qobuzQueueForPlayback(state: QueueState, startIndex: number) {
  const queue: QobuzTrack[] = [];
  for (const item of state.items.slice(startIndex + 1)) {
    if (!item.qobuzTrack) break;
    queue.push(item.qobuzTrack);
  }
  return queue;
}

export function nextQueueItemForPrefetch(state: QueueState, currentIndex: number) {
  if (currentIndex < 0 || currentIndex >= state.items.length) return null;
  if (state.loopMode === 'loop') return state.items[currentIndex];
  return state.items[currentIndex + 1] || null;
}
