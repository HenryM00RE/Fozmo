export function QobuzSourceIcon({ decorative = false }: { decorative?: boolean }) {
  return (
    <span
      className="album-source-icon is-qobuz"
      aria-hidden={decorative ? 'true' : undefined}
      aria-label={decorative ? undefined : 'Qobuz'}
      title={decorative ? undefined : 'Qobuz'}
    >
      <svg viewBox="0 0 100 100" aria-hidden="true">
        <rect width="100" height="100" fill="none" />
        <circle cx="50" cy="50" r="8" />
        <circle cx="50" cy="50" r="32" />
        <line x1="62.73" y1="62.73" x2="79.70" y2="79.70" />
      </svg>
    </span>
  );
}
