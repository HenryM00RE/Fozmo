import { isQobuzPlaylistRectangleImage } from '../model/qobuzPlaylistData';

export function QobuzPlaylistArtwork({ src }: { src: string }) {
  if (!isQobuzPlaylistRectangleImage(src)) return <img alt="" src={src} loading="lazy" />;
  return (
    <span className="qobuz-playlist-editorial-art" aria-hidden="true">
      <img className="qobuz-playlist-editorial-backdrop" alt="" src={src} loading="lazy" />
      <img className="qobuz-playlist-editorial-image" alt="" src={src} loading="lazy" />
    </span>
  );
}
