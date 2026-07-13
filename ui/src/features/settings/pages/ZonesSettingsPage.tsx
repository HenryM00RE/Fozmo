import { type Dispatch, type SetStateAction, useEffect, useRef, useState } from 'react';
import { isBrowserZone } from '../../../shared/lib/browserZone';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import {
  type OutputIconId,
  outputIconOptions,
  ZoneOutputIcon
} from '../../../shared/ui/ZoneOutputIcon';
import {
  BROWSER_OPUS_KBPS_OPTIONS,
  type ZoneBrowserStreamDraft,
  type ZoneDeviceType,
  type ZoneHegelDraft,
  type ZoneUpnpCapabilitiesDraft
} from '../hooks/useZonesSettings';
import {
  hegelAirplayCandidates,
  hegelInputLabel,
  hegelInputOptions
} from '../model/hegelFormModel';
import {
  type HegelModelId,
  hegelModels,
  isAirPlayNetworkZone,
  isUpnpZone,
  upnpDsdCapabilityWarning,
  upnpDsdRateOptions,
  upnpPcmCapabilityWarning,
  upnpPcmRateOptions,
  zoneCapabilityLabels,
  zoneDisplayName,
  zoneFormatLabel
} from '../settingsModel';

type ZoneGroup = {
  key: string;
  label: string;
  zones: ZoneProfile[];
};

export function ZonesSettingsPage({
  calibrateZoneCapabilities,
  disableSettingsZone,
  hegelAvailable,
  onRefresh,
  openZoneSettings,
  refreshZoneHegelStatus,
  saveZoneHegelSettings,
  saveZoneSettings,
  selectSettingsZone,
  setSettingsZoneId,
  setZoneBrowserStreamDraft,
  setZoneDefaultVolumeEnabled,
  setZoneDefaultVolumePercent,
  setZoneQobuzHiresEnabled,
  setZoneDeviceTypeDraft,
  setZoneHegelDraft,
  setZoneHegelSettingsOpen,
  setZoneIconDraft,
  setZoneNameDraft,
  setZoneUpnpCapabilitiesDraft,
  settingsZone,
  status,
  zoneBrowserStreamDraft,
  zoneCalibrationBusy,
  zoneCalibrationMessage,
  zoneDeviceTypeDraft,
  zoneDefaultVolumeEnabled,
  zoneDefaultVolumePercent,
  zoneQobuzHiresEnabled,
  zoneGroups,
  zoneHegelDraft,
  zoneHegelMessage,
  zoneHegelSettingsOpen,
  zoneIconDraft,
  zoneNameDraft,
  zoneUpnpCapabilitiesDraft,
  zones
}: {
  calibrateZoneCapabilities: () => Promise<void>;
  disableSettingsZone: () => Promise<void>;
  hegelAvailable: boolean;
  onRefresh: () => Promise<void>;
  openZoneSettings: (zone: ZoneProfile) => void;
  refreshZoneHegelStatus: () => Promise<void>;
  saveZoneHegelSettings: () => Promise<void>;
  saveZoneSettings: () => Promise<void>;
  selectSettingsZone: (zone: ZoneProfile) => Promise<void>;
  setSettingsZoneId: Dispatch<SetStateAction<string | null>>;
  setZoneBrowserStreamDraft: Dispatch<SetStateAction<ZoneBrowserStreamDraft>>;
  setZoneDefaultVolumeEnabled: Dispatch<SetStateAction<boolean>>;
  setZoneDefaultVolumePercent: Dispatch<SetStateAction<number>>;
  setZoneQobuzHiresEnabled: Dispatch<SetStateAction<boolean>>;
  setZoneDeviceTypeDraft: Dispatch<SetStateAction<ZoneDeviceType>>;
  setZoneHegelDraft: Dispatch<SetStateAction<ZoneHegelDraft>>;
  setZoneHegelSettingsOpen: Dispatch<SetStateAction<boolean>>;
  setZoneIconDraft: Dispatch<SetStateAction<OutputIconId>>;
  setZoneNameDraft: Dispatch<SetStateAction<string>>;
  setZoneUpnpCapabilitiesDraft: Dispatch<SetStateAction<ZoneUpnpCapabilitiesDraft>>;
  settingsZone: ZoneProfile | null;
  status: JsonRecord;
  zoneBrowserStreamDraft: ZoneBrowserStreamDraft;
  zoneCalibrationBusy: boolean;
  zoneCalibrationMessage: string;
  zoneDeviceTypeDraft: ZoneDeviceType;
  zoneDefaultVolumeEnabled: boolean;
  zoneDefaultVolumePercent: number;
  zoneQobuzHiresEnabled: boolean;
  zoneGroups: ZoneGroup[];
  zoneHegelDraft: ZoneHegelDraft;
  zoneHegelMessage: string;
  zoneHegelSettingsOpen: boolean;
  zoneIconDraft: OutputIconId;
  zoneNameDraft: string;
  zoneUpnpCapabilitiesDraft: ZoneUpnpCapabilitiesDraft;
  zones: ZoneProfile[];
}) {
  const [refreshing, setRefreshing] = useState(false);
  const [iconPickerOpen, setIconPickerOpen] = useState(false);
  const iconPickerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setIconPickerOpen(false);
  }, [settingsZone?.id]);

  useEffect(() => {
    if (!iconPickerOpen) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      if (!iconPickerRef.current?.contains(event.target as Node)) setIconPickerOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setIconPickerOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [iconPickerOpen]);

  const settingsZoneIconPreview = settingsZone
    ? {
        ...settingsZone,
        device_type: zoneDeviceTypeDraft === 'hegel' ? 'hegel' : null
      }
    : null;
  const settingsZoneCapabilities = settingsZone ? zoneCapabilityLabels(settingsZone) : null;
  const upnpPcmOptionsForZone = settingsZone
    ? upnpPcmRateOptions.map((option) => {
        const warning = upnpPcmCapabilityWarning(settingsZone, option.value);
        return warning
          ? {
              ...option,
              after: <CapabilityWarning title={warning} />
            }
          : option;
      })
    : upnpPcmRateOptions;
  const upnpDsdOptionsForZone = settingsZone
    ? upnpDsdRateOptions.map((option) => {
        const warning = upnpDsdCapabilityWarning(settingsZone, option.value);
        return warning
          ? {
              ...option,
              after: <CapabilityWarning title={warning} />
            }
          : option;
      })
    : upnpDsdRateOptions;

  const refreshOutputs = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <section className="settings-panel zones-settings-panel">
      <div className="settings-zone-groups">
        {!zones.length ? (
          <div className="panel raised zones-panel-card zone-empty-card">
            <div className="zone-empty">No outputs discovered.</div>
          </div>
        ) : null}
        {zoneGroups.map((group, index) => (
          <section className="zone-output-section zone-settings-group" key={group.key}>
            <div className="zone-output-heading zone-settings-group-heading">
              <div className="section-label">{group.label}</div>
              {index === 0 ? (
                <button
                  className="zone-output-refresh"
                  type="button"
                  title="Refresh outputs"
                  aria-label={refreshing ? 'Refreshing outputs' : 'Refresh outputs'}
                  disabled={refreshing}
                  onClick={refreshOutputs}
                >
                  {refreshing ? (
                    <span className="settings-refresh-spinner" aria-hidden="true" />
                  ) : (
                    <Icon path="M21 3v5h-5M20.1 13.5a7.5 7.5 0 1 1-2-7.1L21 8" />
                  )}
                </button>
              ) : null}
            </div>
            <div className="panel raised zones-panel-card">
              <div className="zone-output-grid">
                {group.zones.map((zone) => {
                  const enabled = zone.enabled !== false;
                  const active =
                    enabled && (zone.status === 'active' || status.active_zone_id === zone.id);
                  return (
                    <div
                      className={`zone-output-card${enabled ? ' is-enabled' : ' is-disabled'}${active ? ' is-active' : ''}`}
                      key={zone.id}
                    >
                      <div className="zone-output-main">
                        <span className="zone-output-logo">
                          <ZoneOutputIcon zone={zone} />
                        </span>
                        <div className="zone-output-copy">
                          <strong>{zoneDisplayName(zone)}</strong>
                          <small>{zoneFormatLabel(zone)}</small>
                        </div>
                      </div>
                      <div className="zone-output-actions">
                        <button
                          className={
                            enabled ? 'zone-output-icon-action' : 'zone-output-action primary'
                          }
                          type="button"
                          title={
                            enabled
                              ? `Settings for ${zoneDisplayName(zone)}`
                              : `Enable ${zoneDisplayName(zone)}`
                          }
                          aria-label={
                            enabled
                              ? `Settings for ${zoneDisplayName(zone)}`
                              : `Enable ${zoneDisplayName(zone)}`
                          }
                          onClick={() =>
                            enabled ? openZoneSettings(zone) : selectSettingsZone(zone)
                          }
                        >
                          {enabled ? (
                            <Icon path="M9.67 4.14a2.34 2.34 0 0 1 4.66 0 2.34 2.34 0 0 0 3.32 1.91 2.34 2.34 0 0 1 2.33 4.03 2.34 2.34 0 0 0 0 3.84 2.34 2.34 0 0 1-2.33 4.03 2.34 2.34 0 0 0-3.32 1.91 2.34 2.34 0 0 1-4.66 0 2.34 2.34 0 0 0-3.32-1.91 2.34 2.34 0 0 1-2.33-4.03 2.34 2.34 0 0 0 0-3.84 2.34 2.34 0 0 1 2.33-4.03 2.34 2.34 0 0 0 3.32-1.91ZM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z" />
                          ) : (
                            <span>Enable</span>
                          )}
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          </section>
        ))}
      </div>
      {settingsZone ? (
        <div
          className="zone-settings-backdrop is-open"
          role="dialog"
          aria-modal="true"
          aria-labelledby="zone-settings-title"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) setSettingsZoneId(null);
          }}
        >
          <div className="zone-settings-panel">
            <header className="zone-settings-head">
              <div className="zone-settings-identity">
                <div className="zone-output-logo-picker-wrap" ref={iconPickerRef}>
                  <button
                    className={`zone-output-logo-control${iconPickerOpen ? ' is-open' : ''}`}
                    type="button"
                    title="Change output icon"
                    aria-label="Change output icon"
                    aria-haspopup="menu"
                    aria-expanded={iconPickerOpen}
                    onClick={() => setIconPickerOpen((current) => !current)}
                  >
                    <span className="zone-output-logo">
                      <ZoneOutputIcon
                        zone={settingsZoneIconPreview || settingsZone}
                        icon={zoneIconDraft}
                      />
                    </span>
                    <span className="zone-output-logo-edit" aria-hidden="true">
                      <Icon path="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
                    </span>
                  </button>
                  {iconPickerOpen ? (
                    <ZoneIconPicker
                      onClose={() => setIconPickerOpen(false)}
                      setValue={setZoneIconDraft}
                      value={zoneIconDraft}
                      zone={settingsZoneIconPreview || settingsZone}
                    />
                  ) : null}
                </div>
                <div>
                  <span className="section-label">{zoneFormatLabel(settingsZone)}</span>
                  <h2 id="zone-settings-title">{zoneDisplayName(settingsZone)}</h2>
                </div>
              </div>
              <button
                className="zone-settings-close"
                type="button"
                aria-label="Close"
                onClick={() => setSettingsZoneId(null)}
              >
                <Icon path="M18 6 6 18M6 6l12 12" />
              </button>
            </header>
            <div className="zone-settings-body">
              {!isBrowserZone(settingsZone) ? (
                <label className="zone-settings-field">
                  <span className="section-label">Rename</span>
                  <input
                    className="zone-settings-input"
                    type="text"
                    value={zoneNameDraft}
                    onChange={(event) => setZoneNameDraft(event.target.value)}
                  />
                </label>
              ) : null}
              {isBrowserZone(settingsZone) ? (
                <div className="zone-settings-device-type" aria-label="Browser stream settings">
                  <span className="section-label">Stream format</span>
                  <div className="zone-device-type-controls zone-browser-stream-controls zones-panel-card">
                    <SelectMenu
                      ariaLabel="Browser stream format"
                      value={zoneBrowserStreamDraft.format}
                      onChange={(value) =>
                        setZoneBrowserStreamDraft((draft) => ({
                          ...draft,
                          format: value === 'opus' ? 'opus' : 'flac'
                        }))
                      }
                      options={[
                        { value: 'flac', label: 'Lossless (FLAC)' },
                        { value: 'opus', label: 'Opus' }
                      ]}
                    />
                    {zoneBrowserStreamDraft.format === 'opus' ? (
                      <div className="zone-browser-bitrate-control">
                        <span className="section-label">Opus bitrate</span>
                        <SelectMenu
                          ariaLabel="Opus bitrate"
                          value={String(zoneBrowserStreamDraft.opusKbps)}
                          onChange={(value) =>
                            setZoneBrowserStreamDraft((draft) => ({
                              ...draft,
                              opusKbps: Number(value)
                            }))
                          }
                          options={BROWSER_OPUS_KBPS_OPTIONS.map((kbps) => ({
                            value: String(kbps),
                            label: `${kbps} kbps`
                          }))}
                        />
                      </div>
                    ) : null}
                  </div>
                  <small className="zone-calibration-message">
                    Parametric EQ for this output is applied on the server and baked into the
                    stream; EQ changes take effect from the next track.
                  </small>
                </div>
              ) : null}
              {isUpnpZone(settingsZone) ? (
                <div
                  className="zone-upnp-capability-controls"
                  aria-label="UPnP output capabilities"
                >
                  <label className="zone-upnp-capability-control">
                    <span>Max PCM Rate</span>
                    <SelectMenu
                      ariaLabel="Maximum UPnP PCM rate"
                      className="zone-capability-select"
                      value={zoneUpnpCapabilitiesDraft.maxPcmRate}
                      onChange={(value) =>
                        setZoneUpnpCapabilitiesDraft((draft) => ({
                          ...draft,
                          maxPcmRate: value
                        }))
                      }
                      options={upnpPcmOptionsForZone}
                    />
                  </label>
                  <label className="zone-upnp-capability-control">
                    <span>Max DSD Rate</span>
                    <SelectMenu
                      ariaLabel="Maximum UPnP DSD rate"
                      className="zone-capability-select"
                      value={zoneUpnpCapabilitiesDraft.maxDsdRate}
                      onChange={(value) =>
                        setZoneUpnpCapabilitiesDraft((draft) => ({
                          ...draft,
                          maxDsdRate: value
                        }))
                      }
                      options={upnpDsdOptionsForZone}
                    />
                  </label>
                </div>
              ) : settingsZoneCapabilities && !isBrowserZone(settingsZone) ? (
                <div className="zone-capability-grid" aria-label="Output capabilities">
                  <div className="zone-capability-item">
                    <span>Max PCM Rate</span>
                    <strong>{settingsZoneCapabilities.pcm}</strong>
                  </div>
                  <div className="zone-capability-item">
                    <span>Max DSD Rate</span>
                    <strong>{settingsZoneCapabilities.dsd}</strong>
                  </div>
                </div>
              ) : null}
              {isUpnpZone(settingsZone) ? (
                <div className="zone-calibration-block">
                  <span className="section-label">Test the maximum accepted sample rate</span>
                  <div className="zone-calibration-row">
                    <button
                      className="zone-settings-pill zone-calibration-button"
                      type="button"
                      title="Test UPnP capabilities"
                      aria-label="Test UPnP capabilities"
                      disabled={zoneCalibrationBusy}
                      onClick={calibrateZoneCapabilities}
                    >
                      <span>{zoneCalibrationBusy ? 'Testing...' : 'Test'}</span>
                    </button>
                    {zoneCalibrationMessage ? (
                      <small className="zone-calibration-message">{zoneCalibrationMessage}</small>
                    ) : null}
                  </div>
                </div>
              ) : null}
              {isUpnpZone(settingsZone) ? (
                <div className="setting-row zone-qobuz-hires-row">
                  <span>
                    <strong>Qobuz Hi-Res</strong>
                    <small>
                      Stream Qobuz FLAC above 16/44.1 when this DLNA renderer supports it.
                    </small>
                  </span>
                  <button
                    className={`toggle${zoneQobuzHiresEnabled ? ' on' : ''}`}
                    type="button"
                    aria-label="Toggle Qobuz Hi-Res"
                    aria-pressed={zoneQobuzHiresEnabled}
                    onClick={() => setZoneQobuzHiresEnabled((enabled) => !enabled)}
                  />
                </div>
              ) : null}
              {isAirPlayNetworkZone(settingsZone) ? (
                <div className="zone-settings-volume">
                  <label className="zone-volume-toggle">
                    <input
                      type="checkbox"
                      checked={zoneDefaultVolumeEnabled}
                      onChange={(event) => setZoneDefaultVolumeEnabled(event.target.checked)}
                    />
                    <span>Default volume</span>
                  </label>
                  <div className="zone-volume-slider-row">
                    <input
                      type="range"
                      min="0"
                      max="100"
                      value={zoneDefaultVolumePercent}
                      className="custom-slider volume-slider"
                      disabled={!zoneDefaultVolumeEnabled}
                      onChange={(event) => setZoneDefaultVolumePercent(Number(event.target.value))}
                      aria-label="Default AirPlay volume"
                    />
                    <strong>{zoneDefaultVolumeEnabled ? zoneDefaultVolumePercent : '--'}</strong>
                  </div>
                </div>
              ) : null}
              {hegelAvailable && !isBrowserZone(settingsZone) ? (
                <div className="zone-settings-device-type">
                  <span className="section-label">Device type</span>
                  <div className="zone-device-type-controls zones-panel-card">
                    <SelectMenu
                      ariaLabel="Device type"
                      value={zoneDeviceTypeDraft}
                      onChange={(value) => setZoneDeviceTypeDraft(value as ZoneDeviceType)}
                      options={[
                        { value: 'none', label: 'None' },
                        { value: 'hegel', label: 'Hegel' }
                      ]}
                    />
                    {zoneDeviceTypeDraft === 'hegel' ? (
                      <button
                        className="zone-output-icon-action"
                        type="button"
                        title="Hegel settings"
                        aria-label="Hegel settings"
                        onClick={() => setZoneHegelSettingsOpen(true)}
                      >
                        <Icon path="M9.67 4.14a2.34 2.34 0 0 1 4.66 0 2.34 2.34 0 0 0 3.32 1.91 2.34 2.34 0 0 1 2.33 4.03 2.34 2.34 0 0 0 0 3.84 2.34 2.34 0 0 1-2.33 4.03 2.34 2.34 0 0 0-3.32 1.91 2.34 2.34 0 0 1-4.66 0 2.34 2.34 0 0 0-3.32-1.91 2.34 2.34 0 0 1-2.33-4.03 2.34 2.34 0 0 0 0-3.84 2.34 2.34 0 0 1 2.33-4.03 2.34 2.34 0 0 0 3.32-1.91ZM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z" />
                      </button>
                    ) : null}
                  </div>
                </div>
              ) : null}
              <footer className="zone-settings-foot">
                <button
                  className="zone-settings-danger"
                  type="button"
                  onClick={disableSettingsZone}
                >
                  Disable
                </button>
                <span className="zone-settings-spacer" />
                <button
                  className="zone-settings-pill"
                  type="button"
                  onClick={() => setSettingsZoneId(null)}
                >
                  Close
                </button>
                <button
                  className="zone-settings-pill primary"
                  type="button"
                  onClick={saveZoneSettings}
                >
                  Save
                </button>
              </footer>
            </div>
          </div>
          {hegelAvailable && zoneHegelSettingsOpen ? (
            <ZoneHegelSettingsModal
              draft={zoneHegelDraft}
              message={zoneHegelMessage}
              onClose={() => setZoneHegelSettingsOpen(false)}
              onRefresh={refreshZoneHegelStatus}
              onSave={saveZoneHegelSettings}
              setDraft={setZoneHegelDraft}
              zones={zones}
            />
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

function ZoneIconPicker({
  onClose,
  setValue,
  value,
  zone
}: {
  onClose: () => void;
  setValue: Dispatch<SetStateAction<OutputIconId>>;
  value: OutputIconId;
  zone: ZoneProfile;
}) {
  return (
    <div className="zone-icon-picker" role="menu" aria-label="Output icon">
      <div className="zone-icon-picker-grid">
        {outputIconOptions.map((option) => {
          const selected = option.value === value;
          return (
            <button
              className={`zone-icon-picker-option${selected ? ' is-selected' : ''}`}
              type="button"
              role="menuitemradio"
              aria-checked={selected}
              aria-label={option.label}
              title={option.label}
              key={option.value}
              onClick={() => {
                setValue(option.value);
                onClose();
              }}
            >
              <span className="zone-output-logo zone-icon-picker-preview">
                <ZoneOutputIcon zone={zone} icon={option.value} />
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

function CapabilityWarning({ title }: { title: string }) {
  return (
    <span className="zone-capability-warning" title={title} aria-label={title}>
      <Icon path="M12 9v4M12 17h.01M10.3 3.9 2.2 18a2 2 0 0 0 1.7 3h16.2a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z" />
    </span>
  );
}

function ZoneHegelSettingsModal({
  draft,
  message,
  onClose,
  onRefresh,
  onSave,
  setDraft,
  zones
}: {
  draft: ZoneHegelDraft;
  message: string;
  onClose: () => void;
  onRefresh: () => Promise<void>;
  onSave: () => Promise<void>;
  setDraft: Dispatch<SetStateAction<ZoneHegelDraft>>;
  zones: ZoneProfile[];
}) {
  const airplayCandidates = hegelAirplayCandidates(zones);
  const linkedAirplayZone = airplayCandidates.find(
    (candidate) => candidate.zone.id === draft.linkedAirplayZoneId
  );
  const inputOptions = hegelInputOptions(draft.model);

  return (
    <div
      className="zone-settings-backdrop zone-hegel-settings-backdrop is-open"
      role="dialog"
      aria-modal="true"
      aria-labelledby="zone-hegel-settings-title"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="zone-settings-panel zone-hegel-settings-panel">
        <header className="zone-settings-head">
          <div className="zone-settings-identity zone-hegel-settings-identity">
            <div>
              <span className="section-label">Hegel control</span>
              <h2 id="zone-hegel-settings-title">Output Hegel settings</h2>
            </div>
          </div>
          <div className="zone-hegel-header-actions">
            <button
              className="zone-output-icon-action"
              type="button"
              title="Refresh Hegel"
              aria-label="Refresh Hegel"
              onClick={onRefresh}
            >
              <Icon path="M21 3v5h-5M20.1 13.5a7.5 7.5 0 1 1-2-7.1L21 8" />
            </button>
            <button
              className="zone-settings-close"
              type="button"
              aria-label="Close"
              onClick={onClose}
            >
              <Icon path="M18 6 6 18M6 6l12 12" />
            </button>
          </div>
        </header>
        <div className="zone-settings-body">
          <div className="setting-row control-row">
            <span>
              <strong>Host</strong>
              <small>{message || 'Enter the amp IP address, then refresh.'}</small>
            </span>
            <input
              className="zone-settings-input hegel-input"
              type="text"
              value={draft.host}
              onChange={(event) =>
                setDraft((current) => ({ ...current, host: event.target.value }))
              }
              placeholder="192.168.1.50"
            />
          </div>
          <div className="setting-row control-row">
            <span>
              <strong>Network link</strong>
              <small>
                {airplayCandidates.length
                  ? 'Link the Hegel AirPlay device so USB can stay selectable in standby.'
                  : 'No network AirPlay devices are visible yet.'}
              </small>
            </span>
            <SelectMenu
              ariaLabel="Network link"
              value={draft.linkedAirplayZoneId}
              onChange={(value) => {
                const next = airplayCandidates.find((candidate) => candidate.zone.id === value);
                setDraft((current) => ({
                  ...current,
                  linkedAirplayZoneId: next?.zone.id || '',
                  host: next?.host || current.host,
                  port: next ? 50001 : current.port
                }));
              }}
              options={[
                { value: '', label: 'No AirPlay link' },
                ...(draft.linkedAirplayZoneId && !linkedAirplayZone
                  ? [{ value: draft.linkedAirplayZoneId, label: 'Saved link (not visible)' }]
                  : []),
                ...airplayCandidates.map(({ zone, host }) => ({
                  value: zone.id,
                  label: `${zone.name} (${host})`
                }))
              ]}
            />
          </div>
          <div className="setting-row">
            <span>
              <strong>Show USB in standby</strong>
              <small>
                Keep the configured USB output selectable when the Hegel network link is visible.
              </small>
            </span>
            <button
              className={`toggle${draft.standbyUsbVisible ? ' on' : ''}`}
              type="button"
              aria-label="Show USB in standby"
              aria-pressed={draft.standbyUsbVisible}
              onClick={() =>
                setDraft((current) => ({
                  ...current,
                  standbyUsbVisible: !current.standbyUsbVisible
                }))
              }
            />
          </div>
          <div className="setting-row control-row">
            <span>
              <strong>Port</strong>
              <small>Hegel IP control usually listens on 50001.</small>
            </span>
            <input
              className="zone-settings-input hegel-port-input"
              type="number"
              min="1"
              max="65535"
              value={draft.port}
              onChange={(event) =>
                setDraft((current) => ({ ...current, port: Number(event.target.value) }))
              }
            />
          </div>
          <div className="setting-row hegel-limits-row">
            <span>
              <strong>Startup volume</strong>
              <small>Applied each time music starts on this output.</small>
            </span>
            <span className="hegel-number-control">
              <input
                className="zone-settings-input"
                type="text"
                inputMode="numeric"
                value={`${draft.defaultVolume}%`}
                onChange={(event) =>
                  setDraft((current) => ({
                    ...current,
                    defaultVolume: Math.min(
                      current.maxVolume,
                      Math.max(0, Number(event.target.value.replace(/\D/g, '')) || 0)
                    )
                  }))
                }
              />
            </span>
          </div>
          <div className="setting-row hegel-limits-row">
            <span>
              <strong>Maximum volume</strong>
              <small>The footer volume control is capped at this level.</small>
            </span>
            <span className="hegel-number-control">
              <input
                className="zone-settings-input"
                type="text"
                inputMode="numeric"
                value={`${draft.maxVolume}%`}
                onChange={(event) =>
                  setDraft((current) => ({
                    ...current,
                    maxVolume: Math.min(
                      100,
                      Math.max(0, Number(event.target.value.replace(/\D/g, '')) || 0)
                    )
                  }))
                }
              />
            </span>
          </div>
          <div className="setting-row control-row">
            <span>
              <strong>Model</strong>
              <small>Used to label the common USB and XLR input shortcuts.</small>
            </span>
            <SelectMenu
              ariaLabel="Hegel model"
              value={draft.model}
              onChange={(value) => {
                const model = value as HegelModelId;
                setDraft((current) => ({
                  ...current,
                  model,
                  input: Math.min(current.input, hegelModels[model].inputs)
                }));
              }}
              options={Object.entries(hegelModels).map(([id, model]) => ({
                value: id,
                label: model.label
              }))}
            />
          </div>
          <div className="setting-row control-row">
            <span>
              <strong>Auto-select input</strong>
              <small>Playback on this output will switch the amp to this input.</small>
            </span>
            <SelectMenu
              ariaLabel="Auto-select input"
              value={String(draft.input)}
              onChange={(value) => setDraft((current) => ({ ...current, input: Number(value) }))}
              options={inputOptions.map((input) => ({
                value: String(input),
                label: hegelInputLabel(input, draft.model)
              }))}
            />
          </div>
          <footer className="zone-settings-foot">
            <span className="zone-settings-spacer" />
            <button className="zone-settings-pill" type="button" onClick={onClose}>
              Close
            </button>
            <button
              className="zone-settings-pill primary"
              type="button"
              onClick={() => {
                onSave()
                  .then(onClose)
                  .catch(() => undefined);
              }}
            >
              Save
            </button>
          </footer>
        </div>
      </div>
    </div>
  );
}
