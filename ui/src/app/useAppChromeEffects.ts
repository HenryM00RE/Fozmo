import { type Dispatch, type SetStateAction, useEffect } from 'react';
import type { ActiveSelectionType } from '../shared/ui/selectionToolbar';

type UseAppChromeEffectsParams = {
  activeSelectionType: ActiveSelectionType;
  albumSelectionActive: boolean;
  globalSearchSetOpen: Dispatch<SetStateAction<boolean>>;
  nowPlayingOpen: boolean;
  recentSelectionActive: boolean;
  setSignalOpen: Dispatch<SetStateAction<boolean>>;
  signalOpen: boolean;
};

export function useAppChromeEffects({
  activeSelectionType,
  albumSelectionActive,
  globalSearchSetOpen,
  nowPlayingOpen,
  recentSelectionActive,
  setSignalOpen,
  signalOpen
}: UseAppChromeEffectsParams) {
  useEffect(() => {
    document.body.classList.toggle('now-playing-open', nowPlayingOpen);
    return () => document.body.classList.remove('now-playing-open');
  }, [nowPlayingOpen]);

  useEffect(() => {
    document.body.classList.toggle('selection-mode', Boolean(activeSelectionType));
    document.body.classList.toggle('recently-played-selection-mode', recentSelectionActive);
    document.body.classList.toggle('album-track-selection-mode', albumSelectionActive);
    return () => {
      document.body.classList.remove('selection-mode');
      document.body.classList.remove('recently-played-selection-mode');
      document.body.classList.remove('album-track-selection-mode');
    };
  }, [activeSelectionType, albumSelectionActive, recentSelectionActive]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      if ((event.ctrlKey || event.metaKey) && key === 'k') {
        event.preventDefault();
        globalSearchSetOpen(true);
        return;
      }
      if (event.key === 'Escape') {
        globalSearchSetOpen(false);
        setSignalOpen(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [globalSearchSetOpen, setSignalOpen]);

  useEffect(() => {
    if (!signalOpen) return undefined;
    const handlePointerDown = (event: PointerEvent) => {
      if (!(event.target instanceof Element)) return;
      if (event.target.closest('.signal-popover, .signal-quality-trigger')) return;
      setSignalOpen(false);
    };
    document.addEventListener('pointerdown', handlePointerDown, true);
    return () => document.removeEventListener('pointerdown', handlePointerDown, true);
  }, [setSignalOpen, signalOpen]);
}
