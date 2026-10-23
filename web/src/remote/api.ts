// B17 §3.3.3: the calls, the texts and the link of the Remote page. `RemoteLink` polls the deck, sends
// one action at a time and gives the mirror state. Time, timers and the calls are injected, so that
// the tests drive it with fake timers (the same shape as `CaptureFlow`).
import type { DisplayActions } from '../displays/actions';
import type { Timers } from '../desktop/flow';
import type { DirectiveBody, WallControlAck, WallLink, WallRemoteView } from '../gen';
import { DeckError, describe } from '../settings/http';
import { applyView, initialMirror, type MirrorState, type PadAction } from './mirror';

/** The feature token of the deck that has the wall remote (`GET /api/features`). */
export const REMOTE_FEATURE = 'wall_control.v1';

/** The wait between two polls while the page is visible, and while it is hidden. */
export const POLL_VISIBLE_MS = 1_000;
export const POLL_HIDDEN_MS = 10_000;
/** A status text clears after this time. */
export const STATUS_CLEAR_MS = 4_000;

export const NO_REMOTE_TEXT = 'This deck has no wall remote.';
export const OWNER_ONLY_TEXT = 'The remote needs the owner login.';
export const SENT_TEXT = 'Sent.';
export const OFFLINE_TEXT = 'The wall is not connected.';
export const OLDER_TEXT = 'This wall runs an older board. Reload the wall to use the remote.';
export const TOO_MANY_TEXT = 'Too many controls. Wait a moment.';
export const GONE_TEXT = 'This display does not exist any more.';
export const CANNOT_SHOW_TEXT = 'The wall cannot show this.';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const LINKS: readonly WallLink[] = ['offline', 'older', 'ready'];

/** Validates the answer of `GET /api/displays/{id}/remote`. */
export function remoteView(v: unknown): WallRemoteView {
  if (
    !record(v) ||
    !record(v['directive']) ||
    typeof v['directive']['generation'] !== 'number' ||
    !record(v['directive']['directive']) ||
    typeof v['directive']['directive']['mode'] !== 'string' ||
    !LINKS.includes(v['link'] as WallLink) ||
    typeof v['incarnation'] !== 'string' ||
    typeof v['seq'] !== 'number' ||
    !record(v['state']) ||
    typeof v['state']['frame_tenths'] !== 'number' ||
    !(v['state']['selected'] === null || record(v['state']['selected'])) ||
    typeof v['hold_left_ms'] !== 'number' ||
    typeof v['complete'] !== 'boolean' ||
    !Array.isArray(v['controls'])
  ) {
    throw new Error('The deck sent an invalid remote state.');
  }
  return v as WallRemoteView;
}

/** Validates the answer of a control or a navigation. */
export function controlAck(v: unknown): WallControlAck {
  if (
    !record(v) ||
    typeof v['incarnation'] !== 'string' ||
    typeof v['seq'] !== 'number' ||
    typeof v['generation'] !== 'number'
  ) {
    throw new Error('The deck sent an invalid answer.');
  }
  return v as WallControlAck;
}

/** The text of a failed call of the remote (§3.3.3, "Status texts"). */
export function failureText(e: unknown): string {
  if (e instanceof DeckError) {
    if (e.status === 409 && e.code === 'wall_offline') return OFFLINE_TEXT;
    if (e.status === 409 && e.code === 'wall_controls_unsupported') return OLDER_TEXT;
    if (e.status === 429) return TOO_MANY_TEXT;
    if (e.status === 404) return GONE_TEXT;
    if (e.status === 400 || (e.status === 403 && e.code === 'outside_ceiling')) {
      return CANNOT_SHOW_TEXT;
    }
  }
  return describe(e);
}

/** The calls of the remote: the three of the deck, and the built directive of "Switch to Mission". */
export type RemoteCalls = Pick<
  DisplayActions,
  'remote' | 'control' | 'remoteNavigate' | 'directive'
>;

/** What the footer and the pad send. */
export type RemoteAction =
  | PadAction
  | { readonly kind: 'switch'; readonly directive: DirectiveBody };

export type Status = { readonly text: string; readonly tone: 'ok' | 'error' };

export type LinkSnapshot = {
  /** The last view of the deck (`null` before the first). */
  readonly view: WallRemoteView | null;
  readonly mirror: MirrorState;
  /** An action is in flight: a press does nothing. */
  readonly busy: boolean;
  readonly status: Status | null;
  /** The text of the last failed poll (`null` when the last poll worked). */
  readonly problem: string | null;
};

const FIRST: LinkSnapshot = {
  view: null,
  mirror: initialMirror,
  busy: false,
  status: null,
  problem: null,
};

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

export type LinkDeps = {
  readonly id: string;
  readonly calls: RemoteCalls;
  readonly now?: () => number;
  readonly timers?: Timers;
};

/**
 * The link to one wall display. It polls `GET /api/displays/{id}/remote?after=<cursor>` (every 1 s
 * while visible, every 10 s while hidden), one poll at a time. A press sends one action and
 * never repeats it; a press while an action is in flight does nothing. The mirror applies nothing
 * from an answer: after each answer (a success or an error) the link polls at once, and the
 * mirror state changes only from polls, in the order of the deck.
 */
export class RemoteLink {
  private snap: LinkSnapshot = FIRST;
  private readonly listeners = new Set<() => void>();
  private live = false;
  private visible = true;
  private polling = false;
  private again = false;
  private pollTimer: unknown = null;
  private statusTimer: unknown = null;
  private readonly now: () => number;
  private readonly timers: Timers;

  constructor(private readonly deps: LinkDeps) {
    this.now = deps.now ?? Date.now;
    this.timers = deps.timers ?? defaultTimers;
  }

  readonly subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  readonly getSnapshot = (): LinkSnapshot => this.snap;

  private set(next: Partial<LinkSnapshot>): void {
    this.snap = { ...this.snap, ...next };
    for (const fn of [...this.listeners]) fn();
  }

  /** The page shows the remote: poll at once and keep polling. */
  start(): void {
    this.live = true;
    void this.poll();
  }

  stop(): void {
    this.live = false;
    this.clearPoll();
    if (this.statusTimer !== null) this.timers.clear(this.statusTimer);
    this.statusTimer = null;
  }

  /** The page is visible or hidden: the wait between polls changes, and a return polls at once. */
  setVisible(visible: boolean): void {
    if (visible === this.visible) return;
    this.visible = visible;
    if (!this.live) return;
    if (visible) void this.poll();
    else if (!this.polling) this.schedule();
  }

  private clearPoll(): void {
    if (this.pollTimer !== null) this.timers.clear(this.pollTimer);
    this.pollTimer = null;
  }

  private schedule(): void {
    this.clearPoll();
    if (!this.live) return;
    this.pollTimer = this.timers.set(
      () => {
        this.pollTimer = null;
        void this.poll();
      },
      this.visible ? POLL_VISIBLE_MS : POLL_HIDDEN_MS,
    );
  }

  /** One poll. A poll while another is in flight asks for one more as soon as the first ends. */
  async poll(): Promise<void> {
    if (!this.live) return;
    if (this.polling) {
      this.again = true;
      return;
    }
    this.polling = true;
    this.clearPoll();
    try {
      const view = await this.deps.calls.remote(this.deps.id, this.snap.mirror.cursor);
      if (this.live) {
        this.set({
          view,
          mirror: applyView(this.snap.mirror, view, this.now()),
          problem: null,
        });
      }
    } catch (e) {
      if (this.live) this.set({ problem: failureText(e) });
    } finally {
      this.polling = false;
    }
    if (!this.live) return;
    if (this.again) {
      this.again = false;
      void this.poll();
    } else {
      this.schedule();
    }
  }

  private setStatus(status: Status): void {
    if (this.statusTimer !== null) this.timers.clear(this.statusTimer);
    this.set({ status });
    this.statusTimer = this.timers.set(() => {
      this.statusTimer = null;
      this.set({ status: null });
    }, STATUS_CLEAR_MS);
  }

  /** Sends one action. Resolves when the answer came and the poll after it was asked for. */
  async press(action: RemoteAction): Promise<void> {
    if (!this.live || this.snap.busy) return;
    const { id, calls } = this.deps;
    this.set({ busy: true });
    let status: Status;
    try {
      if (action.kind === 'control') await calls.control(id, action.control);
      else if (action.kind === 'navigate') await calls.remoteNavigate(id, action.directive);
      else await calls.directive(id, action.directive);
      status = { text: SENT_TEXT, tone: 'ok' };
    } catch (e) {
      status = { text: failureText(e), tone: 'error' };
    }
    if (!this.live) return;
    this.set({ busy: false });
    this.setStatus(status);
    // The answer changes nothing in the mirror: the next view of the deck does.
    await this.poll();
  }
}
