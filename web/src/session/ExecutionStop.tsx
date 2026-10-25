// BG-4 (design `board-gaps.md` §5): "Stop these processes…" in the dialog of the previous run. The
// owner reviews the stop plan of the station, confirms it with the elevation, and sees the result.
// "Mark as ended" stays a separate action: nothing here presses it, except the combined "Stop them
// and resume" of the resume mode (DL-433), which marks the run and resumes after a clean stop.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ExecutionCheck, ExecutionStopOutcome, ExecutionStopPlan } from '../gen';
import { useSettings } from '../settings/context';
import { useFleetElevation } from '../spawn/useFleetElevation';
import { Loading } from '../ui/Loading';
import { commandId, type SessionActions, STOP_TEXTS } from './commands';
import { markAndResume, RESUME_TEXTS } from './resumeRefusal';

/** The label of the elevation that the deck needs for the stop. */
export const STOP_ELEVATION_LABEL = 'Stop previous run processes';
export const STOP_CONFIRM =
  'Stop the listed processes from this previous run? Fleet Deck will ask them to exit, then force-stop surviving targets after five seconds. Unsaved work may be lost.';
export const STOP_ESCAPED_NOTE =
  "Processes outside the run's group and session are listed for information and will not be stopped.";
export const STOP_NOTHING_LEFT =
  'No process of this run is left to stop. Check again, then use Mark as ended.';
const INVALID_CODE = 'The deck sent an invalid stop plan.';
/** The title of the list of the processes that left the group (also used by the check view). */
export const ESCAPED_TITLE = 'Processes that left its group (they do not block)';

/** The plan is shown again for review when it is older than this at the press (the plan lives 30 s). */
export const STALE_PLAN_MS = 25_000;
export const POLL_MS = 1_000;
export const POLL_LIMIT_MS = 20_000;

/** True when the plan is too old to confirm: it is read again and shown for review. */
export const planIsStale = (loadedAt: number, now: number): boolean =>
  now - loadedAt > STALE_PLAN_MS;

/** True when a live process of the run remains (the same rule as the station `still_runs`). */
export const stillRuns = (check: ExecutionCheck): boolean =>
  check.identity_alive || check.group_members.length > 0 || check.session_members.length > 0;

/** The text of a plan that cannot be confirmed; `null` for an eligible plan. */
export function planNotice(plan: ExecutionStopPlan): string | null {
  if (plan.eligible) return null;
  if (plan.code === null) return STOP_NOTHING_LEFT;
  return Object.hasOwn(STOP_TEXTS, plan.code)
    ? (STOP_TEXTS[plan.code] ?? INVALID_CODE)
    : INVALID_CODE;
}

/** The text of the outcome of a stop; `null` (no outcome in time) gives the unknown text. */
export function stopResultText(last: ExecutionStopOutcome | null): string {
  const unknown = STOP_TEXTS['outcome_unknown'] ?? '';
  if (last === null) return unknown;
  if (typeof last.code !== 'string') {
    return `Fleet Deck stopped ${last.gone} processes. ${last.kill_sent} needed a force stop.`;
  }
  return Object.hasOwn(STOP_TEXTS, last.code) ? (STOP_TEXTS[last.code] ?? unknown) : unknown;
}

export type Clock = {
  readonly now: () => number;
  readonly sleep: (ms: number) => Promise<void>;
};

export const realClock: Clock = {
  now: () => Date.now(),
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
};

/**
 * Polls the check every second, at most 20 s, until `last_stop.command_id` is `id`. A read that
 * fails counts as no answer; the end of the time gives `last: null` (the unknown text).
 */
export async function awaitStop(
  read: () => Promise<ExecutionCheck>,
  id: string,
  clock: Clock = realClock,
  isAlive: () => boolean = () => true,
): Promise<{ readonly last: ExecutionStopOutcome | null; readonly check: ExecutionCheck | null }> {
  const start = clock.now();
  let latest: ExecutionCheck | null = null;
  while (isAlive() && clock.now() - start < POLL_LIMIT_MS) {
    await clock.sleep(POLL_MS);
    if (!isAlive()) break;
    try {
      latest = await read();
    } catch {
      // No answer yet: the next read tries again, and the limit gives the unknown text.
    }
    const last = latest?.last_stop;
    if (last?.command_id === id) return { last, check: latest };
  }
  return { last: null, check: latest };
}

export type StopResult =
  | { readonly kind: 'cancelled' }
  | { readonly kind: 'failed'; readonly text: string }
  | {
      readonly kind: 'done';
      readonly text: string;
      /** The stop ended with a code or without an answer. */
      readonly alert: boolean;
      readonly check: ExecutionCheck | null;
    };

export type StopDeps = {
  readonly actions: Pick<SessionActions, 'stopExecution' | 'checkExecution'>;
  /** The elevation: a token, or `null` when the owner cancels. */
  readonly ask: (labels: readonly string[]) => Promise<string | null>;
  readonly clock?: Clock;
  readonly newId?: () => string;
  readonly isAlive?: () => boolean;
  /** The deck accepted the command: the board waits for the result now. */
  readonly onSent?: () => void;
};

/**
 * The confirmed stop: ask for the elevation (nothing is sent when the owner cancels), send the
 * command with its own id, then poll the check for the result. It never marks the run as ended (the caller may, DL-433).
 */
export async function sendStop(
  deps: StopDeps,
  shimId: string,
  plan: ExecutionStopPlan,
): Promise<StopResult> {
  const token = await deps.ask([STOP_ELEVATION_LABEL]);
  if (token === null) return { kind: 'cancelled' };
  const id = (deps.newId ?? commandId)();
  const sent = await deps.actions.stopExecution(shimId, plan.plan_id, token, id);
  if (!sent.ok) return { kind: 'failed', text: sent.text };
  deps.onSent?.();
  const found = await awaitStop(
    () => deps.actions.checkExecution(),
    id,
    deps.clock ?? realClock,
    deps.isAlive ?? (() => true),
  );
  const text = stopResultText(found.last);
  return {
    kind: 'done',
    text,
    alert: found.last === null || typeof found.last.code === 'string',
    check: found.check,
  };
}

/**
 * DL-433: after a stop in resume mode, the run is marked as ended and the session resumes only
 * when the stop ended without a code and the fresh check is complete and shows nothing alive.
 */
export const mayMarkAfterStop = (outcome: StopResult): boolean =>
  outcome.kind === 'done' &&
  !outcome.alert &&
  outcome.check?.complete === true &&
  !stillRuns(outcome.check);

const rowsOf = (list: ExecutionStopPlan['targets']): readonly string[] =>
  list.map((p) => `${p.pid} ${p.command}`);

type PanelProps = {
  readonly plan: ExecutionStopPlan;
  readonly busy: boolean;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
  /** Resume mode (DL-433): the confirmation names the mark and the resume. */
  readonly then?: 'resume' | null;
};

/** The review of an eligible plan: the targets, the escaped note, the confirmation and its buttons. */
export function PlanPanel({ plan, busy, onConfirm, onCancel, then = null }: PanelProps) {
  return (
    <div className="fd-exec-stop">
      <h3 className="fd-label">Processes to stop</h3>
      <ul className="fd-exec-list" aria-label="Processes to stop">
        {rowsOf(plan.targets).map((row) => (
          <li key={row}>{row}</li>
        ))}
      </ul>
      {plan.escaped.length === 0 ? null : (
        <>
          <h3 className="fd-label">{ESCAPED_TITLE}</h3>
          <ul className="fd-exec-list" aria-label={ESCAPED_TITLE}>
            {rowsOf(plan.escaped).map((row) => (
              <li key={row}>{row}</li>
            ))}
          </ul>
          <p className="fd-note">{STOP_ESCAPED_NOTE}</p>
        </>
      )}
      <p className="fd-note">
        {then === 'resume' ? `${STOP_CONFIRM} ${RESUME_TEXTS.stopConfirm}` : STOP_CONFIRM}
      </p>
      <button type="button" className="fd-danger" disabled={busy} onClick={onConfirm}>
        Stop processes
      </button>
      <button type="button" className="fd-ghost" disabled={busy} onClick={onCancel}>
        Cancel
      </button>
    </div>
  );
}

type Phase = 'idle' | 'loading' | 'review' | 'sending' | 'waiting' | 'resuming';

type Props = {
  readonly actions: SessionActions;
  readonly shimId: string;
  /** The check shows a live process of the run: the action is offered. */
  readonly stillRuns: boolean;
  /** The refreshed check after a stop (its lists replace the lists of the dialog). */
  readonly onCheck: (check: ExecutionCheck) => void;
  /** `resume`: the stop also marks the run as ended and resumes the session (DL-433). */
  readonly then?: 'resume' | null;
  /** The session resumed after the stop (only with `then="resume"`). */
  readonly onResumed?: () => void;
};

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

/** "Stop these processes…": the plan, the confirmation, the elevation and the result. */
export function ExecutionStop({
  actions,
  shimId,
  stillRuns: runs,
  onCheck,
  then = null,
  onResumed,
}: Props) {
  const { ask, node: elevationNode } = useFleetElevation(useSettings());
  const [phase, setPhase] = useState<Phase>('idle');
  const [plan, setPlan] = useState<ExecutionStopPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{ text: string; alert: boolean } | null>(null);
  const loadedAt = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const openPlan = useCallback(async () => {
    setPhase('loading');
    setError(null);
    setResult(null);
    try {
      const loaded = await actions.stopPlan(shimId);
      if (!alive.current) return;
      loadedAt.current = Date.now();
      setPlan(loaded);
    } catch (e) {
      if (!alive.current) return;
      setPlan(null);
      setError(message(e));
    }
    setPhase('review');
  }, [actions, shimId]);

  const confirm = async () => {
    if (plan === null) return;
    // The plan lives 30 s on the station: an old plan is read again, and nothing is sent.
    if (planIsStale(loadedAt.current, realClock.now())) {
      await openPlan();
      return;
    }
    setPhase('sending');
    setError(null);
    const outcome = await sendStop(
      {
        actions,
        ask,
        isAlive: () => alive.current,
        onSent: () => {
          if (alive.current) setPhase('waiting');
        },
      },
      shimId,
      plan,
    );
    if (!alive.current) return;
    if (outcome.kind === 'done') {
      setPlan(null);
      setResult({ text: outcome.text, alert: outcome.alert });
      setPhase('idle');
      if (outcome.check !== null) onCheck(outcome.check);
      if (then === 'resume' && mayMarkAfterStop(outcome)) {
        setPhase('resuming');
        const resumed = await markAndResume(actions, shimId);
        if (!alive.current) return;
        setPhase('idle');
        if (resumed.ok) onResumed?.();
        else setError(resumed.text);
      }
      return;
    }
    if (outcome.kind === 'failed') setError(outcome.text);
    setPhase('review');
  };

  const eligible = plan?.eligible === true;
  const notice = plan === null ? null : planNotice(plan);
  const working =
    phase === 'loading' || phase === 'sending' || phase === 'waiting' || phase === 'resuming';
  const offer = runs && !working && !(phase === 'review' && eligible);
  return (
    <div className="fd-exec-stop-area">
      {offer ? (
        <button type="button" className="fd-danger" onClick={() => void openPlan()}>
          {then === 'resume' ? RESUME_TEXTS.stop : 'Stop these processes…'}
        </button>
      ) : null}
      {phase === 'loading' ? <p className="fd-note">Reading the stop plan…</p> : null}
      {phase === 'waiting' ? (
        <p className="fd-note" role="status">
          Waiting for the result…
        </p>
      ) : null}
      {phase === 'resuming' ? <Loading text={RESUME_TEXTS.busy} /> : null}
      {notice === null ? null : (
        <p className="fd-note" role="alert">
          {notice}
        </p>
      )}
      {plan !== null && eligible && phase !== 'loading' ? (
        <PlanPanel
          plan={plan}
          busy={working}
          then={then}
          onConfirm={() => void confirm()}
          onCancel={() => {
            setPlan(null);
            setPhase('idle');
          }}
        />
      ) : null}
      {error === null ? null : (
        <p className="fd-note" role="alert">
          {error}
        </p>
      )}
      {result === null ? null : (
        <p className="fd-note" role={result.alert ? 'alert' : 'status'}>
          {result.text}
        </p>
      )}
      {elevationNode}
    </div>
  );
}
