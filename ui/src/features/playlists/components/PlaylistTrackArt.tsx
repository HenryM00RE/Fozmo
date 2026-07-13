import { endpoints } from '../../../shared/lib/api';
import type { QueueItem } from '../../../shared/types';

export function PlaylistTrackArt({ item }: { item: QueueItem }) {
  const src = item.artId
    ? endpoints.artUrl(item.artId)
    : item.imageUrl || item.qobuzTrack?.image_url || item.resolvedSource?.image_url || null;
  if (src) return <img alt="" src={src} loading="lazy" />;
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="9" />
      <circle cx="12" cy="12" r="2" />
    </svg>
  );
}
