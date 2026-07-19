import { endpoints } from '../../shared/lib/api';
import { safeArray } from '../../shared/lib/appSupport';
import { browserZoneDisplayName, isBrowserZone } from '../../shared/lib/browserZone';
import { capabilityEnabled } from '../../shared/lib/capabilities';
import type { JsonRecord, ZoneProfile } from '../../shared/types';

export type SettingsTabId =
  | 'general'
  | 'zones'
  | 'dsp'
  | 'eq'
  | 'qobuz'
  | 'apple-music'
  | 'metabrainz'
  | 'remote'
  | 'profiles';

export const settingsSections: Array<{ id: SettingsTabId; label: string; path: string }> = [
  { id: 'general', label: 'General', path: 'M4 6h16M4 12h10M4 18h7' },
  { id: 'zones', label: 'Outputs', path: 'M7 7h10v10H7zM2 12h5M17 12h5M12 2v5M12 17v5' },
  { id: 'dsp', label: 'DSP', path: 'M4 7h10M4 17h10M18 5v4M18 15v4M14 7h8M14 17h8' },
  { id: 'eq', label: 'EQ', path: 'M5 20V10M12 20V4M19 20v-7' },
  { id: 'qobuz', label: 'Services', path: 'M12 3 4 7l8 4 8-4-8-4M4 12l8 4 8-4M4 17l8 4 8-4' },
  {
    id: 'apple-music',
    label: 'Apple Music',
    path: 'M9 18V5l12-2v13M9 18a3 3 0 1 1-2-2.83M21 16a3 3 0 1 1-2-2.83M9 9l12-2'
  },
  {
    id: 'metabrainz',
    label: 'Metadata',
    path: 'M20 10 10 20a2 2 0 0 1-2.8 0L4 16.8a2 2 0 0 1 0-2.8L14 4h6v6Z M17 7.5h.01'
  },
  {
    id: 'profiles',
    label: 'Profiles',
    path: 'M18 21a6 6 0 0 0-12 0M12 13a5 5 0 1 0 0-10 5 5 0 0 0 0 10'
  },
  {
    id: 'remote',
    label: 'Remote Access',
    path: 'M12 3a9 9 0 0 0-9 9h18a9 9 0 0 0-9-9ZM3 12a9 9 0 0 0 18 0M12 3c2.5 2.4 3.8 5.4 3.8 9s-1.3 6.6-3.8 9M12 3c-2.5 2.4-3.8 5.4-3.8 9s1.3 6.6 3.8 9'
  }
];

export function visibleSettingsSections(status: JsonRecord | null | undefined) {
  return settingsSections.filter((section) => {
    if (section.id === 'qobuz') return capabilityEnabled(status, 'qobuz');
    if (section.id === 'apple-music') {
      return capabilityEnabled(status, 'apple_music_capture');
    }
    return true;
  });
}

export function settingsTabFromValue(
  value: unknown,
  fallback: SettingsTabId = 'general',
  status?: JsonRecord | null
): SettingsTabId {
  if (value === 'apple_music' || value === 'appleMusic') {
    return settingsTabFromValue('apple-music', fallback, status);
  }
  if (value === 'media' || value === 'appearance' || value === 'data') return 'general';
  return visibleSettingsSections(status).some((section) => section.id === value)
    ? (value as SettingsTabId)
    : fallback;
}

export type ProfilesResponse = {
  profiles?: JsonRecord[];
  active_profile_id?: string;
};

export type ApplyProfilesResponse = (data: ProfilesResponse, preferredProfileId?: string) => void;

export type ProfilesState = {
  profiles: JsonRecord[];
  activeProfileId: string;
};

export type SettingsSupportData = {
  zones?: ZoneProfile[];
  profilesResponse?: ProfilesResponse;
};

export const PROFILE_COLORS = [
  { value: '#4f84a5', label: 'Blue' },
  { value: '#59806c', label: 'Green' },
  { value: '#7c8f6a', label: 'Sage' },
  { value: '#b08a3c', label: 'Gold' },
  { value: '#b76450', label: 'Clay' },
  { value: '#9a6b9b', label: 'Plum' },
  { value: '#6372a0', label: 'Indigo' },
  { value: '#8a6f50', label: 'Walnut' }
] as const;

function settledValue<T>(result: PromiseSettledResult<T>) {
  return result.status === 'fulfilled' ? result.value : undefined;
}

export function profilesStateFromResponse(data: ProfilesResponse): ProfilesState {
  const profiles = Array.isArray(data.profiles) ? data.profiles : [];
  return {
    profiles,
    activeProfileId: data.active_profile_id || String(profiles[0]?.id || 'default')
  };
}

export async function loadSettingsSupportData(): Promise<SettingsSupportData> {
  const [zonesResult, profilesResult] = await Promise.allSettled([
    endpoints.zones(),
    endpoints.profiles()
  ]);

  return {
    zones: settledValue(zonesResult),
    profilesResponse: settledValue(profilesResult)
  };
}

export const filterOptions = [
  ['LinearPhase128k', 'Linear Phase'],
  ['MinimumPhaseCompact128k', 'Minimum Phase'],
  ['SplitPhase128kE2v3', 'Split Phase'],
  ['SmoothPhase128k', 'Smooth Phase']
] as const;

export const dsdModulatorOptions = [
  ['Standard', '7th Order'],
  ['EcBeam2', '7th Order Search']
] as const;

export const legacyFilterIds = [
  'SincExtreme32k',
  'Linear',
  'Mixed16k',
  'Perfect',
  'SincMedium',
  'SincExperimental1m',
  'Split16k',
  'Split16kDsd128',
  'Split32k'
] as const;

const legacyMinimumFilterIds = [
  'Minimum16k',
  'MinimumPhase128k',
  'MinimumPhase128kV2',
  'MinimumPhase128kV3',
  'MinimumPhase128kV4'
] as const;

const hiddenFilterIds = [
  'MinimumPhaseCompact128kV2',
  'Split128k',
  'Split128kV2',
  'SplitPhase128kV3',
  'SplitPhase128kV4'
] as const;

const visibleFilterIds = new Set<string>(filterOptions.map(([value]) => value));
export const knownFilterIds = new Set<string>([
  ...filterOptions.map(([value]) => value),
  ...hiddenFilterIds,
  ...legacyFilterIds,
  ...legacyMinimumFilterIds
]);

export function visibleFilterType(name: unknown) {
  const key = stringValue(name, 'SplitPhase128kE2v3');
  if (key === 'SincExtreme32k') return 'LinearPhase128k';
  if (key === 'MinimumPhaseCompact128kV2') return 'MinimumPhaseCompact128k';
  if (
    key === 'Split128k' ||
    key === 'Split128kV2' ||
    key === 'SplitPhase128kV3' ||
    key === 'SplitPhase128kV4'
  )
    return 'SplitPhase128kE2v3';
  if (legacyMinimumFilterIds.includes(key as (typeof legacyMinimumFilterIds)[number]))
    return 'MinimumPhaseCompact128k';
  if (legacyFilterIds.includes(key as (typeof legacyFilterIds)[number]))
    return 'SplitPhase128kE2v3';
  if (visibleFilterIds.has(key)) return key;
  return 'SplitPhase128kE2v3';
}

const knownDsdModulatorIds = new Set<string>(dsdModulatorOptions.map(([value]) => value));
const defaultHeadroomDb = -4;

export function visibleDsdModulator(name: unknown) {
  const key = stringValue(name, 'Standard');
  if (knownDsdModulatorIds.has(key)) return key;
  if (
    /^(EC ?Beam ?2|ECB2|7th Order Search|7th Order Beam|7th Order ECB2(?: \(Experimental\))?)$/i.test(
      key
    )
  )
    return 'EcBeam2';
  // Collapse retired EC and EcBeam values to the selectable baseline.
  return 'Standard';
}

export function headroomAfterDsdModulatorChange(currentHeadroomDb: number, modulator: string) {
  if (modulator === 'Standard') return -4;
  if (modulator === 'EcBeam2') return -2;
  return currentHeadroomDb;
}

export function headroomAfterUpsamplingChange(
  currentHeadroomDb: number,
  enabled: boolean,
  modulator: string
) {
  return enabled ? headroomAfterDsdModulatorChange(currentHeadroomDb, modulator) : 0;
}

export function headroomLockedForDsdModulator(modulator: string) {
  return modulator === 'Standard' || modulator === 'EcBeam2';
}

export function isiPenaltyAfterDsdModulatorChange(currentIsiPenalty: number, modulator: string) {
  return modulator === 'EcBeam2' ? 0 : currentIsiPenalty;
}

export function ecBeam2FilterSupported(filterType: unknown) {
  return (
    filterType === 'Minimum16k' ||
    filterType === 'LinearPhase128k' ||
    filterType === 'MinimumPhaseCompact128k' ||
    filterType === 'MinimumPhaseCompact128kV2' ||
    filterType === 'Split128k' ||
    filterType === 'Split128kV2' ||
    filterType === 'SplitPhase128kV3' ||
    filterType === 'SplitPhase128kV4' ||
    filterType === 'SplitPhase128kE2v3' ||
    filterType === 'SmoothPhase128k'
  );
}

export function ecBeam2SelectableForDsdConfig(
  outputMode: unknown,
  filterType: unknown,
  dsdRulesEnabled: unknown,
  dsdRules: unknown
) {
  if (
    !['Dsd64', 'Dsd128', 'Dsd256'].includes(stringValue(outputMode)) ||
    !ecBeam2FilterSupported(filterType)
  )
    return false;
  if (!boolValue(dsdRulesEnabled, false)) return true;
  return safeArray<JsonRecord>(dsdRules).every(
    (rule) =>
      ['Dsd64', 'Dsd128', 'Dsd256'].includes(stringValue(rule.output_mode)) &&
      ecBeam2FilterSupported(stringValue(rule.filter_type))
  );
}

export const sampleRateOptions = [
  [0, 'Auto best'],
  [44100, '44.1 kHz'],
  [48000, '48.0 kHz'],
  [88200, '88.2 kHz'],
  [96000, '96.0 kHz'],
  [176400, '176.4 kHz'],
  [192000, '192.0 kHz'],
  [352800, '352.8 kHz'],
  [384000, '384.0 kHz']
] as const;

export const pcmBitDepthOptions = [16, 24, 32] as const;

export const dsdRateOptions = [
  { value: 'Dsd64', label: 'DSD64' },
  { value: 'Dsd128', label: 'DSD128' },
  { value: 'Dsd256', label: 'DSD256' }
] as const;

const DOP_DSD64_MIN_SAMPLE_RATE = 176_400;
const DOP_DSD128_MIN_SAMPLE_RATE = 352_800;
const DOP_DSD256_MIN_SAMPLE_RATE = 705_600;

export function isDsdOutputMode(outputMode: unknown) {
  return outputMode === 'Dsd64' || outputMode === 'Dsd128' || outputMode === 'Dsd256';
}

export function dsdRateFromOutputMode(outputMode: unknown) {
  return isDsdOutputMode(outputMode) ? outputMode : 'Dsd256';
}

export function outputModeForDsdRate(rate: unknown) {
  return isDsdOutputMode(rate) ? rate : 'Dsd256';
}

export function zoneMaxDsdRate(zone: ZoneProfile | null | undefined) {
  return numberValue((zone?.capabilities || {}).max_dsd_rate);
}

export function zoneSupportsNativeDsd(zone: ZoneProfile | null | undefined) {
  const protocol = String(zone?.protocol || '');
  const backend = String(zone?.backend || '').toLowerCase();
  const deviceName = String(zone?.device_name || zone?.name || '');
  return protocol === 'asio_output' || backend === 'asio' || deviceName.startsWith('ASIO: ');
}

function zoneSupportsDopBackend(zone: ZoneProfile | null | undefined) {
  const protocol = String(zone?.protocol || '');
  const backend = String(zone?.backend || '').toLowerCase();
  if (backend === 'coreaudio' || backend === 'wasapi') return true;
  return !backend && protocol === 'local_core_audio';
}

function zoneSupportsLocalDopDsdMode(
  zone: ZoneProfile | null | undefined,
  outputMode: 'Dsd64' | 'Dsd128' | 'Dsd256'
) {
  if (!zoneSupportsDopBackend(zone)) return false;
  const caps = (zone?.capabilities || {}) as JsonRecord;
  const maxSampleRate = numberValue(caps.max_sample_rate);
  const maxBitDepth = numberValue(caps.max_bit_depth);
  const maxDsdRate = zoneMaxDsdRate(zone);
  if (maxBitDepth < 24) return false;

  if (outputMode === 'Dsd64') {
    // Any DSD128-capable DoP device also accepts the lower DSD64 carrier.
    return (
      maxSampleRate >= DOP_DSD64_MIN_SAMPLE_RATE &&
      (boolValue(caps.supports_dsd128, false) || maxDsdRate >= 64)
    );
  }

  if (outputMode === 'Dsd128') {
    return (
      maxSampleRate >= DOP_DSD128_MIN_SAMPLE_RATE &&
      (boolValue(caps.supports_dsd128, false) || maxDsdRate >= 128)
    );
  }

  return (
    maxSampleRate >= DOP_DSD256_MIN_SAMPLE_RATE &&
    (boolValue(caps.supports_dsd256, false) || maxDsdRate >= 256)
  );
}

function dopCarrierRateForMode(outputMode: 'Dsd64' | 'Dsd128' | 'Dsd256') {
  if (outputMode === 'Dsd64') return 192000;
  if (outputMode === 'Dsd128') return 384000;
  return 768000;
}

function zoneSupportsUpnpDopDsdMode(
  zone: ZoneProfile | null | undefined,
  outputMode: 'Dsd64' | 'Dsd128' | 'Dsd256'
) {
  const caps = (zone?.capabilities || {}) as JsonRecord;
  const requiredRate = dopCarrierRateForMode(outputMode);
  return safeArray(caps.pcm_containers).some((entry) => {
    const capability = (entry || {}) as JsonRecord;
    return (
      String(capability.container || '') === 'wav' &&
      numberValue(capability.max_sample_rate) >= requiredRate &&
      numberValue(capability.max_bit_depth) >= 24
    );
  });
}

export function zoneSupportsDsdOutputMode(
  zone: ZoneProfile | null | undefined,
  outputMode: unknown,
  experimentalDsd256 = true
) {
  if (!isDsdOutputMode(outputMode)) return true;
  if (zoneSupportsNativeDsd(zone)) return outputMode !== 'Dsd256' || experimentalDsd256;
  if (String(zone?.protocol || '') !== 'upnp_av_renderer') {
    if (outputMode === 'Dsd256' && !experimentalDsd256) return false;
    return zoneSupportsLocalDopDsdMode(zone, outputMode);
  }
  const maxDsdRate = zoneMaxDsdRate(zone);
  if (outputMode === 'Dsd256' && !experimentalDsd256) return false;
  if (zoneSupportsUpnpDopDsdMode(zone, outputMode)) return true;
  if (outputMode === 'Dsd64') return maxDsdRate >= 64;
  if (outputMode === 'Dsd128') return maxDsdRate >= 128;
  return maxDsdRate >= 256;
}

export function defaultDsdOutputModeForZone(
  zone: ZoneProfile | null | undefined,
  experimentalDsd256 = true
) {
  if (zoneSupportsDsdOutputMode(zone, 'Dsd256', experimentalDsd256)) return 'Dsd256';
  if (zoneSupportsDsdOutputMode(zone, 'Dsd128', experimentalDsd256)) return 'Dsd128';
  if (zoneSupportsDsdOutputMode(zone, 'Dsd64', experimentalDsd256)) return 'Dsd64';
  return 'Dsd128';
}

export function zoneSupportsDopDsd(zone: ZoneProfile | null | undefined) {
  if (zoneSupportsNativeDsd(zone)) return false;
  if (String(zone?.protocol || '') !== 'upnp_av_renderer') {
    return (
      zoneSupportsLocalDopDsdMode(zone, 'Dsd128') || zoneSupportsLocalDopDsdMode(zone, 'Dsd64')
    );
  }
  const caps = (zone?.capabilities || {}) as JsonRecord;
  return (
    zoneSupportsUpnpDopDsdMode(zone, 'Dsd64') ||
    boolValue(caps.supports_dsd128, false) ||
    zoneMaxDsdRate(zone) >= 64
  );
}

export const dsdSourceRates = [44100, 48000, 88200, 96000, 176400, 192000] as const;

export const dsdDefaultRules = [
  { source_rate: 44100, filter_type: 'SplitPhase128kE2v3', output_mode: 'Dsd128' },
  { source_rate: 48000, filter_type: 'SplitPhase128kE2v3', output_mode: 'Dsd128' },
  { source_rate: 88200, filter_type: 'SplitPhase128kE2v3', output_mode: 'Dsd128' },
  { source_rate: 96000, filter_type: 'SplitPhase128kE2v3', output_mode: 'Dsd128' },
  { source_rate: 176400, filter_type: 'SplitPhase128kE2v3', output_mode: 'Dsd128' },
  { source_rate: 192000, filter_type: 'SplitPhase128kE2v3', output_mode: 'Dsd128' }
] as const;

export const eqBandTypes = [
  { value: 'peaking', label: 'Peak' },
  { value: 'low_shelf', label: 'LS' },
  { value: 'high_shelf', label: 'HS' },
  { value: 'low_pass', label: 'LP' },
  { value: 'high_pass', label: 'HP' }
] as const;

export function createNewEqPresetConfig(): JsonRecord {
  const defaultBands = [
    { type: 'low_shelf', freq_hz: 105, q: 0.7 },
    { type: 'peaking', freq_hz: 62, q: 1 },
    { type: 'peaking', freq_hz: 125, q: 1 },
    { type: 'peaking', freq_hz: 250, q: 1 },
    { type: 'peaking', freq_hz: 500, q: 1 },
    { type: 'peaking', freq_hz: 1000, q: 1 },
    { type: 'peaking', freq_hz: 2000, q: 1 },
    { type: 'peaking', freq_hz: 4000, q: 1 },
    { type: 'peaking', freq_hz: 8000, q: 1 },
    { type: 'high_shelf', freq_hz: 10000, q: 0.7 }
  ];
  return {
    enabled: true,
    preamp_db: 0,
    bands: defaultBands.map((band) => ({
      enabled: true,
      type: band.type,
      freq_hz: band.freq_hz,
      gain_db: 0,
      q: band.q
    }))
  };
}

export const EQ_PLOT_WIDTH = 800;
export const EQ_PLOT_HEIGHT = 120;
export const EQ_DB_RANGE = 24;
export const EQ_PLOT_PAD_X = 10;
export const dspBufferOptions = [
  { value: 0, label: 'Auto' },
  { value: 100, label: 'Low latency - 100 ms' },
  { value: 250, label: 'Safe - 250 ms' },
  { value: 1000, label: 'Safest - 1000 ms' }
] as const;
export const DEFAULT_DSP_BUFFER_MS = 250;

export function clampNumber(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

export function normalizeDspBufferMs(value: unknown) {
  const numericValue = numberValue(value, DEFAULT_DSP_BUFFER_MS);
  return dspBufferOptions.some((option) => option.value === numericValue)
    ? numericValue
    : DEFAULT_DSP_BUFFER_MS;
}

export function eqFreqToX(freq: number, width = EQ_PLOT_WIDTH) {
  const minLog = Math.log10(20);
  const maxLog = Math.log10(20000);
  const usable = width - EQ_PLOT_PAD_X * 2;
  return EQ_PLOT_PAD_X + ((Math.log10(freq) - minLog) / (maxLog - minLog)) * usable;
}

export function eqDbToY(db: number, height = EQ_PLOT_HEIGHT, range = EQ_DB_RANGE) {
  const clamped = clampNumber(db, -range, range);
  return height / 2 - (clamped / range) * (height / 2 - 6);
}

export function computeEqBiquad(fs: number, band: JsonRecord) {
  const f0 = clampNumber(numberValue(band.freq_hz, 1000), 10, fs * 0.49);
  const q = Math.max(numberValue(band.q, 0.7), 0.01);
  const w0 = (2 * Math.PI * f0) / fs;
  const cw = Math.cos(w0);
  const sw = Math.sin(w0);
  const alpha = sw / (2 * q);
  const gainDb = numberValue(band.gain_db, 0);
  const A = 10 ** (gainDb / 40);
  let b0 = 1;
  let b1 = 0;
  let b2 = 0;
  let a0 = 1;
  let a1 = 0;
  let a2 = 0;

  switch (String(band.type || 'peaking')) {
    case 'peaking':
      b0 = 1 + alpha * A;
      b1 = -2 * cw;
      b2 = 1 - alpha * A;
      a0 = 1 + alpha / A;
      a1 = -2 * cw;
      a2 = 1 - alpha / A;
      break;
    case 'low_shelf': {
      const s = 2 * Math.sqrt(A) * alpha;
      b0 = A * (A + 1 - (A - 1) * cw + s);
      b1 = 2 * A * (A - 1 - (A + 1) * cw);
      b2 = A * (A + 1 - (A - 1) * cw - s);
      a0 = A + 1 + (A - 1) * cw + s;
      a1 = -2 * (A - 1 + (A + 1) * cw);
      a2 = A + 1 + (A - 1) * cw - s;
      break;
    }
    case 'high_shelf': {
      const s = 2 * Math.sqrt(A) * alpha;
      b0 = A * (A + 1 + (A - 1) * cw + s);
      b1 = -2 * A * (A - 1 + (A + 1) * cw);
      b2 = A * (A + 1 + (A - 1) * cw - s);
      a0 = A + 1 - (A - 1) * cw + s;
      a1 = 2 * (A - 1 - (A + 1) * cw);
      a2 = A + 1 - (A - 1) * cw - s;
      break;
    }
    case 'low_pass':
      b0 = (1 - cw) / 2;
      b1 = 1 - cw;
      b2 = (1 - cw) / 2;
      a0 = 1 + alpha;
      a1 = -2 * cw;
      a2 = 1 - alpha;
      break;
    case 'high_pass':
      b0 = (1 + cw) / 2;
      b1 = -(1 + cw);
      b2 = (1 + cw) / 2;
      a0 = 1 + alpha;
      a1 = -2 * cw;
      a2 = 1 - alpha;
      break;
    case 'notch':
      b0 = 1;
      b1 = -2 * cw;
      b2 = 1;
      a0 = 1 + alpha;
      a1 = -2 * cw;
      a2 = 1 - alpha;
      break;
    case 'all_pass':
      b0 = 1 - alpha;
      b1 = -2 * cw;
      b2 = 1 + alpha;
      a0 = 1 + alpha;
      a1 = -2 * cw;
      a2 = 1 - alpha;
      break;
    default:
      return { b0: 1, b1: 0, b2: 0, a1: 0, a2: 0 };
  }
  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
}

export function eqBiquadMagDb(
  coef: ReturnType<typeof computeEqBiquad>,
  freqHz: number,
  fs: number
) {
  const w = (2 * Math.PI * freqHz) / fs;
  const cw = Math.cos(w);
  const c2w = Math.cos(2 * w);
  const sw = Math.sin(w);
  const s2w = Math.sin(2 * w);
  const nr = coef.b0 + coef.b1 * cw + coef.b2 * c2w;
  const ni = -(coef.b1 * sw + coef.b2 * s2w);
  const dr = 1 + coef.a1 * cw + coef.a2 * c2w;
  const di = -(coef.a1 * sw + coef.a2 * s2w);
  const num = Math.sqrt(nr * nr + ni * ni);
  const den = Math.sqrt(dr * dr + di * di);
  if (den < 1e-12) return 0;
  return 20 * Math.log10(num / den);
}

export function buildEqCurve(config: JsonRecord | null, sampleRate: number) {
  const bands = safeArray<JsonRecord>(config?.bands);
  const fs = sampleRate || 192000;
  const masterOn = Boolean(config?.enabled);
  const preamp = masterOn ? numberValue(config?.preamp_db, 0) : 0;
  const coeffs = bands.map((band) => (band.enabled ? computeEqBiquad(fs, band) : null));
  let path = '';
  let firstY = EQ_PLOT_HEIGHT / 2;
  let lastY = EQ_PLOT_HEIGHT / 2;

  for (let i = 0; i <= 240; i += 1) {
    const freq = 10 ** (Math.log10(20) + (i / 240) * (Math.log10(20000) - Math.log10(20)));
    let dbSum = preamp;
    if (masterOn) {
      coeffs.forEach((coef) => {
        if (coef) dbSum += eqBiquadMagDb(coef, freq, fs);
      });
    }
    const x = eqFreqToX(freq);
    const y = eqDbToY(dbSum);
    if (i === 0) firstY = y;
    if (i === 240) lastY = y;
    path += `${i === 0 ? 'M' : 'L'}${x.toFixed(1)} ${y.toFixed(1)} `;
  }

  const out = 24;
  const fillPath = `M ${-out} ${firstY.toFixed(1)} ${path.replace(/^M/, 'L')} L ${EQ_PLOT_WIDTH + out} ${lastY.toFixed(1)} L ${EQ_PLOT_WIDTH + out} ${EQ_PLOT_HEIGHT + out} L ${-out} ${EQ_PLOT_HEIGHT + out} Z`;
  const markers = bands.map((band, index) => {
    const freq = clampNumber(numberValue(band.freq_hz, 1000), 20, 20000);
    let dbSum = preamp;
    if (masterOn) {
      coeffs.forEach((coef) => {
        if (coef) dbSum += eqBiquadMagDb(coef, freq, fs);
      });
    }
    return {
      index,
      enabled: Boolean(band.enabled),
      x: eqFreqToX(freq),
      y: eqDbToY(dbSum)
    };
  });

  return {
    path,
    fillPath,
    markers,
    verticalGrid: [50, 100, 200, 500, 1000, 2000, 5000, 10000].map((freq) => ({
      freq,
      x: eqFreqToX(freq),
      major: [100, 1000, 10000].includes(freq)
    })),
    horizontalGrid: [-18, -12, -6, 0, 6, 12, 18].map((db) => ({ db, y: eqDbToY(db) }))
  };
}

export const hegelModels = {
  h95: { label: 'H95', inputs: 8, usb: 7 },
  h120: { label: 'H120', inputs: 9, usb: 8, xlr: 1 },
  h190: { label: 'H190', inputs: 9, usb: 8, xlr: 1 },
  h390: { label: 'H390', inputs: 10, usb: 9, xlr: 1 },
  h590: { label: 'H590', inputs: 12, usb: 10, xlr: 1 },
  custom: { label: 'Custom', inputs: 20, usb: 1, xlr: 1 }
} as const;

export type HegelModelId = keyof typeof hegelModels;

export interface HegelFormState {
  zoneId: string;
  linkedAirplayZoneId: string;
  host: string;
  port: number;
  model: HegelModelId;
  input: number;
  defaultVolume: number;
  maxVolume: number;
  standbyUsbVisible: boolean;
  volume: number;
}

export function stringValue(value: unknown, fallback = '') {
  return typeof value === 'string' && value.trim() ? value : fallback;
}

export function numberValue(value: unknown, fallback = 0) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

export function boolValue(value: unknown, fallback = false) {
  return typeof value === 'boolean' ? value : fallback;
}

export function compactFilterName(name: unknown) {
  const overrides: Record<string, string> = {
    Linear: 'Split Phase',
    SincExtreme32k: 'Linear Phase',
    LinearPhase128k: 'Linear Phase',
    Mixed16k: 'Split Phase',
    Minimum16k: 'Minimum Phase',
    MinimumPhase128k: 'Minimum Phase 128k 1',
    MinimumPhase128kV2: 'Minimum Phase 128k 2',
    MinimumPhase128kV3: 'Minimum Phase 128k 3',
    MinimumPhase128kV4: 'Minimum Phase 128k 4',
    MinimumPhaseCompact128k: 'Minimum Phase',
    MinimumPhaseCompact128kV2: 'Minimum Phase',
    SmoothPhase128k: 'Smooth Phase',
    Perfect: 'Split Phase',
    Split16k: 'Split Phase',
    Split16kDsd128: 'Split Phase',
    Split32k: 'Split Phase',
    Split128k: 'Split Phase',
    Split128kV2: 'Split Phase',
    SplitPhase128kV3: 'Split Phase',
    SplitPhase128kV4: 'Split Phase',
    SplitPhase128kE2v3: 'Split Phase',
    IntegratedPhase128k: 'Integrated Phase 1',
    IntegratedPhase128kV2: 'Integrated Phase 2',
    IntegratedPhase128kV3: 'Integrated Phase 3',
    IntegratedPhase128kV4: 'Integrated Phase 4',
    SincExperimental1m: 'Split Phase',
    SincMedium: 'Split Phase'
  };
  const key = stringValue(name, 'Linear');
  return overrides[key] || key.replace(/([a-z])([A-Z])/g, '$1 $2');
}

export function fileFormatLabel(filename: unknown) {
  const match = String(filename || '').match(/\.([^.]+)$/);
  if (!match) return 'Source';
  const labels: Record<string, string> = {
    flac: 'FLAC',
    wav: 'WAV',
    mp3: 'MP3',
    m4a: 'M4A',
    aac: 'AAC',
    aiff: 'AIFF',
    aif: 'AIFF',
    ogg: 'OGG',
    opus: 'Opus',
    caf: 'CAF'
  };
  const ext = match[1].toLowerCase();
  return labels[ext] || ext.toUpperCase();
}

export function isAirPlayProtocol(protocol: unknown) {
  return ['air_play2', 'air_play_raop', 'air_play_core_audio'].includes(String(protocol || ''));
}

export function formatSignalRate(rateValue: unknown, bitsValue: unknown) {
  const rate = numberValue(rateValue);
  if (rate <= 0) return '--';
  const bits = numberValue(bitsValue);
  const bitsPrefix = bits > 0 ? `${bits}/` : '';
  return `${bitsPrefix}${(rate / 1000).toFixed(1)} kHz`;
}

export function formatCpuPercent(value: unknown) {
  return `${Math.round(numberValue(value))}%`;
}

export function formatHz(value: unknown) {
  const rate = numberValue(value);
  if (!rate) return 'Auto';
  const khz = rate / 1000;
  return `${Number.isInteger(khz) ? khz : khz.toFixed(1)} kHz`;
}

export const upnpPcmRateOptions = [44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000].map(
  (rate) => ({
    value: String(rate),
    label: `${Number.isInteger(rate / 1000) ? rate / 1000 : (rate / 1000).toFixed(1)}kHz`
  })
);

export const upnpDsdRateOptions = [
  { value: 'none', label: 'No DSD' },
  { value: '64', label: 'DSD64' },
  { value: '128', label: 'DSD128' },
  { value: '256', label: 'DSD256' }
];

export function formatOutputPcmRate(
  value: unknown,
  detectionSource?: unknown,
  detectionStatus?: unknown
) {
  if (detectionStatus === 'probing' || detectionSource === 'probing') return 'Detecting...';
  if (detectionSource === 'fallback') return 'Unknown (safe 48kHz)';
  const rate = numberValue(value);
  if (rate <= 0) return 'Unknown';
  if (
    (detectionStatus === 'unknown' ||
      detectionStatus === 'failed' ||
      detectionStatus === 'deferred') &&
    rate <= 48000
  ) {
    return 'Unknown (safe 48kHz)';
  }
  const khz = rate / 1000;
  return `${Number.isInteger(khz) ? khz : khz.toFixed(1)}kHz`;
}

export function formatOutputDsdRate(
  value: unknown,
  detectionSource?: unknown,
  detectionStatus?: unknown
) {
  if (detectionStatus === 'probing' || detectionSource === 'probing') return 'Detecting...';
  const rate = numberValue(value);
  if (rate > 0) return `DSD${Math.round(rate)}`;
  if (detectionSource === undefined && detectionStatus === undefined) return 'No DSD';
  if (detectionStatus === 'complete' && detectionSource !== 'fallback') return 'No DSD';
  return 'Unknown';
}

export function zoneCapabilityLabels(zone: ZoneProfile) {
  const caps = (zone.capabilities || {}) as JsonRecord;
  const detectionSource = caps.capability_detection_source;
  const detectionStatus = caps.capability_detection_status;
  return {
    pcm: formatOutputPcmRate(caps.max_sample_rate, detectionSource, detectionStatus),
    dsd: formatOutputDsdRate(caps.max_dsd_rate, detectionSource, detectionStatus)
  };
}

export function zoneUpnpPcmCapabilityValue(zone: ZoneProfile) {
  const rate = numberValue((zone.capabilities || {}).max_sample_rate, 48000);
  return upnpPcmRateOptions.some((option) => option.value === String(rate))
    ? String(rate)
    : '48000';
}

export function zoneUpnpDsdCapabilityValue(zone: ZoneProfile) {
  const rate = numberValue((zone.capabilities || {}).max_dsd_rate);
  return upnpDsdRateOptions.some((option) => option.value === String(rate)) ? String(rate) : 'none';
}

export function upnpPcmCapabilityWarning(zone: ZoneProfile, value: string) {
  const calibrated = zone.upnp_calibrated_capabilities as JsonRecord | undefined;
  if (!calibrated) return '';

  const selectedPcm = numberValue(value);
  const calibratedPcm = numberValue(calibrated.max_sample_rate);
  return calibratedPcm > 0 && selectedPcm > calibratedPcm
    ? `Calibration only confirmed PCM up to ${formatOutputPcmRate(calibratedPcm)}.`
    : '';
}

export function upnpDsdCapabilityWarning(zone: ZoneProfile, value: string) {
  const calibrated = zone.upnp_calibrated_capabilities as JsonRecord | undefined;
  if (!calibrated) return '';

  const selectedDsd = value === 'none' ? 0 : numberValue(value);
  const calibratedDsd = numberValue(calibrated.max_dsd_rate);
  return selectedDsd > calibratedDsd
    ? calibratedDsd > 0
      ? `Calibration only confirmed DSD up to ${formatOutputDsdRate(calibratedDsd)}.`
      : 'Calibration did not confirm DSD support.'
    : '';
}

export function configFromStatus(status: JsonRecord) {
  const filterType = stringValue(
    status.filter_type,
    stringValue(status.active_filter_type, 'SplitPhase128kE2v3')
  );
  return {
    upsamplingEnabled: boolValue(status.upsampling_enabled, false),
    exclusive: boolValue(status.exclusive, false),
    filterType: visibleFilterType(filterType),
    targetRate: numberValue(status.configured_target_rate ?? status.target_rate, 0),
    targetBitDepth: numberValue(status.configured_target_bit_depth ?? status.target_bit_depth, 24),
    headroomDb: numberValue(status.headroom_db, defaultHeadroomDb),
    dspBufferMs: normalizeDspBufferMs(status.dsp_buffer_ms),
    outputMode: stringValue(status.output_mode, 'Pcm'),
    dsdModulator: visibleDsdModulator(status.dsd_modulator),
    dsdIsiPenalty: clampNumber(numberValue(status.dsd_isi_penalty, 0), 0, 0.05)
  };
}

export type PlaybackDspConfig = ReturnType<typeof configFromStatus>;

export function playbackDspConfigKey(config: PlaybackDspConfig) {
  return JSON.stringify(config);
}

export function playbackDspConfigMatchesStatus(config: PlaybackDspConfig, status: JsonRecord) {
  return playbackDspConfigKey(config) === playbackDspConfigKey(configFromStatus(status));
}

export function canSyncPlaybackDspConfigFromStatus({
  dirty,
  localConfigKey,
  appliedConfigKey,
  statusConfigKey
}: {
  dirty: boolean;
  localConfigKey: string;
  appliedConfigKey: string | null;
  statusConfigKey: string;
}) {
  if (!dirty) return true;
  return appliedConfigKey === localConfigKey && statusConfigKey === localConfigKey;
}

export function hegelHostFromZone(zone: ZoneProfile) {
  const address = String(zone.network_address || '');
  if (!address) return '';
  if (address.startsWith('[')) return address.slice(1, address.indexOf(']'));
  const colon = address.lastIndexOf(':');
  return colon > 0 ? address.slice(0, colon) : address;
}

export function zoneBackendDisplayLabel(value: unknown) {
  switch (String(value || '').toLowerCase()) {
    case 'asio':
      return 'ASIO';
    case 'coreaudio':
      return 'CoreAudio';
    case 'wasapi':
      return 'WASAPI';
    case 'alsa':
      return 'ALSA';
    case 'airplay':
      return 'AirPlay';
    case 'system':
      return 'System Audio';
    default:
      return '';
  }
}

export function remoteAgentDeviceLabel(zone: ZoneProfile) {
  const explicit = String(zone.agent_name || '').trim();
  if (explicit) return explicit;
  const name = String(zone.name || '').trim();
  if (name.includes(' - ')) return name.split(' - ')[0]?.trim() || 'Agent';
  return name || 'Agent';
}

export function isAirPlayZone(zone: ZoneProfile) {
  return ['air_play2', 'air_play_raop', 'air_play_core_audio'].includes(
    String(zone.protocol || '')
  );
}

export function isAirPlayNetworkZone(zone: ZoneProfile) {
  return zone.protocol === 'air_play2' || zone.protocol === 'air_play_raop';
}

export function volumeToPercent(volume: unknown) {
  return Math.max(0, Math.min(100, Math.round(numberValue(volume, 0) * 100)));
}

export function isSonosZone(zone: ZoneProfile) {
  return String(zone.protocol || '') === 'sonos_upnp';
}

export function zoneSupportsDsp(zone: ZoneProfile | null | undefined) {
  return !zone || (!isBrowserZone(zone) && !isSonosZone(zone) && !isAirPlayZone(zone));
}

export function isKefZone(zone: ZoneProfile) {
  const identity = [zone.id, zone.name, zone.device_name].filter(Boolean).join(' ');
  return /\bkef\b/i.test(identity) || /\bls(50|60|x|f)\b/i.test(identity);
}

export function isUpnpZone(zone: ZoneProfile) {
  const identity = [zone.protocol, zone.id, zone.device_name, zone.name]
    .filter(Boolean)
    .join(' ')
    .toLowerCase();
  return (
    identity.includes('upnp_av_renderer') ||
    identity.includes('upnpavrenderer') ||
    identity.includes('upnp-') ||
    identity.includes('upnp:') ||
    identity.includes('dlna')
  );
}

export function zoneDisplayName(zone: ZoneProfile) {
  if (isBrowserZone(zone)) return browserZoneDisplayName(zone.name);
  if (isAirPlayZone(zone)) return zone.name || 'AirPlay output';
  if (isSonosZone(zone)) return zone.name || 'Sonos';
  if (isUpnpZone(zone)) return zone.name || 'UPnP renderer';
  const clean = selectedDeviceDisplayName(zone.device_name || zone.name);
  if (zone.id === 'local-core') return zone.name || 'System Output';
  if (zone.protocol === 'remote_agent') {
    const backend = zoneBackendName(zone);
    return backend
      ? `${clean || zone.name || 'Audio output'} (${backend})`
      : clean || zone.name || 'Audio output';
  }
  return clean || zone.name || 'Audio output';
}

export function selectedDeviceDisplayName(value: unknown) {
  const raw = String(value || 'Audio output').trim();
  return (
    raw
      .replace(/^ASIO:\s*/, '')
      .replace(/^Speakers\s+\(/, '')
      .replace(/^Headphones\s+\(/, '')
      .replace(/\)$/, '')
      .trim() || 'Audio output'
  );
}

export function dspSelectedDeviceDisplayName(status: JsonRecord) {
  const zoneName = stringValue(status.active_zone_name);
  if (String(status.active_zone_id || '') !== 'local-core' && zoneName) {
    return zoneName;
  }
  const protocol = String(status.zone_protocol || '');
  if (protocol === 'sonos_upnp') {
    return zoneName || 'Sonos';
  }
  if (protocol === 'upnp_av_renderer') {
    return zoneName || 'UPnP renderer';
  }
  if (isAirPlayProtocol(protocol)) {
    return zoneName || 'AirPlay output';
  }
  return selectedDeviceDisplayName(status.selected_device || zoneName);
}

export function zoneBackendName(zone: ZoneProfile) {
  const backend = zoneBackendDisplayLabel(zone.backend);
  if (backend) return backend;
  if (String(zone.device_name || '').startsWith('ASIO: ')) return 'ASIO';
  return zone.protocol === 'remote_agent' ? 'Agent' : '';
}

export function zoneFormatLabel(zone: ZoneProfile) {
  if (isBrowserZone(zone)) return 'This Browser';
  if (zone.protocol === 'air_play2') return 'AirPlay 2';
  if (zone.protocol === 'air_play_raop') return 'AirPlay';
  if (zone.protocol === 'air_play_core_audio') return 'AirPlay / CoreAudio';
  if (zone.protocol === 'sonos_upnp') return 'Sonos';
  if (zone.protocol === 'upnp_av_renderer') return 'UPnP / DLNA';
  if (zone.protocol === 'asio_output' || String(zone.device_name || '').startsWith('ASIO: '))
    return 'ASIO';
  if (zone.protocol === 'remote_agent')
    return `${remoteAgentDeviceLabel(zone)} / ${zoneBackendName(zone)}`;
  if (zone.id === 'local-core') return 'System Output';
  const backend = zoneBackendDisplayLabel(zone.backend);
  if (backend) return backend;
  if (zone.protocol === 'local_core_audio') return 'CoreAudio';
  return 'System Audio';
}

export function zoneGroupInfo(zone: ZoneProfile) {
  if (isBrowserZone(zone)) return { key: 'browser', label: 'This Browser', order: 10 };
  if (isSonosZone(zone)) return { key: 'sonos', label: 'Sonos', order: 35 };
  if (isUpnpZone(zone)) return { key: 'upnp', label: 'UPnP / DLNA', order: 37 };
  if (isAirPlayZone(zone)) return { key: 'airplay', label: 'AirPlay', order: 40 };
  if (zone.protocol === 'remote_agent') {
    const label = remoteAgentDeviceLabel(zone);
    return { key: `agent:${label.toLowerCase()}`, label, order: 30 };
  }
  return { key: 'server', label: 'Connected to Server', order: 20 };
}

export function groupedSettingsZones(zones: ZoneProfile[]) {
  const groups = new Map<
    string,
    { key: string; label: string; order: number; zones: ZoneProfile[] }
  >();
  zones.forEach((zone) => {
    const info = zoneGroupInfo(zone);
    const group = groups.get(info.key) || { ...info, zones: [] };
    group.zones.push(zone);
    groups.set(info.key, group);
  });
  return Array.from(groups.values()).sort(
    (a, b) => a.order - b.order || a.label.localeCompare(b.label)
  );
}

export function isLocalOutputZone(zone: ZoneProfile) {
  return (
    !isBrowserZone(zone) &&
    zone.protocol !== 'remote_agent' &&
    !isAirPlayZone(zone) &&
    !isSonosZone(zone) &&
    !isUpnpZone(zone)
  );
}

export function isHostDeviceBrowser(hostname?: string) {
  const normalized = (
    hostname ?? (typeof window === 'undefined' ? '' : window.location.hostname)
  ).toLowerCase();
  return (
    normalized === 'localhost' ||
    normalized === '::1' ||
    normalized === '[::1]' ||
    normalized === '0:0:0:0:0:0:0:1' ||
    normalized.startsWith('127.')
  );
}

export function zoneGlyphPath(zone: ZoneProfile) {
  if (isBrowserZone(zone)) return 'M4 5h16v10H4zM9 21h6M12 15v6';
  if (zone.protocol === 'remote_agent') return 'M4 5h16v10H4zM9 21h6M12 15v6';
  if (isAirPlayZone(zone))
    return 'M5 17H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-1M12 11l5 6H7z';
  if (isSonosZone(zone)) return 'M6 5h12v14H6zM9 9h6M9 15h6';
  if (isUpnpZone(zone)) return 'M4 6h16v10H4zM8 20h8M12 16v4M8 11h.01M12 11h.01M16 11h.01';
  return 'M5 3h14v18H5zM12 7h.01M12 14a3 3 0 1 0 0 .01';
}
