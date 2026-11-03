import { useEffect, useState } from 'react';

/** Hash routes keep the build a set of static files that any server can host. */
export type Route =
  | 'ops'
  | 'video'
  | 'transport'
  | 'recording'
  | 'network'
  | 'logs'
  | 'device'
  | 'fullscreen';

const PATHS: Record<Route, string> = {
  ops: '#/',
  video: '#/settings/video',
  transport: '#/settings/transport',
  recording: '#/settings/recording',
  network: '#/admin/network',
  logs: '#/admin/logs',
  device: '#/admin/device',
  fullscreen: '#/fullscreen',
};

export const href = (r: Route) => PATHS[r];

const parse = (hash: string): Route => {
  const found = (Object.keys(PATHS) as Route[]).find((r) => PATHS[r] === hash);
  return found ?? 'ops';
};

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parse(location.hash || '#/'));
  useEffect(() => {
    const on = () => setRoute(parse(location.hash || '#/'));
    addEventListener('hashchange', on);
    return () => removeEventListener('hashchange', on);
  }, []);
  return route;
}

export const go = (r: Route) => {
  location.hash = PATHS[r];
};
