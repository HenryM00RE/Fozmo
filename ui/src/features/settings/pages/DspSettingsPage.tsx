import { capabilityEnabled } from '../../../shared/lib/capabilities';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { SettingsZoneSelect } from '../components/SettingsZoneSelect';
import { zoneIsPlaying } from '../dspTargetZone';
import type { DspApplyState } from '../hooks/useDspSettings';
import {
  configFromStatus,
  defaultDsdOutputModeForZone,
  dsdModulatorOptions,
  dsdRateFromOutputMode,
  dsdRateOptions,
  dspBufferOptions,
  ecBeam2FilterSupported,
  filterOptions,
  headroomAfterDsdModulatorChange,
  headroomLockedForDsdModulator,
  isDsdOutputMode,
  isiPenaltyAfterDsdModulatorChange,
  outputModeForDsdRate,
  pcmBitDepthOptions,
  sampleRateOptions,
  zoneSupportsDopDsd,
  zoneSupportsDsdOutputMode,
  zoneSupportsDsp,
  zoneSupportsNativeDsd
} from '../settingsModel';

type PlaybackConfig = ReturnType<typeof configFromStatus>;

export function DspSettingsPage({
  applyState,
  ecBeam2Selectable,
  selectedDeviceName,
  selectedZoneId,
  settingsZones,
  status,
  onSelectedZoneChange,
  playbackConfig,
  playbackConfigError,
  updatePlaybackConfig
}: {
  applyState: DspApplyState;
  ecBeam2Selectable: boolean;
  selectedDeviceName: string;
  selectedZoneId: string;
  settingsZones: ZoneProfile[];
  status: JsonRecord;
  onSelectedZoneChange: (zoneId: string) => void;
  playbackConfig: PlaybackConfig;
  playbackConfigError: string;
  updatePlaybackConfig: <K extends keyof PlaybackConfig>(key: K, value: PlaybackConfig[K]) => void;
}) {
  const selectedZone = settingsZones.find((zone) => zone.id === selectedZoneId);
  const dspAvailable = zoneSupportsDsp(selectedZone);
  const applyStatusLine = dspApplyStatusLine(applyState);
  const playingElsewhere = settingsZones.find(
    (zone) => zone.id !== selectedZoneId && zoneIsPlaying(zone)
  );
  const selectedZonePlaying = selectedZone ? zoneIsPlaying(selectedZone) : false;
  const experimentalDsd256 = capabilityEnabled(status, 'experimental_dsd256');
  const nativeDsdAvailable = zoneSupportsNativeDsd(selectedZone);
  const dopDsdAvailable = zoneSupportsDopDsd(selectedZone);
  const selectedOutputModeAllowed = zoneSupportsDsdOutputMode(
    selectedZone,
    playbackConfig.outputMode,
    experimentalDsd256
  );
  const effectiveOutputMode = selectedOutputModeAllowed ? playbackConfig.outputMode : 'Pcm';
  const dsdOutputMode = isDsdOutputMode(effectiveOutputMode);
  const dsdRate = dsdRateFromOutputMode(effectiveOutputMode);
  const preferredDsdRate = zoneSupportsDsdOutputMode(selectedZone, dsdRate, experimentalDsd256)
    ? dsdRate
    : defaultDsdOutputModeForZone(selectedZone, experimentalDsd256);
  const outputFormat =
    effectiveOutputMode === 'Pcm' ? 'Pcm' : nativeDsdAvailable ? 'NativeDsd' : 'DopDsd';
  const targetRateDetail = dsdOutputMode ? 'Set the DSD output rate.' : 'Set the PCM output rate.';
  const targetRateValue = dsdOutputMode ? dsdRate : String(playbackConfig.targetRate);
  const targetRateOptions = dsdOutputMode
    ? dsdRateOptions.map((option) => ({
        value: option.value,
        label: option.label,
        disabled:
          !zoneSupportsDsdOutputMode(selectedZone, option.value, experimentalDsd256) ||
          (playbackConfig.dsdModulator === 'EcBeam2' && option.value === 'Dsd256')
      }))
    : sampleRateOptions.map(([value, label]) => ({ value: String(value), label }));

  return (
    <section className="settings-panel">
      <section className="settings-section-block">
        <div className="settings-section-heading dsp-settings-heading">
          <div className="section-label">Playback processing</div>
          <SettingsZoneSelect
            ariaLabel={`DSP output: ${selectedDeviceName}`}
            selectedZoneId={selectedZoneId}
            selectedZoneLabel={selectedDeviceName}
            zones={settingsZones}
            onChange={onSelectedZoneChange}
          />
        </div>
        <div className="panel raised dsp-card">
          {!dspAvailable ? (
            <div className="dsp-card-message" role="status">
              DSP is not available for this device. Choose another output above to adjust its DSP
              settings.
            </div>
          ) : playbackConfigError ? (
            <div className="dsp-settings-error" role="alert">
              {playbackConfigError}
            </div>
          ) : null}
          {dspAvailable && !selectedZonePlaying && playingElsewhere ? (
            <div className="dsp-card-message">
              Music is playing on {playingElsewhere.name}; these settings target{' '}
              {selectedDeviceName}.{' '}
              <button
                type="button"
                className="dsp-settings-hint-link"
                onClick={() => onSelectedZoneChange(playingElsewhere.id)}
              >
                Edit {playingElsewhere.name} instead
              </button>
            </div>
          ) : null}
          {dspAvailable && applyStatusLine ? (
            <div
              className={
                applyStatusLine.kind === 'error' ? 'dsp-settings-error' : 'dsp-settings-hint'
              }
              role={applyStatusLine.kind === 'error' ? 'alert' : 'status'}
            >
              {applyStatusLine.text}
            </div>
          ) : null}
          {dspAvailable ? (
            <div className="settings-list compact-list">
              <ToggleRow
                title="Upsampling / DSP enabled"
                detail={
                  playbackConfig.upsamplingEnabled
                    ? 'DSP is processing playback with the configured format and rate.'
                    : 'DSP is bypassed; saved processing settings are kept.'
                }
                checked={playbackConfig.upsamplingEnabled}
                onChange={(checked) => updatePlaybackConfig('upsamplingEnabled', checked)}
              />
              <div className="setting-row control-row">
                <span>
                  <strong>Output format</strong>
                  <small>Choose final playback format.</small>
                </span>
                <SelectMenu
                  ariaLabel="Output format"
                  value={outputFormat}
                  disabled={!playbackConfig.upsamplingEnabled}
                  onChange={(value) =>
                    updatePlaybackConfig(
                      'outputMode',
                      value === 'Pcm'
                        ? 'Pcm'
                        : playbackConfig.dsdModulator === 'EcBeam2' && preferredDsdRate === 'Dsd256'
                          ? zoneSupportsDsdOutputMode(selectedZone, 'Dsd128', experimentalDsd256)
                            ? 'Dsd128'
                            : 'Dsd64'
                          : preferredDsdRate
                    )
                  }
                  options={[
                    { value: 'Pcm', label: 'PCM' },
                    {
                      value: 'DopDsd',
                      label: 'DoP (DSD)',
                      disabled: nativeDsdAvailable || !dopDsdAvailable
                    },
                    { value: 'NativeDsd', label: 'Native DSD', disabled: !nativeDsdAvailable }
                  ]}
                />
              </div>
              <ToggleRow
                title="Exclusive mode"
                detail="Bypass the OS mixer where supported."
                checked={playbackConfig.exclusive}
                onChange={(checked) => updatePlaybackConfig('exclusive', checked)}
              />
              <div className="setting-row control-row">
                <span>
                  <strong>Target rate</strong>
                  <small>{targetRateDetail}</small>
                </span>
                <SelectMenu
                  ariaLabel="Target rate"
                  value={targetRateValue}
                  disabled={!playbackConfig.upsamplingEnabled}
                  onChange={(value) => {
                    if (dsdOutputMode)
                      updatePlaybackConfig('outputMode', outputModeForDsdRate(value));
                    else updatePlaybackConfig('targetRate', Number(value));
                  }}
                  options={targetRateOptions}
                />
              </div>
              <div className="setting-row control-row">
                <span>
                  <strong>Target BitDepth</strong>
                  <small>Set rendered PCM bit depth.</small>
                </span>
                <SelectMenu
                  ariaLabel="Target bit depth"
                  value={String(playbackConfig.targetBitDepth)}
                  disabled={!playbackConfig.upsamplingEnabled || dsdOutputMode}
                  onChange={(value) => updatePlaybackConfig('targetBitDepth', Number(value))}
                  options={pcmBitDepthOptions.map((value) => ({
                    value: String(value),
                    label: `${value} bit`,
                    after: value === 24 ? <FavoriteStarIcon /> : undefined
                  }))}
                />
              </div>
              <div className="setting-row control-row">
                <span>
                  <strong>Filter</strong>
                  <small>Choose the upsampling algorithm.</small>
                </span>
                <SelectMenu
                  ariaLabel="Filter"
                  value={playbackConfig.filterType}
                  disabled={!playbackConfig.upsamplingEnabled}
                  onChange={(value) => updatePlaybackConfig('filterType', value)}
                  options={filterOptions.map(([value, label]) => ({
                    value,
                    label,
                    disabled:
                      playbackConfig.dsdModulator === 'EcBeam2' && !ecBeam2FilterSupported(value)
                  }))}
                />
              </div>
              <div className="setting-row control-row">
                <span>
                  <strong>Headroom</strong>
                  <small>Attenuate before output and DSD modulation.</small>
                </span>
                <SelectMenu
                  ariaLabel="Headroom"
                  value={String(playbackConfig.headroomDb)}
                  disabled={headroomLockedForDsdModulator(playbackConfig.dsdModulator)}
                  onChange={(value) => updatePlaybackConfig('headroomDb', Number(value))}
                  options={[0, -1, -2, -3, -4, -5, -6, -9, -12].map((value) => ({
                    value: String(value),
                    label: value === 0 ? 'Off (0.0 dB)' : `${value.toFixed(1)} dB`,
                    after: value === -4 ? <FavoriteStarIcon /> : undefined
                  }))}
                />
              </div>
              <div className="setting-row control-row">
                <span>
                  <strong>DSD modulator</strong>
                  <small>Choose the noise-shaping algorithm used for DSD output.</small>
                </span>
                <SelectMenu
                  ariaLabel="DSD modulator"
                  value={playbackConfig.dsdModulator}
                  disabled={
                    !playbackConfig.upsamplingEnabled || playbackConfig.outputMode === 'Pcm'
                  }
                  onChange={(value) => {
                    updatePlaybackConfig('dsdModulator', value);
                    const headroomDb = headroomAfterDsdModulatorChange(
                      playbackConfig.headroomDb,
                      value
                    );
                    if (headroomDb !== playbackConfig.headroomDb) {
                      updatePlaybackConfig('headroomDb', headroomDb);
                    }
                    const isiPenalty = isiPenaltyAfterDsdModulatorChange(
                      playbackConfig.dsdIsiPenalty,
                      value
                    );
                    if (isiPenalty !== playbackConfig.dsdIsiPenalty) {
                      updatePlaybackConfig('dsdIsiPenalty', isiPenalty);
                    }
                  }}
                  options={dsdModulatorOptions.map(([value, label]) => ({
                    value,
                    label,
                    disabled: value === 'EcBeam2' && !ecBeam2Selectable,
                    after: value === 'EcDepth2' ? <FavoriteStarIcon /> : undefined
                  }))}
                />
              </div>
              <div className="setting-row control-row">
                <span>
                  <strong>DSP buffer</strong>
                  <small>Prerender cushion for playback stability.</small>
                </span>
                <SelectMenu
                  ariaLabel="DSP buffer"
                  value={String(playbackConfig.dspBufferMs)}
                  onChange={(value) => updatePlaybackConfig('dspBufferMs', Number(value))}
                  options={dspBufferOptions.map((option) => ({
                    value: String(option.value),
                    label: option.label
                  }))}
                />
              </div>
              <label className="setting-row control-row">
                <span>
                  <strong>DSD ISI penalty</strong>
                  <small>Leave at 0 for most DACs.</small>
                </span>
                <input
                  type="number"
                  min="0"
                  max="0.05"
                  step="0.001"
                  value={playbackConfig.dsdIsiPenalty}
                  disabled={
                    !playbackConfig.upsamplingEnabled ||
                    playbackConfig.outputMode === 'Pcm' ||
                    playbackConfig.dsdModulator === 'EcBeam2'
                  }
                  onChange={(event) =>
                    updatePlaybackConfig('dsdIsiPenalty', Number(event.target.value))
                  }
                />
              </label>
            </div>
          ) : null}
        </div>
      </section>
    </section>
  );
}

export function dspApplyStatusLine(
  applyState: DspApplyState
): { kind: 'info' | 'error'; text: string } | null {
  const playing = applyState.playState === 'Playing' || applyState.playState === 'Transitioning';
  if (applyState.zoneProtocol === 'upnp_av_renderer') {
    if (applyState.renderStatus === 'failed') {
      return {
        kind: 'error',
        text: applyState.notice
          ? `Settings saved, but applying them to the current stream failed: ${applyState.notice}`
          : 'Settings saved, but applying them to the current stream failed.'
      };
    }
    if (applyState.renderStatus === 'pending' || applyState.renderStatus === 'rendering') {
      return {
        kind: 'info',
        text: 'Applying settings to the current stream — re-rendering the track. Streaming sources can take a minute; playback keeps the previous settings until the switch.'
      };
    }
    if (applyState.renderStatus === 'switching') {
      return { kind: 'info', text: 'Applying settings — restarting the stream…' };
    }
    return null;
  }
  if (applyState.zoneProtocol === 'sonos_upnp' && playing) {
    return {
      kind: 'info',
      text: 'Sonos zones pick up DSP changes from the next track; the current stream keeps playing unchanged.'
    };
  }
  return null;
}

function FavoriteStarIcon() {
  return (
    <svg className="dsp-filter-favorite-star" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 2.72c.35 0 .66.2.82.52l2.47 5.01 5.53.8c.35.05.64.29.75.63.11.34.02.71-.24.96l-4 3.9.94 5.5c.06.35-.08.71-.37.92-.29.21-.67.24-.99.07L12 18.44l-4.93 2.59c-.32.17-.7.14-.99-.07-.29-.21-.43-.57-.37-.92l.94-5.5-4-3.9c-.26-.25-.35-.62-.24-.96.11-.34.4-.58.75-.63l5.53-.8 2.47-5.01c.16-.32.47-.52.82-.52Z" />
    </svg>
  );
}

function ToggleRow({
  title,
  detail,
  checked,
  onChange
}: {
  title: string;
  detail: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="setting-row dsp-toggle-row">
      <span>
        <strong>{title}</strong>
        <small>{detail}</small>
      </span>
      <button
        className={`toggle${checked ? ' on' : ''}`}
        type="button"
        aria-label={title}
        aria-pressed={checked}
        onClick={() => onChange(!checked)}
      />
    </div>
  );
}
