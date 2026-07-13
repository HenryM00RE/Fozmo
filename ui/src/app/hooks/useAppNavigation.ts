import { type Dispatch, type SetStateAction, useCallback, useEffect, useState } from 'react';
import { primaryArtistName } from '../../shared/lib/appSupport';
import { routeFromHash, routeToHash } from '../../shared/lib/route';
import type { RouteState } from '../../shared/types';

type UseAppNavigationParams = {
  setNowPlayingOpen: Dispatch<SetStateAction<boolean>>;
};

export function useAppNavigation({ setNowPlayingOpen }: UseAppNavigationParams) {
  const [route, setRoute] = useState<RouteState>(() => routeFromHash(window.location.hash));

  useEffect(() => {
    const onHash = () => setRoute(routeFromHash(window.location.hash));
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  const navigate = useCallback((next: RouteState) => {
    const hash = routeToHash(next);
    if (window.location.hash !== hash) window.history.pushState(null, '', hash);
    setRoute(next);
  }, []);

  const openArtistName = useCallback(
    (rawName: unknown) => {
      const artistName = primaryArtistName(rawName);
      if (!artistName) return;
      setNowPlayingOpen(false);
      navigate({ view: 'artist', id: artistName });
    },
    [navigate, setNowPlayingOpen]
  );

  return {
    navigate,
    openArtistName,
    route,
    setRoute
  };
}
