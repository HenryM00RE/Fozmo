type SpeakerIconProps = {
  className?: string;
};

export function SpeakerIcon({ className }: SpeakerIconProps) {
  return (
    <svg viewBox="0 0 24 24" className={className} aria-hidden="true" focusable="false">
      <g fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
        <path d="M7.15 4.85 15.7 3.7c.72-.1 1.3.46 1.3 1.18v14.24c0 .72-.58 1.28-1.3 1.18l-8.55-1.15A1.34 1.34 0 0 1 6 17.82V6.18c0-.67.49-1.24 1.15-1.33Z" />
        <path d="m17 4.55 1.95.96c.64.31 1.05.97 1.05 1.68v9.62c0 .71-.41 1.37-1.05 1.68L17 19.45" />
        <circle cx="11.5" cy="8.1" r="1.05" />
        <circle cx="11.5" cy="14.45" r="3.35" />
        <circle cx="11.5" cy="14.45" r="1.95" />
      </g>
    </svg>
  );
}
