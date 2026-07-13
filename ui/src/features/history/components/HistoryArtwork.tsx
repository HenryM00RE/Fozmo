import { useEffect, useState } from 'react';
import type { JsonRecord } from '../../../shared/types';
import { lookupQobuzArtistProfileImage } from '../model/historyData';

export function HistoryArtwork({ item }: { item: JsonRecord }) {
  const imageUrl = String(item.image_url || item.cover_url || '');
  const artId = item.art_id || item.cover_art_id;
  if (imageUrl) return <img alt="" src={imageUrl} loading="lazy" />;
  if (artId)
    return (
      <img alt="" src={`/api/library/art/${encodeURIComponent(String(artId))}`} loading="lazy" />
    );
  return <AlbumPlaceholder />;
}

export function ArtistAvatar({ name }: { name: string }) {
  const [imageUrl, setImageUrl] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    lookupQobuzArtistProfileImage(name).then((url) => {
      if (active) setImageUrl(url);
    });
    return () => {
      active = false;
    };
  }, [name]);

  if (imageUrl) return <img alt="" src={imageUrl} loading="lazy" />;
  return <span>{name.trim().slice(0, 1) || '?'}</span>;
}

function AlbumPlaceholder() {
  return (
    <svg className="file-cover-placeholder" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="9" />
      <circle cx="12" cy="12" r="2" />
    </svg>
  );
}
