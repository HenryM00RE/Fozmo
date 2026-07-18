import type { Page, Route } from '@playwright/test';
import { validateContractResponse } from './contractValidator';

type JsonRecord = Record<string, unknown>;

export type ApiCall = {
  method: string;
  path: string;
  body: unknown;
};

type Failure = {
  status: number;
  body?: unknown;
  abort?: boolean;
};

type MockBackendOptions = {
  status?: JsonRecord;
  zoneStatus?: JsonRecord;
  zoneStatusDelayMs?: number;
  zones?: JsonRecord[];
  qobuzStatus?: JsonRecord;
  qobuzHome?: JsonRecord;
  queueState?: JsonRecord;
  websocketMode?: 'status' | 'error' | 'close';
  websocketIntervalMs?: number;
  failures?: Record<string, Failure>;
};

const defaultZone = {
  id: 'local-core',
  name: 'Studio',
  protocol: 'local_core_audio',
  backend: 'coreaudio',
  capabilities: {
    max_sample_rate: 192000,
    max_bit_depth: 24,
    exclusive_supported: true,
    gapless_supported: true
  },
  dsp_profile: {
    upsampling_enabled: true,
    filter_type: 'Split128k',
    target_rate: 176400,
    dither_mode: 'Auto'
  },
  status: 'active',
  enabled: true,
  device_name: 'Studio DAC'
};

export const fixtures = {
  status: {
    surface: 'local',
    capabilities: {
      airplay2: false,
      apple_music_capture: false,
      asio: false,
      experimental_dsd256: false,
      hegel: true,
      local_library: true,
      pcm_output: true,
      qobuz: true,
      sonos: true,
      upnp: true
    },
    airplay_helper_state: 'ready',
    state: 'Playing',
    file_name: 'midnight-city.flac',
    current_source: {
      kind: 'local_track',
      track_id: 1,
      file_name: 'midnight-city.flac',
      title: 'Midnight City',
      artist: 'M83',
      album: 'Hurry Up, We Are Dreaming',
      album_id: 1,
      duration_secs: 243
    },
    track_title: 'Midnight City',
    track_artist: 'M83',
    track_album: 'Hurry Up, We Are Dreaming',
    cover_version: 0,
    position_secs: 42,
    duration_secs: 243,
    active_zone_id: 'local-core',
    active_zone_name: 'Studio',
    selected_device: 'Studio DAC',
    zone_protocol: 'local_core_audio',
    source_rate: 44100,
    source_bits: 16,
    target_rate: 176400,
    target_bits: 24,
    configured_target_rate: 176400,
    configured_target_bit_depth: 24,
    upnp_config_applied_to_current_playback: true,
    upnp_restart_pending: false,
    upnp_render_status: 'idle',
    transport_pending: 'none',
    upsampling_enabled: true,
    exclusive: true,
    filter_type: 'Split128k',
    active_filter_type: 'Split128k',
    dither_mode: 'Auto',
    output_mode: 'Pcm',
    active_output_mode: 'Pcm',
    dsd_modulator: 'Standard',
    dsd_isi_penalty: 0,
    src_capped_fallback: false,
    src_phase_profile_preserved: true,
    src_ratio_num: 4,
    src_ratio_den: 1,
    dsd_rules_enabled: false,
    dsd_rules: [],
    dsd_stability_resets: 0,
    output_transport: 'coreaudio',
    output_notice_id: 0,
    resample_time_ns: 0,
    dsd_upsample_time_ns: 0,
    dsd_modulate_time_ns: 0,
    dsd_output_pending_samples: 0,
    dsd_overbudget_blocks: 0,
    dsd_last_load: 0,
    dsd_recent_load_p95: 0,
    dsd_recent_load_p99: 0,
    dop_ring_capacity_ms: 0,
    dop_ring_fill_ms: 0,
    dop_ring_low_watermark_ms: 0,
    dop_callback_frames: 0,
    dop_callback_ms: 0,
    dop_requested_hardware_buffer_frames: 0,
    dop_requested_hardware_buffer_ms: 0,
    dop_hardware_buffer_min_frames: 0,
    dop_hardware_buffer_max_frames: 0,
    dop_hardware_buffer_frames: 0,
    dop_hardware_buffer_ms: 0,
    dop_lock_miss_events: 0,
    dop_callback_deadline_miss_events: 0,
    dop_soft_callback_gap_125_events: 0,
    dop_soft_callback_gap_150_events: 0,
    dop_soft_callback_gap_175_events: 0,
    dop_last_soft_callback_gap_ms: 0,
    dop_last_soft_callback_gap_at_ms: 0,
    dop_ring_below_250ms_events: 0,
    dop_ring_below_100ms_events: 0,
    dop_ring_below_50ms_events: 0,
    dop_ring_below_callback_events: 0,
    dop_last_ring_pressure_at_ms: 0,
    dop_marker_error_events: 0,
    dop_program_idle_splice_events: 0,
    dop_program_to_idle_events: 0,
    dop_idle_to_program_events: 0,
    dop_mixed_output_events: 0,
    dop_last_output_transition_id: 0,
    dop_last_output_transition_at_ms: 0,
    dop_repeated_payload_events: 0,
    dop_callback_index: 0,
    dop_last_callback_at_ms: 0,
    dop_last_callback_gap_ms: 0,
    dop_last_callback_frames: 0,
    dop_last_output_kind_id: 0,
    dop_last_ring_fill_samples: 0,
    dop_last_program_read_samples: 0,
    dop_ring_read_cursor_samples: 0,
    dop_last_payload_fingerprint: 0,
    dop_last_payload_fingerprint_at_ms: 0,
    dop_marker_scan_count: 0,
    dop_every_callback_scan_enabled: false,
    dop_last_underrun_at_ms: 0,
    output_ring_fill_now_ms: 0,
    output_ring_fill_min_ms: 0,
    startup_ring_low_watermark_ms: 0,
    startup_ready_ms: 0,
    startup_first_render_block_ms: 0,
    startup_producer_over_budget_count: 0,
    startup_callback_gaps_ms: [],
    underrun_count: 0,
    producer_over_budget_count: 0,
    max_render_block_ms: 0,
    max_audio_callback_gap_ms: 0,
    dsp_graph_rebuild_count: 0,
    sample_rate_change_count: 0,
    dop_alignment_reset_count: 0,
    coreaudio_dop_open_count: 0,
    coreaudio_dop_start_count: 0,
    coreaudio_dop_stop_count: 0,
    coreaudio_dop_drop_count: 0,
    coreaudio_dop_quiesce_count: 0,
    coreaudio_dop_last_lifecycle_event_id: 0,
    coreaudio_dop_last_lifecycle_at_ms: 0,
    reopen_reason_count: 0,
    last_reopen_reason_id: 0,
    last_reopen_reason_at_ms: 0,
    flush_reason_count: 0,
    last_flush_reason_id: 0,
    last_flush_reason_at_ms: 0,
    modulator_reset_count: 0,
    decoder_starved_count: 0,
    source_read_time_ms: 0,
    max_source_read_ms: 0,
    source_read_stall_count: 0,
    source_read_stall_last_at_ms: 0,
    decoder_decode_time_ms: 0,
    max_decoder_decode_ms: 0,
    decoder_decode_stall_count: 0,
    decoder_decode_stall_last_at_ms: 0,
    lock_wait_max_ms: 0,
    block_duration_ns: 0,
    cpu_percent: 0,
    meter_l: 0,
    meter_r: 0,
    signal_peak: 0,
    signal_peak_max: 0,
    signal_clipping: false,
    signal_clip_events: 0,
    signal_clip_samples: 0,
    dsd_limiter_peak_ratio: 0,
    dsd_limiter_peak_ratio_max: 0,
    dsd_limiter_active: false,
    dsd_limiter_events: 0,
    dsd_limiter_samples: 0,
    underrun_events: 0,
    underrun_samples: 0,
    headroom_db: -4,
    dsp_buffer_ms: 250,
    volume: 0.72,
    device_volume: 0.72,
    device_volume_supported: true,
    remote_connected: false
  },
  queueState: {
    kind: 'local',
    cursor: 0,
    loopMode: 'off',
    items: [
      {
        title: 'Midnight City',
        artist: 'M83',
        album: 'Hurry Up, We Are Dreaming',
        durationSecs: 243,
        filename: 'midnight-city.flac',
        ref: { track_id: 1, file_name: 'midnight-city.flac' }
      },
      {
        title: 'Reunion',
        artist: 'M83',
        album: 'Hurry Up, We Are Dreaming',
        durationSecs: 235,
        filename: 'reunion.flac',
        ref: { track_id: 2, file_name: 'reunion.flac' }
      },
      {
        title: 'Steve McQueen',
        artist: 'M83',
        album: 'Hurry Up, We Are Dreaming',
        durationSecs: 229,
        filename: 'steve-mcqueen.flac',
        ref: { track_id: 3, file_name: 'steve-mcqueen.flac' }
      }
    ]
  },
  qobuzLoggedOut: {
    initialized: true,
    logged_in: false,
    authenticated: false,
    user: null,
    radio_enabled: true
  },
  qobuzLoggedIn: {
    initialized: true,
    logged_in: true,
    authenticated: true,
    user: {
      email: 'listener@example.test',
      display_name: 'Casey Listener',
      subscription_label: 'Studio Premier'
    },
    radio_enabled: true
  },
  qobuzHome: {
    logged_in: true,
    partial_errors: [],
    sections: [
      {
        id: 'new-releases',
        title: 'New releases',
        item_type: 'albums',
        albums: [
          {
            id: 'qobuz-album-1',
            title: 'Test Pressing',
            artist: 'Fixture Artist',
            image_url: null,
            year: 2026,
            hires: true
          }
        ]
      }
    ]
  }
};

export async function installMockBackend(page: Page, options: MockBackendOptions = {}) {
  const status = { ...fixtures.status, ...(options.status || {}) };
  let zoneStatus = { ...status, ...(options.zoneStatus || {}) };
  const zones = (options.zones || [defaultZone]).map((zone) => ({ ...zone }));
  const queueState = options.queueState || fixtures.queueState;
  const qobuzStatus = options.qobuzStatus || fixtures.qobuzLoggedOut;
  const qobuzHome = options.qobuzHome || fixtures.qobuzHome;
  const websocketMode = options.websocketMode || 'status';
  const websocketIntervalMs = options.websocketIntervalMs || 0;
  const failures = options.failures || {};
  const calls: ApiCall[] = [];

  validateContractResponse('GET', '/api/ws', status);

  await page.addInitScript(
    ({ status, websocketMode, websocketIntervalMs }) => {
      window.localStorage.setItem('fozmoGettingStartedV2Complete', '1');

      class MockWebSocket extends EventTarget {
        static CONNECTING = 0;
        static OPEN = 1;
        static CLOSING = 2;
        static CLOSED = 3;
        readyState = MockWebSocket.CONNECTING;
        url: string;
        onopen: ((event: Event) => void) | null = null;
        onmessage: ((event: MessageEvent) => void) | null = null;
        onerror: ((event: Event) => void) | null = null;
        onclose: ((event: CloseEvent) => void) | null = null;
        timer = 0;

        constructor(url: string) {
          super();
          this.url = url;
          window.setTimeout(() => this.open(), 0);
        }

        open() {
          if (websocketMode === 'error') {
            const event = new Event('error');
            this.onerror?.(event);
            this.dispatchEvent(event);
            return;
          }
          if (websocketMode === 'close') {
            this.readyState = MockWebSocket.CLOSED;
            const event = new CloseEvent('close');
            this.onclose?.(event);
            this.dispatchEvent(event);
            return;
          }
          this.readyState = MockWebSocket.OPEN;
          const openEvent = new Event('open');
          this.onopen?.(openEvent);
          this.dispatchEvent(openEvent);
          const sendStatus = () => {
            const messageEvent = new MessageEvent('message', {
              data: JSON.stringify(status)
            });
            this.onmessage?.(messageEvent);
            this.dispatchEvent(messageEvent);
          };
          sendStatus();
          if (websocketIntervalMs > 0) {
            this.timer = window.setInterval(sendStatus, websocketIntervalMs);
          }
        }

        send(value: string) {
          const target = window as typeof window & { __fozmoWsMessages?: string[] };
          target.__fozmoWsMessages = target.__fozmoWsMessages || [];
          target.__fozmoWsMessages.push(value);
        }

        close() {
          window.clearInterval(this.timer);
          this.readyState = MockWebSocket.CLOSED;
          const event = new CloseEvent('close');
          this.onclose?.(event);
          this.dispatchEvent(event);
        }
      }

      Object.defineProperty(window, 'WebSocket', {
        configurable: true,
        writable: true,
        value: MockWebSocket
      });
    },
    { status, websocketMode, websocketIntervalMs }
  );

  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const path = url.pathname;
    if (!path.startsWith('/api/')) {
      await route.continue();
      return;
    }
    const body = parseBody(request.postData());
    calls.push({ method, path, body });

    const failure = failures[`${method} ${path}`] || failures[path];
    if (failure?.abort) {
      await route.abort('failed');
      return;
    }
    if (failure) {
      await json(route, failure.body ?? { error: 'Fixture failure' }, failure.status);
      return;
    }

    if (method === 'GET' && path === '/api/status') return json(route, status);
    if (method === 'GET' && path === '/api/zones') return json(route, zones);
    if (method === 'GET' && path === '/api/profiles') {
      return json(route, {
        profiles: [{ id: 'default', name: 'Default', color: '#7c8f6a' }],
        active_profile_id: 'default'
      });
    }
    if (method === 'GET' && path === '/api/library/summary') {
      return json(route, { albums: 1, artists: 1, tracks: 3, unmatched_albums: 0 });
    }
    if (method === 'GET' && path === '/api/library/albums') {
      return json(route, [
        {
          id: 1,
          title: 'Hurry Up, We Are Dreaming',
          album_artist: 'M83',
          track_count: 3,
          confidence: 100,
          match_status: 'matched'
        }
      ]);
    }
    if (method === 'GET' && path === '/api/library/tracks') {
      return json(route, [
        {
          id: 1,
          file_name: 'midnight-city.flac',
          title: 'Midnight City',
          artist: 'M83',
          album: 'Hurry Up, We Are Dreaming',
          duration_secs: 243,
          album_id: 1,
          play_count: 0,
          listened_secs: 0
        }
      ]);
    }
    if (method === 'GET' && path === '/api/library/artists') {
      return json(route, [
        { name: 'M83', album_count: 1, track_count: 3, play_count: 0, listened_secs: 0 }
      ]);
    }
    if (method === 'GET' && path.startsWith('/api/library/browse/')) {
      return json(route, { items: [], total: 0, limit: 50, offset: 0, has_more: false });
    }
    if (method === 'GET' && path === '/api/library/search') {
      return json(route, { query: url.searchParams.get('q') || '', albums: [], artists: [], tracks: [] });
    }
    if (method === 'GET' && path === '/api/library/folders') return json(route, { folders: [] });
    if (method === 'GET' && path === '/api/library/recent-albums') return json(route, []);
    if (method === 'GET' && path === '/api/history/stats') return json(route, emptyHistoryStats());
    if (method === 'GET' && path === '/api/history/recent') return json(route, []);
    if (method === 'GET' && path === '/api/playlists') return json(route, []);
    if (method === 'GET' && path === '/api/playlists/recent') return json(route, []);
    if (method === 'GET' && path === '/api/qobuz/status') return json(route, qobuzStatus);
    if (method === 'GET' && path === '/api/qobuz/search') return json(route, { tracks: [] });
    if (method === 'GET' && path === '/api/qobuz/home') return json(route, qobuzHome);
    if (method === 'GET' && path === '/api/qobuz/home/album-of-the-week') return json(route, qobuzHome);
    if (method === 'GET' && path === '/api/qobuz/cache') return json(route, { bytes: 0, files: 0 });
    if (method === 'GET' && path === '/api/lastfm/status') return json(route, { configured: false });
    if (method === 'GET' && path === '/api/eq') return json(route, { enabled: false, bands: [] });
    if (method === 'GET' && path === '/api/eq/presets') return json(route, []);
    if (method === 'GET' && path.endsWith('/now-playing-queue')) {
      return json(route, {
        state: queueState,
        current_source: status.current_source,
        queued_sources: queueSources(queueState)
      });
    }
    if (method === 'POST' && path === '/api/qobuz/logout') {
      return json(route, fixtures.qobuzLoggedOut);
    }
    if (method === 'POST' && path === '/api/pairing/start') {
      return json(route, {
        token: 'fixture-token',
        auth_required: false,
        expires_at_unix_secs: 0,
        token_kind: 'pairing_token',
        scopes: ['session:create']
      });
    }
    if (method === 'POST' && path === '/api/sessions/browser') {
      return json(route, {
        auth_required: false,
        expires_at_unix_secs: 0,
        token_kind: 'control_session',
        scopes: ['control']
      });
    }
    if (method === 'POST' && path === '/api/agents/token') {
      return json(route, {
        token: 'fixture-agent-token',
        auth_required: false,
        expires_at_unix_secs: 0,
        token_kind: 'agent_token',
        scopes: ['agent:connect', 'stream:read']
      });
    }
    if (method === 'POST' && path.startsWith('/api/pairing/revoke-')) {
      return json(route, { revoked: 1 });
    }
    if (method === 'POST' && path === '/api/config') return json(route, { ok: true });
    const zoneSettingsMatch = path.match(/^\/api\/zones\/([^/]+)\/settings$/);
    if (method === 'POST' && zoneSettingsMatch) {
      const zoneId = decodeURIComponent(zoneSettingsMatch[1]);
      const zone = zones.find((candidate) => candidate.id === zoneId);
      if (zone && body && typeof body === 'object') Object.assign(zone, body);
      return json(route, zone || {});
    }
    if (method === 'POST' && path.endsWith('/config')) return json(route, status);
    if (method === 'GET' && path.endsWith('/status')) {
      if (options.zoneStatusDelayMs) {
        await new Promise((resolve) => setTimeout(resolve, options.zoneStatusDelayMs));
      }
      return json(route, zoneStatus);
    }
    if (method === 'POST' || method === 'PUT' || method === 'DELETE') return json(route, {});
    return json(route, {});
  });

  return {
    calls,
    setZoneStatus(next: JsonRecord) {
      zoneStatus = { ...zoneStatus, ...next };
    }
  };
}

export async function waitForApiCall(
  page: Page,
  calls: ApiCall[],
  predicate: (call: ApiCall) => boolean
) {
  const deadline = Date.now() + 7_500;
  while (Date.now() < deadline) {
    if (calls.some(predicate)) return;
    await page.waitForTimeout(50);
  }
  throw new Error(`Timed out waiting for API call. Seen: ${JSON.stringify(calls, null, 2)}`);
}

export function lastApiCall(calls: ApiCall[], predicate: (call: ApiCall) => boolean) {
  return calls.filter(predicate).at(-1);
}

function parseBody(data: string | null) {
  if (!data) return null;
  try {
    return JSON.parse(data);
  } catch {
    return data;
  }
}

async function json(route: Route, body: unknown, status = 200) {
  if (status >= 200 && status < 300) {
    const request = route.request();
    const url = new URL(request.url());
    validateContractResponse(request.method(), url.pathname, body);
  }
  await route.fulfill({
    status,
    contentType: 'application/json',
    body: JSON.stringify(body)
  });
}

function emptyHistoryStats() {
  return {
    range: '30d',
    total_listened_secs: 0,
    weekly_buckets: [],
    weekday_buckets: [],
    top_artists: [],
    top_albums: [],
    top_songs: [],
    top_genres: [],
    recent_tracks: []
  };
}

function queueSources(queueState: JsonRecord) {
  const items = Array.isArray(queueState.items) ? queueState.items : [];
  return items
    .map((item) => {
      const record = item && typeof item === 'object' ? (item as JsonRecord) : {};
      const ref = record.ref && typeof record.ref === 'object' ? (record.ref as JsonRecord) : {};
      const trackId = Number(ref.track_id);
      const fileName = String(ref.file_name || record.filename || '');
      if (!Number.isFinite(trackId) || trackId <= 0) return null;
      return {
        kind: 'local_track',
        track_id: trackId,
        file_name: fileName,
        title: String(record.title || `Track ${trackId}`),
        artist: String(record.artist || ''),
        album: String(record.album || ''),
        duration_secs: Number(record.durationSecs || 0)
      };
    })
    .filter(Boolean);
}
