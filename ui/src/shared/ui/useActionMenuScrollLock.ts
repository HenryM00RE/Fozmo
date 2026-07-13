import { useEffect } from 'react';

let actionMenuScrollLockCount = 0;

function setActionMenuScrollLocked(locked: boolean) {
  if (typeof document === 'undefined') return;
  document.body.classList.toggle('action-menu-scroll-locked', locked);
}

export function useActionMenuScrollLock(active: boolean) {
  useEffect(() => {
    if (!active) return undefined;
    actionMenuScrollLockCount += 1;
    setActionMenuScrollLocked(true);
    return () => {
      actionMenuScrollLockCount = Math.max(0, actionMenuScrollLockCount - 1);
      setActionMenuScrollLocked(actionMenuScrollLockCount > 0);
    };
  }, [active]);
}
