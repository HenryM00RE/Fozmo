import { afterEach, describe, expect, it, vi } from 'vitest';

class FakeAudio extends EventTarget {
  static instances: FakeAudio[] = [];

  src = '';
  currentTime = 0;
  duration = 180;
  ended = false;
  volume = 1;
  playbackRate = 1;
  preload = '';
  style: Record<string, string> = {};

  constructor() {
    super();
    FakeAudio.instances.push(this);
  }

  canPlayType() {
    return 'probably';
  }

  play() {
    return Promise.resolve();
  }

  pause() {
    this.dispatchEvent(new Event('pause'));
  }

  setAttribute() {}

  removeAttribute(name: string) {
    if (name === 'src') this.src = '';
  }

  remove() {}
}

class FakeWebSocket {
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  readyState = FakeWebSocket.OPEN;
  bufferedAmount = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor() {
    FakeWebSocket.instances.push(this);
  }

  send() {}
  close() {}
}

function installBrowserFakes() {
  FakeAudio.instances = [];
  FakeWebSocket.instances = [];
  vi.stubGlobal('Audio', FakeAudio);
  vi.stubGlobal('WebSocket', FakeWebSocket);
  vi.stubGlobal('navigator', { userAgent: 'Chrome/1 Macintosh', maxTouchPoints: 0 });
  vi.stubGlobal('document', {
    body: { appendChild() {} },
    createElement: () => new FakeAudio(),
    addEventListener() {},
    visibilityState: 'visible'
  });
  vi.stubGlobal('window', {
    location: { protocol: 'http:', host: 'localhost:3001', href: 'http://localhost:3001/' },
    addEventListener() {},
    dispatchEvent() {},
    setInterval: () => 1,
    clearInterval() {},
    setTimeout,
    clearTimeout
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe('browser zone live EQ stream replacement', () => {
  it('ignores error and ended events from the deliberately retired stream', async () => {
    installBrowserFakes();
    const agent = await import('./browserZoneAgent');
    agent.initBrowserZoneAgent();

    const socket = FakeWebSocket.instances[0];
    expect(socket).toBeDefined();
    socket?.onopen?.();
    socket?.onmessage?.({
      data: JSON.stringify({
        type: 'play_source',
        source_ref: { kind: 'local_track', track_id: 1, file_name: 'one.flac', title: 'One' },
        queue: [{ kind: 'local_track', track_id: 2, file_name: 'two.flac', title: 'Two' }],
        playback_config: { eq: { enabled: false, preamp_db: 0, bands: [] } }
      })
    });

    const original = FakeAudio.instances.at(-1);
    expect(original?.src).toContain('/api/stream/local/1');
    if (original) original.currentTime = 30;
    socket?.onmessage?.({
      data: JSON.stringify({
        type: 'set_playback_config',
        playback_config: {
          eq: {
            enabled: true,
            preamp_db: 0,
            bands: [{ enabled: true, type: 'peaking', freq_hz: 1000, gain_db: 3, q: 1 }]
          }
        }
      })
    });

    const replacement = FakeAudio.instances.at(-1);
    expect(replacement).not.toBe(original);
    expect(replacement?.src).toContain('/api/stream/local/1');
    original?.dispatchEvent(new Event('error'));
    original?.dispatchEvent(new Event('ended'));

    expect(agent.getBrowserZoneSnapshot().playback.trackTitle).toBe('One');
  });
});
