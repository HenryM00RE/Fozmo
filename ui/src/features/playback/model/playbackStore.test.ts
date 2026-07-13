import { beforeEach, describe, expect, it, vi } from 'vitest';

const apiMock = vi.hoisted(() => ({ get: vi.fn() }));

vi.mock('../../../shared/lib/api', () => ({ api: apiMock }));

class MockSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: MockSocket[] = [];

  readyState = MockSocket.CONNECTING;
  listeners = new Map<string, Array<(event: { data?: string }) => void>>();

  constructor(_url: string) {
    MockSocket.instances.push(this);
  }

  addEventListener(name: string, listener: (event: { data?: string }) => void) {
    const listeners = this.listeners.get(name) || [];
    listeners.push(listener);
    this.listeners.set(name, listeners);
  }

  dispatch(name: string, event: { data?: string } = {}) {
    for (const listener of this.listeners.get(name) || []) listener(event);
  }

  open() {
    this.readyState = MockSocket.OPEN;
    this.dispatch('open');
  }

  message(value: unknown) {
    this.dispatch('message', { data: JSON.stringify(value) });
  }

  close() {
    this.readyState = MockSocket.CLOSED;
    this.dispatch('close');
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe('playback status ordering', () => {
  beforeEach(() => {
    vi.resetModules();
    apiMock.get.mockReset();
    MockSocket.instances = [];
    vi.stubGlobal('WebSocket', MockSocket);
    vi.stubGlobal('window', {
      location: { protocol: 'https:', host: 'fozmo.test' },
      setTimeout: () => 1,
      clearTimeout: () => undefined,
      setInterval: () => 1,
      clearInterval: () => undefined,
      addEventListener: () => undefined
    });
    vi.stubGlobal('document', {
      visibilityState: 'visible',
      addEventListener: () => undefined
    });
  });

  it('does not let an older HTTP poll overwrite a websocket message', async () => {
    const request = deferred<Record<string, unknown>>();
    apiMock.get.mockReturnValue(request.promise);
    const store = await import('./playbackStore');
    const unsubscribe = store.subscribePlayback(() => undefined);
    const socket = MockSocket.instances[0];
    socket.open();
    socket.message({ state: 'Playing', track_title: 'New Song' });

    request.resolve({ state: 'Playing', track_title: 'Old Song' });
    await request.promise;
    await Promise.resolve();

    expect(store.getPlaybackSnapshot().status.track_title).toBe('New Song');
    unsubscribe();
  });

  it('ignores a replaced socket closing after the new socket connects', async () => {
    apiMock.get.mockResolvedValue({ state: 'Stopped' });
    const store = await import('./playbackStore');
    const unsubscribe = store.subscribePlayback(() => undefined);
    const first = MockSocket.instances[0];
    first.open();

    store.reconnectPlaybackStore();
    const replacement = MockSocket.instances[1];
    replacement.open();
    replacement.message({ state: 'Playing', track_title: 'Replacement Song' });
    first.dispatch('close');

    expect(MockSocket.instances).toHaveLength(2);
    expect(store.getPlaybackSnapshot().connection).toBe('connected');
    expect(store.getPlaybackSnapshot().status.track_title).toBe('Replacement Song');
    unsubscribe();
  });
});
