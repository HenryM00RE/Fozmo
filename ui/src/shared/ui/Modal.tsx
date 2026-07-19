import type { ReactNode } from 'react';

export function Modal({
  ariaLabel,
  ariaLabelledBy,
  children,
  className = '',
  onClose,
  open
}: {
  ariaLabel?: string;
  ariaLabelledBy?: string;
  children: ReactNode;
  className?: string;
  onClose: () => void;
  open: boolean;
}) {
  if (!open) return null;
  return (
    <div
      className={`modal-backdrop app-modal-backdrop${className ? ` ${className}` : ''}`}
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      aria-labelledby={ariaLabelledBy}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      {children}
    </div>
  );
}
