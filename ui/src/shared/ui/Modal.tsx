import { type ReactNode, useLayoutEffect, useState } from 'react';
import { createPortal } from 'react-dom';

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
  const [portalHost, setPortalHost] = useState<Element | null>(() =>
    typeof document === 'undefined' ? null : document.querySelector('.react-app')
  );

  useLayoutEffect(() => {
    if (!open || portalHost) return;
    setPortalHost(document.querySelector('.react-app'));
  }, [open, portalHost]);

  if (!open) return null;
  const modal = (
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

  return portalHost ? createPortal(modal, portalHost) : modal;
}
