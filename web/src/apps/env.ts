// B15 (design `docs/v2/design/mcp-apps.md` §4.8.2): what the chat needs to host apps. `appsEnvOf` is
// a pure function (not a hook): `SessionView` calls it with two lines. The calls of the deck are
// registered once by `main.tsx` (`registerAppActions`): `SessionActions` has no deck transport.
import type { DeckSession } from '../gen';
import type { SessionActions } from '../session/commands';
import type { AppActions } from './actions';

/** What the chat gives to its app rows and to the open-calls strip. */
export type AppsEnv = {
  readonly station: string;
  readonly session: string;
  /** False for a member of a shared session: no frame, no strip. */
  readonly owner: boolean;
  readonly actions: AppActions;
  sendMessage(text: string): Promise<void>;
  /** Changes when the revert of the session changes: the strip asks the list route again. */
  readonly revertKey: string | null;
  /** The strip asks the list route again (after an app call that did not end with `done`). */
  refreshCalls(): void;
};

let registered: AppActions | null = null;

/** `main.tsx` registers the app calls over the deck transport; `null` turns apps off (tests). */
export function registerAppActions(actions: AppActions | null): void {
  registered = actions;
}

const versions = new Map<string, number>();
const listeners = new Map<string, Set<() => void>>();
const keyOf = (station: string, session: string): string => `${station}/${session}`;

/** The counter of `station/session`: it grows when `refreshCalls` of that session runs. */
export function callsVersion(station: string, session: string): number {
  return versions.get(keyOf(station, session)) ?? 0;
}

/** Calls `onChange` after each bump of the counter of `station/session`; returns the stop. */
export function subscribeCalls(station: string, session: string, onChange: () => void): () => void {
  const key = keyOf(station, session);
  const set = listeners.get(key) ?? new Set();
  set.add(onChange);
  listeners.set(key, set);
  return () => {
    set.delete(onChange);
  };
}

/** Bumps the counter of `station/session` and tells the strip. */
export function refreshCalls(station: string, session: string): void {
  const key = keyOf(station, session);
  versions.set(key, (versions.get(key) ?? 0) + 1);
  for (const fn of listeners.get(key) ?? []) fn();
}

/**
 * The apps environment of a session: `guest` is the member of a shared session (`null` for the
 * owner). Without registered actions there is no environment and no app row.
 */
export function appsEnvOf(
  session: DeckSession,
  actions: SessionActions | null,
  guest: unknown,
): AppsEnv | undefined {
  if (registered === null) return undefined;
  const station = session.station_id;
  const id = session.summary.session_id;
  const revert = session.summary.revert;
  return {
    station,
    session: id,
    owner: guest === null,
    actions: registered,
    sendMessage: async (text) => {
      if (actions === null) throw new Error('The session is not available.');
      const kind = session.summary.current_run !== null ? 'follow_up' : 'prompt';
      const outcome = await actions.send(kind, text);
      if (!outcome.ok) throw new Error(outcome.text);
    },
    revertKey: revert === null ? null : `${revert.revert_id}:${revert.state}:${revert.reason}`,
    refreshCalls: () => refreshCalls(station, id),
  };
}

/** At most `max` frames are mounted in one chat; the first mounted one gives way to a new one. */
export type FrameSlots = {
  /** Claims a slot for `key`; the oldest claim is evicted (its `onEvict` runs) over the limit. */
  claim(key: string, onEvict: () => void): void;
  release(key: string): void;
  count(): number;
};

export function createFrameSlots(max = 3): FrameSlots {
  const order: { key: string; onEvict: () => void }[] = [];
  return {
    claim(key, onEvict) {
      const at = order.findIndex((s) => s.key === key);
      if (at >= 0) order.splice(at, 1);
      order.push({ key, onEvict });
      while (order.length > max) order.shift()?.onEvict();
    },
    release(key) {
      const at = order.findIndex((s) => s.key === key);
      if (at >= 0) order.splice(at, 1);
    },
    count: () => order.length,
  };
}
