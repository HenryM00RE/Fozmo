import { useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { browserZoneAgentId, isBrowserZone } from '../../../shared/lib/browserZone';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import { type OutputIconId, savedOutputIcon } from '../../../shared/ui/ZoneOutputIcon';
import {
  groupedSettingsZones,
  type HegelModelId,
  hegelModels,
  isAirPlayNetworkZone,
  isUpnpZone,
  numberValue,
  stringValue,
  volumeToPercent,
  zoneDisplayName,
  zoneUpnpDsdCapabilityValue,
  zoneUpnpPcmCapabilityValue
} from '../settingsModel';

export type ZoneDeviceType = 'none' | 'hegel';

export type ZoneHegelDraft = {
  linkedAirplayZoneId: string;
  host: string;
  port: number;
  model: HegelModelId;
  input: number;
  defaultVolume: number;
  maxVolume: number;
  standbyUsbVisible: boolean;
};

export type ZoneUpnpCapabilitiesDraft = {
  maxPcmRate: string;
  maxDsdRate: string;
};

export type ZoneBrowserStreamDraft = {
  format: 'flac' | 'opus';
  opusKbps: number;
};

export const BROWSER_OPUS_KBPS_OPTIONS = [128, 256, 320] as const;
const DEFAULT_BROWSER_OPUS_KBPS = 256;

export function browserOwnedSettingsZones(zones: ZoneProfile[], ownBrowserZoneId: string) {
  return zones.filter((zone) => !isBrowserZone(zone) || zone.id === ownBrowserZoneId);
}

function zoneBrowserStreamDraftFromZone(zone: ZoneProfile): ZoneBrowserStreamDraft {
  const saved = (zone.browser_stream || {}) as JsonRecord;
  const kbps = numberValue(saved.opus_kbps, DEFAULT_BROWSER_OPUS_KBPS);
  return {
    format: stringValue(saved.format) === 'opus' ? 'opus' : 'flac',
    opusKbps: BROWSER_OPUS_KBPS_OPTIONS.includes(kbps as (typeof BROWSER_OPUS_KBPS_OPTIONS)[number])
      ? kbps
      : DEFAULT_BROWSER_OPUS_KBPS
  };
}

function defaultZoneHegelDraft(): ZoneHegelDraft {
  return {
    linkedAirplayZoneId: '',
    host: '',
    port: 50001,
    model: 'h390',
    input: hegelModels.h390.usb,
    defaultVolume: 20,
    maxVolume: 50,
    standbyUsbVisible: false
  };
}

function zoneHegelDraftFromZone(zone: ZoneProfile): ZoneHegelDraft {
  const hegel = (zone.hegel || {}) as JsonRecord;
  const savedModel = stringValue(hegel.model, 'h390') as HegelModelId;
  const model = hegelModels[savedModel] ? savedModel : 'h390';
  return {
    linkedAirplayZoneId: stringValue(hegel.linked_airplay_zone_id),
    host: stringValue(hegel.host),
    port: numberValue(hegel.port, 50001),
    model,
    input: numberValue(hegel.input, hegelModels[model].usb),
    defaultVolume: numberValue(hegel.default_volume, 20),
    maxVolume: numberValue(hegel.max_volume, 50),
    standbyUsbVisible: Boolean(hegel.standby_usb_visible)
  };
}

function zoneHegelPayload(draft: ZoneHegelDraft) {
  const maxVolume = Math.max(0, Math.min(100, Math.round(draft.maxVolume)));
  return {
    linked_airplay_zone_id: draft.linkedAirplayZoneId || null,
    host: draft.host.trim() || null,
    port: Math.max(1, Math.min(65535, Math.round(draft.port || 50001))),
    model: draft.model,
    input: Math.max(1, Math.min(20, Math.round(draft.input))),
    default_volume: Math.min(maxVolume, Math.max(0, Math.round(draft.defaultVolume))),
    max_volume: maxVolume,
    standby_usb_visible: draft.standbyUsbVisible
  };
}

export function useZonesSettings(zones: ZoneProfile[], onRefresh: () => Promise<void>) {
  const [settingsZoneId, setSettingsZoneId] = useState<string | null>(null);
  const [zoneNameDraft, setZoneNameDraft] = useState('');
  const [zoneDefaultVolumeEnabled, setZoneDefaultVolumeEnabled] = useState(false);
  const [zoneDefaultVolumePercent, setZoneDefaultVolumePercent] = useState(40);
  const [zoneQobuzHiresEnabled, setZoneQobuzHiresEnabled] = useState(false);
  const [zoneIconDraft, setZoneIconDraft] = useState<OutputIconId>('auto');
  const [zoneDeviceTypeDraft, setZoneDeviceTypeDraft] = useState<ZoneDeviceType>('none');
  const [zoneHegelDraft, setZoneHegelDraft] = useState<ZoneHegelDraft>(defaultZoneHegelDraft);
  const [zoneHegelSettingsOpen, setZoneHegelSettingsOpen] = useState(false);
  const [zoneHegelMessage, setZoneHegelMessage] = useState('');
  const [zoneCalibrationBusy, setZoneCalibrationBusy] = useState(false);
  const [zoneCalibrationMessage, setZoneCalibrationMessage] = useState('');
  const [zoneUpnpCapabilitiesDraft, setZoneUpnpCapabilitiesDraft] =
    useState<ZoneUpnpCapabilitiesDraft>({
      maxPcmRate: '48000',
      maxDsdRate: 'none'
    });
  const [zoneBrowserStreamDraft, setZoneBrowserStreamDraft] = useState<ZoneBrowserStreamDraft>({
    format: 'flac',
    opusKbps: DEFAULT_BROWSER_OPUS_KBPS
  });

  // The server already scopes browser zones to the requesting browser. Keep
  // the same ownership boundary in the UI so an accidentally over-broad zone
  // response can never expose another browser's private output settings.
  const outputSettingsZones = browserOwnedSettingsZones(zones, browserZoneAgentId());
  const zoneGroups = groupedSettingsZones(outputSettingsZones);
  const settingsZone = outputSettingsZones.find((zone) => zone.id === settingsZoneId) || null;

  const selectSettingsZone = async (zone: ZoneProfile) => {
    if (zone.enabled === false) {
      await endpoints.enableZone(zone.id);
      await onRefresh();
      return;
    }
    if (isBrowserZone(zone)) return;
    await endpoints.selectZone(zone.id);
    await onRefresh();
  };

  const openZoneSettings = (zone: ZoneProfile) => {
    const hasDefaultVolume = Number.isFinite(Number(zone.airplay_default_volume));
    const fallbackVolume = Number.isFinite(Number(zone.airplay_last_volume))
      ? zone.airplay_last_volume
      : 0.4;
    setSettingsZoneId(zone.id);
    setZoneNameDraft(String(zone.name || zoneDisplayName(zone)));
    setZoneDefaultVolumeEnabled(hasDefaultVolume);
    setZoneDefaultVolumePercent(
      volumeToPercent(hasDefaultVolume ? zone.airplay_default_volume : fallbackVolume)
    );
    setZoneQobuzHiresEnabled(Boolean(zone.qobuz_hires_enabled));
    setZoneIconDraft(savedOutputIcon(zone));
    setZoneDeviceTypeDraft(zone.device_type === 'hegel' ? 'hegel' : 'none');
    setZoneHegelDraft(zoneHegelDraftFromZone(zone));
    setZoneUpnpCapabilitiesDraft({
      maxPcmRate: zoneUpnpPcmCapabilityValue(zone),
      maxDsdRate: zoneUpnpDsdCapabilityValue(zone)
    });
    setZoneBrowserStreamDraft(zoneBrowserStreamDraftFromZone(zone));
    setZoneHegelSettingsOpen(false);
    setZoneHegelMessage('');
    setZoneCalibrationBusy(false);
    setZoneCalibrationMessage('');
  };

  const saveZoneSettings = async () => {
    if (!settingsZone) return;
    if (isBrowserZone(settingsZone)) {
      // Browser zones persist only their icon and stream delivery choice;
      // the name comes from the agent registration and DSP is server-side.
      await endpoints.updateZoneSettings(settingsZone.id, {
        icon: zoneIconDraft,
        browser_stream: {
          format: zoneBrowserStreamDraft.format,
          opus_kbps: zoneBrowserStreamDraft.opusKbps
        }
      });
      setSettingsZoneId(null);
      await onRefresh();
      return;
    }
    const next = zoneNameDraft.trim();
    if (next && next !== settingsZone.name) {
      await endpoints.renameZone(settingsZone.id, next);
    }
    if (isAirPlayNetworkZone(settingsZone)) {
      await endpoints.updateZoneSettings(settingsZone.id, {
        airplay_default_volume_enabled: zoneDefaultVolumeEnabled,
        airplay_default_volume: zoneDefaultVolumeEnabled ? zoneDefaultVolumePercent / 100 : null
      });
    }
    const zoneSettingsPayload: JsonRecord = {
      icon: zoneIconDraft,
      device_type: zoneDeviceTypeDraft,
      hegel: zoneDeviceTypeDraft === 'hegel' ? zoneHegelPayload(zoneHegelDraft) : null
    };
    if (isUpnpZone(settingsZone)) {
      zoneSettingsPayload.qobuz_hires_enabled = zoneQobuzHiresEnabled;
      zoneSettingsPayload.upnp_capabilities = {
        max_sample_rate: Number(zoneUpnpCapabilitiesDraft.maxPcmRate),
        max_bit_depth: numberValue((settingsZone.capabilities || {}).max_bit_depth, 24),
        max_dsd_rate:
          zoneUpnpCapabilitiesDraft.maxDsdRate === 'none'
            ? null
            : Number(zoneUpnpCapabilitiesDraft.maxDsdRate)
      };
    }
    await endpoints.updateZoneSettings(settingsZone.id, zoneSettingsPayload);
    setSettingsZoneId(null);
    await onRefresh();
  };

  const saveZoneHegelSettings = async () => {
    if (!settingsZone) return;
    if (isBrowserZone(settingsZone)) return;
    setZoneHegelMessage('Saving Hegel setup...');
    await endpoints.updateZoneSettings(settingsZone.id, {
      device_type: 'hegel',
      hegel: zoneHegelPayload(zoneHegelDraft)
    });
    setZoneDeviceTypeDraft('hegel');
    setZoneHegelMessage('Hegel setup saved');
    await onRefresh();
  };

  const refreshZoneHegelStatus = async () => {
    if (!settingsZone) return;
    if (isBrowserZone(settingsZone)) return;
    const host = zoneHegelDraft.host.trim();
    if (!host) {
      setZoneHegelMessage('Enter the Hegel IP address first');
      return;
    }
    setZoneHegelMessage('Contacting Hegel...');
    try {
      await endpoints.zoneHegelStatus(settingsZone.id, {
        host,
        port: Math.max(1, Math.min(65535, Math.round(zoneHegelDraft.port || 50001)))
      });
      setZoneHegelMessage('Hegel status refreshed');
    } catch (error) {
      setZoneHegelMessage(error instanceof Error ? error.message : 'Hegel status refresh failed');
    }
  };

  const calibrateZoneCapabilities = async () => {
    if (!settingsZone || zoneCalibrationBusy) return;
    if (isBrowserZone(settingsZone)) return;
    setZoneCalibrationBusy(true);
    setZoneCalibrationMessage('Testing...');
    try {
      const response = await endpoints.calibrateZone(settingsZone.id);
      const message =
        typeof response.message === 'string' && response.message.trim()
          ? response.message
          : 'Test finished';
      setZoneCalibrationMessage(message);
      await onRefresh();
    } catch (error) {
      setZoneCalibrationMessage(error instanceof Error ? error.message : 'Capability test failed');
    } finally {
      setZoneCalibrationBusy(false);
    }
  };

  const disableSettingsZone = async () => {
    if (!settingsZone) return;
    await endpoints.disableZone(settingsZone.id);
    setSettingsZoneId(null);
    await onRefresh();
  };

  return {
    calibrateZoneCapabilities,
    disableSettingsZone,
    openZoneSettings,
    saveZoneSettings,
    selectSettingsZone,
    setSettingsZoneId,
    setZoneDefaultVolumeEnabled,
    setZoneDefaultVolumePercent,
    setZoneQobuzHiresEnabled,
    setZoneIconDraft,
    setZoneDeviceTypeDraft,
    setZoneHegelDraft,
    setZoneHegelSettingsOpen,
    setZoneBrowserStreamDraft,
    setZoneNameDraft,
    setZoneUpnpCapabilitiesDraft,
    settingsZone,
    saveZoneHegelSettings,
    refreshZoneHegelStatus,
    zoneBrowserStreamDraft,
    zoneCalibrationBusy,
    zoneCalibrationMessage,
    zoneDeviceTypeDraft,
    zoneDefaultVolumeEnabled,
    zoneDefaultVolumePercent,
    zoneQobuzHiresEnabled,
    zoneIconDraft,
    zoneGroups,
    zoneHegelDraft,
    zoneHegelMessage,
    zoneHegelSettingsOpen,
    zoneNameDraft,
    zoneUpnpCapabilitiesDraft,
    outputSettingsZones
  };
}
