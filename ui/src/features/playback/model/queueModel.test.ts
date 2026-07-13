import { describe, expect, it } from 'vitest';
import { normalizeQueueState } from '../../../shared/lib/queue';
import type { QobuzTrack, QueueItem, QueueState } from '../../../shared/types';
import {
  nextQueueItemForPrefetch,
  qobuzQueueForPlayback,
  shuffleUpcomingQueue,
  sourceRefsForBackendQueue,
  sourceRefsForPlayback
} from './queueModel';

function qobuzTrack(id: number): QobuzTrack {
  return {
    id,
    title: `Track ${id}`,
    artist: 'Qobuz Artist',
    album: 'Qobuz Album',
    duration_secs: 180
  };
}

function qobuzItem(id: number): QueueItem {
  const track = qobuzTrack(id);
  return {
    title: track.title || '',
    artist: track.artist || '',
    album: track.album || '',
    durationSecs: Number(track.duration_secs) || 0,
    filename: `Qobuz Artist - Track ${id}`,
    qobuzTrack: track
  };
}

function localItem(id: number): QueueItem {
  return {
    title: `Local ${id}`,
    artist: 'Local Artist',
    album: 'Local Album',
    durationSecs: 200,
    filename: `local-${id}.flac`,
    ref: { track_id: id }
  };
}

function queueState(items: QueueItem[], cursor = -1): QueueState {
  return {
    kind: 'mixed',
    cursor,
    items,
    loopMode: 'off'
  };
}

describe('queueModel playback queues', () => {
  it('builds the backend tail after the selected playlist track', () => {
    const state = queueState(
      [qobuzItem(1), qobuzItem(2), qobuzItem(3), qobuzItem(4), qobuzItem(5), qobuzItem(6)],
      3
    );

    expect(sourceRefsForPlayback(state, 3).map((source) => source.track_id)).toEqual([5, 6]);
    expect(qobuzQueueForPlayback(state, 3).map((track) => track.id)).toEqual([5, 6]);
  });

  it('keeps the generic backend tail when a Qobuz play payload stops at a local item', () => {
    const state = queueState([
      qobuzItem(1),
      qobuzItem(2),
      qobuzItem(3),
      localItem(99),
      qobuzItem(4)
    ]);

    expect(sourceRefsForPlayback(state, 1)).toMatchObject([
      { kind: 'qobuz_track', track_id: 3 },
      { kind: 'local_track', track_id: 99 },
      { kind: 'qobuz_track', track_id: 4 }
    ]);
    expect(qobuzQueueForPlayback(state, 1).map((track) => track.id)).toEqual([3]);
  });

  it('shuffles only upcoming queue items and keeps paused current fixed', () => {
    const state = queueState(
      [qobuzItem(1), qobuzItem(2), qobuzItem(3), qobuzItem(4), qobuzItem(5)],
      1
    );

    const shuffled = shuffleUpcomingQueue(state, () => 0);

    expect(shuffled.items.slice(0, 2).map((item) => item.qobuzTrack?.id)).toEqual([1, 2]);
    expect(shuffled.items.slice(2).map((item) => item.qobuzTrack?.id)).toEqual([4, 5, 3]);
    expect(sourceRefsForBackendQueue(shuffled).map((source) => source.track_id)).toEqual([4, 5, 3]);
  });

  it('prefetches the current item when loop is enabled', () => {
    const state: QueueState = {
      ...queueState([qobuzItem(1), qobuzItem(2), qobuzItem(3)], 2),
      loopMode: 'loop'
    };

    expect(nextQueueItemForPrefetch(state, 2)?.qobuzTrack?.id).toBe(3);
  });

  it('does not wrap the prefetch target when loop is off', () => {
    const state = queueState([qobuzItem(1), qobuzItem(2), qobuzItem(3)], 2);

    expect(nextQueueItemForPrefetch(state, 2)).toBeNull();
  });

  it('normalizes legacy repeat-one into the two-state loop UI', () => {
    expect(normalizeQueueState({ items: [], loopMode: 'one' as never }).loopMode).toBe('loop');
    expect(normalizeQueueState({ items: [], loopMode: 'loop' }).loopMode).toBe('loop');
    expect(normalizeQueueState({ items: [], loopMode: 'off' }).loopMode).toBe('off');
  });
});
