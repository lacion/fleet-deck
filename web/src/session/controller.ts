// DL-140: the one session controller. It owns the pending command and the
// last outcome of a session; the header, the composer and the approval dock
// use it, and no other component orchestrates a session action.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { DeckSession, OpenRequest } from '../gen';
import type { Freshness } from '../runtime/state';
import type { Outcome, SessionActions } from './commands';
import type { Reply } from './question';

export type SessionFlags = {
  readonly live: boolean;
  readonly ended: boolean;
  /** A provider TUI holds the session. */
  readonly held: boolean;
  readonly working: boolean;
  readonly drive: boolean;
  readonly approve: boolean;
  readonly canResume: boolean;
  readonly canStop: boolean;
  readonly canInterrupt: boolean;
};

/** What the owner may do with a session now. */
export function sessionFlags(
  session: DeckSession,
  freshness: Freshness,
  actions: SessionActions | null,
): SessionFlags {
  const s = session.summary;
  const caps = s.capabilities;
  const live = freshness === 'live' && session.status !== 'unreachable';
  const ended = s.state === 'exited' || s.state === 'lost';
  const held = s.terminal != null;
  const working = s.current_run !== null;
  const canDrive = actions?.canDrive === true;
  const drive = live && !ended && !held && canDrive;
  return {
    live,
    ended,
    held,
    working,
    drive,
    approve: live && !ended && actions?.canApprove === true,
    canResume: live && ended && !held && caps?.resume === true && canDrive,
    canStop: live && !ended && canDrive,
    canInterrupt: drive && working && caps?.interrupt === true,
  };
}

/** The outcome that the controller shows (G4): the action that gave it, the request of an
 * answer, and how many times in a row the same action on the same target gave the same text. */
export type Shown = Outcome & {
  readonly action?: string;
  readonly target?: string | null;
  readonly repeats?: number;
};

/** G4: the next shown outcome; an equal action, target, `ok` and text counts one more repeat. */
export function nextShown(
  prev: Shown | null,
  action: string,
  target: string | null,
  outcome: Outcome,
): Shown {
  const same =
    prev !== null &&
    prev.action === action &&
    (prev.target ?? null) === target &&
    prev.ok === outcome.ok &&
    prev.text === outcome.text;
  return {
    ok: outcome.ok,
    text: outcome.text,
    action,
    target,
    repeats: same ? (prev.repeats ?? 1) + 1 : 1,
  };
}

/** G4: the words of a shown outcome: ` (n times)` after a repeat. */
export function shownText(o: Shown): string {
  return (o.repeats ?? 1) > 1 ? `${o.text} (${o.repeats} times)` : o.text;
}

/** G4: the refused answers of the session, by request id (the dock shows them). */
export type Refusals = Readonly<Record<string, Shown>>;

/** G4: what the controller shows. `outcome` is the header line; `settled` is the last final
 *  outcome (the base of the repeat count); `refusals` are the refused answers by request. */
export type ShownState = {
  readonly outcome: Shown | null;
  readonly settled: Shown | null;
  readonly refusals: Refusals;
};
export const EMPTY_SHOWN: ShownState = { outcome: null, settled: null, refusals: {} };

/** A final outcome: it becomes `settled`; an answer updates its request's refusal; `show` puts
 *  it on the header line. */
function settle(
  s: ShownState,
  action: string,
  target: string | null,
  o: Outcome,
  show: boolean,
): ShownState {
  const shown = nextShown(s.settled, action, target, o);
  let refusals = s.refusals;
  if (action === 'answer' && target !== null) {
    const { [target]: prev, ...rest } = s.refusals;
    refusals = o.ok ? rest : { ...rest, [target]: nextShown(prev ?? null, action, target, o) };
  }
  return { outcome: show ? shown : s.outcome, settled: shown, refusals };
}

/** G4: the state after the first outcome of an action. A provisional outcome (with `later`)
 *  goes on the header line only: it is not counted and does not touch the refusals. */
export function onFirst(
  s: ShownState,
  action: string,
  target: string | null,
  o: Outcome,
): ShownState {
  if (o.later !== undefined) return { ...s, outcome: nextShown(null, action, target, o) };
  return settle(s, action, target, o, true);
}

/** G4: the state after the final outcome of a followed action whose first shown outcome is
 *  `first`. A refusal always shows; a success shows only while `first` is still shown. */
export function onFinal(
  s: ShownState,
  first: Shown | null,
  action: string,
  target: string | null,
  o: Outcome,
): ShownState {
  return settle(s, action, target, o, !o.ok || s.outcome === first);
}

/** G4: the dock lines: the refusal text of each open request that has one. */
export function dockRefusals(
  refusals: Refusals,
  open: readonly OpenRequest[],
): Readonly<Record<string, string>> {
  const lines: Record<string, string> = {};
  for (const r of open) {
    const o = refusals[r.request_id];
    if (o !== undefined) lines[r.request_id] = shownText(o);
  }
  return lines;
}

/** G4: the header outcome: not a refused answer that the dock shows (it is not shown twice).
 *  `docked` is the list of requests that the dock shows now: empty when the dock is not
 *  mounted (another tab), so the header then keeps the refusal. */
export function headerOutcome(
  c: Pick<Controller, 'outcome' | 'refusals'>,
  docked: readonly OpenRequest[],
): Shown | null {
  const o = c.outcome;
  if (o === null || o.ok || o.action !== 'answer' || o.target == null) return o;
  const t = o.target;
  return c.refusals[t] !== undefined && docked.some((r) => r.request_id === t) ? null : o;
}

/** G4: the dock's answer handler; it names the request as the target of the action. */
export const answerVia =
  (c: Pick<Controller, 'act'>, actions: SessionActions | null | undefined) =>
  (request: string, option: string, reply: Reply | null) => {
    if (actions) c.act('answer', () => actions.answer(request, option, reply), request);
  };

export type Controller = {
  readonly flags: SessionFlags;
  /** The name of the running action, or `null`. */
  readonly busy: string | null;
  readonly outcome: Shown | null;
  /** G4: the refused answers of the session, by request id. */
  readonly refusals: Refusals;
  act(name: string, call: () => Promise<Outcome>, target?: string): void;
  dismiss(): void;
};

export function useSessionController(
  session: DeckSession,
  freshness: Freshness,
  actions: SessionActions | null,
): Controller {
  const sessionId = session.summary.session_id;
  const [busy, setBusy] = useState<string | null>(null);
  const [shown, setShown] = useState<ShownState>(EMPTY_SHOWN);
  const shownRef = useRef<ShownState>(EMPTY_SHOWN);
  const [seen, setSeen] = useState(sessionId);
  const current = useRef(sessionId);
  const follows = useRef(new Set<AbortController>());
  if (seen !== sessionId) {
    setSeen(sessionId);
    setShown(EMPTY_SHOWN);
    setBusy(null);
  }
  useEffect(() => {
    current.current = sessionId;
    shownRef.current = EMPTY_SHOWN;
    const all = follows.current;
    // G4: every follow of this session ends with the session view or a session change.
    return () => {
      for (const c of all) c.abort();
      all.clear();
    };
  }, [sessionId]);

  const apply = useCallback((f: (s: ShownState) => ShownState): ShownState => {
    const next = f(shownRef.current);
    shownRef.current = next;
    setShown(next);
    return next;
  }, []);

  const act = useCallback(
    (name: string, call: () => Promise<Outcome>, target?: string) => {
      const started = current.current;
      setBusy(name);
      void (async () => {
        let result: Outcome;
        try {
          result = await call();
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          result = { ok: false, text: `Not delivered: ${message}` };
        }
        // The owner opened another session meanwhile: the result is not theirs to see.
        if (current.current !== started) return;
        setBusy(null);
        const t = target ?? null;
        const first = apply((s) => onFirst(s, name, t, result)).outcome;
        if (result.later === undefined) return;
        // G4: each pending command has its own follow; a newer action does not end it.
        const ctl = new AbortController();
        follows.current.add(ctl);
        const final = await result.later(ctl.signal).catch(() => null);
        follows.current.delete(ctl);
        if (final === null || ctl.signal.aborted || current.current !== started) return;
        apply((s) => onFinal(s, first, name, t, final));
      })();
    },
    [apply],
  );
  const dismiss = useCallback(
    () => apply((s) => ({ ...s, outcome: null, settled: null })),
    [apply],
  );
  return {
    flags: sessionFlags(session, freshness, actions),
    busy,
    outcome: shown.outcome,
    refusals: shown.refusals,
    act,
    dismiss,
  };
}
