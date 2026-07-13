import { useEffect, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import { PlayingEqualizer } from '../../../shared/ui/PlayingEqualizer';
import { ZoneOutputIcon } from '../../../shared/ui/ZoneOutputIcon';
import {
  groupedSettingsZones,
  stringValue,
  zoneDisplayName,
  zoneFormatLabel
} from '../../settings/settingsModel';

function isPlayingState(value: unknown) {
  return String(value || '').toLowerCase() === 'playing';
}

export function ZonePicker({
  zones,
  activeZoneId,
  activeZoneName,
  status,
  onSelect
}: {
  zones: ZoneProfile[];
  activeZoneId: string;
  activeZoneName: string;
  status: JsonRecord;
  onSelect: (zoneId: string) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [hegelZoneId, setHegelZoneId] = useState('');
  const [liveZoneStates, setLiveZoneStates] = useState<Record<string, string>>({});
  const [pendingZoneId, setPendingZoneId] = useState<string | null>(null);
  const [pausingZoneId, setPausingZoneId] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const enabledZones = zones.filter((zone) => zone.enabled !== false);
  const enabledZoneIdsKey = enabledZones.map((zone) => zone.id).join('|');
  const zoneGroups = groupedSettingsZones(enabledZones);
  const displayedActiveZoneId = pendingZoneId || activeZoneId;
  const activeZone = enabledZones.find((zone) => zone.id === displayedActiveZoneId);
  const label = activeZone ? zoneDisplayName(activeZone) : activeZoneName || 'Core';

  useEffect(() => {
    if (status.surface === 'remote') {
      setHegelZoneId('');
      return undefined;
    }
    let cancelled = false;
    endpoints
      .hegelSettings()
      .then((settings) => {
        if (cancelled) return;
        setHegelZoneId(settings.enabled === true ? stringValue(settings.zone_id) : '');
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [open, status.surface]);

  useEffect(() => {
    setLiveZoneStates((current) => ({
      ...current,
      [activeZoneId]: String(status.state || '')
    }));
  }, [activeZoneId, status.state]);

  useEffect(() => {
    if (!open || enabledZones.length === 0) return undefined;
    let cancelled = false;

    const refreshZoneStates = async () => {
      const results = await Promise.allSettled(
        enabledZones.map(async (zone) => {
          const next = await endpoints.zoneStatus(zone.id);
          return [zone.id, String(next.state || '')] as const;
        })
      );
      if (cancelled) return;
      setLiveZoneStates((current) => {
        const next = { ...current };
        results.forEach((result) => {
          if (result.status === 'fulfilled') {
            const [zoneId, stateName] = result.value;
            next[zoneId] = stateName;
          }
        });
        return next;
      });
    };

    refreshZoneStates().catch(() => undefined);
    const timer = window.setInterval(() => {
      refreshZoneStates().catch(() => undefined);
    }, 1500);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [enabledZoneIdsKey, open]);

  useEffect(() => {
    if (pendingZoneId && activeZoneId === pendingZoneId) {
      setPendingZoneId(null);
      setOpen(false);
    }
  }, [activeZoneId, pendingZoneId]);

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

  const selectZone = async (zoneId: string, active: boolean) => {
    if (active || pendingZoneId) return;
    setPendingZoneId(zoneId);
    try {
      await onSelect(zoneId);
      setOpen(false);
      setPendingZoneId(null);
    } catch {
      setPendingZoneId(null);
    }
  };

  const pauseZone = async (zoneId: string) => {
    if (pausingZoneId) return;
    setPausingZoneId(zoneId);
    try {
      await endpoints.pauseZone(zoneId);
      setLiveZoneStates((current) => ({
        ...current,
        [zoneId]: 'Paused'
      }));
    } finally {
      setPausingZoneId(null);
    }
  };

  return (
    <div className="zone-control" ref={rootRef}>
      <button
        className="footer-output-button"
        type="button"
        title="Playback output"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-busy={Boolean(pendingZoneId)}
        onClick={() => setOpen((current) => !current)}
      >
        <ZoneOutputIcon zone={activeZone} hegelZoneId={hegelZoneId} detail="panel" />
        <span>{label}</span>
      </button>
      {open ? (
        <div className="zone-menu" role="listbox" aria-label="Playback output">
          {zoneGroups.length ? (
            zoneGroups.map((group) => (
              <div
                className="zone-menu-section"
                role="group"
                aria-label={group.label}
                key={group.key}
              >
                <div className="zone-menu-heading">{group.label}</div>
                {group.zones.map((zone) => {
                  const active = zone.id === displayedActiveZoneId;
                  const loading = zone.id === pendingZoneId;
                  const pausing = zone.id === pausingZoneId;
                  const displayName = zoneDisplayName(zone);
                  const liveState = liveZoneStates[zone.id];
                  const playing = isPlayingState(liveState ?? zone.playing_state);
                  return (
                    <div
                      className={`zone-option${active ? ' active' : ''}${loading ? ' is-loading' : ''}`}
                      role="option"
                      aria-selected={active}
                      aria-busy={loading}
                      aria-disabled={Boolean(pendingZoneId)}
                      tabIndex={pendingZoneId ? -1 : 0}
                      key={zone.id}
                      onClick={() => selectZone(zone.id, active)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault();
                          selectZone(zone.id, active);
                        }
                      }}
                    >
                      {loading ? (
                        <span className="zone-loading-spinner" aria-hidden="true" />
                      ) : (
                        <ZoneOutputIcon zone={zone} hegelZoneId={hegelZoneId} />
                      )}
                      <span>
                        <strong>{displayName}</strong>
                        <small>{zoneFormatLabel(zone) || zone.playing_state || 'Output'}</small>
                      </span>
                      {playing ? (
                        <button
                          className="zone-pause-button"
                          type="button"
                          aria-label={`Pause ${displayName}`}
                          title={`Pause ${displayName}`}
                          disabled={pausing}
                          onClick={(event) => {
                            event.stopPropagation();
                            pauseZone(zone.id).catch(() => undefined);
                          }}
                        >
                          {pausing ? (
                            <span className="zone-loading-spinner" aria-hidden="true" />
                          ) : (
                            <>
                              <PlayingEqualizer className="zone-playing-equalizer" />
                              <svg
                                className="zone-pause-icon"
                                viewBox="0 0 24 24"
                                aria-hidden="true"
                              >
                                <rect x="7" y="5" width="3.5" height="14" rx="1.2" />
                                <rect x="13.5" y="5" width="3.5" height="14" rx="1.2" />
                              </svg>
                            </>
                          )}
                        </button>
                      ) : null}
                    </div>
                  );
                })}
              </div>
            ))
          ) : (
            <div className="zone-empty">No enabled outputs found</div>
          )}
        </div>
      ) : null}
    </div>
  );
}
