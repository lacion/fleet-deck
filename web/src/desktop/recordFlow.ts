// R13 P5b task W1 (design `omarchy-p5b.md` §4, §12; decisions O12-10 items 5 and 31): the flow of
// one monitor recording as a pure controller, in the pattern of `flow.ts`. Time, timers, storage,
// the ID source and the calls are injected, so that the tests drive it with fake timers. Rules:
// - the capture ID is stored (`fd.recording.<station>`) before the one POST, and cleared at a
//   terminal state;
// - the POST is never repeated: a timeout, a network error or a reload only polls the lookup;
// - the lookup runs every 2 seconds from the start for `seconds × 1000 + 45000` ms, then the owner
//   sees "Check result" (one manual lookup);
// - on open and on reload the deck says whether a recording holds the station slot;
// - "Stop recording" is available whenever an ID is known, whatever the capture setting says.
import type {
  ActiveRecording,
  ArtifactPublished,
  RecordingCaptureStatus,
  RecordingPhase,
} from '../gen';
import { DeckError, describe } from '../settings/http';
import type { CaptureActions } from './capture';
import { type KeyStore, POLL_MS, type Timers } from './flow';
import { ALLOWANCE_MS, MAX_SECONDS, MIN_SECONDS, recordingKey, windowMs } from './recording';
import { newUlid } from './ulid';

export { POLL_MS };

export type RecState =
  | { readonly kind: 'idle' }
  /** The deck is asked for an active recording; the start is off meanwhile. */
  | { readonly kind: 'discovering' }
  | {
      readonly kind: 'running';
      readonly monitor: string | null;
      readonly captureId: string;
      /** The phase of the last station answer; `null` before the first one. */
      readonly phase: RecordingPhase | null;
      /** The one POST has not answered yet. */
      readonly posting: boolean;
      /** The POST answered `202 pending`: the station has the file, the deck is still indexing. */
      readonly after202: boolean;
      readonly error: string | null;
    }
  /** The polling window ended: only "Check result" (and Stop) remain. */
  | {
      readonly kind: 'expired';
      readonly monitor: string | null;
      readonly captureId: string;
      readonly checking: boolean;
      readonly error: string | null;
    }
  /** The station is not connected (`409 station_unreachable`); the stored ID stays. */
  | {
      readonly kind: 'unreachable';
      readonly captureId: string | null;
      readonly error: string | null;
    }
  | {
      readonly kind: 'committed';
      readonly published: ArtifactPublished;
      readonly after202: boolean;
    }
  | { readonly kind: 'failed'; readonly text: string }
  | { readonly kind: 'deleted' };

export const IDLE: RecState = { kind: 'idle' };

/** A recording (or its discovery) that holds the start buttons off. */
export const isActive = (s: RecState): boolean => s.kind === 'running' || s.kind === 'discovering';

/** The capture ID that "Stop recording" would stop; `null` when no ID is known. */
export function recordingId(s: RecState): string | null {
  switch (s.kind) {
    case 'running':
    case 'expired':
      return s.captureId;
    case 'unreachable':
      return s.captureId;
    default:
      return null;
  }
}

export type RecordDeps = {
  readonly stationName: string;
  readonly stationId: string;
  readonly actions: Pick<CaptureActions, 'record' | 'status' | 'active' | 'stop'>;
  /** `null`: no storage; the flow then lives for the current page only. */
  readonly storage: KeyStore | null;
  readonly now?: () => number;
  readonly timers?: Timers;
  readonly newId?: () => string;
};

type Stored = {
  readonly captureId: string;
  readonly monitor: string;
  readonly seconds: number;
  readonly at: number;
};

/** The stored recording of a station, or `null`. Never throws. */
export function readStored(storage: KeyStore | null, stationId: string): Stored | null {
  try {
    const raw = storage?.getItem(recordingKey(stationId));
    if (raw === null || raw === undefined) return null;
    const v: unknown = JSON.parse(raw);
    if (
      typeof v === 'object' &&
      v !== null &&
      'captureId' in v &&
      'monitor' in v &&
      'seconds' in v &&
      'at' in v &&
      typeof v.captureId === 'string' &&
      typeof v.monitor === 'string' &&
      typeof v.seconds === 'number' &&
      typeof v.at === 'number'
    ) {
      return { captureId: v.captureId, monitor: v.monitor, seconds: v.seconds, at: v.at };
    }
  } catch {
    // Storage blocked or the value is not JSON: there is no stored recording.
  }
  return null;
}

const defaultTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/**
 * The POST failed with a fixed deck error that says the recording did not run. The deck timeout
 * (`504 desktop_timeout`, "the action may have run") is not one: it only leads to the lookup.
 */
const definite = (e: unknown): boolean =>
  e instanceof DeckError && e.status !== 504 && e.code !== 'desktop_timeout';

const unreachable = (e: unknown): boolean =>
  e instanceof DeckError && e.code === 'station_unreachable';

type Terminal = Extract<RecordingCaptureStatus, { state: 'committed' | 'failed' | 'deleted' }>;

const isTerminal = (s: RecordingCaptureStatus): s is Terminal =>
  s.state === 'committed' || s.state === 'failed' || s.state === 'deleted';

export class RecordFlow {
  private state: RecState = IDLE;
  private readonly listeners = new Set<() => void>();
  private timer: unknown = null;
  private since = 0;
  private window = 0;
  private attached = false;
  /** Changes at each `detach`: an answer of an earlier page is dropped. */
  private generation = 0;
  private readonly now: () => number;
  private readonly timers: Timers;
  private readonly newId: () => string;

  constructor(private readonly deps: RecordDeps) {
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

  readonly getSnapshot = (): RecState => this.state;

  /** The page shows the flow: ask the deck for an active recording, or poll again after `detach`. */
  attach(): void {
    this.attached = true;
    if (this.state.kind === 'idle') void this.discover();
    else if (this.state.kind === 'running') this.schedule(POLL_MS);
  }

  /** The page goes away. The stored ID stays, so that the next page resumes. */
  detach(): void {
    this.attached = false;
    this.generation += 1;
    this.clearTimer();
    if (this.state.kind === 'discovering') this.set(IDLE);
  }

  /**
   * The one POST of a recording. A recording that runs is not started again. `max` is the longest
   * recording of the station (`maxSeconds` of its desktop info); 120 when the caller has none.
   */
  async start(monitor: string, seconds: number, max: number = MAX_SECONDS): Promise<void> {
    if (isActive(this.state)) return;
    if (!Number.isInteger(seconds) || seconds < MIN_SECONDS || seconds > max) return;
    const captureId = this.newId();
    this.write({ captureId, monitor, seconds, at: this.now() });
    this.since = this.now();
    this.window = windowMs(seconds);
    this.set({
      kind: 'running',
      monitor,
      captureId,
      phase: null,
      posting: true,
      after202: false,
      error: null,
    });
    this.schedule(POLL_MS);
    let answer: Awaited<ReturnType<CaptureActions['record']>>;
    try {
      answer = await this.deps.actions.record(this.deps.stationName, monitor, seconds, captureId);
    } catch (e) {
      if (!this.same(captureId)) return;
      if (definite(e)) {
        this.erase();
        this.set({ kind: 'failed', text: describe(e) });
      } else {
        // A timeout or a network error: the recording may have run. Only the lookup may follow.
        this.settled(false);
      }
      return;
    }
    if (!this.same(captureId)) return;
    if (answer.kind === 'done') {
      const capture = answer.result.capture;
      this.erase();
      if (capture === undefined) {
        this.set({ kind: 'failed', text: 'The deck sent an invalid desktop answer.' });
      } else {
        this.set({ kind: 'committed', published: capture.published, after202: false });
      }
    } else if (answer.kind === 'deleted') {
      this.erase();
      this.set({ kind: 'deleted' });
    } else {
      this.settled(answer.kind === 'pending');
    }
  }

  /** "Stop recording": the owner stop of the known ID (§4.4); `200` ends it, `202` keeps polling. */
  async stop(): Promise<void> {
    const id = recordingId(this.state);
    if (id === null) return;
    let status: RecordingCaptureStatus;
    try {
      status = await this.deps.actions.stop(this.deps.stationName, id);
    } catch (e) {
      const cur = this.state;
      if (recordingId(cur) !== id) return;
      if (cur.kind === 'running' || cur.kind === 'expired' || cur.kind === 'unreachable') {
        this.set({ ...cur, error: describe(e) });
      }
      return;
    }
    const cur = this.state;
    if (recordingId(cur) !== id) return;
    if (isTerminal(status)) {
      this.finish(status);
      return;
    }
    const phase = status.recording?.phase ?? 'stopping';
    if (cur.kind === 'running') {
      this.set({ ...cur, phase, error: null });
    } else if (cur.kind === 'expired' || cur.kind === 'unreachable') {
      // The station took the stop: follow it for the completion allowance.
      const monitor = cur.kind === 'expired' ? cur.monitor : null;
      this.since = this.now();
      this.window = ALLOWANCE_MS;
      this.set({
        kind: 'running',
        monitor,
        captureId: id,
        phase,
        posting: false,
        after202: false,
        error: null,
      });
      this.schedule(POLL_MS);
    }
  }

  /** "Check result": one more lookup after the window. It never repeats the POST. */
  async check(): Promise<void> {
    const s = this.state;
    if (s.kind !== 'expired' || s.checking) return;
    this.set({ ...s, checking: true, error: null });
    let status: RecordingCaptureStatus | null = null;
    try {
      status = await this.deps.actions.status(this.deps.stationName, s.captureId);
    } catch {
      status = null;
    }
    const cur = this.state;
    if (cur.kind !== 'expired' || cur.captureId !== s.captureId) return;
    if (status !== null && isTerminal(status)) this.finish(status);
    else this.set({ ...cur, checking: false });
  }

  private same(captureId: string): boolean {
    const s = this.state;
    return (s.kind === 'running' || s.kind === 'expired') && s.captureId === captureId;
  }

  /** The POST is answered, and not with a result: the lookup decides. */
  private settled(after202: boolean): void {
    const s = this.state;
    if (s.kind === 'running') this.set({ ...s, posting: false, after202 });
  }

  /** On open and on reload: is a recording holding the station slot? (§4.3, decision 4). */
  private async discover(): Promise<void> {
    const generation = this.generation;
    const stored = readStored(this.deps.storage, this.deps.stationId);
    this.set({ kind: 'discovering' });
    const live = (): boolean => generation === this.generation && this.state.kind === 'discovering';
    let answer: ActiveRecording;
    try {
      answer = await this.deps.actions.active(this.deps.stationName);
    } catch (e) {
      if (!live()) return;
      if (unreachable(e))
        this.set({ kind: 'unreachable', captureId: stored?.captureId ?? null, error: null });
      else if (stored === null) this.set(IDLE);
      else this.resume(stored.captureId, null, stored);
      return;
    }
    if (!live()) return;
    if (answer.state === 'active') {
      this.resume(answer.capture_id, answer.status, stored);
      return;
    }
    if (stored === null) {
      this.set(IDLE);
      return;
    }
    // Idle: the stored ID is cleared only after one lookup of it returns a terminal state.
    let status: RecordingCaptureStatus | null = null;
    let offline = false;
    try {
      status = await this.deps.actions.status(this.deps.stationName, stored.captureId);
    } catch (e) {
      offline = unreachable(e);
    }
    if (!live()) return;
    if (status !== null && isTerminal(status)) this.finish(status);
    else if (offline) this.set({ kind: 'unreachable', captureId: stored.captureId, error: null });
    else {
      this.set({
        kind: 'expired',
        monitor: stored.monitor,
        captureId: stored.captureId,
        checking: false,
        error: null,
      });
    }
  }

  /** A recording is known after open or reload: show its phase and poll that ID. */
  private resume(
    captureId: string,
    status: RecordingCaptureStatus | null,
    stored: Stored | null,
  ): void {
    if (status !== null && isTerminal(status)) {
      this.finish(status);
      return;
    }
    const mine = stored !== null && stored.captureId === captureId ? stored : null;
    const seconds = status?.recording?.requested_seconds ?? mine?.seconds ?? MAX_SECONDS;
    this.since = mine?.at ?? this.now();
    this.window = windowMs(seconds);
    this.set({
      kind: 'running',
      monitor: mine?.monitor ?? null,
      captureId,
      phase: status?.recording?.phase ?? null,
      posting: false,
      after202: false,
      error: null,
    });
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
    if (s.kind !== 'running') return;
    let status: RecordingCaptureStatus | null = null;
    try {
      status = await this.deps.actions.status(this.deps.stationName, s.captureId);
    } catch {
      // A failed lookup is not an answer: the next one decides, or the window ends.
      status = null;
    }
    const cur = this.state;
    if (cur.kind !== 'running' || cur.captureId !== s.captureId) return;
    if (status !== null) {
      if (isTerminal(status)) {
        this.finish(status);
        return;
      }
      const phase = status.recording?.phase;
      if (phase !== undefined && phase !== cur.phase) this.set({ ...cur, phase });
      if (status.state === 'unknown' && !cur.posting) {
        this.expire();
        return;
      }
    }
    const left = this.since + this.window - this.now();
    if (left <= 0) {
      this.expire();
    } else {
      // The last lookup falls on the end of the window, not after it.
      this.schedule(Math.min(POLL_MS, left));
    }
  }

  /** The wait ends without a result: the owner sees "Check result". */
  private expire(): void {
    const last = this.state;
    if (last.kind === 'running') {
      this.set({
        kind: 'expired',
        monitor: last.monitor,
        captureId: last.captureId,
        checking: false,
        error: last.error,
      });
    }
  }
  /** A terminal answer: clear the stored ID and show the result. */
  private finish(status: Terminal): void {
    const prev = this.state;
    this.erase();
    if (status.state === 'committed') {
      this.set({
        kind: 'committed',
        published: status.published,
        after202: prev.kind === 'running' && prev.after202,
      });
    } else if (status.state === 'failed') {
      this.set({ kind: 'failed', text: status.message });
    } else {
      this.set({ kind: 'deleted' });
    }
  }

  private set(next: RecState): void {
    this.state = next;
    if (next.kind !== 'running') this.clearTimer();
    for (const fn of [...this.listeners]) fn();
  }

  private clearTimer(): void {
    if (this.timer !== null) this.timers.clear(this.timer);
    this.timer = null;
  }

  private write(v: Stored): void {
    try {
      this.deps.storage?.setItem(recordingKey(this.deps.stationId), JSON.stringify(v));
    } catch {
      // Storage blocked: the flow works for this page only.
    }
  }

  private erase(): void {
    try {
      this.deps.storage?.removeItem(recordingKey(this.deps.stationId));
    } catch {
      // Storage blocked.
    }
  }
}
