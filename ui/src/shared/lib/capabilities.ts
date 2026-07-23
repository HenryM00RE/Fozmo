import type { JsonRecord, ZoneProfile } from '../types';

export type BuildCapabilityKey =
  | 'local_library'
  | 'qobuz'
  | 'pcm_output'
  | 'airplay2'
  | 'asio'
  | 'apple_music_capture'
  | 'apple_music_musickit'
  | 'sonos'
  | 'hegel'
  | 'upnp'
  | 'experimental_dsd256';

export type BuildCapabilities = Record<BuildCapabilityKey, boolean>;

const defaultCapabilities: BuildCapabilities = {
  local_library: true,
  qobuz: true,
  pcm_output: true,
  airplay2: false,
  asio: false,
  apple_music_capture: false,
  apple_music_musickit: false,
  sonos: false,
  hegel: false,
  upnp: false,
  experimental_dsd256: false
};

export function buildCapabilities(status: JsonRecord | null | undefined): BuildCapabilities {
  const raw = status?.capabilities;
  if (!raw || typeof raw !== 'object') return defaultCapabilities;
  const value = raw as JsonRecord;
  return Object.fromEntries(
    Object.entries(defaultCapabilities).map(([key, fallback]) => [
      key,
      typeof value[key] === 'boolean' ? value[key] : fallback
    ])
  ) as BuildCapabilities;
}

export function capabilityEnabled(status: JsonRecord | null | undefined, key: BuildCapabilityKey) {
  return buildCapabilities(status)[key];
}

export function zoneAvailableForCapabilities(zone: ZoneProfile, capabilities: BuildCapabilities) {
  const protocol = String(zone.protocol || '');
  const backend = String(zone.backend || '');
  const deviceName = String(zone.device_name || '');
  // Remote agents advertise the capabilities of their own build and host.
  // Applying the core's build flags here hides valid outputs such as a
  // Windows agent's ASIO devices when the core itself was built on Linux.
  if (protocol === 'remote_agent') return true;
  if (protocol === 'airplay2') return capabilities.airplay2;
  if (protocol === 'sonos_upnp' || backend === 'sonos') return capabilities.sonos;
  if (protocol === 'upnp_av_renderer' || backend === 'upnp') return capabilities.upnp;
  if (protocol === 'asio_output' || backend === 'asio' || deviceName.startsWith('ASIO: ')) {
    return capabilities.asio;
  }
  return true;
}

export function filterZonesByCapabilities(
  zones: ZoneProfile[],
  status: JsonRecord | null | undefined
) {
  const capabilities = buildCapabilities(status);
  return zones.filter((zone) => zoneAvailableForCapabilities(zone, capabilities));
}
