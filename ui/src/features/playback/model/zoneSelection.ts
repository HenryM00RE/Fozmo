import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { storageKey } from '../../../shared/identity';
import { endpoints } from '../../../shared/lib/api';
import { isOwnBrowserZoneId } from '../../../shared/lib/browserZone';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import {
  type BrowserZoneSnapshot,
  getBrowserZoneSnapshot,
  subscribeBrowserZone
} from '../../browserZone/browserZoneAgent';

const SELECTED_ZONE_STORAGE_KEY = storageKey('SelectedPlaybackZone');
const SELECTED_ZONE_STATUS_POLL_MS = 1000;

function storedZoneId() {
  try {
    return localStorage.getItem(SELECTED_ZONE_STORAGE_KEY);
  } catch {
    return null;
  }
}

function rememberZoneId(zoneId: string) {
  try {
    localStorage.setItem(SELECTED_ZONE_STORAGE_KEY, zoneId);
  } catch {
    // Local storage is an enhancement; in-memory selection still works.
  }
}

function forgetZoneId() {
  try {
    localStorage.removeItem(SELECTED_ZONE_STORAGE_KEY);
  } catch {
    // Local storage is an enhancement; in-memory selection still works.
  }
}

/**
 * Whether a selected own-browser-zone should be kept even though it is not in
 * the enabled zone set.
 *
 * This browser's private zone is registered by its in-page agent over a
 * WebSocket, and that registration is re-established after every reload and
 * reconnect. So a browser zone that is simply *absent* from the server's zone
 * list is "pending" — it is coming back once the agent re-registers — not gone.
 * Treating that gap as a vanished zone is what used to bounce the selection to
 * another zone (e.g. Sonos) on refresh. A browser zone that IS listed but
 * reported disabled is genuinely off and should fall back like any other zone.
 */
export function isOwnBrowserZonePending(
  selectedZoneId: string | null,
  zones: ZoneProfile[],
  isOwnZone: (zoneId: string) => boolean = isOwnBrowserZoneId
) {
  if (!selectedZoneId || !isOwnZone(selectedZoneId)) return false;
  return !zones.some((zone) => zone.id === selectedZoneId);
}

function enabledZones(zones: ZoneProfile[]) {
  return zones.filter((zone) => zone.enabled !== false);
}

function enabledZoneIds(zones: ZoneProfile[]) {
  return new Set(enabledZones(zones).map((zone) => zone.id));
}

function fallbackZoneId(status: JsonRecord, zones: ZoneProfile[]) {
  const enabled = enabledZones(zones);
  const enabledIds = new Set(enabled.map((zone) => zone.id));
  const statusZoneId = String(status.active_zone_id || '');
  if (statusZoneId && (zones.length === 0 || enabledIds.has(statusZoneId))) {
    return statusZoneId;
  }
  return String(
    enabled.find((zone) => zone.status === 'active')?.id || enabled[0]?.id || 'local-core'
  );
}

function zoneName(zoneId: string, zones: ZoneProfile[]) {
  return zones.find((zone) => zone.id === zoneId)?.name || null;
}

function statusForZone(status: JsonRecord, zoneId: string, zones: ZoneProfile[]) {
  if (String(status.active_zone_id || '') === zoneId) return status;
  return {
    ...status,
    state: 'Stopped',
    file_name: null,
    current_source: null,
    track_title: null,
    track_artist: null,
    track_album: null,
    position_secs: 0,
    duration_secs: 0,
    active_zone_id: zoneId,
    active_zone_name: zoneName(zoneId, zones) || zoneId
  };
}

export function statusWithBrowserPlayback(
  status: JsonRecord,
  browser: BrowserZoneSnapshot
): JsonRecord {
  const playback = browser.playback;
  return {
    ...status,
    state: playback.state,
    file_name: playback.fileName,
    current_source: playback.currentSource,
    track_title: playback.trackTitle,
    track_artist: playback.trackArtist,
    track_album: playback.trackAlbum,
    position_secs: playback.positionSecs,
    duration_secs: playback.durationSecs,
    volume: playback.volume,
    active_zone_id: browser.zoneId,
    active_zone_name: browser.zoneName,
    browser_player_notice: browser.notice,
    remote_connected: browser.connection === 'connected'
  };
}

export function useSelectedPlaybackZone(status: JsonRecord, zones: ZoneProfile[]) {
  const browserZone = useSyncExternalStore(
    subscribeBrowserZone,
    getBrowserZoneSnapshot,
    getBrowserZoneSnapshot
  );
  const enabledZoneIdSet = useMemo(() => enabledZoneIds(zones), [zones]);
  const fallback = fallbackZoneId(status, zones);
  const [selectedZoneId, setSelectedZoneId] = useState<string | null>(() => storedZoneId());
  const ownBrowserZonePending = isOwnBrowserZonePending(selectedZoneId, zones);
  const selectedZoneAvailable =
    Boolean(selectedZoneId && enabledZoneIdSet.has(selectedZoneId)) ||
    Boolean(selectedZoneId && zones.length === 0 && !isOwnBrowserZoneId(selectedZoneId)) ||
    // Keep this browser's own zone selected while its agent re-registers after
    // a reload/reconnect, instead of resolving to a fallback zone.
    ownBrowserZonePending;
  const activeZoneId = selectedZoneId && selectedZoneAvailable ? selectedZoneId : fallback;
  const activeZoneIsEnabled = enabledZoneIdSet.has(activeZoneId);
  const [selectedStatus, setSelectedStatus] = useState<JsonRecord>(() =>
    statusForZone(status, activeZoneId, zones)
  );
  const restoreRequestZoneRef = useRef('');
  const latestStatusRef = useRef(status);
  const latestZonesRef = useRef(zones);
  const selectedStatusRequestRef = useRef(0);
  latestStatusRef.current = status;
  latestZonesRef.current = zones;
  const globalActiveZoneId = String(status.active_zone_id || '');

  useEffect(() => {
    // The browser zone drops out of the list while its agent re-registers
    // (every reload/reconnect) and also when it is genuinely turned off. Only
    // fall back in the latter case — a zone that is merely absent is still
    // coming back, so keeping it selected avoids bouncing playback to another
    // zone on refresh.
    if (
      selectedZoneId &&
      isOwnBrowserZoneId(selectedZoneId) &&
      !enabledZoneIdSet.has(selectedZoneId)
    ) {
      if (ownBrowserZonePending) return;
      if (zones.length === 0) return;
      setSelectedZoneId(fallback);
      rememberZoneId(fallback);
      return;
    }
    if (selectedZoneId && enabledZoneIdSet.has(selectedZoneId)) return;
    if (selectedZoneId && zones.length === 0) return;
    if (zones.length === 0 && !String(status.active_zone_id || '')) return;
    if (zones.length > 0 && fallback === 'local-core' && !enabledZoneIdSet.has(fallback)) {
      setSelectedZoneId(null);
      forgetZoneId();
      return;
    }
    setSelectedZoneId(fallback);
    rememberZoneId(fallback);
  }, [
    enabledZoneIdSet,
    fallback,
    ownBrowserZonePending,
    selectedZoneId,
    status.active_zone_id,
    zones.length
  ]);

  useEffect(() => {
    if (!selectedZoneId || zones.length === 0 || !enabledZoneIdSet.has(selectedZoneId)) return;
    // A browser zone is a per-browser selection: it never becomes the
    // server-wide active zone, so there is nothing to restore.
    if (isOwnBrowserZoneId(selectedZoneId)) {
      restoreRequestZoneRef.current = '';
      return;
    }
    if (String(status.active_zone_id || '') === selectedZoneId) {
      restoreRequestZoneRef.current = '';
      return;
    }
    if (restoreRequestZoneRef.current === selectedZoneId) return;
    restoreRequestZoneRef.current = selectedZoneId;
    endpoints.selectZone(selectedZoneId).catch(() => {
      if (restoreRequestZoneRef.current === selectedZoneId) {
        restoreRequestZoneRef.current = '';
      }
    });
  }, [enabledZoneIdSet, selectedZoneId, status.active_zone_id, zones.length]);

  useEffect(() => {
    if (globalActiveZoneId === activeZoneId) {
      setSelectedStatus(status);
    }
  }, [activeZoneId, globalActiveZoneId, status]);

  useEffect(() => {
    if (globalActiveZoneId === activeZoneId) return undefined;
    if (!activeZoneIsEnabled && !ownBrowserZonePending && zones.length > 0) {
      setSelectedStatus(
        statusForZone(latestStatusRef.current, activeZoneId, latestZonesRef.current)
      );
      return undefined;
    }

    let cancelled = false;
    let inFlight = false;
    let refreshAgain = false;
    let timer = 0;
    let controller: AbortController | null = null;

    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(refreshSelectedStatus, SELECTED_ZONE_STATUS_POLL_MS);
    };
    const refreshSelectedStatus = async () => {
      if (cancelled) return;
      if (inFlight) {
        refreshAgain = true;
        return;
      }
      inFlight = true;
      refreshAgain = false;
      const requestId = selectedStatusRequestRef.current + 1;
      selectedStatusRequestRef.current = requestId;
      controller = new AbortController();
      try {
        const next = await endpoints.zoneStatus(activeZoneId, controller.signal);
        if (!cancelled && selectedStatusRequestRef.current === requestId) {
          setSelectedStatus(next);
        }
      } catch (error) {
        // Keep the last valid snapshot on transient remote failures. Zone-list
        // changes handle genuine disable/removal separately.
        if (error instanceof DOMException && error.name === 'AbortError') return;
      } finally {
        inFlight = false;
        controller = null;
        if (!cancelled) {
          if (refreshAgain) void refreshSelectedStatus();
          else schedule();
        }
      }
    };

    const refreshWhenActive = () => {
      if (document.visibilityState !== 'hidden') void refreshSelectedStatus();
    };
    void refreshSelectedStatus();
    document.addEventListener('visibilitychange', refreshWhenActive);
    window.addEventListener('focus', refreshWhenActive);
    window.addEventListener('online', refreshWhenActive);
    return () => {
      cancelled = true;
      selectedStatusRequestRef.current += 1;
      controller?.abort();
      window.clearTimeout(timer);
      document.removeEventListener('visibilitychange', refreshWhenActive);
      window.removeEventListener('focus', refreshWhenActive);
      window.removeEventListener('online', refreshWhenActive);
    };
  }, [activeZoneId, activeZoneIsEnabled, globalActiveZoneId, ownBrowserZonePending, zones.length]);

  const selectZone = useCallback(
    async (zoneId: string) => {
      const requestId = selectedStatusRequestRef.current + 1;
      selectedStatusRequestRef.current = requestId;
      if (isOwnBrowserZoneId(zoneId)) {
        // Selecting the browser zone is local to this browser; the zone is
        // still driven through the normal zone-scoped endpoints.
        restoreRequestZoneRef.current = '';
        setSelectedZoneId(zoneId);
        rememberZoneId(zoneId);
        try {
          const next = await endpoints.zoneStatus(zoneId);
          if (selectedStatusRequestRef.current === requestId) setSelectedStatus(next);
        } catch {
          if (selectedStatusRequestRef.current === requestId) {
            setSelectedStatus(statusForZone(status, zoneId, zones));
          }
        }
        return;
      }
      restoreRequestZoneRef.current = zoneId;
      try {
        await endpoints.selectZone(zoneId);
      } catch (error) {
        if (restoreRequestZoneRef.current === zoneId) {
          restoreRequestZoneRef.current = '';
        }
        throw error;
      }
      setSelectedZoneId(zoneId);
      rememberZoneId(zoneId);
      try {
        const next = await endpoints.zoneStatus(zoneId);
        if (selectedStatusRequestRef.current === requestId) setSelectedStatus(next);
      } catch {
        if (selectedStatusRequestRef.current === requestId) {
          setSelectedStatus(statusForZone(status, zoneId, zones));
        }
      }
    },
    [status, zones]
  );

  return {
    activeZoneId,
    selectZone,
    status: isOwnBrowserZoneId(activeZoneId)
      ? statusWithBrowserPlayback(selectedStatus, browserZone)
      : selectedStatus
  };
}
