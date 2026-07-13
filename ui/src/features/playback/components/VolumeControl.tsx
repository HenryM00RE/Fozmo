import { useEffect, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { clampPercent, sliderFillStyle } from '../../../shared/lib/appSupport';
import type { JsonRecord } from '../../../shared/types';

const VOLUME_OPTIMISTIC_MS = 3000;
const VOLUME_COMMIT_DEBOUNCE_MS = 700;
const VOLUME_READBACK_MIN_INTERVAL_MS = 700;
const VOLUME_SPEAKER_BODY_PATH =
  'M3.45 8.85h2.36c.44 0 .87-.16 1.2-.45l3.54-3.12c.78-.69 2-.14 2 .9v11.64c0 1.04-1.22 1.59-2 .9L7.01 15.6a1.82 1.82 0 0 0-1.2-.45H3.45A1.45 1.45 0 0 1 2 13.7v-3.4a1.45 1.45 0 0 1 1.45-1.45Z';

type PendingVolume = {
  percent: number;
  requestedAt: number;
};

function formatVolumePercent(value: number) {
  return String(Math.round(value));
}

function VolumeIcon({ percent, className }: { percent: number; className?: string }) {
  if (percent <= 0) {
    return (
      <svg
        viewBox="0 0 24 24"
        className={className}
        aria-hidden="true"
        shapeRendering="geometricPrecision"
      >
        <path d={VOLUME_SPEAKER_BODY_PATH} fill="currentColor" stroke="none" />
        <path
          d="m16 9 5 5M21 9l-5 5"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.7"
          strokeLinecap="round"
        />
      </svg>
    );
  }

  return (
    <svg
      viewBox="0 0 24 24"
      className={className}
      aria-hidden="true"
      shapeRendering="geometricPrecision"
    >
      <path d={VOLUME_SPEAKER_BODY_PATH} fill="currentColor" stroke="none" />
      <path
        d="M15.54 8.46a5 5 0 0 1 0 7.07"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
      />
      {percent >= 60 ? (
        <path
          d="M19.07 4.93a10 10 0 0 1 0 14.14"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.7"
          strokeLinecap="round"
        />
      ) : null}
    </svg>
  );
}

export function VolumeControl({
  activeZoneId,
  status
}: {
  activeZoneId: string;
  status: JsonRecord;
}) {
  const [open, setOpen] = useState(false);
  const [sourceScrubbing, setSourceScrubbing] = useState(false);
  const [deviceScrubbing, setDeviceScrubbing] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const pendingSourceVolumeRef = useRef<PendingVolume | null>(null);
  const pendingDeviceVolumeRef = useRef<PendingVolume | null>(null);
  const sourceCommitTimerRef = useRef<number | null>(null);
  const deviceCommitTimerRef = useRef<number | null>(null);
  const queuedSourceVolumeRef = useRef<number | null>(null);
  const queuedDeviceVolumeRef = useRef<number | null>(null);
  const sourceReadbackTimerRef = useRef<number | null>(null);
  const deviceReadbackTimerRef = useRef<number | null>(null);
  const lastSourceReadbackAtRef = useRef(0);
  const lastDeviceReadbackAtRef = useRef(0);

  const sourcePercentFromStatus = clampPercent(Number(status.volume ?? 1) * 100);
  const deviceSupported = status.device_volume_supported === true;
  const deviceMessage = String(status.device_volume_message || '');
  const deviceMaxPercent = clampPercent(Number(status.device_volume_max ?? 1) * 100, 100) || 100;
  const rawDeviceVolumeValue = status.device_volume;
  const rawDeviceVolume = Number(rawDeviceVolumeValue);
  const hasDeviceVolume =
    deviceSupported &&
    rawDeviceVolumeValue !== null &&
    rawDeviceVolumeValue !== undefined &&
    rawDeviceVolumeValue !== '' &&
    Number.isFinite(rawDeviceVolume);
  const devicePercentFromStatus = hasDeviceVolume
    ? clampPercent(rawDeviceVolume * 100, deviceMaxPercent)
    : 0;

  const [sourcePercent, setSourcePercent] = useState(sourcePercentFromStatus);
  const [devicePercent, setDevicePercent] = useState(devicePercentFromStatus);
  const networkRendererVolume = ['sonos_upnp', 'upnp_av_renderer'].includes(
    String(status.output_transport || '')
  );
  const singleDeviceVolume = networkRendererVolume && deviceSupported;

  const clearSourceReadbackTimer = () => {
    if (sourceReadbackTimerRef.current !== null) {
      window.clearTimeout(sourceReadbackTimerRef.current);
      sourceReadbackTimerRef.current = null;
    }
  };
  const clearDeviceReadbackTimer = () => {
    if (deviceReadbackTimerRef.current !== null) {
      window.clearTimeout(deviceReadbackTimerRef.current);
      deviceReadbackTimerRef.current = null;
    }
  };
  const applySourceReadback = (percent: number) => {
    clearSourceReadbackTimer();
    lastSourceReadbackAtRef.current = Date.now();
    setSourcePercent(percent);
  };
  const applyDeviceReadback = (percent: number) => {
    clearDeviceReadbackTimer();
    lastDeviceReadbackAtRef.current = Date.now();
    setDevicePercent(percent);
  };

  useEffect(() => {
    const pending = pendingSourceVolumeRef.current;
    if (pending) {
      if (Math.abs(sourcePercentFromStatus - pending.percent) <= 1) {
        pendingSourceVolumeRef.current = null;
      } else if (Date.now() - pending.requestedAt < VOLUME_OPTIMISTIC_MS) {
        return;
      } else {
        pendingSourceVolumeRef.current = null;
      }
    }
    if (sourceScrubbing) {
      clearSourceReadbackTimer();
      return;
    }
    const elapsed = Date.now() - lastSourceReadbackAtRef.current;
    if (elapsed >= VOLUME_READBACK_MIN_INTERVAL_MS) {
      applySourceReadback(sourcePercentFromStatus);
    } else {
      clearSourceReadbackTimer();
      sourceReadbackTimerRef.current = window.setTimeout(() => {
        applySourceReadback(sourcePercentFromStatus);
      }, VOLUME_READBACK_MIN_INTERVAL_MS - elapsed);
    }
  }, [sourcePercentFromStatus, sourceScrubbing]);

  useEffect(() => {
    const pending = pendingDeviceVolumeRef.current;
    if (pending) {
      if (Math.abs(devicePercentFromStatus - pending.percent) <= 1) {
        pendingDeviceVolumeRef.current = null;
      } else if (Date.now() - pending.requestedAt < VOLUME_OPTIMISTIC_MS) {
        return;
      } else {
        pendingDeviceVolumeRef.current = null;
      }
    }
    if (deviceScrubbing) {
      clearDeviceReadbackTimer();
      return;
    }
    const elapsed = Date.now() - lastDeviceReadbackAtRef.current;
    if (elapsed >= VOLUME_READBACK_MIN_INTERVAL_MS) {
      applyDeviceReadback(devicePercentFromStatus);
    } else {
      clearDeviceReadbackTimer();
      deviceReadbackTimerRef.current = window.setTimeout(() => {
        applyDeviceReadback(devicePercentFromStatus);
      }, VOLUME_READBACK_MIN_INTERVAL_MS - elapsed);
    }
  }, [devicePercentFromStatus, deviceScrubbing]);

  useEffect(() => {
    if (!open) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  useEffect(() => {
    return () => {
      if (sourceCommitTimerRef.current !== null) {
        window.clearTimeout(sourceCommitTimerRef.current);
      }
      if (deviceCommitTimerRef.current !== null) {
        window.clearTimeout(deviceCommitTimerRef.current);
      }
      clearSourceReadbackTimer();
      clearDeviceReadbackTimer();
    };
  }, []);

  const displayPercent = hasDeviceVolume ? devicePercent : sourcePercent;
  const displayPercentLabel = formatVolumePercent(displayPercent);
  const title =
    singleDeviceVolume || hasDeviceVolume
      ? `Device volume ${displayPercentLabel}%`
      : `Source volume ${displayPercentLabel}%`;
  const deviceLabel = /hegel/i.test(deviceMessage) ? 'Hegel' : 'Device';

  const sendSourceVolume = (percent: number) => {
    endpoints.volumeZone(activeZoneId, percent / 100).catch(() => undefined);
  };
  const sendDeviceVolume = (percent: number) => {
    endpoints.deviceVolumeZone(activeZoneId, percent / 100).catch(() => undefined);
  };
  const queueSourceVolumeCommit = (percent: number) => {
    queuedSourceVolumeRef.current = percent;
    if (sourceCommitTimerRef.current !== null) {
      window.clearTimeout(sourceCommitTimerRef.current);
    }
    sourceCommitTimerRef.current = window.setTimeout(() => {
      sourceCommitTimerRef.current = null;
      const queued = queuedSourceVolumeRef.current;
      queuedSourceVolumeRef.current = null;
      if (queued !== null) sendSourceVolume(queued);
    }, VOLUME_COMMIT_DEBOUNCE_MS);
  };
  const queueDeviceVolumeCommit = (percent: number) => {
    queuedDeviceVolumeRef.current = percent;
    if (deviceCommitTimerRef.current !== null) {
      window.clearTimeout(deviceCommitTimerRef.current);
    }
    deviceCommitTimerRef.current = window.setTimeout(() => {
      deviceCommitTimerRef.current = null;
      const queued = queuedDeviceVolumeRef.current;
      queuedDeviceVolumeRef.current = null;
      if (queued !== null) sendDeviceVolume(queued);
    }, VOLUME_COMMIT_DEBOUNCE_MS);
  };
  const flushSourceVolumeCommit = () => {
    if (sourceCommitTimerRef.current !== null) {
      window.clearTimeout(sourceCommitTimerRef.current);
      sourceCommitTimerRef.current = null;
    }
    const queued = queuedSourceVolumeRef.current;
    queuedSourceVolumeRef.current = null;
    if (queued !== null) sendSourceVolume(queued);
  };
  const flushDeviceVolumeCommit = () => {
    if (deviceCommitTimerRef.current !== null) {
      window.clearTimeout(deviceCommitTimerRef.current);
      deviceCommitTimerRef.current = null;
    }
    const queued = queuedDeviceVolumeRef.current;
    queuedDeviceVolumeRef.current = null;
    if (queued !== null) sendDeviceVolume(queued);
  };

  const commitSourceVolume = (percent: number) => {
    const next = clampPercent(percent);
    setSourcePercent(next);
    pendingSourceVolumeRef.current = { percent: next, requestedAt: Date.now() };
    if (networkRendererVolume) {
      setDevicePercent(next);
      pendingDeviceVolumeRef.current = { percent: next, requestedAt: Date.now() };
    }
    queueSourceVolumeCommit(next);
  };
  const commitDeviceVolume = (percent: number) => {
    if (!deviceSupported) return;
    const next = clampPercent(percent, deviceMaxPercent);
    setDevicePercent(next);
    pendingDeviceVolumeRef.current = { percent: next, requestedAt: Date.now() };
    if (networkRendererVolume) {
      setSourcePercent(next);
      pendingSourceVolumeRef.current = { percent: next, requestedAt: Date.now() };
    }
    queueDeviceVolumeCommit(next);
  };
  return (
    <div className="volume-popover-control" ref={rootRef}>
      <button
        className="footer-output-button icon-only"
        type="button"
        title={title}
        aria-label={title}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <VolumeIcon percent={displayPercent} />
        <span className="volume-trigger-value">{displayPercentLabel}</span>
      </button>
      {open ? (
        <div className="volume-popover" role="dialog" aria-label="Volume Control">
          <div className="volume-popover-content">
            {singleDeviceVolume ? null : (
              <div className="volume-popover-row">
                <span className="volume-control-label">Source</span>
                <div className="volume-slider-wrapper">
                  <input
                    type="range"
                    min="0"
                    max="100"
                    value={sourcePercent}
                    className={`custom-slider volume-slider${sourceScrubbing ? ' is-scrubbing' : ''}`}
                    style={sliderFillStyle(sourcePercent)}
                    aria-label="Source volume"
                    onChange={(event) => commitSourceVolume(Number(event.currentTarget.value))}
                    onPointerDown={() => setSourceScrubbing(true)}
                    onPointerUp={() => {
                      setSourceScrubbing(false);
                      flushSourceVolumeCommit();
                    }}
                    onPointerCancel={() => {
                      setSourceScrubbing(false);
                      flushSourceVolumeCommit();
                    }}
                    onTouchEnd={() => {
                      setSourceScrubbing(false);
                      flushSourceVolumeCommit();
                    }}
                  />
                </div>
                <strong className="volume-value-text">{formatVolumePercent(sourcePercent)}</strong>
              </div>
            )}
            <div
              className={`volume-popover-row${singleDeviceVolume ? ' is-single-control' : ''}${
                deviceSupported ? '' : ' is-disabled'
              }`}
              title={
                deviceSupported
                  ? `${deviceLabel} volume`
                  : deviceMessage || 'Device volume unavailable'
              }
            >
              {singleDeviceVolume ? null : (
                <span className="volume-control-label">{deviceLabel}</span>
              )}
              <div className="volume-slider-wrapper">
                <input
                  type="range"
                  min="0"
                  max={deviceMaxPercent}
                  value={devicePercent}
                  className={`custom-slider volume-slider${deviceScrubbing ? ' is-scrubbing' : ''}`}
                  style={sliderFillStyle(
                    deviceMaxPercent > 0 ? (devicePercent / deviceMaxPercent) * 100 : 0
                  )}
                  aria-label={`${deviceLabel} volume`}
                  disabled={!deviceSupported}
                  onChange={(event) => commitDeviceVolume(Number(event.currentTarget.value))}
                  onPointerDown={() => setDeviceScrubbing(true)}
                  onPointerUp={() => {
                    setDeviceScrubbing(false);
                    flushDeviceVolumeCommit();
                  }}
                  onPointerCancel={() => {
                    setDeviceScrubbing(false);
                    flushDeviceVolumeCommit();
                  }}
                  onTouchEnd={() => {
                    setDeviceScrubbing(false);
                    flushDeviceVolumeCommit();
                  }}
                />
              </div>
              <strong className="volume-value-text">
                {hasDeviceVolume ? formatVolumePercent(devicePercent) : '--'}
              </strong>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
