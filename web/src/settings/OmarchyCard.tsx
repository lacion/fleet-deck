// Omarchy admin task D (design `docs/v2/design/omarchy-admin.md` §5): the Omarchy card on the
// station tab of Settings. It shows the state of Omarchy on the station and runs the actions
// (install, uninstall, the screensaver, retry provisioning). Each action asks the elevation first.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Icon, RefreshCw } from '../ui/icons';
import type { ElevationAsk } from './elevation';
import { DeckError } from './http';
import {
  buttons,
  type CardState,
  cardLines,
  errorText,
  LOADING_TEXT,
  type OmarchyAction,
  type OmarchyActions,
  type OmarchyStatus,
  POLL_LIMIT_MS,
  POLL_MS,
  pollRetry,
  READ_ERROR_TEXT,
  runAction,
  stateOfReadError,
  UNSUPPORTED_TEXT,
  WORKING_TEXT,
} from './omarchy';
import {
  FileLines,
  type PlanLoad,
  RepairsSection,
  type RepairsState,
  RestoreDialog,
  ReviewDialog,
} from './OmarchyRepairPanels';
import {
  type ActExtra,
  doneText,
  type FileLine,
  INSTALL_REFUSED,
  type InstallPlanView,
  isRepairAction,
  type OmarchyAssetId,
  PLAN_ERROR_TEXT,
  REVIEW_BUTTON,
  type RestorePlanView,
  replaceExtra,
  restoreExtra,
  resultFileLines,
  resumeExtra,
  reviewVisible,
} from './omarchyRepair';

/** A failed action: its fixed text, and the message of the station as plain text. */
export type Failure = {
  readonly text: string;
  readonly message: string | null;
  /** `warn` for a done action that printed warnings, `ok` for a done repair; `error` otherwise. */
  readonly tone?: 'error' | 'warn' | 'ok';
  /** The files of a failed repair: each file's state and kept copy. */
  readonly lines?: readonly FileLine[];
};

/** The text above the warnings of a done action. */
export const WARNINGS_TEXT = 'Done, with warnings from the station:';
/** The pause after an action, so that a double click does not run the next button. */
const SETTLE_MS = 1_000;

type ViewProps = {
  readonly state: CardState;
  readonly busy: boolean;
  readonly failure: Failure | null;
  readonly onAct: (action: OmarchyAction) => void;
  readonly onRetryRead: () => void;
  /** The repairs of the station (BG-2); absent reads as not read yet. */
  readonly repairs?: RepairsState;
  readonly repairsOpen?: boolean;
  /** The last install was refused: the review is offered. */
  readonly refused?: boolean;
  readonly onReview?: () => void;
  readonly onToggleRepairs?: () => void;
  readonly onResume?: (repairId: string) => void;
  readonly onRestore?: (repairId: string) => void;
};

const IDLE: RepairsState = { kind: 'idle' };
const none = (): void => undefined;

/** The one-at-a-time guard of the card: a call that comes while `flag` is set does nothing. */
export function guarded<A extends unknown[]>(
  flag: { current: boolean },
  run: (...args: A) => Promise<void>,
): (...args: A) => Promise<void> {
  return async (...args) => {
    if (flag.current) return;
    flag.current = true;
    try {
      await run(...args);
    } finally {
      flag.current = false;
    }
  };
}

/** The text of a failed read of a plan: the fixed text of the code, else a plain sentence. */
export function planErrorText(e: unknown): string {
  return e instanceof DeckError && e.code !== null ? errorText(e.code) : PLAN_ERROR_TEXT;
}

/** The background read of a mounted card: every 15 s. */
export const REFRESH_MS = 15_000;

/** A sleep that `cancel` ends at once: no timer stays after the card is gone. */
export function makeSleeper(): {
  readonly sleep: (ms: number) => Promise<void>;
  readonly cancel: () => void;
} {
  const pending = new Set<() => void>();
  return {
    sleep: (ms) =>
      new Promise<void>((resolve) => {
        const end = () => {
          clearTimeout(id);
          pending.delete(end);
          resolve();
        };
        const id = setTimeout(end, ms);
        pending.add(end);
      }),
    cancel: () => {
      for (const end of [...pending]) end();
    },
  };
}

/** The one request gate of a card: a promise chain that never rejects. */
export type Gate = { current: Promise<void> };

/**
 * The calls of `actions` through `gate`: every status read and every action of the card is sent
 * after the one before it has ended, so the station never runs two children of the card at once.
 * A failed call rejects for its own caller only; the chain goes on.
 */
export function gateActions(actions: OmarchyActions, gate: Gate): OmarchyActions {
  const through = <T,>(call: () => Promise<T>): Promise<T> => {
    const result = gate.current.then(call);
    gate.current = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  };
  return {
    status: (station) => through(() => actions.status(station)),
    act: (station, action, elevation, extra) =>
      through(() => actions.act(station, action, elevation, extra)),
    installPlan: (station) => through(() => actions.installPlan(station)),
    repairs: (station) => through(() => actions.repairs(station)),
    restorePlan: (station, repairId) => through(() => actions.restorePlan(station, repairId)),
  };
}

/** Something runs on the station: an action, or the provisioning. */
export const stationBusy = (status: OmarchyStatus): boolean =>
  status.running !== null || (status.provision !== null && status.provision.bootstrap_running);

export type WatchEnv = {
  readonly sleep: (ms: number) => Promise<void>;
  readonly now: () => number;
  /** True when the card is gone: the watch ends. */
  readonly stopped: () => boolean;
  /** True while an action call or another read of the card runs: no read starts. */
  readonly paused: () => boolean;
  /** The status that the card shows, or `null`. */
  readonly known: () => OmarchyStatus | null;
  /** The number of the last change of the card: a read that a change overtook is dropped. */
  readonly turn: () => number;
  readonly onStatus: (status: OmarchyStatus) => void;
};

/**
 * Reads the status of a mounted card until `stopped()`. A tick of 5 s decides: a read is due every
 * 15 s, or every 5 s while the shown status has a running action or a running provisioning (at
 * most 10 min from the first such tick). No read starts while `paused()`. A failed read keeps
 * the last state. A read that a change of the card overtook is dropped.
 */
export async function watchStatus(
  actions: OmarchyActions,
  station: string,
  env: WatchEnv,
): Promise<void> {
  let lastRead = env.now();
  let fastSince: number | null = null;
  while (!env.stopped()) {
    await env.sleep(POLL_MS);
    if (env.stopped()) return;
    const known = env.known();
    const t = env.now();
    let every = REFRESH_MS;
    if (known !== null && stationBusy(known)) {
      fastSince ??= t;
      if (t - fastSince < POLL_LIMIT_MS) every = POLL_MS;
    } else {
      fastSince = null;
    }
    if (t - lastRead < every || env.paused()) continue;
    lastRead = t;
    const turn = env.turn();
    try {
      const status = await actions.status(station);
      if (env.stopped()) return;
      if (env.turn() === turn && !env.paused()) env.onStatus(status);
    } catch {
      // A failed background read keeps the last state.
    }
  }
}

/** The card for a state. No card for an older deck (404). */
export function OmarchyView({
  state,
  busy,
  failure,
  onAct,
  onRetryRead,
  repairs = IDLE,
  repairsOpen = false,
  refused = false,
  onReview = none,
  onToggleRepairs = none,
  onResume = none,
  onRestore = none,
}: ViewProps) {
  if (state.kind === 'hidden') return null;
  let body: React.ReactNode;
  if (state.kind === 'loading') {
    body = <p role="status">{LOADING_TEXT}</p>;
  } else if (state.kind === 'unsupported') {
    body = <p className="fd-note">{UNSUPPORTED_TEXT}</p>;
  } else if (state.kind === 'error') {
    body = (
      <div className="fd-set-notice" data-tone="error" role="alert">
        <span>{READ_ERROR_TEXT}</span>
        <button type="button" className="fd-sm" onClick={onRetryRead}>
          <Icon of={RefreshCw} size={14} /> Retry
        </button>
      </div>
    );
  } else {
    const lines = cardLines(state.status);
    const shown = buttons(state.status);
    const working = busy || state.status.running !== null;
    // The review needs the feature: it shows once the repairs were read, and a 501 hides it.
    const review =
      (repairs.kind === 'ready' || repairs.kind === 'error') &&
      reviewVisible(state.status, refused);
    body = (
      <>
        {lines.map((line) => (
          <p className="fd-note" key={line}>
            {line}
          </p>
        ))}
        {busy && state.status.running === null ? <p role="status">{WORKING_TEXT}</p> : null}
        {failure === null ? null : (
          <div
            className="fd-set-notice"
            data-tone={failure.tone ?? 'error'}
            role={failure.tone === 'ok' ? 'status' : 'alert'}
          >
            <span>{failure.text}</span>
            <FileLines lines={failure.lines ?? []} />
            {failure.message === null ? null : <pre>{failure.message}</pre>}
          </div>
        )}
        {shown.length === 0 && !review ? null : (
          <div className="fd-set-bar-actions">
            {shown.map((b) => (
              <button
                key={b.action}
                type="button"
                className="fd-sm"
                disabled={working}
                onClick={() => onAct(b.action)}
              >
                {b.label}
              </button>
            ))}
            {review ? (
              <button type="button" className="fd-sm" disabled={working} onClick={onReview}>
                {REVIEW_BUTTON}
              </button>
            ) : null}
          </div>
        )}
        <RepairsSection
          state={repairs}
          open={repairsOpen}
          working={working}
          onToggle={onToggleRepairs}
          onResume={onResume}
          onRestore={onRestore}
        />
      </>
    );
  }
  return (
    <section className="fd-panel fd-omarchy" aria-label="Omarchy">
      <h2>Omarchy</h2>
      {body}
    </section>
  );
}

type Props = {
  /** The station name, for the path of the calls. */
  readonly station: string;
  readonly actions: OmarchyActions;
  readonly ask: ElevationAsk;
  /** The state at the first render (tests). */
  readonly initial?: CardState;
};

export function OmarchyCard({ station, actions: direct, ask, initial }: Props) {
  const [state, setState] = useState<CardState>(initial ?? { kind: 'loading' });
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [repairs, setRepairs] = useState<RepairsState>(IDLE);
  const [repairsOpen, setRepairsOpen] = useState(false);
  const [refused, setRefused] = useState(false);
  const [dialog, setDialog] = useState<
    { readonly kind: 'review' } | { readonly kind: 'restore'; readonly repairId: string } | null
  >(null);
  const repairsRead = useRef(false);
  const turn = useRef(0);
  const alive = useRef(true);
  const shown = useRef<CardState>(initial ?? { kind: 'loading' });
  const acting = useRef(false);
  const sleeper = useRef(makeSleeper());
  const gate = useRef<Promise<void>>(Promise.resolve());
  // Every call of the card goes through the one gate (status reads and actions).
  const actions = useMemo(() => gateActions(direct, gate), [direct]);
  useEffect(() => {
    alive.current = true;
    const timers = sleeper.current;
    return () => {
      alive.current = false;
      timers.cancel();
    };
  }, []);

  const put = useCallback((next: CardState) => {
    shown.current = next;
    setState(next);
  }, []);

  const load = useCallback(() => {
    const mine = ++turn.current;
    put({ kind: 'loading' });
    actions.status(station).then(
      (status) => {
        if (mine === turn.current) put({ kind: 'ready', status });
      },
      (e: unknown) => {
        if (mine === turn.current) put(stateOfReadError(e));
      },
    );
  }, [actions, station, put]);
  useEffect(() => {
    if (initial === undefined) load();
    return () => {
      turn.current += 1;
    };
  }, [load, initial]);

  useEffect(() => {
    let dead = false;
    void watchStatus(actions, station, {
      sleep: sleeper.current.sleep,
      now: Date.now,
      stopped: () => dead,
      paused: () =>
        acting.current ||
        shown.current.kind === 'loading' ||
        shown.current.kind === 'hidden' ||
        shown.current.kind === 'unsupported',
      known: () => (shown.current.kind === 'ready' ? shown.current.status : null),
      turn: () => turn.current,
      onStatus: (status) => put({ kind: 'ready', status }),
    });
    return () => {
      dead = true;
      sleeper.current.cancel();
    };
  }, [actions, station, put]);

  const show = (status: OmarchyStatus) => put({ kind: 'ready', status });

  // The repairs: read once after the first status, when the section opens, and after each repair
  // action. A 501 means that the station lacks the feature.
  const loadRepairs = useCallback(async () => {
    try {
      const view = await actions.repairs(station);
      if (alive.current) setRepairs({ kind: 'ready', journals: view.journals });
    } catch (e) {
      if (!alive.current) return;
      setRepairs(
        e instanceof DeckError && e.status === 501 ? { kind: 'unsupported' } : { kind: 'error' },
      );
    }
  }, [actions, station]);
  useEffect(() => {
    repairsRead.current = false;
    setRepairs(IDLE);
    setRepairsOpen(false);
    setDialog(null);
    setRefused(false);
  }, [station]);
  const ready = state.kind === 'ready';
  useEffect(() => {
    if (!ready || repairsRead.current) return;
    repairsRead.current = true;
    void loadRepairs();
  }, [ready, loadRepairs]);

  const loadReview = useCallback(async (): Promise<PlanLoad<InstallPlanView>> => {
    try {
      return { kind: 'ready', plan: await actions.installPlan(station) };
    } catch (e) {
      return { kind: 'error', text: planErrorText(e) };
    }
  }, [actions, station]);
  const restoreId = dialog?.kind === 'restore' ? dialog.repairId : null;
  const loadRestore = useCallback(async (): Promise<PlanLoad<RestorePlanView>> => {
    try {
      return { kind: 'ready', plan: await actions.restorePlan(station, restoreId ?? '') };
    } catch (e) {
      return { kind: 'error', text: planErrorText(e) };
    }
  }, [actions, station, restoreId]);

  const act = guarded(acting, async (action: OmarchyAction, extra?: ActExtra) => {
    const repair = isRepairAction(action);
    setFailure(null);
    setRefused(false);
    setBusy(true);
    const mine = ++turn.current;
    const end = await runAction(actions, station, action, ask, extra);
    if (!alive.current) return;
    if (end.kind === 'cancelled') {
      setBusy(false);
      return;
    }
    if (repair) void loadRepairs();
    // The buttons stay off for a moment: the next button appears where the
    // pressed one was, and a double click must not press it.
    await sleeper.current.sleep(SETTLE_MS);
    if (!alive.current) return;
    setBusy(false);
    if (end.kind === 'failed') {
      if (end.status !== null) show(end.status);
      setRefused(end.code === INSTALL_REFUSED);
      setFailure({
        text: end.text,
        message: repair ? null : end.message,
        lines: resultFileLines(end.repair),
      });
      return;
    }
    show(end.status);
    if (end.kind === 'done' && repair) {
      setFailure({ text: doneText(action, end.repair), message: null, tone: 'ok' });
    } else if (end.kind === 'done' && end.message !== null) {
      setFailure({ text: WARNINGS_TEXT, message: end.message, tone: 'warn' });
    }
    if (end.kind === 'started') {
      await pollRetry(actions, station, {
        sleep: sleeper.current.sleep,
        now: Date.now,
        stopped: () => !alive.current || mine !== turn.current,
        onStatus: show,
      });
    }
  });

  return (
    <>
      <OmarchyView
        state={state}
        busy={busy}
        failure={failure}
        onAct={(action) => void act(action)}
        onRetryRead={load}
        repairs={repairs}
        repairsOpen={repairsOpen}
        refused={refused}
        onReview={() => setDialog({ kind: 'review' })}
        onToggleRepairs={() => {
          setRepairsOpen(!repairsOpen);
          if (!repairsOpen) void loadRepairs();
        }}
        onResume={(repairId) => void act('install_resume', resumeExtra(repairId))}
        onRestore={(repairId) => setDialog({ kind: 'restore', repairId })}
      />
      {dialog?.kind === 'review' ? (
        <ReviewDialog
          load={loadReview}
          busy={busy}
          onConfirm={(plan: InstallPlanView, edits: OmarchyAssetId[]) => {
            // The dialog closes first: the confirmation dialog of the elevation must not stack on it.
            setDialog(null);
            void act('install_replace', replaceExtra(plan, edits));
          }}
          onCancel={() => setDialog(null)}
        />
      ) : null}
      {dialog?.kind === 'restore' ? (
        <RestoreDialog
          load={loadRestore}
          busy={busy}
          onConfirm={(plan: RestorePlanView, assets: OmarchyAssetId[]) => {
            setDialog(null);
            void act('restore', restoreExtra(plan, assets));
          }}
          onCancel={() => setDialog(null)}
        />
      ) : null}
    </>
  );
}
