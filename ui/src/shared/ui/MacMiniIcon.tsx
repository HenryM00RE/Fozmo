type MacMiniIconProps = {
  className?: string;
  detail?: 'simple' | 'panel';
};

export function MacMiniIcon({ className, detail = 'panel' }: MacMiniIconProps) {
  if (detail === 'panel') {
    return (
      <svg viewBox="0 0 120 88" className={className} aria-hidden="true" focusable="false">
        <g fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
          <path
            d="M20.9 42.15 L20.9 58.15 Q20.9 61.6 27.8 65.2 L46.2 74.8 Q60 82 73.8 75.4 L92.2 66.6 Q99.1 63.3 99.1 59.85 L99.1 43.85"
            vectorEffect="non-scaling-stroke"
          />
          <path
            d="M27.8 35.4 L46.2 26.6 Q60 20 73.8 27.2 L92.2 36.8 Q106 44 92.2 50.6 L73.8 59.4 Q60 66 46.2 58.8 L27.8 49.2 Q14 42 27.8 35.4 Z"
            vectorEffect="non-scaling-stroke"
          />
          <circle cx="86" cy="61" r="1.7" fill="currentColor" stroke="none" />
        </g>
      </svg>
    );
  }

  return (
    <svg viewBox="0 0 24 24" className={className} aria-hidden="true" focusable="false">
      <g fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
        <path d="M4.18 9.96 L4.18 13.36 Q4.18 14.08 5.56 14.86 L9.24 16.66 Q12 18.1 14.76 16.78 L18.44 15.02 Q19.82 14.36 19.82 13.67 L19.82 10.27" />
        <path d="M5.56 8.58 L9.24 6.82 Q12 5.5 14.76 6.94 L18.44 8.86 Q21.2 10.3 18.44 11.62 L14.76 13.38 Q12 14.7 9.24 13.26 L5.56 11.46 Q2.8 9.9 5.56 8.58 Z" />
        <circle cx="16.6" cy="12.7" r="0.5" fill="currentColor" stroke="none" />
      </g>
    </svg>
  );
}
