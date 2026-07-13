# HDR Web UI Experiment Notes

Date: 2026-06-26

This captures what we learned while experimenting with HDR-like hover/interaction effects in the web UI.

## Goal

Create a subtle HDR/specular brightness lift for UI elements, first on artist portraits and then on the playback progress bar.

## What Worked

The only path that visibly worked was a transparent overlay above normal DOM/image content using `backdrop-filter`, gated to Safari/HDR:

```css
@supports (background: -webkit-named-image(i)) {
  @media (dynamic-range: high) {
    .shader {
      backdrop-filter: brightness(1.15) contrast(1.03) saturate(1.03);
      -webkit-backdrop-filter: brightness(1.15) contrast(1.03) saturate(1.03);
    }
  }
}
```

For artist portraits, this worked because the overlay sat directly above real painted image pixels. Chrome/SDR needed to be excluded because it clipped the image when treated as HDR-capable.

## What Failed

- `filter: brightness(...)` on SDR images clipped highlights immediately.
- CSS pseudo-element glints/sweeps caused transient artifacts, including square/circular compositing glitches in Safari.
- WebGPU canvas experiments still appeared SDR once composited into the DOM.
- Drawing the image and highlight together in WebGPU still appeared clamped in the browser compositor.
- Progress bar HDR was unreliable while the visible bar was a native `input[type="range"]` track.

## Important Browser Notes

- `@media (dynamic-range: high)` means the display/path may support HDR. It does not guarantee that a given DOM/CSS/canvas layer is presented unclamped.
- `(-webkit-min-device-pixel-ratio: 0)` also matches Chrome, so it is not Safari-only.
- `@supports (background: -webkit-named-image(i))` was a more useful Safari-specific gate.
- `/styles.css` is runtime-loaded and cacheable. For React UI experiments, prefer putting test CSS in `ui/src/app.css` so Vite produces a cache-busted asset.

## Progress Bar Lesson

The artist-image trick did not transfer cleanly to the progress bar because native range controls are UA-painted/composited. If this is revisited, make the visual progress bar ordinary DOM, and use the range input only as a transparent hit target, or replace it with a fully custom accessible DOM slider.

Recommended structure:

```tsx
<div className="seek-slider-shell" style={sliderStyle}>
  <span className="seek-visual-track">
    <span className="seek-visual-fill" />
  </span>
  <span className="seek-brightness-shader" />
  <input className="seek-slider" type="range" />
</div>
```

Then ensure the shader samples only the filled portion if the desired effect is on played progress:

```css
.seek-brightness-shader {
  left: 0;
  width: var(--slider-fill, 0%);
  pointer-events: none;
}
```

## Practical Recommendation

Do not ship broad HDR CSS experiments globally. If using this again:

1. Keep SDR behavior neutral.
2. Gate strong brightness to Safari plus `dynamic-range: high`.
3. Use modest values first, around `brightness(1.1)` to `brightness(1.2)`.
4. Put experiment CSS in `ui/src/app.css`, not only `static/styles.css`.
5. Verify in Safari on an actual HDR display, not from screenshots, because screenshots are SDR/tonemapped.
