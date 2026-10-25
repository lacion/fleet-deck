// Wall apps (C2): the feed of an app on a wall. One read of the deck route at once, then one more
// 5 s after each answer. A failed read keeps the last good answer.
import { useEffect, useRef, useState } from 'react';
import { appFeedPath } from '../display/api';
import type { WallAppFeed } from '../gen';
import type { DisplayEnv } from './displayChanges';

export const FEED_INTERVAL_MS = 5_000;

/** Validates the answer of the feed route. */
export function wallAppFeed(v: unknown): WallAppFeed {
  if (
    typeof v !== 'object' ||
    v === null ||
    typeof (v as Record<string, unknown>)['at'] !== 'number' ||
    !Array.isArray((v as Record<string, unknown>)['sessions'])
  ) {
    throw new Error('The deck sent an invalid app feed.');
  }
  return v as WallAppFeed;
}

export type FeedTimers = {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
};

const realTimers: FeedTimers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/**
 * Reads the feed at once and then `FEED_INTERVAL_MS` after each answer, one request at a time.
 * `onData` gets each good answer; a failed read changes nothing. Returns the stop function.
 */
export function pollFeed(
  get: (path: string) => Promise<unknown>,
  path: string,
  onData: (feed: WallAppFeed) => void,
  timers: FeedTimers = realTimers,
): () => void {
  let live = true;
  let timer: unknown = null;
  const run = async (): Promise<void> => {
    timer = null;
    try {
      const feed = wallAppFeed(await get(path));
      if (live) onData(feed);
    } catch {
      // The last good answer stays.
    }
    if (live) timer = timers.set(() => void run(), FEED_INTERVAL_MS);
  };
  void run();
  return () => {
    live = false;
    if (timer !== null) timers.clear(timer);
  };
}

/** The feed of the app of one artifact version; `undefined` until the first good answer. */
export function useAppFeed(
  env: DisplayEnv | undefined,
  station: string,
  id: string,
  version: number,
  enabled: boolean,
): WallAppFeed | undefined {
  const [feed, setFeed] = useState<WallAppFeed | undefined>(undefined);
  const latest = useRef(env);
  latest.current = env;
  const has = env !== undefined;
  useEffect(() => {
    if (!has || !enabled) return undefined;
    setFeed(undefined);
    return pollFeed(
      (path) =>
        latest.current === undefined
          ? Promise.reject(new Error('no env'))
          : latest.current.get(path),
      appFeedPath(station, id, version),
      setFeed,
    );
  }, [has, enabled, station, id, version]);
  return feed;
}
