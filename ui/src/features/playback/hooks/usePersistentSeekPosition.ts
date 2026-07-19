import { useEffect } from 'react';
import { sourceRefKey } from '../../../shared/lib/queue';
import {
  clearTransportPending,
  type TransportPendingSnapshot
} from '../model/playbackControlStore';
import type { PlaybackStatus } from '../model/playbackStore';
import { refreshPlaybackStatus } from '../model/playbackStore';

const SEEK_CONFIRMATION_TOLERANCE_SECONDS = 2.5;
const SEEK_PENDING_TIMEOUT_MS = 25_000;

function statusTrackKey(status: PlaybackStatus) {
  return (
    sourceRefKey(status.current_source) ||
    [status.file_name || '', status.track_title || '', status.track_artist || ''].join('|')
  );
}

function projectedSeekPosition(
  status: PlaybackStatus,
  pending: TransportPendingSnapshot,
  now: number
) {
  const expectedPosition = Math.max(0, Number(pending.expectedPosition) || 0);
  if (status.state !== 'Playing') return expectedPosition;
  return expectedPosition + Math.max(0, now - pending.requestedAt) / 1_000;
}

function matchingPendingSeek(status: PlaybackStatus, pending: TransportPendingSnapshot | null) {
  if (pending?.kind !== 'seek' || !Number.isFinite(pending.expectedPosition)) return null;
  if (pending.expectedTrackKey && pending.expectedTrackKey !== statusTrackKey(status)) return null;
  return pending;
}

export function usePersistentSeekPosition(
  status: PlaybackStatus,
  interpolatedPosition: number,
  transportPending: TransportPendingSnapshot | null
) {
  const pendingSeek = matchingPendingSeek(status, transportPending);

  useEffect(() => {
    if (transportPending?.kind !== 'seek') return undefined;
    if (!pendingSeek) {
      clearTransportPending('seek');
      return undefined;
    }

    const now = Date.now();
    const measuredPosition = Math.max(0, Number(status.position_secs) || 0);
    const expectedPosition = projectedSeekPosition(status, pendingSeek, now);
    const backendStillSeeking = status.transport_pending === 'seeking';
    if (
      !backendStillSeeking &&
      Math.abs(measuredPosition - expectedPosition) <= SEEK_CONFIRMATION_TOLERANCE_SECONDS
    ) {
      clearTransportPending('seek');
      return undefined;
    }

    const remaining = SEEK_PENDING_TIMEOUT_MS - Math.max(0, now - pendingSeek.requestedAt);
    if (remaining <= 0) {
      clearTransportPending('seek');
      refreshPlaybackStatus({ force: true }).catch(() => undefined);
      return undefined;
    }

    const timeoutId = window.setTimeout(() => {
      clearTransportPending('seek');
      refreshPlaybackStatus({ force: true }).catch(() => undefined);
    }, remaining);
    return () => window.clearTimeout(timeoutId);
  }, [pendingSeek, status, transportPending]);

  if (!pendingSeek) return interpolatedPosition;
  const duration = Math.max(0, Number(status.duration_secs) || 0);
  const position = projectedSeekPosition(status, pendingSeek, Date.now());
  return duration > 0 ? Math.min(position, duration) : position;
}
