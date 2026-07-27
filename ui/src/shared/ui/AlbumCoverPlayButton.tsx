import type { MouseEventHandler } from 'react';
import { useEffect, useRef, useState } from 'react';
import { PlaybarPlayIcon } from './PlaybarPlayIcon';

type AlbumCoverPlayButtonProps = {
  ariaLabel: string;
  onClick: MouseEventHandler<HTMLButtonElement>;
  title: string;
};

// Streaming covers come straight from the provider's CDN, and drawing a
// cross-origin image taints the canvas so `getImageData` throws. That left
// every Apple Music and Qobuz cover falling back to the light icon, which
// disappears against a white sleeve. Re-requesting the same URL with CORS
// yields a sampleable copy, cached so one cover is only fetched once. The
// rendered <img> is deliberately left alone: a CDN that withholds the header
// degrades to the previous icon rather than failing to load the cover at all.
const brightCoverCache = new Map<string, Promise<boolean>>();
const BRIGHT_COVER_CACHE_LIMIT = 512;

function isCrossOrigin(source: string) {
  try {
    return new URL(source, window.location.href).origin !== window.location.origin;
  } catch {
    return false;
  }
}

function loadSampleableImage(source: string) {
  return new Promise<HTMLImageElement | null>((resolve) => {
    const probe = new Image();
    probe.crossOrigin = 'anonymous';
    probe.decoding = 'async';
    probe.addEventListener('load', () => resolve(probe));
    probe.addEventListener('error', () => resolve(null));
    probe.src = source;
  });
}

function coverIsBright(image: HTMLImageElement, region: SampleRegion): Promise<boolean> {
  const source = image.currentSrc || image.src;
  if (!source) return Promise.resolve(false);
  if (!isCrossOrigin(source)) return Promise.resolve(imageButtonAreaIsBright(image, region));

  // Cards are a fixed size per shelf, so the region is stable for a given cover.
  const cacheKey = `${source}|${region.x},${region.y},${region.width},${region.height}`;
  const cached = brightCoverCache.get(cacheKey);
  if (cached) return cached;
  const pending = loadSampleableImage(source).then((probe) =>
    probe ? imageButtonAreaIsBright(probe, region) : false
  );
  if (brightCoverCache.size >= BRIGHT_COVER_CACHE_LIMIT) {
    const oldest = brightCoverCache.keys().next().value;
    if (oldest !== undefined) brightCoverCache.delete(oldest);
  }
  brightCoverCache.set(cacheKey, pending);
  return pending;
}

const SAMPLE_SIZE = 64;

type SampleRegion = { x: number; y: number; width: number; height: number };

// Where the icon actually sits, in sampled pixels.
//
// Reading a fixed fraction of the sleeve sampled far more than the icon covers,
// so one dark shape elsewhere in the lower half — a record, a face — vetoed the
// darkening even when the pixels behind the icon were white. Measuring the
// button keeps the question to "what is under this icon", at any card size.
function buttonSampleRegion(cover: Element, button: Element): SampleRegion {
  const fallback = {
    x: Math.floor(SAMPLE_SIZE * 0.72),
    y: Math.floor(SAMPLE_SIZE * 0.72),
    width: Math.ceil(SAMPLE_SIZE * 0.28),
    height: Math.ceil(SAMPLE_SIZE * 0.28)
  };
  const coverRect = cover.getBoundingClientRect();
  const buttonRect = button.getBoundingClientRect();
  if (!coverRect.width || !coverRect.height || !buttonRect.width || !buttonRect.height) {
    return fallback;
  }

  const clamp = (value: number) => Math.min(1, Math.max(0, value));
  const left = clamp((buttonRect.left - coverRect.left) / coverRect.width);
  const top = clamp((buttonRect.top - coverRect.top) / coverRect.height);
  const right = clamp((buttonRect.right - coverRect.left) / coverRect.width);
  const bottom = clamp((buttonRect.bottom - coverRect.top) / coverRect.height);

  const x = Math.min(SAMPLE_SIZE - 1, Math.floor(left * SAMPLE_SIZE));
  const y = Math.min(SAMPLE_SIZE - 1, Math.floor(top * SAMPLE_SIZE));
  return {
    x,
    y,
    width: Math.max(1, Math.min(SAMPLE_SIZE - x, Math.ceil(right * SAMPLE_SIZE) - x)),
    height: Math.max(1, Math.min(SAMPLE_SIZE - y, Math.ceil(bottom * SAMPLE_SIZE) - y))
  };
}

function imageButtonAreaIsBright(image: HTMLImageElement, region: SampleRegion) {
  if (!image.complete || !image.naturalWidth || !image.naturalHeight) return false;

  const canvas = document.createElement('canvas');
  canvas.width = SAMPLE_SIZE;
  canvas.height = SAMPLE_SIZE;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) return false;

  try {
    context.drawImage(image, 0, 0, SAMPLE_SIZE, SAMPLE_SIZE);
    const { data } = context.getImageData(region.x, region.y, region.width, region.height);
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

    const button = buttonRef.current;
    const cover = button?.closest('.album-cover, .playlist-card-art');
    if (!button || !cover) return undefined;

    const images = Array.from(cover.querySelectorAll('img'));
    if (!images.length) {
      setBrightCover(false);
      return undefined;
    }

    let cancelled = false;
    const update = () => {
      const region = buttonSampleRegion(cover, button);
      Promise.all(images.map((image) => coverIsBright(image, region))).then((results) => {
        if (!cancelled) setBrightCover(results.some(Boolean));
      });
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
