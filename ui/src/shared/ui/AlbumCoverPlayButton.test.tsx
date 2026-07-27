// @vitest-environment jsdom

import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AlbumCoverPlayButton } from './AlbumCoverPlayButton';

const LOCAL_COVER = 'http://localhost:3000/api/library/art/12';
const REMOTE_COVER = 'https://is1-ssl.mzstatic.com/image/thumb/source/600x600bb.jpg';
// The component caches probe results per cover, so each case needs its own URL.
const NO_CORS_COVER = 'https://static.qobuz.com/images/covers/ab/cd/no-cors_600.jpg';
const MOSTLY_DARK_COVER = 'https://static.qobuz.com/images/covers/ab/cd/disco-ball_600.jpg';

// Captured before any spy is installed, so re-stubbing cannot wrap itself.
const nativeCreateElement = document.createElement.bind(document);

type Probe = { src: string; crossOrigin: string | null };

let corsProbes: Probe[] = [];
let sampledRects: Array<[number, number, number, number]> = [];

// jsdom never decodes an image, so stand in for one that loaded successfully.
function fakeImageClass(outcome: 'load' | 'error') {
  return class {
    crossOrigin: string | null = null;
    decoding = 'auto';
    complete = true;
    naturalWidth = 600;
    naturalHeight = 600;
    private handlers: Record<string, Array<() => void>> = {};
    private value = '';

    addEventListener(type: string, handler: () => void) {
      const existing = this.handlers[type] ?? [];
      existing.push(handler);
      this.handlers[type] = existing;
    }

    set src(next: string) {
      this.value = next;
      corsProbes.push(this);
      queueMicrotask(() => {
        for (const handler of this.handlers[outcome] ?? []) handler();
      });
    }

    get src() {
      return this.value;
    }
  };
}

// jsdom has no canvas, so paint a synthetic sleeve: `luminanceAt` answers for
// any pixel of the 64x64 sample the component draws.
function installCanvas(luminanceAt: (x: number, y: number) => number) {
  const context = {
    drawImage: vi.fn(),
    getImageData: (x: number, y: number, width: number, height: number) => {
      sampledRects.push([x, y, width, height]);
      const data = new Uint8ClampedArray(width * height * 4);
      for (let row = 0; row < height; row += 1) {
        for (let column = 0; column < width; column += 1) {
          const value = luminanceAt(x + column, y + row);
          const index = (row * width + column) * 4;
          data[index] = value;
          data[index + 1] = value;
          data[index + 2] = value;
          data[index + 3] = 255;
        }
      }
      return { data };
    }
  };
  vi.spyOn(document, 'createElement').mockImplementation(((tag: string) =>
    tag === 'canvas'
      ? ({ width: 0, height: 0, getContext: () => context } as unknown as HTMLCanvasElement)
      : nativeCreateElement(tag)) as typeof document.createElement);
}

function renderCover(src: string) {
  return render(
    <div className="album-cover">
      <img alt="" src={src} />
      <AlbumCoverPlayButton ariaLabel="Play album" title="Play album" onClick={() => {}} />
    </div>
  );
}

// jsdom reports every element as zero-sized, which the component treats as
// "geometry unavailable". Give it a 200px card with the real 40px icon inset.
function stubLayout(container: HTMLElement) {
  const cover = container.querySelector<HTMLElement>('.album-cover');
  const button = container.querySelector<HTMLElement>('.album-cover-play');
  if (!cover || !button) throw new Error('expected a cover and a play button');
  cover.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: 200, bottom: 200, width: 200, height: 200 }) as DOMRect;
  button.getBoundingClientRect = () =>
    ({ left: 152, top: 152, right: 192, bottom: 192, width: 40, height: 40 }) as DOMRect;
}

// Present the rendered <img> as decoded and re-fire the load the component
// listens for, so sampling re-runs against the stubbed layout.
function markLoaded(container: HTMLElement) {
  const image = container.querySelector('img');
  if (!image) throw new Error('expected a cover image');
  Object.defineProperty(image, 'complete', { value: true });
  Object.defineProperty(image, 'naturalWidth', { value: 600 });
  Object.defineProperty(image, 'naturalHeight', { value: 600 });
  image.dispatchEvent(new Event('load'));
}

describe('AlbumCoverPlayButton', () => {
  beforeEach(() => {
    corsProbes = [];
    sampledRects = [];
    vi.stubGlobal('Image', fakeImageClass('load'));
    vi.stubGlobal(
      'matchMedia',
      vi.fn(() => ({ matches: false }) as unknown as MediaQueryList)
    );
    installCanvas(() => 255);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('darkens the icon on a white streaming cover by re-reading it with CORS', async () => {
    const { container } = renderCover(REMOTE_COVER);

    await waitFor(() => {
      expect(container.querySelector('.album-cover-play')).toHaveClass('is-bright-cover');
    });
    expect(corsProbes).toHaveLength(1);
    expect(corsProbes[0].src).toBe(REMOTE_COVER);
    expect(corsProbes[0].crossOrigin).toBe('anonymous');
  });

  it('samples a same-origin cover directly instead of re-requesting it', async () => {
    const { container } = renderCover(LOCAL_COVER);
    markLoaded(container);

    await waitFor(() => {
      expect(container.querySelector('.album-cover-play')).toHaveClass('is-bright-cover');
    });
    expect(corsProbes).toHaveLength(0);
  });

  it('keeps the light icon when a CDN refuses the CORS request', async () => {
    vi.stubGlobal('Image', fakeImageClass('error'));

    const { container } = renderCover(NO_CORS_COVER);

    await waitFor(() => {
      expect(corsProbes).toHaveLength(1);
    });
    expect(container.querySelector('.album-cover-play')).not.toHaveClass('is-bright-cover');
  });

  it('judges only the pixels under the icon, not the rest of the sleeve', async () => {
    // A dark subject filling the sleeve, with white paper behind the icon —
    // sampling a fixed lower-right fraction would read this as dark.
    installCanvas((x, y) => (x >= 45 && y >= 45 ? 255 : 0));

    const { container } = renderCover(MOSTLY_DARK_COVER);
    stubLayout(container);
    markLoaded(container);

    await waitFor(() => {
      expect(container.querySelector('.album-cover-play')).toHaveClass('is-bright-cover');
    });
    // The 40px icon inset 8px into a 200px card, in 64px sample space.
    expect(sampledRects).toContainEqual([48, 48, 14, 14]);
    // Every read stays in the icon's corner, measured or fallback. The old
    // fixed fraction started at (28, 37) of 64 and swept most of the sleeve.
    expect(sampledRects.every(([x, y]) => x >= 44 && y >= 44)).toBe(true);
  });
});
