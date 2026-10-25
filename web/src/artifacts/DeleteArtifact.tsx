// R13 P5a task F (design `omarchy-p5a.md` §3.2, §3.3): the owner deletes an artifact from the
// viewer. "Delete artifact" asks for a confirmation and the elevation, then sends `DELETE` with
// the elevation in the header. `200` leaves the viewer; `202` keeps it, hides the artifact from the
// lists and polls `GET …/deletion` every 5 seconds for up to 60 seconds.
import { type ReactNode, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import { type DeckHttp, describe } from '../settings/http';
import { useSettings } from '../settings/context';
import { useFleetElevation } from '../spawn/useFleetElevation';
import type { Timers } from '../desktop/flow';

export const DELETE_LABEL = 'Delete artifact';
export const CONFIRM_TEXT =
  'Delete this artifact and all its versions? Downloaded copies and provider conversations will remain.';
export const PENDING_TEXT = 'Hidden on this deck. Waiting for station deletion.';
export const UNSUPPORTED_TEXT = 'Hidden on this deck. Upgrade the station to delete its copy.';
export const DELETED_TEXT = 'Deleted.';

export const POLL_MS = 5_000;
export const POLL_WINDOW_MS = 60_000;

export type DeleteReason = 'offline' | 'unsupported' | 'retry';

/** The state body of `DELETE` and of `GET …/deletion` (§3.2). */
export type DeleteState =
  | { readonly state: 'deleted' }
  | { readonly state: 'pending'; readonly reason: DeleteReason };

export type DeleteActions = {
  /** `DELETE /api/artifacts/{station}/{id}` with the elevation in `X-FleetDeck-Elevation`. */
  remove(station: string, id: string, elevation: string): Promise<DeleteState>;
  /** `GET /api/artifacts/{station}/{id}/deletion`. */
  state(station: string, id: string): Promise<DeleteState>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const seg = encodeURIComponent;
const base = (station: string, id: string) => `/api/artifacts/${seg(station)}/${seg(id)}`;

function parse(v: unknown): DeleteState {
  if (record(v) && v['state'] === 'deleted') return { state: 'deleted' };
  if (
    record(v) &&
    v['state'] === 'pending' &&
    (v['reason'] === 'offline' || v['reason'] === 'unsupported' || v['reason'] === 'retry')
  ) {
    return { state: 'pending', reason: v['reason'] };
  }
  throw new Error('The deck sent an invalid deletion answer.');
}

export function deleteActions(io: DeckHttp): DeleteActions {
  return {
    remove: async (station, id, elevation) =>
      parse(await io.del(base(station, id), { 'X-FleetDeck-Elevation': elevation })),
    state: async (station, id) => parse(await io.get(`${base(station, id)}/deletion`)),
  };
}

/** The text of a state that keeps the viewer open. */
export const stateText = (s: DeleteState): string =>
  s.state === 'deleted'
    ? DELETED_TEXT
    : s.reason === 'unsupported'
      ? UNSUPPORTED_TEXT
      : PENDING_TEXT;

// The artifacts that this page hid after a deletion; the lists filter them out at once.
const hiddenKeys = new Set<string>();
let hiddenSnapshot: ReadonlySet<string> = new Set();
const hiddenListeners = new Set<() => void>();

export const artifactKey = (station: string, id: string): string => `${station}/${id}`;

export function hideArtifact(station: string, id: string): void {
  hiddenKeys.add(artifactKey(station, id));
  hiddenSnapshot = new Set(hiddenKeys);
  for (const fn of [...hiddenListeners]) fn();
}

/** Forgets every hidden artifact (tests). */
export function resetHidden(): void {
  hiddenKeys.clear();
  hiddenSnapshot = new Set();
  for (const fn of [...hiddenListeners]) fn();
}

const subscribeHidden = (fn: () => void): (() => void) => {
  hiddenListeners.add(fn);
  return () => {
    hiddenListeners.delete(fn);
  };
};

/** The set of hidden artifacts (`station/id`); it changes identity when an artifact is hidden. */
export const useHiddenArtifacts = (): ReadonlySet<string> =>
  useSyncExternalStore(
    subscribeHidden,
    () => hiddenSnapshot,
    () => hiddenSnapshot,
  );

export type DeletionPhase =
  | { readonly kind: 'idle' }
  | { readonly kind: 'confirm' }
  | { readonly kind: 'working' }
  | { readonly kind: 'error'; readonly text: string }
  /** The deletion is pending or done on the deck, and the viewer stays open with this text. */
  | { readonly kind: 'hidden'; readonly state: DeleteState };

export const IDLE_PHASE: DeletionPhase = { kind: 'idle' };

const NEVER = () => () => {};

export type DeletionDeps = {
  readonly station: string;
  readonly id: string;
  readonly actions: DeleteActions;
  /** "Confirm it is you": the elevation, or `null` when the owner cancels. */
  readonly ask: () => Promise<string | null>;
  /** The viewer goes back to the list (after `200`). */
  readonly leave: () => void;
  readonly hide?: (station: string, id: string) => void;
  readonly now?: () => number;
  readonly timers?: Timers;
};

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/** The flow of one deletion as a controller; the tests drive it with fake timers. */
export class DeletionFlow {
  private phase: DeletionPhase = IDLE_PHASE;
  private readonly listeners = new Set<() => void>();
  private timer: unknown = null;
  private since = 0;
  private gone = false;
  private readonly now: () => number;
  private readonly timers: Timers;

  constructor(private readonly deps: DeletionDeps) {
    this.now = deps.now ?? Date.now;
    this.timers = deps.timers ?? defaultTimers;
  }

  readonly subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  readonly getSnapshot = (): DeletionPhase => this.phase;

  /** "Delete artifact": the confirmation opens. */
  begin(): void {
    if (this.phase.kind === 'idle' || this.phase.kind === 'error') this.set({ kind: 'confirm' });
  }

  cancel(): void {
    if (this.phase.kind === 'confirm') this.set(IDLE_PHASE);
  }

  /** The owner confirmed: the elevation, then the one `DELETE`. A cancelled elevation sends nothing. */
  async confirm(): Promise<void> {
    if (this.phase.kind !== 'confirm') return;
    this.set({ kind: 'working' });
    try {
      const elevation = await this.deps.ask();
      if (elevation === null) {
        this.set(IDLE_PHASE);
        return;
      }
      const answer = await this.deps.actions.remove(this.deps.station, this.deps.id, elevation);
      (this.deps.hide ?? hideArtifact)(this.deps.station, this.deps.id);
      if (answer.state === 'deleted') {
        this.deps.leave();
        return;
      }
      this.set({ kind: 'hidden', state: answer });
      this.since = this.now();
      this.schedule();
    } catch (e) {
      this.set({ kind: 'error', text: describe(e) });
    }
  }

  /** The viewer goes away: the polling stops. */
  dispose(): void {
    this.gone = true;
    this.clear();
  }

  private schedule(): void {
    if (this.gone) return;
    this.clear();
    this.timer = this.timers.set(() => {
      this.timer = null;
      void this.poll();
    }, POLL_MS);
  }

  private async poll(): Promise<void> {
    if (this.gone || this.phase.kind !== 'hidden') return;
    try {
      const answer = await this.deps.actions.state(this.deps.station, this.deps.id);
      if (this.gone) return;
      this.set({ kind: 'hidden', state: answer });
      if (answer.state === 'deleted') return;
    } catch {
      // A failed read is not an answer: the next one decides, or the 60 seconds end.
    }
    if (this.now() - this.since < POLL_WINDOW_MS) this.schedule();
  }

  private set(next: DeletionPhase): void {
    this.phase = next;
    for (const fn of [...this.listeners]) fn();
  }

  private clear(): void {
    if (this.timer !== null) this.timers.clear(this.timer);
    this.timer = null;
  }
}

/** What `ArtifactViewerView` needs to show the deletion. */
export type DeletionView = {
  readonly phase: DeletionPhase;
  readonly onBegin: () => void;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
};

/**
 * The deletion of one artifact for the viewer; `undefined` when the login is not the owner's or
 * not an admin. `leave` goes back to the list.
 */
export function useDeletion(
  station: string,
  id: string,
  allowed: boolean,
  leave: () => void,
): { readonly view: DeletionView | undefined; readonly node: ReactNode } {
  const env = useSettings();
  const { ask, node } = useFleetElevation(env);
  const latest = useRef({ ask, leave });
  latest.current = { ask, leave };
  const deletion = env?.deletion;
  const flow = useMemo(
    () =>
      deletion === undefined || !allowed
        ? null
        : new DeletionFlow({
            station,
            id,
            actions: deletion,
            ask: () => latest.current.ask([DELETE_LABEL]),
            leave: () => latest.current.leave(),
          }),
    [deletion, allowed, station, id],
  );
  useEffect(() => () => flow?.dispose(), [flow]);
  const phase = useSyncExternalStore(
    flow === null ? NEVER : flow.subscribe,
    flow === null ? () => IDLE_PHASE : flow.getSnapshot,
    () => IDLE_PHASE,
  );
  if (flow === null) return { view: undefined, node };
  return {
    view: {
      phase,
      onBegin: () => flow.begin(),
      onConfirm: () => void flow.confirm(),
      onCancel: () => flow.cancel(),
    },
    node,
  };
}
