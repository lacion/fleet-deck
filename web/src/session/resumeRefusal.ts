// DL-433 (design `resume-after-stop.md` §3.4): a resume that the station refuses because a
// previous run is not proven ended opens the previous-run dialog in resume mode (owner only).
import { useMemo, useState } from 'react';
import type { Outcome, SessionActions } from './commands';

/** The station refused a resume because a previous run is not proven ended (DL-182, DL-433). */
export const UNPROVEN_RUN = 'not_resumable: the previous execution may still run';

export function isUnprovenRun(text: string): boolean {
  return text.includes(UNPROVEN_RUN);
}

/** What opened the previous-run dialog: the session menu, or a refused resume. */
export type CheckMode = 'menu' | 'resume';

/** The texts of the resume mode of the dialog, in one place (design §3.4.3). */
export const RESUME_TEXTS = {
  title: 'Resume needs the previous run to end',
  description:
    'Fleet Deck cannot prove that the previous run of this session stopped. It checked the processes again.',
  nothingLives:
    'Nothing of the previous run runs now. Fleet Deck cannot see a process that left its process group. Mark it ended only if you know that the agent is not running.',
  mark: 'Mark as ended and resume',
  busy: 'Resuming…',
  stop: 'Stop them and resume',
  stopConfirm:
    'Then Fleet Deck marks the run as ended and resumes the session. Fleet Deck cannot see a process that left its process group.',
  resume: 'Resume',
} as const;

/**
 * The actions with a `resume` that calls `onRefused` when the owner (`admin` scope) gets the
 * refusal of an unproven previous run. A member or a guest sees only the refusal text.
 */
export function withResumeCheck(actions: SessionActions, onRefused: () => void): SessionActions {
  return {
    ...actions,
    resume: async (): Promise<Outcome> => {
      const outcome = await actions.resume();
      if (!outcome.ok && actions.canAdmin && isUnprovenRun(outcome.text)) onRefused();
      return outcome;
    },
  };
}

/**
 * DL-433: the owner marks the run as ended, then resumes the session. A refusal of the mark
 * stops the chain (nothing resumes); the outcome is the first refusal or the resume outcome.
 */
export async function markAndResume(
  actions: Pick<SessionActions, 'reconcileExecution' | 'resume'>,
  shimId: string,
): Promise<Outcome> {
  const marked = await actions.reconcileExecution(shimId);
  if (!marked.ok) return marked;
  return actions.resume();
}

const NO_ACTIONS: Outcome = { ok: false, text: 'The session is not available.' };

/**
 * The state of the previous-run dialog (`null` = closed) and the actions that open it on a
 * refused resume. The header, the composer resume bar and the hibernated input path use
 * `actions` and `resume`; the dialog itself gets the plain actions.
 */
export function useResumeCheck(actions: SessionActions | undefined) {
  const [mode, setMode] = useState<CheckMode | null>(null);
  const wrapped = useMemo(
    () => (actions === undefined ? undefined : withResumeCheck(actions, () => setMode('resume'))),
    [actions],
  );
  const close = () => setMode(null);
  return {
    /** The props of the dialog; `null` while it is closed. */
    dialog: mode === null ? null : { purpose: mode, onResumed: close, onClose: close },
    actions: wrapped,
    resume: (): Promise<Outcome> => wrapped?.resume() ?? Promise.resolve(NO_ACTIONS),
    open: () => setMode('menu'),
  };
}
