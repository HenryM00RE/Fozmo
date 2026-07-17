import { describe, expect, it } from 'vitest';
import type { ZoneProfile } from '../../shared/types';
import {
  canSyncPlaybackDspConfigFromStatus,
  configFromStatus,
  defaultDsdOutputModeForZone,
  dsdModulatorOptions,
  ecBeam2FilterSupported,
  ecBeam2SelectableForDsdConfig,
  filterOptions,
  formatOutputDsdRate,
  formatOutputPcmRate,
  groupedSettingsZones,
  headroomAfterDsdModulatorChange,
  headroomLockedForDsdModulator,
  isHostDeviceBrowser,
  isiPenaltyAfterDsdModulatorChange,
  playbackDspConfigKey,
  playbackDspConfigMatchesStatus,
  settingsTabFromValue,
  upnpDsdCapabilityWarning,
  upnpPcmCapabilityWarning,
  visibleDsdModulator,
  visibleFilterType,
  visibleSettingsSections,
  zoneCapabilityLabels,
  zoneDisplayName,
  zoneFormatLabel,
  zoneSupportsDopDsd,
  zoneSupportsDsdOutputMode,
  zoneSupportsDsp,
  zoneSupportsNativeDsd
} from './settingsModel';

describe('resampling filter choices', () => {
  it('exposes only the supported filter choices and collapses hidden filters', () => {
    expect(filterOptions).toEqual([
      ['LinearPhase128k', 'Linear Phase'],
      ['MinimumPhaseCompact128kV2', 'Minimum Phase'],
      ['MinimumPhaseCompact128k', 'Minimum Phase B'],
      ['Split128k', 'Split Phase'],
      ['SmoothPhase128k', 'Smooth Phase']
    ]);
    expect(visibleFilterType('IntegratedPhase128k')).toBe('Split128k');
    expect(visibleFilterType('SincExtreme32k')).toBe('LinearPhase128k');
    expect(visibleFilterType('LinearPhase128k')).toBe('LinearPhase128k');
    expect(visibleFilterType('Minimum16k')).toBe('MinimumPhaseCompact128kV2');
    expect(visibleFilterType('MinimumPhase128k')).toBe('MinimumPhaseCompact128kV2');
    expect(visibleFilterType('MinimumPhaseCompact128k')).toBe('MinimumPhaseCompact128k');
    expect(visibleFilterType('MinimumPhaseCompact128kV2')).toBe('MinimumPhaseCompact128kV2');
    expect(visibleFilterType('SmoothPhase128k')).toBe('SmoothPhase128k');
    expect(visibleFilterType('unknown-filter')).toBe('Split128k');
    expect(visibleFilterType('Split16k')).toBe('Split128k');
  });
});

describe('DSP output support', () => {
  it('excludes Browser, Sonos, and AirPlay outputs', () => {
    expect(zoneSupportsDsp(undefined)).toBe(true);
    expect(zoneSupportsDsp({ id: 'browser-1', name: 'Browser', browser: true })).toBe(false);
    expect(zoneSupportsDsp({ id: 'sonos-1', name: 'Sonos', protocol: 'sonos_upnp' })).toBe(false);
    expect(zoneSupportsDsp({ id: 'airplay-1', name: 'AirPlay', protocol: 'air_play_raop' })).toBe(
      false
    );
    expect(zoneSupportsDsp({ id: 'airplay-2', name: 'AirPlay 2', protocol: 'air_play2' })).toBe(
      false
    );
    expect(
      zoneSupportsDsp({
        id: 'airplay-coreaudio',
        name: 'AirPlay / CoreAudio',
        protocol: 'air_play_core_audio'
      })
    ).toBe(false);
    expect(zoneSupportsDsp({ id: 'local-core', name: 'System Output' })).toBe(true);
  });
});

describe('DSD modulator choices', () => {
  it('exposes 7th Order and 7th Order Search while normalizing retired values', () => {
    expect(dsdModulatorOptions).toEqual([
      ['Standard', '7th Order'],
      ['EcBeam2', '7th Order Search']
    ]);
    expect(visibleDsdModulator('Standard')).toBe('Standard');
    expect(visibleDsdModulator('EcBeam')).toBe('Standard');
    expect(visibleDsdModulator('7th Order ECB')).toBe('Standard');
    expect(visibleDsdModulator('EcDepth2')).toBe('Standard');
    expect(visibleDsdModulator('EC depth 4')).toBe('Standard');
    expect(visibleDsdModulator('EcBeam2')).toBe('EcBeam2');
    expect(visibleDsdModulator('7th Order Search')).toBe('EcBeam2');
    expect(visibleDsdModulator('7th Order Beam')).toBe('EcBeam2');
    expect(visibleDsdModulator('7th Order ECB2')).toBe('EcBeam2');
    expect(visibleDsdModulator('7th Order ECB2 (Experimental)')).toBe('EcBeam2');
    expect(headroomAfterDsdModulatorChange(-2, 'Standard')).toBe(-4);
    expect(headroomAfterDsdModulatorChange(-4, 'EcBeam2')).toBe(-2);
    expect(headroomAfterDsdModulatorChange(-6, 'Standard')).toBe(-4);
    expect(headroomLockedForDsdModulator('EcBeam2')).toBe(true);
    expect(headroomLockedForDsdModulator('Standard')).toBe(true);
    expect(isiPenaltyAfterDsdModulatorChange(0.01, 'EcBeam2')).toBe(0);
    expect(isiPenaltyAfterDsdModulatorChange(0.01, 'Standard')).toBe(0.01);
  });

  it('makes ECB2 selectable only for qualified DSD64/128/256 rates and filters', () => {
    expect(ecBeam2SelectableForDsdConfig('Dsd64', 'MinimumPhaseCompact128kV2', false, [])).toBe(
      true
    );
    expect(ecBeam2SelectableForDsdConfig('Dsd64', 'Split128k', false, [])).toBe(true);
    expect(ecBeam2SelectableForDsdConfig('Dsd64', 'LinearPhase128k', false, [])).toBe(true);
    expect(ecBeam2SelectableForDsdConfig('Dsd64', 'SincExtreme32k', false, [])).toBe(false);
    expect(
      ecBeam2SelectableForDsdConfig('Dsd64', 'Split128k', true, [
        {
          source_rate: 44100,
          filter_type: 'MinimumPhaseCompact128kV2',
          output_mode: 'Dsd64'
        }
      ])
    ).toBe(true);
    expect(
      ecBeam2SelectableForDsdConfig('Dsd64', 'Split128k', true, [
        { source_rate: 44100, filter_type: 'Split128k', output_mode: 'Dsd128' }
      ])
    ).toBe(true);
    expect(
      ecBeam2SelectableForDsdConfig('Dsd64', 'Split128k', true, [
        { source_rate: 44100, filter_type: 'SincExtreme32k', output_mode: 'Dsd64' }
      ])
    ).toBe(false);
    expect(ecBeam2SelectableForDsdConfig('Dsd128', 'Split128k', false, [])).toBe(true);
    expect(ecBeam2SelectableForDsdConfig('Dsd128', 'MinimumPhaseCompact128kV2', false, [])).toBe(
      true
    );
    expect(ecBeam2SelectableForDsdConfig('Dsd128', 'SmoothPhase128k', false, [])).toBe(true);
    expect(ecBeam2SelectableForDsdConfig('Dsd256', 'Split128k', false, [])).toBe(true);
    expect(
      ecBeam2SelectableForDsdConfig('Dsd64', 'Split128k', true, [
        { source_rate: 176400, filter_type: 'Split128k', output_mode: 'Dsd256' }
      ])
    ).toBe(true);
    expect(ecBeam2SelectableForDsdConfig('Pcm', 'Split128k', false, [])).toBe(false);
    expect(ecBeam2FilterSupported('Minimum16k')).toBe(true);
    expect(ecBeam2FilterSupported('MinimumPhaseCompact128k')).toBe(true);
    expect(ecBeam2FilterSupported('MinimumPhaseCompact128kV2')).toBe(true);
    expect(ecBeam2FilterSupported('Split128k')).toBe(true);
    expect(ecBeam2FilterSupported('SmoothPhase128k')).toBe(true);
    expect(ecBeam2FilterSupported('LinearPhase128k')).toBe(true);
    expect(ecBeam2FilterSupported('SincExtreme32k')).toBe(false);
  });
});

describe('settings navigation', () => {
  it('identifies only loopback browser hosts as the server device', () => {
    expect(isHostDeviceBrowser('localhost')).toBe(true);
    expect(isHostDeviceBrowser('127.0.0.42')).toBe(true);
    expect(isHostDeviceBrowser('::1')).toBe(true);
    expect(isHostDeviceBrowser('192.168.1.20')).toBe(false);
    expect(isHostDeviceBrowser('music.example.com')).toBe(false);
  });

  it('recognizes the Remote Access tab locally and remotely', () => {
    expect(settingsTabFromValue('remote', 'general', { surface: 'local' })).toBe('remote');
    expect(settingsTabFromValue('remote', 'general', { surface: 'remote' })).toBe('remote');
    expect(
      visibleSettingsSections({ surface: 'local' }).some((section) => section.id === 'remote')
    ).toBe(true);
  });

  it('falls back safely for legacy and invalid settings tabs', () => {
    expect(settingsTabFromValue('apple-music', 'general', {})).toBe('general');
    expect(settingsTabFromValue('unknown', 'zones', {})).toBe('zones');
  });

  it('shows the Apple Music tab only when the capture capability is enabled', () => {
    const enabled = { capabilities: { apple_music_capture: true } };
    expect(visibleSettingsSections(enabled).some((section) => section.id === 'apple-music')).toBe(
      true
    );
    expect(visibleSettingsSections({}).some((section) => section.id === 'apple-music')).toBe(false);
    expect(settingsTabFromValue('apple-music', 'general', enabled)).toBe('apple-music');
    expect(settingsTabFromValue('apple_music', 'general', enabled)).toBe('apple-music');
  });
});

describe('output capability labels', () => {
  it('formats PCM rates for compact output settings labels', () => {
    expect(formatOutputPcmRate(192000)).toBe('192kHz');
    expect(formatOutputPcmRate(44100)).toBe('44.1kHz');
  });

  it('formats DSD rates and no-DSD outputs', () => {
    expect(formatOutputDsdRate(64)).toBe('DSD64');
    expect(formatOutputDsdRate(null)).toBe('No DSD');
  });

  it('shows detecting while backend probing is pending or in progress', () => {
    const zone = {
      id: 'upnp-test',
      name: 'UPnP',
      capabilities: {
        max_sample_rate: 48000,
        max_dsd_rate: null,
        capability_detection_source: 'probing',
        capability_detection_status: 'probing'
      }
    } satisfies ZoneProfile;

    expect(zoneCapabilityLabels(zone)).toEqual({
      pcm: 'Detecting...',
      dsd: 'Detecting...'
    });
  });

  it('does not present fallback safe defaults as final caps', () => {
    const zone = {
      id: 'upnp-test',
      name: 'UPnP',
      capabilities: {
        max_sample_rate: 48000,
        max_dsd_rate: null,
        capability_detection_source: 'fallback',
        capability_detection_status: 'failed'
      }
    } satisfies ZoneProfile;

    expect(zoneCapabilityLabels(zone)).toEqual({
      pcm: 'Unknown (safe 48kHz)',
      dsd: 'Unknown'
    });
  });

  it('shows observed PCM while DSD is still unknown', () => {
    const zone = {
      id: 'upnp-test',
      name: 'UPnP',
      capabilities: {
        max_sample_rate: 192000,
        max_dsd_rate: null,
        capability_detection_source: 'probed',
        capability_detection_status: 'unknown'
      }
    } satisfies ZoneProfile;

    expect(zoneCapabilityLabels(zone)).toEqual({
      pcm: '192kHz',
      dsd: 'Unknown'
    });
  });

  it('keeps advertised 48k no-DSD outputs final', () => {
    const zone = {
      id: 'sonos-test',
      name: 'Sonos',
      capabilities: {
        max_sample_rate: 48000,
        max_dsd_rate: null,
        capability_detection_source: 'advertised',
        capability_detection_status: 'complete'
      }
    } satisfies ZoneProfile;

    expect(zoneCapabilityLabels(zone)).toEqual({
      pcm: '48kHz',
      dsd: 'No DSD'
    });
  });

  it('warns only the UPnP PCM options above calibrated support', () => {
    const zone = {
      id: 'upnp-hegel',
      name: 'Hegel',
      upnp_calibrated_capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: 64
      }
    } satisfies ZoneProfile;

    expect(upnpPcmCapabilityWarning(zone, '192000')).toBe('');
    expect(upnpPcmCapabilityWarning(zone, '384000')).toBe(
      'Calibration only confirmed PCM up to 192kHz.'
    );
  });

  it('warns only the UPnP DSD options above calibrated support', () => {
    const zone = {
      id: 'upnp-hegel',
      name: 'Hegel',
      upnp_calibrated_capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: 64
      }
    } satisfies ZoneProfile;

    expect(upnpDsdCapabilityWarning(zone, '64')).toBe('');
    expect(upnpDsdCapabilityWarning(zone, '128')).toBe(
      'Calibration only confirmed DSD up to DSD64.'
    );
    expect(upnpDsdCapabilityWarning(zone, '256')).toBe(
      'Calibration only confirmed DSD up to DSD64.'
    );
  });

  it('warns DSD options when calibration found no DSD support', () => {
    const zone = {
      id: 'upnp-kef',
      name: 'KEF',
      upnp_calibrated_capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: null
      }
    } satisfies ZoneProfile;

    expect(upnpDsdCapabilityWarning(zone, 'none')).toBe('');
    expect(upnpDsdCapabilityWarning(zone, '64')).toBe('Calibration did not confirm DSD support.');
  });

  it('allows generated DSD64 but rejects higher DSD rates for UPnP DSD64 zones', () => {
    const zone = {
      id: 'upnp-hegel',
      name: 'Hegel H390',
      protocol: 'upnp_av_renderer',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: 64,
        supports_dsd128: false,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd64', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(false);
    expect(defaultDsdOutputModeForZone(zone, true)).toBe('Dsd64');
  });

  it('keeps zones without any DSD capability locked out of DSD output', () => {
    const zone = {
      id: 'upnp-no-dsd',
      name: 'PCM Renderer',
      protocol: 'upnp_av_renderer',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: null,
        supports_dsd128: false,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd64', true)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(false);
  });

  it('allows UPnP DSD64 over a 192 kHz WAV DoP carrier without native DSD', () => {
    const zone = {
      id: 'upnp-hegel-dop',
      name: 'Hegel H390',
      protocol: 'upnp_av_renderer',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: null,
        supports_dsd128: false,
        supports_dsd256: false,
        pcm_containers: [{ container: 'wav', max_sample_rate: 192000, max_bit_depth: 24 }]
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd64', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(false);
    expect(defaultDsdOutputModeForZone(zone, true)).toBe('Dsd64');
  });

  it('allows generated DoP only up to the UPnP DSD capability', () => {
    const zone = {
      id: 'upnp-dsd128',
      name: 'DSD128 Renderer',
      protocol: 'upnp_av_renderer',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: 128,
        supports_dsd128: true,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(false);
  });

  it('allows local DoP DSD128 but greys out DSD256 for 384 kHz carriers', () => {
    const zone = {
      id: 'mytek-coreaudio',
      name: 'Mytek Brooklyn',
      protocol: 'local_core_audio',
      backend: 'coreaudio',
      capabilities: {
        max_sample_rate: 384000,
        max_bit_depth: 32,
        max_dsd_rate: 128,
        supports_dsd128: true,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(false);
    expect(defaultDsdOutputModeForZone(zone, true)).toBe('Dsd128');
  });

  it('greys out local DoP entirely below the DSD128 carrier rate', () => {
    const zone = {
      id: 'desktop-speakers',
      name: 'Desktop Speakers',
      protocol: 'local_core_audio',
      backend: 'coreaudio',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 32,
        max_dsd_rate: null,
        supports_dsd128: false,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(false);
  });

  it('allows local DoP DSD64 on 192 kHz carriers when the device reports DSD64', () => {
    const zone = {
      id: 'dsd64-coreaudio',
      name: 'DSD64 DAC',
      protocol: 'local_core_audio',
      backend: 'coreaudio',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: 64,
        supports_dsd128: false,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd64', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(false);
    expect(defaultDsdOutputModeForZone(zone, true)).toBe('Dsd64');
  });

  it('allows local DoP DSD256 only when carrier and capability flags support it', () => {
    const zone = {
      id: 'high-rate-coreaudio',
      name: 'High Rate DAC',
      protocol: 'local_core_audio',
      backend: 'coreaudio',
      capabilities: {
        max_sample_rate: 705600,
        max_bit_depth: 24,
        max_dsd_rate: 256,
        supports_dsd128: true,
        supports_dsd256: true
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsDopDsd(zone)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', false)).toBe(false);
    expect(defaultDsdOutputModeForZone(zone, true)).toBe('Dsd256');
  });

  it('keeps ASIO native DSD separate from DoP carrier limits', () => {
    const zone = {
      id: 'asio-rme',
      name: 'ASIO: RME',
      protocol: 'asio_output',
      backend: 'asio',
      capabilities: {
        max_sample_rate: 192000,
        max_bit_depth: 24,
        max_dsd_rate: null,
        supports_dsd128: false,
        supports_dsd256: false
      }
    } satisfies ZoneProfile;

    expect(zoneSupportsNativeDsd(zone)).toBe(true);
    expect(zoneSupportsDopDsd(zone)).toBe(false);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd128', true)).toBe(true);
    expect(zoneSupportsDsdOutputMode(zone, 'Dsd256', true)).toBe(true);
  });
});

describe('DSP playback config reconciliation', () => {
  const savedStatus = {
    upsampling_enabled: true,
    exclusive: false,
    filter_type: 'Split128k',
    configured_target_rate: 0,
    configured_target_bit_depth: 24,
    headroom_db: -12,
    dsp_buffer_ms: 250,
    output_mode: 'Pcm',
    dsd_modulator: 'EcDepth2',
    dsd_isi_penalty: 0
  };

  it('keeps optimistic DSP changes while refreshed status is still stale', () => {
    const optimisticConfig = {
      ...configFromStatus(savedStatus),
      headroomDb: 0
    };

    expect(
      canSyncPlaybackDspConfigFromStatus({
        dirty: true,
        localConfigKey: playbackDspConfigKey(optimisticConfig),
        appliedConfigKey: playbackDspConfigKey(optimisticConfig),
        statusConfigKey: playbackDspConfigKey(configFromStatus(savedStatus))
      })
    ).toBe(false);
  });

  it('accepts refreshed status once it confirms the applied DSP change', () => {
    const optimisticConfig = {
      ...configFromStatus(savedStatus),
      headroomDb: 0
    };
    const confirmedStatus = {
      ...savedStatus,
      headroom_db: 0
    };

    expect(playbackDspConfigMatchesStatus(optimisticConfig, confirmedStatus)).toBe(true);
    expect(
      canSyncPlaybackDspConfigFromStatus({
        dirty: true,
        localConfigKey: playbackDspConfigKey(optimisticConfig),
        appliedConfigKey: playbackDspConfigKey(optimisticConfig),
        statusConfigKey: playbackDspConfigKey(configFromStatus(confirmedStatus))
      })
    ).toBe(true);
  });
});

describe('browser zones', () => {
  const remoteAgentZone = {
    id: 'agent-ipad',
    name: 'iPad',
    protocol: 'remote_agent',
    device_name: 'iPad'
  } satisfies ZoneProfile;
  const browserZone = {
    id: 'browser-3f9c2ab1d0e4',
    name: 'Safari on iPhone',
    protocol: 'remote_agent',
    browser: true
  } satisfies ZoneProfile;

  it('groups browser zones separately from remote agents', () => {
    const groups = groupedSettingsZones([browserZone, remoteAgentZone]);

    expect(groups.map((group) => group.label)).toContain('This Browser');
    expect(
      groups.find((group) => group.label === 'This Browser')?.zones.map((zone) => zone.id)
    ).toEqual(['browser-3f9c2ab1d0e4']);
    expect(groups.find((group) => group.label === 'iPad')?.zones.map((zone) => zone.id)).toEqual([
      'agent-ipad'
    ]);
  });

  it('labels browser zones with only the browser name', () => {
    expect(zoneDisplayName(browserZone)).toBe('Safari');
    expect(zoneFormatLabel(browserZone)).toBe('This Browser');
  });
});
