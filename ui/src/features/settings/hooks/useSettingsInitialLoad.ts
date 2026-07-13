import { useEffect } from 'react';

type UseSettingsInitialLoadParams = {
  reloadEq: () => void;
  reloadQobuzCache: () => void;
};

export function useSettingsInitialLoad({
  reloadEq,
  reloadQobuzCache
}: UseSettingsInitialLoadParams) {
  useEffect(() => {
    reloadQobuzCache();
    reloadEq();
  }, [reloadEq, reloadQobuzCache]);
}
