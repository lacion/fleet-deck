// R13 P5a task F (design `omarchy-p5a.md` §6.4, §7.2, §7.3): the flow of one screenshot as a pure
// controller. Time, timers, storage, the ID source and the calls are injected, so that the tests
// drive it with fake timers. The rules:
// - the ID is stored before the one POST and cleared at a terminal state or at "Dismiss";
// - the POST is never repeated: a timeout, a network error or a reload only polls the lookup;
// - the lookup runs every 2 seconds for 60 seconds, then the owner sees "Check result".
import type { ArtifactPublished, CaptureStatus } from '../gen';
import { DeckError, describe } from '../settings/http';
import type { CaptureActions } from './capture';
import { newUlid } from './ulid';

export const POLL_MS = 2_000;
export const POLL_WINDOW_MS = 60_000;

export const WAITING_TEXT = 'Waiting for the screenshot…';
export const UNKNOWN_TEXT =
  'The screenshot result is unknown. Check the artifact list before taking another screenshot.';
export const DELETED_TEXT = 'The screenshot was deleted.';
export const SAVED_TEXT =
  'The screenshot was saved on the station. The artifact list is still updating.';

export type FlowState =
  | { readonly kind: 'idle' }
  | { readonly kind: 'taking'; readonly monitor: string; readonly captureId: string }
  | {
      readonly kind: 'waiting';
      readonly monitor: string;
      readonly captureId: string;
      /** The POST answered `202 pending`: the capture is on the station, the deck is still indexing. */
      readonly after202: boolean;
    }
  | {
      readonly kind: 'unknown';
      readonly monitor: string;
      readonly captureId: string;
      readonly checking: boolean;
    }
  | {
      readonly kind: 'committed';
      readonly published: ArtifactPublished;
      readonly after202: boolean;
    }
  | { readonly kind: 'failed'; readonly text: string }
  | { readonly kind: 'deleted' };

export const IDLE: FlowState = { kind: 'idle' };

/** A capture that is not finished: the button is off and no second capture may start. */
export const isActive = (s: FlowState): boolean =>
  s.kind === 'taking' || s.kind === 'waiting' || s.kind === 'unknown';

export type Timers = {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
};

export type KeyStore = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

export type FlowDeps = {
  readonly stationName: string;
  readonly stationId: string;
  readonly actions: Pick<CaptureActions, 'take' | 'status'>;
  /** `null`: no storage; the flow then lives for the current page only. */
  readonly storage: KeyStore | null;
  readonly now?: () => number;
  readonly timers?: Timers;
  readonly newId?: () => string;
};

type Stored = { readonly captureId: string; readonly monitor: string; readonly at: number };

export const storageKey = (stationId: string): string => `fd.capture.${stationId}`;

/** The pending capture of a station in `sessionStorage`, or `null`. Never throws. */
export function readPending(storage: KeyStore | null, stationId: string): Stored | null {
  try {
    const raw = storage?.getItem(storageKey(stationId));
    if (raw === null || raw === undefined) return null;
    const v: unknown = JSON.parse(raw);
    if (
      typeof v === 'object' &&
      v !== null &&
      'captureId' in v &&
      'monitor' in v &&
      'at' in v &&
      typeof v.captureId === 'string' &&
      typeof v.monitor === 'string' &&
      typeof v.at === 'number'
    ) {
      return { captureId: v.captureId, monitor: v.monitor, at: v.at };
    }
  } catch {
    // Storage blocked or the value is not JSON: there is no pending capture to resume.
  }
  return null;
}

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/**
 * The POST failed with a fixed deck error that says the capture did not finish. The deck timeout
 * (`504 desktop_timeout`, "the action may have run") is not one: it only leads to the lookup.
 */
const definite = (e: unknown): boolean =>
  e instanceof DeckError && e.status !== 504 && e.code !== 'desktop_timeout';

export class CaptureFlow {
  private state: FlowState = IDLE;
  private readonly listeners = new Set<() => void>();
  private timer: unknown = null;
  private since = 0;
  private attached = false;
  private readonly now: () => number;
  private readonly timers: Timers;
  private readonly newId: () => string;

  constructor(private readonly deps: FlowDeps) {
    this.now = deps.now ?? Date.now;
    this.timers = deps.timers ?? defaultTimers;
    this.newId = deps.newId ?? (() => newUlid(this.now()));
  }

  readonly subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  readonly getSnapshot = (): FlowState => this.state;

  /** The page shows the flow: resume a stored capture, or poll again after a `detach`. */
  attach(): void {
    this.attached = true;
    if (this.state.kind === 'idle') {
      const stored = readPending(this.deps.storage, this.deps.stationId);
      if (stored !== null) this.toWaiting(stored.monitor, stored.captureId, false);
    } else if (this.state.kind === 'waiting') {
      this.schedule(POLL_MS);
    }
  }

  /** The page goes away. The stored ID stays, so that the next page resumes. */
  detach(): void {
    this.attached = false;
    this.clearTimer();
  }

  /** The one POST of a capture. A capture that is active is not started again. */
  async start(monitor: string): Promise<void> {
    if (isActive(this.state)) return;
    const captureId = this.newId();
    this.write({ captureId, monitor, at: this.now() });
    this.set({ kind: 'taking', monitor, captureId });
    try {
      const r = await this.deps.actions.take(this.deps.stationName, monitor, captureId);
      if (r.kind === 'done') {
        const capture = r.result.capture;
        this.erase();
        if (capture === undefined) {
          this.set({ kind: 'failed', text: 'The deck sent an invalid desktop answer.' });
        } else {
          this.set({ kind: 'committed', published: capture.published, after202: false });
        }
      } else if (r.kind === 'deleted') {
        this.erase();
        this.set({ kind: 'deleted' });
      } else if (r.kind === 'unknown') {
        this.set({ kind: 'unknown', monitor, captureId, checking: false });
      } else {
        this.toWaiting(monitor, captureId, true);
      }
    } catch (e) {
      if (definite(e)) {
        this.erase();
        this.set({ kind: 'failed', text: describe(e) });
      } else {
        // A timeout or a network error: the capture may have run. Only the lookup may follow.
        this.toWaiting(monitor, captureId, false);
      }
    }
  }

  /** "Check result": one more lookup of an unknown capture. */
  async check(): Promise<void> {
    const s = this.state;
    if (s.kind !== 'unknown' || s.checking) return;
    this.set({ ...s, checking: true });
    let status: CaptureStatus | null = null;
    try {
      status = await this.deps.actions.status(this.deps.stationName, s.captureId);
    } catch {
      status = null;
    }
    if (this.state.kind !== 'unknown') return;
    if (status === null || !this.apply(status, false)) this.set({ ...s, checking: false });
  }

  /** "Dismiss": forget an unknown capture. */
  dismiss(): void {
    this.clearTimer();
    this.erase();
    this.set(IDLE);
  }

  private toWaiting(monitor: string, captureId: string, after202: boolean): void {
    this.since = this.now();
    this.set({ kind: 'waiting', monitor, captureId, after202 });
    this.schedule(POLL_MS);
  }

  private schedule(delay: number): void {
    if (!this.attached) return;
    this.clearTimer();
    this.timer = this.timers.set(() => {
      this.timer = null;
      void this.poll();
    }, delay);
  }

  private async poll(): Promise<void> {
    const s = this.state;
    if (s.kind !== 'waiting') return;
    let status: CaptureStatus | null = null;
    try {
      status = await this.deps.actions.status(this.deps.stationName, s.captureId);
    } catch {
      // A failed lookup is not an answer: the next one decides, or the 60 seconds end.
      status = null;
    }
    if (this.state !== s) return;
    if (status !== null && this.apply(status, s.after202)) return;
    if (this.now() - this.since >= POLL_WINDOW_MS) {
      this.set({ kind: 'unknown', monitor: s.monitor, captureId: s.captureId, checking: false });
    } else {
      this.schedule(POLL_MS);
    }
  }

  /** Applies a lookup answer; `true` when it ended the wait. */
  private apply(status: CaptureStatus, after202: boolean): boolean {
    const prev = this.state;
    switch (status.state) {
      case 'committed':
        this.erase();
        this.set({ kind: 'committed', published: status.published, after202 });
        return true;
      case 'failed':
        this.erase();
        this.set({ kind: 'failed', text: status.message });
        return true;
      case 'deleted':
        this.erase();
        this.set({ kind: 'deleted' });
        return true;
      case 'unknown':
        if (prev.kind === 'waiting') {
          this.set({
            kind: 'unknown',
            monitor: prev.monitor,
            captureId: prev.captureId,
            checking: false,
          });
        }
        return prev.kind === 'waiting';
      case 'pending':
        return false;
    }
  }

  private set(next: FlowState): void {
    this.state = next;
    if (!isActive(next)) this.clearTimer();
    for (const fn of [...this.listeners]) fn();
  }

  private clearTimer(): void {
    if (this.timer !== null) this.timers.clear(this.timer);
    this.timer = null;
  }

  private write(v: Stored): void {
    try {
      this.deps.storage?.setItem(storageKey(this.deps.stationId), JSON.stringify(v));
    } catch {
      // Storage blocked: the flow works for this page only.
    }
  }

  private erase(): void {
    try {
      this.deps.storage?.removeItem(storageKey(this.deps.stationId));
    } catch {
      // Storage blocked.
    }
  }
}
