// R11 (DL-166): which sessions the owner shares now. The People tab is always on for them, and a
// share with `mob` steering puts the mob strip on them.
import { useEffect, useMemo, useState } from 'react';
import type { ShareInfo, SpaceView } from '../gen';
import { sessionKey } from '../runtime/state';
import { ALL_SESSIONS } from './context';
import type { ShareCalls } from './people';
import { useSharesVersion } from './shareStore';

export type SharedSets = {
  readonly shared: ReadonlySet<string>;
  readonly mob: ReadonlySet<string>;
};

const NONE: SharedSets = { shared: new Set(), mob: new Set() };

/** The session keys that the live shares cover, and the ones that steer `mob`. */
export function sharedSets(
  shares: readonly ShareInfo[],
  spaces: readonly SpaceView[],
  now: number,
): SharedSets {
  const shared = new Set<string>();
  const mob = new Set<string>();
  for (const share of shares) {
    if (share.revoked_at !== null || share.expires_at <= now) continue;
    const keys: string[] = [];
    if (share.kind === 'board') keys.push(ALL_SESSIONS);
    else if (share.kind === 'session') {
      const [station = '', session = ''] = share.target.split('/');
      keys.push(sessionKey({ station_id: station, session_id: session }));
    } else {
      const space = spaces.find((s) => s.space_id === share.target);
      for (const key of space?.sessions ?? []) keys.push(sessionKey(key));
    }
    for (const key of keys) {
      shared.add(key);
      if (share.steering === 'mob') mob.add(key);
    }
  }
  return { shared, mob };
}

/** Loads the shares and the spaces of the owner; again after a change. A failed load keeps the last sets. */
export function useOwnerShares(calls: ShareCalls | undefined): SharedSets {
  const version = useSharesVersion();
  const [sets, setSets] = useState<SharedSets>(NONE);
  useEffect(() => {
    if (calls === undefined) return undefined;
    let live = true;
    void Promise.all([calls.shares(), calls.spaces()]).then(
      ([shares, spaces]) => {
        if (live) setSets(sharedSets(shares, spaces, Date.now()));
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [calls, version]);
  return useMemo(() => (calls === undefined ? NONE : sets), [calls, sets]);
}
