import { useEffect, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { safeArray } from '../../../shared/lib/appSupport';
import { browserZoneDisplayName } from '../../../shared/lib/browserZone';
import type { JsonRecord } from '../../../shared/types';
import { QobuzSourceIcon } from '../../../shared/ui/QobuzSourceIcon';
import {
  boolValue,
  compactFilterName,
  dsdModulatorOptions,
  fileFormatLabel,
  formatCpuPercent,
  formatSignalRate,
  isAirPlayProtocol,
  numberValue,
  stringValue
} from '../../settings/settingsModel';

function formatPercent(value: number) {
  if (!Number.isFinite(value)) return '0%';
  return `${Math.max(0, value * 100).toFixed(1)}%`;
}

function formatRealtimeSpeed(speed: number) {
  if (!Number.isFinite(speed) || speed <= 0) return '';
  return `${speed < 1 ? '<1' : Math.round(speed)}x Realtime`;
}

/**
 * Browser-zone playback processing chain. The chain lives server-side (the
 * stream route records what it served: source spec, baked-in EQ, encode), so
 * this renders from `status.browser_stream_signal` and deliberately skips the
 * engine's Filter/DSD/CPU stages — those describe the local player, not the
 * browser stream.
 */
function BrowserSignalPath({ status, signal }: { status: JsonRecord; signal: JsonRecord }) {
  const playbackActive = status.state === 'Playing' || status.state === 'Paused';
  const variant = stringValue(signal.variant);
  const sourceFormat = stringValue(signal.source_format, 'Source');
  const sourceRate = numberValue(signal.source_rate);
  const sourceDetail = !playbackActive
    ? 'No active stream'
    : sourceRate > 0
      ? formatSignalRate(signal.source_rate, signal.source_bits)
      : variant.startsWith('qobuz')
        ? 'Qobuz stream'
        : 'Signal';
  const eqActive = boolValue(signal.eq_active);
  const eqBands = numberValue(signal.eq_active_bands);
  const outputRateLabel =
    numberValue(signal.output_rate) > 0
      ? formatSignalRate(signal.output_rate, signal.output_bits)
      : '';
  const encode =
    variant === 'opus' || variant === 'qobuz_opus'
      ? {
          label: 'Opus',
          detail: `${numberValue(signal.opus_kbps) || ''} kbps · ${outputRateLabel || '48 kHz'}`
        }
      : variant === 'flac' || variant === 'qobuz_flac_eq'
        ? {
            label: 'FLAC',
            detail: `EQ applied · ${outputRateLabel || '24-bit'}`
          }
        : variant === 'qobuz_lossy'
          ? { label: 'MP3', detail: 'Data saver · Qobuz proxy' }
          : variant === 'qobuz_flac'
            ? { label: 'FLAC', detail: 'Original quality · Qobuz proxy' }
            : { label: 'Passthrough', detail: 'Original file · no DSP' };
  const isQobuz = variant.startsWith('qobuz');
  return (
    <div className="signal-popover" role="dialog" aria-modal="false" aria-label="Playback Chain">
      <div className="signal-device">
        {browserZoneDisplayName(status.active_zone_name || 'Browser')}
      </div>
      <div className="signal-path">
        <section className="signal-stage">
          <div className="stage-icon">SRC</div>
          <div>
            {isQobuz ? (
              <strong className="signal-source-label" aria-label={`${sourceFormat} Qobuz`}>
                {sourceFormat}
                <QobuzSourceIcon decorative />
              </strong>
            ) : (
              <strong>{sourceFormat}</strong>
            )}
            <span>{sourceDetail}</span>
          </div>
        </section>
        {eqActive ? (
          <section className="signal-stage">
            <div className="stage-icon">EQ</div>
            <div>
              <strong>Parametric EQ</strong>
              <span>{`10-band parametric · ${eqBands} active · server-side`}</span>
            </div>
          </section>
        ) : null}
        <section className="signal-stage">
          <div className="stage-icon">ENC</div>
          <div>
            <strong>{encode.label}</strong>
            <span>{encode.detail}</span>
          </div>
        </section>
        <section className="signal-stage">
          <div className="stage-icon">OUT</div>
          <div>
            <strong>Browser</strong>
            <span>Media element playback · no client DSP</span>
          </div>
        </section>
      </div>
    </div>
  );
}

export function SignalPopover({
  status,
  sourceProvider
}: {
  status: JsonRecord;
  sourceProvider?: string;
}) {
  const [eqConfig, setEqConfig] = useState<JsonRecord | null>(null);
  const activeZoneId = stringValue(status.active_zone_id);
  const playbackActive = status.state === 'Playing' || status.state === 'Paused';
  const hasActiveStream = playbackActive && Boolean(status.file_name || status.current_source);
  const sourceRate = formatSignalRate(status.source_rate, status.source_bits);
  const targetRate = formatSignalRate(status.target_rate, status.target_bits);
  const sourceRateNumber = numberValue(status.source_rate);
  const targetRateNumber = numberValue(status.target_rate);
  const hasKnownSignalRate = hasActiveStream && sourceRateNumber > 0;
  const configuredTargetRate = numberValue(status.configured_target_rate ?? status.target_rate);
  const upsamplingEnabled = isAirPlayProtocol(status.zone_protocol)
    ? false
    : boolValue(status.upsampling_enabled, false);
  const resamplingActive =
    upsamplingEnabled &&
    sourceRateNumber > 0 &&
    targetRateNumber > 0 &&
    sourceRateNumber !== targetRateNumber;
  const rateMode = resamplingActive
    ? configuredTargetRate === 0
      ? 'Auto'
      : `${(configuredTargetRate / 1000).toFixed(1)} kHz`
    : 'Native';
  const signalRatePath = resamplingActive ? `${sourceRate} -> ${targetRate}` : sourceRate;
  const outputMode = stringValue(status.active_output_mode ?? status.output_mode, 'Pcm');
  const isDsd = outputMode === 'Dsd64' || outputMode === 'Dsd128' || outputMode === 'Dsd256';
  const dsdLabel =
    outputMode === 'Dsd64'
      ? 'DSD64'
      : outputMode === 'Dsd128'
        ? 'DSD128'
        : outputMode === 'Dsd256'
          ? 'DSD256'
          : '';
  const headroomDb = numberValue(status.headroom_db);
  const headroomLabel = headroomDb < 0 ? ` / headroom ${headroomDb.toFixed(1)} dB` : '';
  const outputTransport = stringValue(status.output_transport);
  const dsdCarrierDetail =
    outputTransport === 'upnp_av_renderer' &&
    targetRateNumber > 0 &&
    numberValue(status.target_bits) > 1
      ? ` (DoP ${targetRate})`
      : targetRateNumber > 0
        ? ` (${(targetRateNumber / 1_000_000).toFixed(4)} MHz)`
        : '';
  const dsdSignalPath = `${sourceRate} -> ${dsdLabel}${dsdCarrierDetail}`;
  const filterName = compactFilterName(status.active_filter_type || status.filter_type);
  const dspDetail = hasActiveStream
    ? isDsd
      ? `${filterName} -> ${dsdLabel} cascade${headroomLabel}`
      : resamplingActive
        ? `${compactFilterName(status.filter_type)} · ${rateMode}`
        : `Filter off${headroomLabel}`
    : 'Waiting for playback';
  const eqOn = Boolean(eqConfig?.enabled);
  const activeEqBands = safeArray<JsonRecord>(eqConfig?.bands).filter(
    (band) => band.enabled
  ).length;
  const transportNames: Record<string, string> = {
    dop_wasapi: 'DoP via WASAPI',
    dop_coreaudio: 'DoP via CoreAudio',
    native_dsd_asio: 'native DSD via ASIO',
    upnp_av_renderer: 'DoP via UPnP'
  };
  const transport = transportNames[outputTransport] || 'DSD';
  const dsdModulatorName =
    dsdModulatorOptions.find(([value]) => value === status.dsd_modulator)?.[1] || '7th Order';
  const dsdLastLoad = numberValue(status.dsd_last_load);
  const dsdRecentLoadP95 = numberValue(status.dsd_recent_load_p95);
  const blockDurationNs = numberValue(status.block_duration_ns);
  const resampleTimeNs = numberValue(status.resample_time_ns);
  const dsdLimiterPeakRatio = Math.max(0, numberValue(status.dsd_limiter_peak_ratio));
  const dsdLimiterPeakRatioMax = Math.max(0, numberValue(status.dsd_limiter_peak_ratio_max));
  const renderLoad =
    dsdRecentLoadP95 > 0
      ? dsdRecentLoadP95
      : dsdLastLoad > 0
        ? dsdLastLoad
        : blockDurationNs > 0 && resampleTimeNs > 0
          ? resampleTimeNs / blockDurationNs
          : 0;
  const realtimeSpeed = renderLoad > 0 ? 1 / renderLoad : 0;
  const upnpRenderMs = numberValue(status.upnp_last_render_ms);
  const upnpDurationSecs = numberValue(status.duration_secs);
  const upnpRealtimeSpeed =
    outputTransport === 'upnp_av_renderer' && upnpRenderMs > 0 && upnpDurationSecs > 0
      ? (upnpDurationSecs * 1000) / upnpRenderMs
      : 0;
  const realtimeSpeedLabel =
    formatRealtimeSpeed(realtimeSpeed) ||
    formatRealtimeSpeed(upnpRealtimeSpeed) ||
    (outputTransport === 'upnp_av_renderer' && upnpRenderMs === 0
      ? 'Realtime stream'
      : 'Realtime n/a');
  const dspBufferMs = numberValue(status.dsp_buffer_ms);
  const dspBufferLabel = dspBufferMs > 0 ? `${dspBufferMs}ms` : 'Auto';
  const cpuPercentLabel = formatCpuPercent(status.cpu_percent);
  const normalizedSourceProvider = stringValue(sourceProvider).toLowerCase();
  const isQobuzSource =
    normalizedSourceProvider === 'qobuz' ||
    String((status.current_source as JsonRecord | null)?.kind || '')
      .toLowerCase()
      .includes('qobuz');
  const sourceFormat = isQobuzSource ? 'FLAC' : fileFormatLabel(status.file_name);
  const sourceDetail = hasActiveStream
    ? hasKnownSignalRate
      ? isDsd
        ? dsdSignalPath
        : signalRatePath
      : 'Signal'
    : 'No active stream';
  const dsdLimiterDetail = isDsd
    ? `Input ${formatPercent(dsdLimiterPeakRatio)} · Max ${formatPercent(dsdLimiterPeakRatioMax)}`
    : null;
  const sourceLabel = isQobuzSource ? (
    <strong className="signal-source-label" aria-label={`${sourceFormat} Qobuz`}>
      {sourceFormat}
      <QobuzSourceIcon decorative />
    </strong>
  ) : (
    <strong>{hasActiveStream ? sourceFormat : 'Source'}</strong>
  );

  useEffect(() => {
    let cancelled = false;
    const configRequest = activeZoneId ? endpoints.zoneEq(activeZoneId) : endpoints.eq();
    configRequest
      .then((config) => {
        if (!cancelled) setEqConfig(config);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [activeZoneId]);

  const browserSignal =
    status.browser_stream_signal && typeof status.browser_stream_signal === 'object'
      ? (status.browser_stream_signal as JsonRecord)
      : null;
  if (browserSignal) {
    return <BrowserSignalPath status={status} signal={browserSignal} />;
  }

  return (
    <div className="signal-popover" role="dialog" aria-modal="false" aria-label="Playback Chain">
      <div className="signal-device">{String(status.active_zone_name || 'Output')}</div>
      <div className="signal-path">
        <section className="signal-stage">
          <div className="stage-icon">SRC</div>
          <div>
            {sourceLabel}
            <span>{sourceDetail}</span>
          </div>
        </section>
        {eqOn && isDsd ? (
          <section className="signal-stage">
            <div className="stage-icon">EQ</div>
            <div>
              <strong>Parametric EQ</strong>
              <span>{`10-band parametric · ${activeEqBands} active`}</span>
            </div>
          </section>
        ) : null}
        <section className="signal-stage">
          <div className="stage-icon">DSP</div>
          <div>
            <strong>Filter</strong>
            <span>{dspDetail}</span>
          </div>
        </section>
        {eqOn && !isDsd ? (
          <section className="signal-stage">
            <div className="stage-icon">EQ</div>
            <div>
              <strong>Parametric EQ</strong>
              <span>{`10-band parametric · ${activeEqBands} active`}</span>
            </div>
          </section>
        ) : null}
        {isDsd ? (
          <section className="signal-stage">
            <div className="stage-icon">DSD</div>
            <div>
              <strong>DSD Modulator</strong>
              <span>{`${dsdModulatorName} · ${transport}`}</span>
              {dsdLimiterDetail ? <span>{dsdLimiterDetail}</span> : null}
            </div>
          </section>
        ) : null}
        <section className="signal-stage">
          <div className="stage-icon">CPU</div>
          <div>
            <strong>CPU</strong>
            <span className="signal-cpu-detail">
              {[
                <span className="signal-cpu-percent" key="cpu-percent">
                  {cpuPercentLabel}
                </span>,
                <span key="cpu-buffer">{`· Buffer ${dspBufferLabel} ·`}</span>,
                <span className="signal-realtime-speed" key="cpu-realtime">
                  {realtimeSpeedLabel}
                </span>
              ]}
            </span>
          </div>
        </section>
      </div>
    </div>
  );
}
