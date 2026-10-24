// DL-182 (design `shim-resume.md` §4.2): "Check the previous run". The owner sees what the
// station finds of the unresolved run of the session, and marks the run as ended when nothing
// of it lives. The station checks the processes again and refuses while any of them lives.
import { useCallback, useEffect, useRef, useState } from 'react';
import { trapTab, useFocusReturn } from '../app/focus';
import { useOverlay } from '../app/overlay';
import type { ExecutionCheck } from '../gen';
import { Icon, X } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import type { Outcome, SessionActions } from './commands';
import { ESCAPED_TITLE, ExecutionStop, stillRuns } from './ExecutionStop';
import { type CheckMode, markAndResume, RESUME_TEXTS } from './resumeRefusal';

type ProcessList = { readonly title: string; readonly rows: readonly string[] };

export type ExecutionView = {
  /** No earlier run is unresolved: nothing to mark. */
  readonly nothing: boolean;
  /** "Mark as ended" is allowed: the check is complete and nothing of the run lives. */
  readonly canMark: boolean;
  readonly lines: readonly string[];
  readonly lists: readonly ProcessList[];
  /** The text for an incomplete check; `null` when the check is complete. */
  readonly incomplete: string | null;
};

const NOTHING = 'Nothing to reconcile: no earlier run of this session is unresolved.';
const yesNo = (v: boolean): string => (v ? 'yes' : 'no');
const rowsOf = (list: ExecutionCheck['group_members']): readonly string[] =>
  list.length === 0 ? ['none'] : list.map((p) => `${p.pid} ${p.command}`);

/** What the dialog shows of a check, and whether "Mark as ended" is allowed. */
export function executionView(check: ExecutionCheck): ExecutionView {
  if (check.shim_id === null) {
    return { nothing: true, canMark: false, lines: [NOTHING], lists: [], incomplete: null };
  }
  const lists: ProcessList[] = [
    { title: 'Processes in its process group', rows: rowsOf(check.group_members) },
    { title: 'Processes in its session', rows: rowsOf(check.session_members) },
  ];
  if (check.escaped.length > 0) {
    lists.push({ title: ESCAPED_TITLE, rows: rowsOf(check.escaped) });
  }
  return {
    nothing: false,
    canMark:
      check.complete &&
      !check.identity_alive &&
      check.group_members.length === 0 &&
      check.session_members.length === 0,
    lines: [
      `Why it is unresolved: ${check.reason}`,
      `Same boot as the run: ${yesNo(check.same_boot)}`,
      `The provider process is alive: ${yesNo(check.identity_alive)}`,
    ],
    lists,
    incomplete: check.complete
      ? null
      : `The check is incomplete: ${check.error ?? 'unknown reason'}. Fleet Deck cannot prove that the run ended.`,
  };
}

/** Runs a step of the resume mode: `null` and `onResumed` when it is accepted, else its refusal text. */
export async function finishResume(
  step: () => Promise<Outcome>,
  onResumed: () => void,
): Promise<string | null> {
  const outcome = await step();
  if (!outcome.ok) return outcome.text;
  onResumed();
  return null;
}

const message = (e: unknown) => (e instanceof Error ? e.message : 'The request failed.');

type BodyProps = {
  readonly check: ExecutionCheck;
  readonly purpose: CheckMode;
  readonly actions: SessionActions;
  readonly busy: boolean;
  readonly marked: boolean;
  readonly markError: string | null;
  /** `mark`: Mark as ended (menu). `both`: mark, then resume. `resume`: resume only. */
  readonly onAct: (act: 'mark' | 'both' | 'resume', shimId: string) => void;
  readonly onCheck: (check: ExecutionCheck) => void;
  readonly onResumed?: () => void;
};

/** What the dialog shows of a check: the lines, the lists, the stop action and the mark. */
export function ExecutionBody(p: BodyProps) {
  const { check, busy, marked, markError, onAct } = p;
  const resumeMode = p.purpose === 'resume';
  const view = executionView(check);
  const shimId = check.shim_id;
  const alert =
    markError === null ? null : (
      <p className="fd-note" role="alert">
        {markError}
      </p>
    );
  return (
    <div className="fd-exec-body">
      {view.lines.map((line) => (
        <p key={line} className={view.nothing ? 'fd-note' : undefined}>
          {line}
        </p>
      ))}
      {view.lists.map((list) => (
        <div key={list.title}>
          <h3 className="fd-label">{list.title}</h3>
          <ul className="fd-exec-list" aria-label={list.title}>
            {list.rows.map((row) => (
              <li key={row}>{row}</li>
            ))}
          </ul>
        </div>
      ))}
      {view.incomplete === null ? null : (
        <p className="fd-note" role="alert">
          {view.incomplete}
        </p>
      )}
      {view.nothing || shimId === null || marked ? null : (
        <ExecutionStop
          actions={p.actions}
          shimId={shimId}
          stillRuns={stillRuns(check)}
          onCheck={p.onCheck}
          then={resumeMode ? 'resume' : null}
          {...(p.onResumed ? { onResumed: p.onResumed } : {})}
        />
      )}
      {view.nothing && resumeMode ? (
        <>
          <button
            type="button"
            className="fd-primary"
            disabled={busy}
            onClick={() => onAct('resume', '')}
          >
            {busy ? RESUME_TEXTS.busy : RESUME_TEXTS.resume}
          </button>
          {alert}
        </>
      ) : null}
      {view.nothing || shimId === null ? null : resumeMode ? (
        view.canMark ? (
          <>
            <p className="fd-note">{RESUME_TEXTS.nothingLives}</p>
            <button
              type="button"
              className="fd-danger"
              disabled={busy}
              onClick={() => onAct('both', shimId)}
            >
              {busy ? RESUME_TEXTS.busy : RESUME_TEXTS.mark}
            </button>
            {alert}
          </>
        ) : null
      ) : marked ? (
        <p className="fd-note" role="status">
          Marked as ended. You can resume the session now.
        </p>
      ) : (
        <>
          <p className="fd-note">
            Mark it as ended only when you know that the previous run stopped. The station checks
            the processes again and refuses while any of them lives.
          </p>
          <button
            type="button"
            className="fd-danger"
            disabled={!view.canMark || busy}
            onClick={() => onAct('mark', shimId)}
          >
            {busy ? 'Marking…' : 'Mark as ended'}
          </button>
          {alert}
        </>
      )}
    </div>
  );
}

type Props = {
  readonly actions: SessionActions;
  readonly onClose: () => void;
  /** `menu`: the session menu entry (default). `resume`: a refused resume opened it (DL-433). */
  readonly purpose?: CheckMode;
  /** Resume mode: the run is marked as ended and the session resumed; the dialog closes. */
  readonly onResumed?: () => void;
};

/** "Check the previous run" (owner only): the processes of the run, and "Mark as ended". */
export function ExecutionDialog({ actions, onClose, purpose = 'menu', onResumed }: Props) {
  const resumeMode = purpose === 'resume';
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusReturn(root, close);
  useOverlay();
  const [check, setCheck] = useState<ExecutionCheck | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [marked, setMarked] = useState(false);
  const [markError, setMarkError] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    setMarkError(null);
    try {
      const result = await actions.checkExecution();
      if (!alive.current) return;
      setCheck(result);
    } catch (e) {
      if (!alive.current) return;
      setCheck(null);
      setError(message(e));
    } finally {
      if (alive.current) setLoading(false);
    }
  }, [actions]);
  useEffect(() => {
    void load();
  }, [load]);

  // Resume mode: a step that resumes (mark then resume, or resume only); a refusal keeps the
  // dialog open and shows its text, an accepted resume closes it.
  const resumeStep = async (step: () => Promise<Outcome>) => {
    setBusy(true);
    setMarkError(null);
    const text = await finishResume(step, () => onResumed?.());
    if (!alive.current) return;
    setBusy(false);
    setMarkError(text);
  };
  const markAndGo = (shimId: string) => resumeStep(() => markAndResume(actions, shimId));
  const resumeOnly = () => resumeStep(() => actions.resume());

  const mark = async (shimId: string) => {
    setBusy(true);
    setMarkError(null);
    const outcome = await actions.reconcileExecution(shimId);
    if (!alive.current) return;
    setBusy(false);
    if (outcome.ok) setMarked(true);
    else setMarkError(outcome.text);
  };

  const view = check === null ? null : executionView(check);
  return (
    // The overlay click is a mouse shortcut; the Close X and Escape serve the keyboard.
    <div
      className="fd-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={root}
        className="fd-dialog fd-exec"
        role="dialog"
        aria-modal="true"
        aria-label="Previous run of this session"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            onClose();
          } else trapTab(e, root.current);
        }}
      >
        <div className="fd-dialog-head">
          <h2>{resumeMode ? RESUME_TEXTS.title : 'Previous run of this session'}</h2>
          {resumeMode ? <p>{RESUME_TEXTS.description}</p> : null}
          <button
            ref={close}
            type="button"
            className="fd-iconbtn"
            aria-label="Close"
            title="Close"
            onClick={onClose}
          >
            <Icon of={X} />
          </button>
        </div>
        {loading ? <Loading text="Checking the processes…" /> : null}
        {error === null ? null : <ErrorNotice text={error} onRetry={() => void load()} />}
        {check === null || loading ? null : (
          <ExecutionBody
            check={check}
            purpose={purpose}
            actions={actions}
            busy={busy}
            marked={marked}
            markError={markError}
            onAct={(act, id) =>
              void (act === 'mark' ? mark(id) : act === 'both' ? markAndGo(id) : resumeOnly())
            }
            onCheck={setCheck}
            {...(onResumed ? { onResumed } : {})}
          />
        )}
        {view?.nothing === true ? null : (
          <div className="fd-exec-foot">
            <button
              type="button"
              className="fd-ghost"
              disabled={loading || busy}
              onClick={() => void load()}
            >
              Check again
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
