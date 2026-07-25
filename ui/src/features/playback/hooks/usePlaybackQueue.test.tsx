// @vitest-environment jsdom

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { QueueItem, SourceRef } from '../../../shared/types';
import { getNowPlayingQueueActions } from '../model/nowPlayingQueueStore';
import { playbackChromeTrackModel } from '../model/playbackChromeModel';
import {
  clearTransportPending,
  setPendingPlaybackArt,
  setPendingPlaybackIntent,
  setPlaybackLoading,
  usePlaybackControlSnapshot
} from '../model/playbackControlStore';
import { usePlaybackQueue } from './usePlaybackQueue';

const mocks = vi.hoisted(() => ({
  albumPlaySources: vi.fn(),
  apiGet: vi.fn(),
  nowPlayingQueue: vi.fn(),
  playAppleMusicScenario: vi.fn(),
  saveNowPlayingQueue: vi.fn(),
  zoneQueue: vi.fn()
}));

vi.mock('../../../shared/lib/api', () => ({
  api: {
    get: mocks.apiGet
  },
  endpoints: {
    albumPlaySources: mocks.albumPlaySources,
    artUrl: () => null,
    nowPlayingQueue: mocks.nowPlayingQueue,
    playAppleMusicScenario: mocks.playAppleMusicScenario,
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
  clearTransportPending();
  setPendingPlaybackIntent(null);
  setPendingPlaybackArt(null);
  setPlaybackLoading(false);
  mocks.albumPlaySources.mockResolvedValue({ sources: [] });
  mocks.apiGet.mockResolvedValue({});
  mocks.playAppleMusicScenario.mockResolvedValue({});
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
  it('normalizes linked Apple Music album sources before sending the play request', async () => {
    mocks.albumPlaySources.mockResolvedValue({
      sources: [
        {
          kind: 'apple_music',
          song_id: '1109715066',
          storefront: 'nz',
          title: '15 Step',
          artist: 'Radiohead',
          album: 'In Rainbows',
          album_artist: 'Radiohead',
          album_id: '1109714933',
          image_url: 'https://example.test/in-rainbows.jpg',
          duration_secs: 237,
          track_number: 1,
          disc_number: 1,
          isrc: 'GBSTK0700001'
        },
        {
          kind: 'apple_music',
          song_id: '1109715161',
          storefront: 'nz',
          title: 'Bodysnatchers',
          artist: 'Radiohead',
          album: 'In Rainbows',
          album_artist: 'Radiohead',
          album_id: '1109714933',
          image_url: 'https://example.test/in-rainbows.jpg',
          duration_secs: 242,
          track_number: 2,
          disc_number: 1
        }
      ]
    });
    const hook = renderQueueHook();
    await waitFor(() => expect(hook.result.current.queue.items).toHaveLength(3));

    act(() => {
      hook.result.current.playAlbum(7, 0, false, 12);
    });

    await waitFor(() => expect(mocks.playAppleMusicScenario).toHaveBeenCalledTimes(1));
    expect(mocks.albumPlaySources).toHaveBeenCalledWith(7, 0, false, 12);
    expect(mocks.playAppleMusicScenario).toHaveBeenCalledWith(
      'local-core',
      expect.objectContaining({
        kind: 'apple_music_track',
        song_id: '1109715066',
        artwork_url: 'https://example.test/in-rainbows.jpg'
      }),
      [
        expect.objectContaining({
          kind: 'apple_music_track',
          song_id: '1109715161',
          artwork_url: 'https://example.test/in-rainbows.jpg'
        })
      ]
    );
  });

  it('keeps a newly requested Apple track visible until slow startup completes', async () => {
    const playRequest = deferred<Record<string, never>>();
    mocks.playAppleMusicScenario.mockReturnValue(playRequest.promise);
    const hook = renderQueueHook();
    const controls = renderHook(() => usePlaybackControlSnapshot());
    await waitFor(() => expect(hook.result.current.queue.items).toHaveLength(3));

    act(() => {
      hook.result.current.playItems(
        [appleItem('apple-1', 'Apple One'), appleItem('apple-2', 'Apple Two')],
        1
      );
    });

    await waitFor(() => expect(mocks.playAppleMusicScenario).toHaveBeenCalledTimes(1));
    expect(controls.result.current.pendingPlaybackIntent?.title).toBe('Apple Two');
    expect(controls.result.current.playbackLoading).toBe(true);
    expect(
      playbackChromeTrackModel({
        pendingArtSrc: controls.result.current.pendingArtSrc,
        playbackLoading: controls.result.current.playbackLoading,
        queue: hook.result.current.queue,
        status: {
          state: 'Playing',
          file_name: 'apple_music:apple-1',
          track_title: 'Apple One',
          track_artist: 'Apple Artist',
          track_album: 'Apple Album',
          current_source: appleItem('apple-1', 'Apple One').resolvedSource
        }
      }).currentTrackName
    ).toBe('Apple Two');

    playRequest.resolve({});

    await waitFor(() => expect(controls.result.current.playbackLoading).toBe(false));
    expect(controls.result.current.pendingPlaybackIntent).toBeNull();
    expect(mocks.apiGet).toHaveBeenCalledWith('/api/status', undefined, undefined, 'no-store');
  });

  it('does not let a stale queue refresh undo an Apple track selection during startup', async () => {
    const playRequest = deferred<Record<string, never>>();
    mocks.playAppleMusicScenario.mockReturnValue(playRequest.promise);
    const hook = renderQueueHook();
    await waitFor(() => expect(hook.result.current.queue.items).toHaveLength(3));

    act(() => {
      hook.result.current.playItems(
        [appleItem('apple-1', 'Apple One'), appleItem('apple-2', 'Apple Two')],
        1
      );
    });

    await waitFor(() => expect(hook.result.current.queue.cursor).toBe(1));
    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    await waitFor(() => expect(mocks.nowPlayingQueue.mock.calls.length).toBeGreaterThan(1));

    expect(hook.result.current.queue.cursor).toBe(1);
    expect(hook.result.current.queue.items[1]?.title).toBe('Apple Two');

    playRequest.resolve({});
    await waitFor(() => expect(mocks.apiGet).toHaveBeenCalled());
  });

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
