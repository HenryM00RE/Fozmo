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
