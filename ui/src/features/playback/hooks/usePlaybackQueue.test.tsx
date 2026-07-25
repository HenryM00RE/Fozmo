// @vitest-environment jsdom

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { QueueItem, SourceRef } from '../../../shared/types';
import { getNowPlayingQueueActions } from '../model/nowPlayingQueueStore';
import { usePlaybackQueue } from './usePlaybackQueue';

const mocks = vi.hoisted(() => ({
  apiGet: vi.fn(),
  nowPlayingQueue: vi.fn(),
  saveNowPlayingQueue: vi.fn(),
  zoneQueue: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({
  api: {
    get: mocks.apiGet
  },
  endpoints: {
    artUrl: () => null,
    nowPlayingQueue: mocks.nowPlayingQueue,
    saveNowPlayingQueue: mocks.saveNowPlayingQueue,
    zoneQueue: mocks.zoneQueue
  }
}));

function localItem(id: number, title: string): QueueItem {
  return {
    title,
    artist: 'Artist',
    album: 'Album',
    durationSecs: 180,
    filename: `${id}.flac`,
    ref: { track_id: id }
  };
}

function appleItem(songId: string, title: string): QueueItem {
  return {
    title,
    artist: 'Apple Artist',
    album: 'Apple Album',
    durationSecs: 200,
    filename: `apple_music:${songId}`,
    imageUrl: `https://example.test/${songId}.jpg`,
    resolvedSource: {
      kind: 'apple_music_track',
      song_id: songId,
      storefront: 'nz',
      title,
      artist: 'Apple Artist',
      album: 'Apple Album',
      artwork_url: `https://example.test/${songId}.jpg`,
      duration_secs: 200
    }
  };
}

function sourceKeys(body: unknown) {
  const queue = (body as { queue?: SourceRef[] }).queue || [];
  return queue.map((source) =>
    source.kind === 'apple_music_track'
      ? `apple:${source.song_id}`
      : `local:${String(source.track_id)}`
  );
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function renderQueueHook() {
  return renderHook(() =>
    usePlaybackQueue({
      activeZoneId: 'local-core',
      refreshRecentlyPlayed: vi.fn().mockResolvedValue(undefined),
      setNotice: vi.fn(),
      setSignalOpen: vi.fn(),
      status: {
        state: 'Stopped',
        file_name: '',
        current_source: null,
        transport_pending: 'none'
      },
      tracks: []
    })
  );
}

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.apiGet.mockResolvedValue({});
  mocks.saveNowPlayingQueue.mockResolvedValue({});
  mocks.nowPlayingQueue.mockResolvedValue({
    state: {
      kind: 'local',
      cursor: 0,
      items: [localItem(1, 'Current'), localItem(2, 'Beta'), localItem(3, 'Gamma')],
      loopMode: 'off'
    },
    current_source: null,
    queued_sources: []
  });
});

describe('usePlaybackQueue serialized mutations', () => {
  it('keeps the visible removal target when Apple Play next is still committing', async () => {
    const firstCommit = deferred<Record<string, never>>();
    mocks.zoneQueue.mockImplementationOnce(() => firstCommit.promise).mockResolvedValueOnce({});
    const hook = renderQueueHook();
    await waitFor(() => expect(hook.result.current.queue.items).toHaveLength(3));

    let addPromise!: Promise<boolean>;
    act(() => {
      addPromise = hook.result.current.addItemsToQueue(
        [appleItem('apple-4', 'Apple Four')],
        'next'
      );
    });
    await waitFor(() => expect(mocks.zoneQueue).toHaveBeenCalledTimes(1));
    expect(sourceKeys(mocks.zoneQueue.mock.calls[0][1])).toEqual([
      'apple:apple-4',
      'local:2',
      'local:3'
    ]);

    act(() => {
      getNowPlayingQueueActions().removeIndex?.(1);
    });
    expect(mocks.zoneQueue).toHaveBeenCalledTimes(1);

    firstCommit.resolve({});
    await expect(addPromise).resolves.toBe(true);
    await waitFor(() => expect(mocks.zoneQueue).toHaveBeenCalledTimes(2));
    expect(sourceKeys(mocks.zoneQueue.mock.calls[1][1])).toEqual(['apple:apple-4', 'local:3']);
    await waitFor(() =>
      expect(hook.result.current.queue.items.map((item) => item.title)).toEqual([
        'Current',
        'Apple Four',
        'Gamma'
      ])
    );
  });

  it('rebases a visible reorder after Apple Play next instead of writing stale indices', async () => {
    const firstCommit = deferred<Record<string, never>>();
    mocks.zoneQueue.mockImplementationOnce(() => firstCommit.promise).mockResolvedValueOnce({});
    const hook = renderQueueHook();
    await waitFor(() => expect(hook.result.current.queue.items).toHaveLength(3));

    let addPromise!: Promise<boolean>;
    act(() => {
      addPromise = hook.result.current.addItemsToQueue(
        [appleItem('apple-4', 'Apple Four')],
        'next'
      );
    });
    await waitFor(() => expect(mocks.zoneQueue).toHaveBeenCalledTimes(1));

    act(() => {
      getNowPlayingQueueActions().reorderQueue?.(2, 1);
    });
    expect(mocks.zoneQueue).toHaveBeenCalledTimes(1);

    firstCommit.resolve({});
    await expect(addPromise).resolves.toBe(true);
    await waitFor(() => expect(mocks.zoneQueue).toHaveBeenCalledTimes(2));
    expect(sourceKeys(mocks.zoneQueue.mock.calls[1][1])).toEqual([
      'apple:apple-4',
      'local:3',
      'local:2'
    ]);
    await waitFor(() =>
      expect(hook.result.current.queue.items.map((item) => item.title)).toEqual([
        'Current',
        'Apple Four',
        'Gamma',
        'Beta'
      ])
    );
  });

  it('runs clear after an in-flight Apple Play next commit', async () => {
    const firstCommit = deferred<Record<string, never>>();
    mocks.zoneQueue.mockImplementationOnce(() => firstCommit.promise).mockResolvedValueOnce({});
    const hook = renderQueueHook();
    await waitFor(() => expect(hook.result.current.queue.items).toHaveLength(3));

    let addPromise!: Promise<boolean>;
    act(() => {
      addPromise = hook.result.current.addItemsToQueue(
        [appleItem('apple-4', 'Apple Four')],
        'next'
      );
    });
    await waitFor(() => expect(mocks.zoneQueue).toHaveBeenCalledTimes(1));

    act(() => {
      hook.result.current.clearQueue();
    });
    expect(mocks.zoneQueue).toHaveBeenCalledTimes(1);

    firstCommit.resolve({});
    await expect(addPromise).resolves.toBe(true);
    await waitFor(() => expect(mocks.zoneQueue).toHaveBeenCalledTimes(2));
    expect(sourceKeys(mocks.zoneQueue.mock.calls[1][1])).toEqual([]);
    await waitFor(() =>
      expect(hook.result.current.queue.items.map((item) => item.title)).toEqual(['Current'])
    );
  });
});
