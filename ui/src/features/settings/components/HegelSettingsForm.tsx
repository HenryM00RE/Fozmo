import { type Dispatch, type SetStateAction, useState } from 'react';
import { storageKey } from '../../../shared/identity';
import type { ZoneProfile } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import {
  type HegelControlActions,
  hegelAirplayCandidates,
  hegelInputLabel,
  hegelInputOptions,
  hegelSavedInputLabel
} from '../model/hegelFormModel';
import { type HegelFormState, type HegelModelId, hegelModels } from '../settingsModel';

export function HegelSettingsForm({
  hegelControls,
  hegelMessage,
  hegelSettings,
  setHegelSettings,
  zones
}: {
  hegelControls: HegelControlActions;
  hegelMessage: string;
  hegelSettings: HegelFormState;
  setHegelSettings: Dispatch<SetStateAction<HegelFormState>>;
  zones: ZoneProfile[];
}) {
  const inputOptions = hegelInputOptions(hegelSettings.model);
  const savedInputLabel = hegelSavedInputLabel(hegelSettings.input, hegelSettings.model);
  const airplayCandidates = hegelAirplayCandidates(zones);
  const linkedAirplayZone = airplayCandidates.find(
    (candidate) => candidate.zone.id === hegelSettings.linkedAirplayZoneId
  );
  const linkedAirplaySelectValue = hegelSettings.linkedAirplayZoneId || '';
  const [refreshing, setRefreshing] = useState(false);

  const refreshHegel = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await hegelControls.refreshStatus();
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <div className="settings-grid two-col hegel-settings-grid">
      <section className="settings-section-block">
        <div className="settings-section-heading">
          <div className="section-label">Hegel setup</div>
          <button
            className="settings-heading-refresh"
            type="button"
            title="Refresh Hegel"
            aria-label={refreshing ? 'Refreshing Hegel' : 'Refresh Hegel'}
            disabled={refreshing}
            onClick={refreshHegel}
          >
            {refreshing ? (
              <span className="settings-refresh-spinner" aria-hidden="true" />
            ) : (
              <Icon path="M21 3v5h-5M20.1 13.5a7.5 7.5 0 1 1-2-7.1L21 8" />
            )}
          </button>
        </div>
        <div className="panel raised">
          <div className="settings-list compact-list">
            <div className="setting-row control-row">
              <span>
                <strong>Hegel output</strong>
                <small>
                  When this output plays, the amp powers on and selects {savedInputLabel}.
                </small>
              </span>
              <SelectMenu
                ariaLabel="Hegel output"
                value={hegelSettings.zoneId}
                onChange={(value) => setHegelSettings((current) => ({ ...current, zoneId: value }))}
                options={[
                  { value: '', label: 'Choose output' },
                  ...zones.map((zone) => ({ value: zone.id, label: zone.name }))
                ]}
              />
            </div>
            <div className="setting-row control-row">
              <span>
                <strong>Host</strong>
                <small>{hegelMessage}</small>
              </span>
              <input
                className="zone-settings-input hegel-input"
                type="text"
                value={hegelSettings.host}
                onChange={(event) =>
                  setHegelSettings((current) => ({ ...current, host: event.target.value }))
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
              <div className="hegel-detected-control">
                <SelectMenu
                  ariaLabel="Network link"
                  value={linkedAirplaySelectValue}
                  onChange={(value) => {
                    const next = airplayCandidates.find((candidate) => candidate.zone.id === value);
                    setHegelSettings((current) => ({
                      ...current,
                      linkedAirplayZoneId: next?.zone.id || '',
                      host: next?.host || current.host,
                      port: next ? 50001 : current.port
                    }));
                  }}
                  options={[
                    { value: '', label: 'No AirPlay link' },
                    ...(hegelSettings.linkedAirplayZoneId && !linkedAirplayZone
                      ? [
                          {
                            value: hegelSettings.linkedAirplayZoneId,
                            label: 'Saved link (not visible)'
                          }
                        ]
                      : []),
                    ...airplayCandidates.map(({ zone, host }) => ({
                      value: zone.id,
                      label: `${zone.name} (${host})`
                    }))
                  ]}
                />
              </div>
            </div>
            <div className="setting-row">
              <span>
                <strong>Show USB in standby</strong>
                <small>
                  Keep the configured USB output selectable when the Hegel network link is visible.
                </small>
              </span>
              <button
                className={`toggle${hegelSettings.standbyUsbVisible ? ' on' : ''}`}
                type="button"
                aria-label="Show USB in standby"
                aria-pressed={hegelSettings.standbyUsbVisible}
                onClick={() =>
                  setHegelSettings((current) => ({
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
                value={hegelSettings.port}
                onChange={(event) =>
                  setHegelSettings((current) => ({ ...current, port: Number(event.target.value) }))
                }
              />
            </div>
            <div className="setting-row hegel-limits-row">
              <span>
                <strong>Startup volume</strong>
                <small>Applied each time music starts on the Hegel output.</small>
              </span>
              <span className="hegel-number-control">
                <input
                  className="zone-settings-input"
                  type="text"
                  inputMode="numeric"
                  value={`${hegelSettings.defaultVolume}%`}
                  onChange={(event) =>
                    setHegelSettings((current) => ({
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
                  value={`${hegelSettings.maxVolume}%`}
                  onChange={(event) =>
                    setHegelSettings((current) => ({
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
                value={hegelSettings.model}
                onChange={(value) => {
                  const model = value as HegelModelId;
                  localStorage.setItem(storageKey('HegelModel'), model);
                  setHegelSettings((current) => ({
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
                <small>Playback on the Hegel output will switch the amp to this input.</small>
              </span>
              <SelectMenu
                ariaLabel="Auto-select input"
                value={String(hegelSettings.input)}
                onChange={(value) =>
                  setHegelSettings((current) => ({ ...current, input: Number(value) }))
                }
                options={inputOptions.map((input) => ({
                  value: String(input),
                  label: hegelInputLabel(input, hegelSettings.model)
                }))}
              />
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}
