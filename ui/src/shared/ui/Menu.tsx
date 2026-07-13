import type { CSSProperties, MouseEventHandler, ReactNode } from 'react';

export function Menu({
  ariaLabel,
  children,
  className = '',
  onClick,
  style
}: {
  ariaLabel: string;
  children: ReactNode;
  className?: string;
  onClick?: MouseEventHandler<HTMLDivElement>;
  style?: CSSProperties;
}) {
  return (
    <div
      className={`menu${className ? ` ${className}` : ''}`}
      role="menu"
      aria-label={ariaLabel}
      style={style}
      onClick={onClick}
    >
      {children}
    </div>
  );
}
