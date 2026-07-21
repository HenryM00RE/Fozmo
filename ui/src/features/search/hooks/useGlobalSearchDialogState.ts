import { useCallback, useEffect, useRef, useState } from 'react';

type OpenSearchMenu = {
  rowId: string;
  x: number;
  y: number;
};

export function useGlobalSearchDialogState(query: string) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const [showAll, setShowAll] = useState(false);
  const [openMenu, setOpenMenu] = useState<OpenSearchMenu | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  useEffect(() => {
    setActiveIndex(0);
    setShowAll(false);
    setOpenMenu(null);
  }, [query]);

  const toggleShowAll = useCallback(() => {
    setShowAll((current) => !current);
  }, []);

  const toggleMenu = useCallback((menu: OpenSearchMenu) => {
    setOpenMenu((current) => (current?.rowId === menu.rowId ? null : menu));
  }, []);

  const closeMenu = useCallback(() => {
    setOpenMenu(null);
  }, []);

  useEffect(() => {
    if (!openMenu) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest('.track-actions-menu, .global-search-menu-button')) return;
      setOpenMenu(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      setOpenMenu(null);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown, true);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown, true);
    };
  }, [openMenu]);

  return {
    activeIndex,
    closeMenu,
    inputRef,
    openMenu,
    setActiveIndex,
    showAll,
    toggleShowAll,
    toggleMenu
  };
}
