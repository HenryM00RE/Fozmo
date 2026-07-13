import { endpoints } from '../../../shared/lib/api';
import type { Playlist, QueueItem } from '../../../shared/types';
import { playlistItems } from '../model/playlistModel';

function playlistItemCover(item: QueueItem) {
  if (item.artId) return { key: `art:${item.artId}`, src: endpoints.artUrl(item.artId) };
  const src = item.imageUrl || item.qobuzTrack?.image_url || item.resolvedSource?.image_url || null;
  return src ? { key: `image:${src}`, src } : null;
}

function playlistCoverKeys(item: QueueItem, coverKey: string) {
  const keys = [coverKey];
  const albumId = item.qobuzTrack?.album_id || item.albumId || item.resolvedSource?.album_id;
  if (albumId) keys.push(`album-id:${albumId}`);
  else if (item.album)
    keys.push(`album:${(item.artist || '').toLowerCase()}|${item.album.toLowerCase()}`);
  return keys;
}

export function PlaylistCover({ playlist }: { playlist: Playlist }) {
  const covers = [];
  const seen = new Set<string>();
  for (const item of playlistItems(playlist)) {
    const cover = playlistItemCover(item);
    if (!cover?.src) continue;
    const keys = playlistCoverKeys(item, cover.key);
    if (keys.some((key) => seen.has(key))) continue;
    keys.forEach((key) => seen.add(key));
    covers.push(cover.src);
    if (covers.length >= 4) break;
  }

  if (!covers.length) {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M4 7h12M4 12h12M4 17h8" />
        <path d="M18 15v5l4-2.5L18 15Z" />
      </svg>
    );
  }

  return (
    <div className={`playlist-cover-mosaic count-${covers.length}`}>
      {covers.map((src) => (
        <img alt="" src={src} loading="lazy" key={src} />
      ))}
    </div>
  );
}
