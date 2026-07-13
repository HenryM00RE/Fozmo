export function PlaybarPlayIcon({ className = 'playbar-play-icon' }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 100 100"
      className={className}
      aria-hidden="true"
      shapeRendering="geometricPrecision"
    >
      <path d="M 39 32 C 36.2 33.3 35 35.7 35 39 L 35 61 C 35 64.3 36.2 66.7 39 68 C 41.4 69.1 43.5 68.2 46.2 66.6 L 66.4 54.4 C 71.2 51.5 71.2 48.5 66.4 45.6 L 46.2 33.4 C 43.5 31.8 41.4 30.9 39 32 Z" />
    </svg>
  );
}
