import type { SessionSummary } from '../gen';
import type { Outcome } from '../session/commands';

const message = (e: unknown) => (e instanceof Error ? e.message : 'Request failed');

/**
 * Resolves true as soon as `pred(get())` holds (at once when it holds now), false after
 * `timeoutMs`. It always unsubscribes.
 */
export function waitFor<T>(
  get: () => T,
  subscribe: (cb: () => void) => () => void,
  pred: (v: T) => boolean,
  timeoutMs: number,
): Promise<boolean> {
  if (pred(get())) return Promise.resolve(true);
  return new Promise<boolean>((resolve) => {
    let done = false;
    let unsubscribe: () => void = () => {};
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finish = (ok: boolean) => {
      if (done) return;
      done = true;
      if (timer !== undefined) clearTimeout(timer);
      unsubscribe();
      resolve(ok);
    };
    unsubscribe = subscribe(() => {
      if (pred(get())) finish(true);
    });
    // The subscription may have fired before `unsubscribe` was assigned.
    if (done) unsubscribe();
    else timer = setTimeout(() => finish(false), timeoutMs);
  });
}

/** The newest summary of one session, and a way to hear about changes. */
export type Watch = {
  get: () => SessionSummary | null;
  subscribe: (cb: () => void) => () => void;
};

type Result = { ok: true; shellId: string } | { ok: false; text: string };

/** Stop the session when it runs, wait until it has ended, then run its TUI (PLAN §3.6). */
export async function startHandover(
  deps: {
    watch: Watch;
    stop: () => Promise<Outcome>;
    handover: () => Promise<string>;
  },
  // Longer than the station's stop grace (30 s by default) plus its TERM grace.
  timeoutMs = 60000,
): Promise<Result> {
  const { watch } = deps;
  const first = watch.get();
  if (first === null) return { ok: false, text: 'The session is not on the board.' };
  if (first.terminal != null) {
    return { ok: false, text: 'A terminal holds this session already.' };
  }
  if (first.state !== 'exited' && first.state !== 'lost') {
    const stopped = await deps.stop();
    if (!stopped.ok) return { ok: false, text: stopped.text };
    const ended = await waitFor(
      watch.get,
      watch.subscribe,
      (s) => s !== null && (s.state === 'exited' || s.state === 'lost') && s.current_run === null,
      timeoutMs,
    );
    if (!ended) {
      return {
        ok: false,
        text: 'The session is still stopping. Try again when it has stopped.',
      };
    }
  }
  try {
    return { ok: true, shellId: await deps.handover() };
  } catch (e) {
    return { ok: false, text: `Not opened: ${message(e)}` };
  }
}

/** End the TUI, wait until the hold is gone, then resume the driven session (PLAN §3.6). */
export async function returnToBoard(
  deps: {
    watch: Watch;
    shellLive: () => Promise<boolean>;
    close: () => Promise<void>;
    resume: () => Promise<Outcome>;
  },
  timeoutMs = 15000,
): Promise<Outcome> {
  if (await deps.shellLive()) {
    try {
      await deps.close();
    } catch (e) {
      return { ok: false, text: `Not closed: ${message(e)}` };
    }
  }
  const free = await waitFor(deps.watch.get, deps.watch.subscribe, (s) => !s?.terminal, timeoutMs);
  if (!free) {
    return {
      ok: false,
      text: 'The terminal process has not ended yet. Try again, or end it in the terminal.',
    };
  }
  const resumed = await deps.resume();
  const back = (o: Outcome): Outcome =>
    o.ok
      ? { ok: true, text: 'Back under board control.' }
      : { ok: false, text: `Returned, but the resume failed: ${o.text}` };
  const later = resumed.later;
  if (later === undefined) return back(resumed);
  // G4: a pending resume keeps its follow-up; the final text is the same as without the wait.
  return {
    ...back(resumed),
    text: 'Back under board control. The station has not confirmed the resume yet.',
    later: async (signal) => {
      const final = await later(signal);
      return final === null ? null : back(final);
    },
  };
}

/** Clear a terminal hold whose process the station cannot see. */
export async function releaseHold(
  release: () => Promise<void>,
): Promise<{ ok: boolean; text: string }> {
  try {
    await release();
    return { ok: true, text: 'The terminal hold is released.' };
  } catch (e) {
    return { ok: false, text: `Not released: ${message(e)}` };
  }
}
