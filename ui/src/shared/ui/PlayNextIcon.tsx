export function PlayNextIcon({ className = 'play-next-icon' }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" aria-hidden="true">
      <path
        className="play-next-glyph"
        d="M4.6 7.15c0-.74.8-1.2 1.43-.82l4.45 2.68c.61.37.61 1.25 0 1.62l-4.45 2.68c-.63.38-1.43-.08-1.43-.82V7.15Z"
      />
      <path className="play-next-lines" d="M12.25 6.75h7.1M12.25 10.35h7.1M12.25 13.95h7.1" />
    </svg>
  );
}
