import type { MouseEventHandler } from 'react';
import { useEffect, useRef, useState } from 'react';
import { PlaybarPlayIcon } from './PlaybarPlayIcon';

type AlbumCoverPlayButtonProps = {
  ariaLabel: string;
  onClick: MouseEventHandler<HTMLButtonElement>;
  title: string;
};

function imageButtonAreaIsBright(image: HTMLImageElement) {
  if (!image.complete || !image.naturalWidth || !image.naturalHeight) return false;

  const size = 32;
  const canvas = document.createElement('canvas');
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) return false;

  try {
    context.drawImage(image, 0, 0, size, size);
    const sampleX = Math.floor(size * 0.45);
    const sampleY = Math.floor(size * 0.58);
    const sampleWidth = size - sampleX;
    const sampleHeight = size - sampleY;
    const { data } = context.getImageData(sampleX, sampleY, sampleWidth, sampleHeight);
    let pixels = 0;
    let luminanceTotal = 0;
    let brightPixels = 0;

    for (let index = 0; index < data.length; index += 4) {
      const alpha = data[index + 3];
      if (alpha < 32) continue;
      const red = data[index];
      const green = data[index + 1];
      const blue = data[index + 2];
      const luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
      pixels += 1;
      luminanceTotal += luminance;
      if (luminance >= 235) brightPixels += 1;
    }

    if (!pixels) return false;
    const averageLuminance = luminanceTotal / pixels;
    const brightRatio = brightPixels / pixels;
    return averageLuminance >= 226 && brightRatio >= 0.48;
  } catch {
    return false;
  }
}

export function AlbumCoverPlayButton({ ariaLabel, onClick, title }: AlbumCoverPlayButtonProps) {
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const [brightCover, setBrightCover] = useState(false);

  useEffect(() => {
    if (window.matchMedia('(hover: none), (pointer: coarse)').matches) {
      setBrightCover(false);
      return undefined;
    }

    const cover = buttonRef.current?.closest('.album-cover, .playlist-card-art');
    if (!cover) return undefined;

    const images = Array.from(cover.querySelectorAll('img'));
    if (!images.length) {
      setBrightCover(false);
      return undefined;
    }

    let cancelled = false;
    const update = () => {
      if (!cancelled) setBrightCover(images.some(imageButtonAreaIsBright));
    };

    update();
    images.forEach((image) => {
      image.addEventListener('load', update);
      image.addEventListener('error', update);
    });

    return () => {
      cancelled = true;
      images.forEach((image) => {
        image.removeEventListener('load', update);
        image.removeEventListener('error', update);
      });
    };
  }, []);

  return (
    <button
      className={`album-cover-play${brightCover ? ' is-bright-cover' : ''}`}
      type="button"
      title={title}
      aria-label={ariaLabel}
      onClick={onClick}
      ref={buttonRef}
    >
      <PlaybarPlayIcon className="album-cover-play-icon" />
    </button>
  );
}
