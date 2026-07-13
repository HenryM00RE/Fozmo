import type { PointerEvent as ReactPointerEvent } from 'react';
import { isBrowserZone } from '../../../shared/lib/browserZone';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { type EqCurve, EqCurveEditor } from '../components/EqCurveEditor';
import { SettingsZoneSelect } from '../components/SettingsZoneSelect';
import { eqBandTypes, numberValue } from '../settingsModel';

export function EqSettingsPage({
  deleteEqPreset,
  dragEqBand,
  draggingEqBand,
  eqBands,
  eqConfig,
  eqCurve,
  eqPresetName,
  eqPresets,
  loadEqPreset,
  resetEqParameters,
  saveEqPreset,
  selectedDeviceName,
  selectedZoneId,
  settingsZones,
  setEqPresetName,
  startEqBandDrag,
  stopEqBandDrag,
  updateEq,
  updateEqBand,
  onSelectedZoneChange
}: {
  deleteEqPreset: () => Promise<void>;
  dragEqBand: (event: ReactPointerEvent<SVGSVGElement>) => void;
  draggingEqBand: number | null;
  eqBands: JsonRecord[];
  eqConfig: JsonRecord | null;
  eqCurve: EqCurve;
  eqPresetName: string;
  eqPresets: JsonRecord[];
  loadEqPreset: (name: string) => Promise<void>;
  resetEqParameters: () => void;
  saveEqPreset: () => Promise<void>;
  selectedDeviceName: string;
  selectedZoneId: string;
  settingsZones: ZoneProfile[];
  setEqPresetName: (name: string) => void;
  startEqBandDrag: (index: number, event: ReactPointerEvent<SVGCircleElement>) => void;
  stopEqBandDrag: (event: ReactPointerEvent<SVGSVGElement>) => void;
  updateEq: <K extends keyof JsonRecord>(key: K, value: JsonRecord[K]) => void;
  updateEqBand: (index: number, patch: JsonRecord) => void;
  onSelectedZoneChange: (zoneId: string) => void;
}) {
  const browserZoneSelected = isBrowserZone(
    settingsZones.find((zone) => zone.id === selectedZoneId)
  );
  return (
    <section className="settings-panel">
      <section className="settings-section-block">
        {browserZoneSelected ? (
          <div className="dsp-settings-hint" role="note">
            EQ for browser outputs is applied on the server and baked into the stream. Changes take
            effect from the next track.
          </div>
        ) : null}
        <div className="settings-section-heading dsp-settings-heading">
          <div className="section-label">Ten-band parametric EQ</div>
          <SettingsZoneSelect
            ariaLabel={`EQ output: ${selectedDeviceName}`}
            selectedZoneId={selectedZoneId}
            selectedZoneLabel={selectedDeviceName}
            zones={settingsZones}
            onChange={onSelectedZoneChange}
          />
          <button
            className={`toggle${eqConfig?.enabled ? ' on' : ''}`}
            type="button"
            aria-label="Enable EQ"
            aria-pressed={Boolean(eqConfig?.enabled)}
            onClick={() => updateEq('enabled', !eqConfig?.enabled)}
          />
        </div>
        <div className="panel raised eq-panel">
          <div className="eq-header">
            <div className="eq-preset-controls">
              <SelectMenu
                ariaLabel="EQ preset"
                value={eqPresetName}
                onChange={loadEqPreset}
                options={[
                  { value: '', label: 'New Preset' },
                  ...eqPresets.map((preset) => ({
                    value: String(preset.name || ''),
                    label: String(preset.name)
                  }))
                ]}
              />
              <input
                className="eq-preset-name"
                type="text"
                value={eqPresetName}
                onChange={(event) => setEqPresetName(event.target.value)}
                placeholder="Preset name"
              />
              <div className="eq-preset-actions">
                <button className="pill eq-preset-save" type="button" onClick={saveEqPreset}>
                  Save
                </button>
                <div className="eq-preset-icon-pill" aria-label="Preset actions">
                  <button
                    className="eq-preset-icon-button"
                    type="button"
                    aria-label="Reset preset parameters"
                    title="Reset"
                    onClick={resetEqParameters}
                  >
                    <Icon path="M18 3v4h-4M6 21v-4h4M18 7a8 8 0 0 0-13.3 3M6 17a8 8 0 0 0 13.3-3" />
                  </button>
                  <span className="eq-preset-icon-divider" aria-hidden="true" />
                  <button
                    className="eq-preset-icon-button danger"
                    type="button"
                    aria-label="Delete preset"
                    title="Delete"
                    onClick={deleteEqPreset}
                  >
                    <Icon path="M3 6h18M8 6V4h8v2m-9 0 1 14h8l1-14M10 11v6M14 11v6" />
                  </button>
                </div>
              </div>
            </div>
          </div>

          <EqCurveEditor
            dragEqBand={dragEqBand}
            draggingEqBand={draggingEqBand}
            eqCurve={eqCurve}
            startEqBandDrag={startEqBandDrag}
            stopEqBandDrag={stopEqBandDrag}
          />

          <div className="eq-bands-row">
            <div className="eq-bands">
              {eqBands.map((band, index) => (
                <div className={`eq-band${band.enabled ? ' band-enabled-on' : ''}`} key={index}>
                  <div className="eq-band-header">
                    <span className="eq-band-index">{index + 1}</span>
                    <input
                      className="eq-band-enable"
                      type="checkbox"
                      checked={Boolean(band.enabled)}
                      onChange={(event) => updateEqBand(index, { enabled: event.target.checked })}
                    />
                  </div>
                  <div className="eq-band-type-wrap">
                    <SelectMenu
                      ariaLabel={`Band ${index + 1} type`}
                      className="eq-band-type"
                      value={String(band.type || 'peaking')}
                      onChange={(value) => updateEqBand(index, { type: value })}
                      options={eqBandTypes.map((type) => ({
                        value: type.value,
                        label: type.label
                      }))}
                    />
                  </div>
                  <div className="eq-band-meta">
                    <div className="eq-band-input-cell">
                      <span className="eq-cell-label">Hz</span>
                      <input
                        className="eq-band-freq"
                        type="number"
                        min="20"
                        max="22050"
                        step="1"
                        value={numberValue(band.freq_hz, 1000)}
                        onChange={(event) =>
                          updateEqBand(index, { freq_hz: Number(event.target.value) })
                        }
                      />
                    </div>
                    <div className="eq-band-input-cell">
                      <span className="eq-cell-label">Q</span>
                      <input
                        className="eq-band-q"
                        type="number"
                        min="0.1"
                        max="20"
                        step="0.1"
                        value={numberValue(band.q, 0.7)}
                        onChange={(event) => updateEqBand(index, { q: Number(event.target.value) })}
                      />
                    </div>
                    <div className="eq-band-input-cell">
                      <span className="eq-cell-label">dB</span>
                      <input
                        className="eq-band-gain-num"
                        type="number"
                        min="-24"
                        max="24"
                        step="0.1"
                        value={numberValue(band.gain_db, 0).toFixed(1)}
                        onChange={(event) =>
                          updateEqBand(index, { gain_db: Number(event.target.value) })
                        }
                      />
                    </div>
                  </div>
                </div>
              ))}
            </div>
            <div className="eq-preamp">
              <div className="eq-preamp-label">Preamp</div>
              <div className="eq-slider-well preamp-well">
                <input
                  type="range"
                  className="eq-preamp-slider"
                  min="-24"
                  max="6"
                  step="0.1"
                  value={numberValue(eqConfig?.preamp_db, 0)}
                  onChange={(event) => updateEq('preamp_db', Number(event.target.value))}
                />
              </div>
              <span className="eq-preamp-value">
                {numberValue(eqConfig?.preamp_db, 0).toFixed(1)} dB
              </span>
            </div>
          </div>
        </div>
      </section>
    </section>
  );
}
