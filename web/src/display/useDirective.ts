// R13 P2c (design §5D item 3): a wall display polls `GET /api/displays/self/directive` every 2 s
// and moves the wall to the mode that the directive asks for. A directive applies once for each
// generation, and the first answer always applies (a reload restores the last directive).
import { useEffect, useRef, useState } from 'react';
import type { Navigate, Route } from '../app/routes';
import { wallDefaults, type WallRoute } from '../app/routes';
import { ApiError } from '../runtime/api';
import { directiveRoute, directiveView } from './api';

/** The wait between two reads of the directive. */
export const DIRECTIVE_POLL_MS = 2_000;

/**
 * One poll: the route that the directive asks for and its generation, or `null` when the
 * generation is the one that already applied.
 */
export async function directiveStep(
  get: (path: string) => Promise<unknown>,
  last: number | null,
  current: Route,
): Promise<{ readonly route: WallRoute; readonly generation: number } | null> {
  const view = directiveView(await get('/api/displays/self/directive'));
  if (view.generation === last) return null;
  const base: WallRoute = current.name === 'wall' ? current : wallDefaults;
  return { route: directiveRoute(view.directive, base), generation: view.generation };
}

/**
 * Polls the directive; one request at a time. Returns the generation that the wall shows (`null`
 * before the first answer). A 401 calls `onEnded` (the login ended or the display was revoked).
 * Another failure keeps the wall as it is.
 */
export function useDirective(
  get: (path: string) => Promise<unknown>,
  route: Route,
  navigate: Navigate,
  onEnded: () => void,
  pollMs: number = DIRECTIVE_POLL_MS,
): number | null {
  const [generation, setGeneration] = useState<number | null>(null);
  const latest = useRef({ route, navigate, onEnded, get });
  latest.current = { route, navigate, onEnded, get };
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let last: number | null = null;
    const tick = async () => {
      try {
        const step = await directiveStep(latest.current.get, last, latest.current.route);
        if (!live) return;
        if (step !== null) {
          last = step.generation;
          latest.current.navigate(step.route, { replace: true });
          setGeneration(step.generation);
        }
      } catch (e) {
        if (live && e instanceof ApiError && e.status === 401) latest.current.onEnded();
      }
      if (live) timer = setTimeout(() => void tick(), pollMs);
    };
    void tick();
    return () => {
      live = false;
      if (timer !== null) clearTimeout(timer);
    };
  }, [pollMs]);
  return generation;
}
