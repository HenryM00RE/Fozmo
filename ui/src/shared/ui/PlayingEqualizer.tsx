export function PlayingEqualizer({ className = '' }: { className?: string }) {
  return (
    <div className={`playing-equalizer${className ? ` ${className}` : ''}`} aria-hidden="true">
      <div className="bar bar-1" />
      <div className="bar bar-2" />
      <div className="bar bar-3" />
      <div className="bar bar-4" />
    </div>
  );
}
