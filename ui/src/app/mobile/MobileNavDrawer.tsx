import { type ReactNode, useEffect } from 'react';

type MobileNavDrawerProps = {
  children: ReactNode;
  mode: 'main' | 'settings';
  open: boolean;
  onClose: () => void;
};

export function MobileNavDrawer({ children, mode, open, onClose }: MobileNavDrawerProps) {
  useEffect(() => {
    if (!open) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [onClose, open]);

  if (!open) return null;

  return (
    <div className="mobile-nav-backdrop" role="presentation" onClick={onClose}>
      <aside
        className={`mobile-nav-drawer app-sidebar app-sidebar-${mode}-mode`}
        aria-label="Navigation"
        aria-modal="true"
        role="dialog"
        onClick={(event) => event.stopPropagation()}
      >
        {children}
      </aside>
    </div>
  );
}
