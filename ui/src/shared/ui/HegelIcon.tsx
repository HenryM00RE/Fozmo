import { useId } from 'react';

type HegelIconProps = {
  className?: string;
  detail?: 'simple' | 'panel';
};

export function HegelIcon({ className, detail = 'panel' }: HegelIconProps) {
  const maskIdPrefix = useId().replace(/:/g, '');
  const leftKnobMaskId = `${maskIdPrefix}-left-knob-mask`;
  const middleControlMaskId = `${maskIdPrefix}-middle-control-mask`;
  const rightKnobMaskId = `${maskIdPrefix}-right-knob-mask`;

  if (detail === 'panel') {
    return (
      <svg viewBox="160 180 570 262" className={className} aria-hidden="true" focusable="false">
        <g fill="none" stroke="currentColor" strokeLinejoin="round">
          <polygon
            points="355,415 355,438 395,433 395,410"
            fill="currentColor"
            stroke="currentColor"
            strokeWidth="6"
          />

          <polygon
            points="635,380 635,403 675,398 675,375"
            fill="currentColor"
            stroke="currentColor"
            strokeWidth="6"
          />

          <polygon points="170,240 320,300 720,250 570,190" strokeWidth="18" />

          <g transform="translate(170 240) skewY(21.8)">
            <rect width="150" height="120" strokeWidth="18" />
          </g>

          <g transform="translate(320 300) skewY(-7.125)">
            <rect width="400" height="120" rx="2" ry="2" strokeWidth="18" />

            <mask id={leftKnobMaskId}>
              <rect x="0" y="0" width="400" height="120" fill="white" />
              <line
                x1="65"
                y1="60"
                x2="65"
                y2="36"
                stroke="black"
                strokeWidth="5"
                strokeLinecap="round"
              />
            </mask>

            <circle
              cx="65"
              cy="60"
              r="28"
              fill="currentColor"
              stroke="none"
              mask={`url(#${leftKnobMaskId})`}
            />

            <mask id={middleControlMaskId}>
              <rect x="0" y="0" width="400" height="120" fill="white" />
              <line
                x1="170"
                y1="68"
                x2="230"
                y2="68"
                stroke="black"
                strokeWidth="5"
                strokeLinecap="round"
              />
            </mask>

            <rect
              x="130"
              y="38"
              width="140"
              height="44"
              rx="4"
              fill="currentColor"
              stroke="none"
              mask={`url(#${middleControlMaskId})`}
            />

            <mask id={rightKnobMaskId}>
              <rect x="0" y="0" width="400" height="120" fill="white" />
              <line
                x1="335"
                y1="60"
                x2="335"
                y2="36"
                stroke="black"
                strokeWidth="5"
                strokeLinecap="round"
              />
            </mask>

            <circle
              cx="335"
              cy="60"
              r="28"
              fill="currentColor"
              stroke="none"
              mask={`url(#${rightKnobMaskId})`}
            />
          </g>
        </g>
      </svg>
    );
  }

  return (
    <svg viewBox="0 0 24 24" className={className} aria-hidden="true" focusable="false">
      <g fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
        <path d="M5 15.65v1.15h1.75v-1.15" />
        <path d="M17.25 15.65v1.15H19v-1.15" />
        <rect x="2.5" y="8.25" width="19" height="7.4" rx="1.2" />
      </g>
    </svg>
  );
}
