// DL-140: the one session controller. It owns the pending command and the
// last outcome of a session; the header, the composer and the approval dock
// use it, and no other component orchestrates a session action.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { DeckSession } from '../gen';
import type { Freshness } from '../runtime/state';
import type { Outcome, SessionActions } from './commands';

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

export type Controller = {
  readonly flags: SessionFlags;
  /** The name of the running action, or `null`. */
  readonly busy: string | null;
  readonly outcome: Outcome | null;
  act(name: string, call: () => Promise<Outcome>): void;
  dismiss(): void;
};

export function useSessionController(
  session: DeckSession,
  freshness: Freshness,
  actions: SessionActions | null,
): Controller {
  const sessionId = session.summary.session_id;
  const [busy, setBusy] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [seen, setSeen] = useState(sessionId);
  const current = useRef(sessionId);
  if (seen !== sessionId) {
    setSeen(sessionId);
    setOutcome(null);
    setBusy(null);
  }
  useEffect(() => {
    current.current = sessionId;
  }, [sessionId]);

  const act = useCallback((name: string, call: () => Promise<Outcome>) => {
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
      setOutcome(result);
      setBusy(null);
    })();
  }, []);
  const dismiss = useCallback(() => setOutcome(null), []);
  return { flags: sessionFlags(session, freshness, actions), busy, outcome, act, dismiss };
}
