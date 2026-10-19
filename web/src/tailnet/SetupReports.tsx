// BG-3 (design `docs/v2/design/board-gaps.md` 4.4): "Setup reports from unpaired keys". The owner
// opens the one-hour reporting window, reads the reports, allows or cancels one setup retry, and
// dismisses a report. A report is self-reported by a key that is not paired: it is never shown
// as a station, a paired badge or a grant. The reporting window is not the pairing window.
import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react';
import { DeckError, describe } from '../settings/http';
import { clockText, groupFingerprint } from './model';
import {
  codeInfo,
  DELIVERED_TEXT,
  DISABLED_TEXT,
  isUnavailable,
  pruneRequestIds,
  QUEUED_TEXT,
  type ReportsActions,
  type ReportsView,
  type ReportsWindow,
  requestIdFor,
  retryError,
  rowControl,
  type SetupReport,
  shortKey,
} from './reports';

export const TITLE = 'Setup reports from unpaired keys';
export const ELEVATION_LABEL = 'Setup reports';
export const LOADING_TEXT = 'Loading setup reports…';
export const READ_ERROR_TEXT = 'The setup reports could not be read.';
export const CLOSED_TEXT = 'Reporting is closed. Unpaired keys cannot send setup reports.';
export const NONE_TEXT = 'No setup reports.';
export const SELF_REPORTED_TEXT =
  'This key has not been paired. Its name and setup status are self-reported.';
export const CONFIRM_TEXT =
  'Allow this key to try setup once more? This does not pair it, change its trusted deck, replace its identity, or override setup checks.';

/** What the section knows: it is hidden when the deck has no pairing. */
export type ReportsState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'hidden' }
  | { readonly kind: 'error' }
  | { readonly kind: 'ready'; readonly view: ReportsView };

export type Ask = (labels?: readonly string[]) => Promise<string | null>;

/** The result of an owner action. The list is read again after every result but `cancelled`. */
export type FlowResult =
  | { readonly kind: 'done' }
  | { readonly kind: 'cancelled' }
  | {
      readonly kind: 'failed';
      readonly text: string;
      /** The deck refused the elevation: drop it, so that the next try asks again. */
      readonly refused: boolean;
    };

function failed(e: unknown, text: string = describe(e)): FlowResult {
  return {
    kind: 'failed',
    text,
    refused: e instanceof DeckError && e.code === 'elevation_required',
  };
}

/** Opens the window (with the elevation) or closes it (no elevation, as the pairing window). */
export async function changeWindow(
  actions: ReportsActions,
  ask: Ask,
  open: boolean,
): Promise<FlowResult> {
  try {
    let elevation: string | null = null;
    if (open) {
      elevation = await ask([ELEVATION_LABEL]);
      if (elevation === null) return { kind: 'cancelled' };
    }
    await actions.setWindow(open, elevation);
    return { kind: 'done' };
  } catch (e) {
    return failed(e);
  }
}

/** Allows one setup retry for the exact report that the owner saw. */
export async function allowOneRetry(
  actions: ReportsActions,
  ask: Ask,
  ids: Map<string, string>,
  report: SetupReport,
): Promise<FlowResult> {
  try {
    const elevation = await ask([ELEVATION_LABEL]);
    if (elevation === null) return { kind: 'cancelled' };
    await actions.allowRetry(
      report.fingerprint,
      report.terminal_id,
      report.state_digest,
      requestIdFor(ids, report),
      elevation,
    );
    return { kind: 'done' };
  } catch (e) {
    if (e instanceof DeckError) {
      return failed(e, retryError(e.code, e.message));
    }
    return failed(e);
  }
}

/** Cancels a queued retry, or dismisses the report: no confirmation, no elevation. */
export async function plainCall(call: () => Promise<void>): Promise<FlowResult> {
  try {
    await call();
    return { kind: 'done' };
  } catch (e) {
    return failed(e);
  }
}

function WindowLine({
  win,
  busy,
  error,
  onOpen,
  onClose,
}: {
  readonly win: ReportsWindow;
  readonly busy: boolean;
  readonly error: string | null;
  readonly onOpen: () => void;
  readonly onClose: () => void;
}) {
  const until = win.deadline_ms === null ? '' : clockText(win.deadline_ms);
  return (
    <div className="fd-reports-window">
      <p>{win.open ? `Reporting is open until ${until}.` : CLOSED_TEXT}</p>
      <div className="fd-pair-actions">
        {win.open ? (
          <button type="button" disabled={busy} onClick={onClose}>
            Close setup reports
          </button>
        ) : (
          <button type="button" disabled={busy} onClick={onOpen}>
            Open setup reports for one hour
          </button>
        )}
      </div>
      {error === null ? null : <p role="alert">{error}</p>}
    </div>
  );
}

function Controls({
  report,
  busy,
  confirming,
  onAsk,
  onConfirm,
  onDismissAsk,
  onCancelRetry,
}: {
  readonly report: SetupReport;
  readonly busy: boolean;
  readonly confirming: boolean;
  readonly onAsk: () => void;
  readonly onConfirm: () => void;
  readonly onDismissAsk: () => void;
  readonly onCancelRetry: () => void;
}) {
  const control = rowControl(report);
  switch (control.kind) {
    case 'disabled':
      return <p>{DISABLED_TEXT}</p>;
    case 'delivered':
      return <p>{DELIVERED_TEXT}</p>;
    case 'queued':
      return (
        <>
          <p>{QUEUED_TEXT}</p>
          <div className="fd-pair-actions">
            <button type="button" disabled={busy} onClick={onCancelRetry}>
              Cancel retry
            </button>
          </div>
        </>
      );
    case 'allow':
      return confirming ? (
        <div className="fd-reports-confirm">
          <p>{CONFIRM_TEXT}</p>
          <div className="fd-pair-actions">
            <button type="button" className="fd-primary" disabled={busy} onClick={onConfirm}>
              Allow one retry
            </button>
            <button type="button" disabled={busy} onClick={onDismissAsk}>
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <div className="fd-pair-actions">
          <button type="button" disabled={busy} onClick={onAsk}>
            Allow one setup retry
          </button>
        </div>
      );
  }
}

export type RowHandlers = {
  readonly onAsk: () => void;
  readonly onConfirm: () => void;
  readonly onDismissAsk: () => void;
  readonly onCancelRetry: () => void;
  readonly onDismiss: () => void;
};

/** One report. All the texts are fixed, except the self-reported name and the phase. */
export function ReportRow({
  report,
  busy,
  confirming,
  error,
  handlers,
}: {
  readonly report: SetupReport;
  readonly busy: boolean;
  readonly confirming: boolean;
  readonly error: string | null;
  readonly handlers: RowHandlers;
}) {
  return (
    <article
      className="fd-pair-request fd-report"
      aria-label={`Setup report from key ${shortKey(report.fingerprint)}`}
    >
      <p>{SELF_REPORTED_TEXT}</p>
      <p>
        {'Key: '}
        <code className="fd-fingerprint">{groupFingerprint(report.fingerprint)}</code>
      </p>
      <p>{`Reported name: ${report.name}`}</p>
      <p>{codeInfo(report.code).text}</p>
      <p>{`Phase: ${report.phase}`}</p>
      <p>{`Received: ${clockText(report.received_at)}`}</p>
      <p>{`Last contact: ${clockText(report.last_contact)}`}</p>
      <Controls
        report={report}
        busy={busy}
        confirming={confirming}
        onAsk={handlers.onAsk}
        onConfirm={handlers.onConfirm}
        onDismissAsk={handlers.onDismissAsk}
        onCancelRetry={handlers.onCancelRetry}
      />
      <div className="fd-pair-actions">
        <button type="button" disabled={busy} onClick={handlers.onDismiss}>
          Dismiss report
        </button>
      </div>
      {error === null ? null : <p role="alert">{error}</p>}
    </article>
  );
}

/** The section without state: the container passes the state and the handlers. */
export function SetupReportsView({
  state,
  busy,
  windowBusy,
  windowError,
  confirming,
  errors,
  onOpen,
  onClose,
  handlersOf,
}: {
  readonly state: ReportsState;
  readonly busy: string | null;
  readonly windowBusy: boolean;
  readonly windowError: string | null;
  readonly confirming: string | null;
  readonly errors: Readonly<Record<string, string>>;
  readonly onOpen: () => void;
  readonly onClose: () => void;
  readonly handlersOf: (report: SetupReport) => RowHandlers;
}) {
  if (state.kind === 'hidden') return null;
  let body: ReactNode;
  if (state.kind === 'loading') {
    body = <p className="fd-note">{LOADING_TEXT}</p>;
  } else if (state.kind === 'error') {
    body = <p role="alert">{READ_ERROR_TEXT}</p>;
  } else {
    const { window: win, reports } = state.view;
    body = (
      <>
        <WindowLine
          win={win}
          busy={windowBusy}
          error={windowError}
          onOpen={onOpen}
          onClose={onClose}
        />
        {reports.length === 0 ? (
          <p className="fd-note">{NONE_TEXT}</p>
        ) : (
          reports.map((report) => (
            <ReportRow
              key={report.fingerprint}
              report={report}
              busy={busy === report.fingerprint}
              confirming={confirming === report.fingerprint}
              error={errors[report.fingerprint] ?? null}
              handlers={handlersOf(report)}
            />
          ))
        )}
      </>
    );
  }
  return (
    <section className="fd-reports" aria-label={TITLE}>
      <h3>{TITLE}</h3>
      {body}
    </section>
  );
}

/**
 * The state of the section and its refresh. The pairing page calls `refresh` in its own loop
 * (the same `REFRESH_MS`), so the section does not run a second timer.
 */
export function useSetupReports(actions: ReportsActions): {
  readonly state: ReportsState;
  readonly refresh: () => Promise<void>;
} {
  const [state, setState] = useState<ReportsState>({ kind: 'loading' });
  const calls = useRef(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const refresh = useCallback(async () => {
    const call = ++calls.current;
    let next: ReportsState;
    try {
      next = { kind: 'ready', view: await actions.list() };
    } catch (e) {
      next = isUnavailable(e) ? { kind: 'hidden' } : { kind: 'error' };
    }
    if (alive.current && call === calls.current) setState(next);
  }, [actions]);
  return { state, refresh };
}

/** The section with its state: the window, the rows and the owner actions. */
export function SetupReports({
  actions,
  state,
  refresh,
  ask,
  onRefused,
}: {
  readonly actions: ReportsActions;
  readonly state: ReportsState;
  readonly refresh: () => Promise<void>;
  readonly ask: Ask;
  /** The deck refused the elevation: forget it. */
  readonly onRefused: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [windowBusy, setWindowBusy] = useState(false);
  const [windowError, setWindowError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  // One request ID for each row and terminal/digest pair, kept while the pair is shown.
  const ids = useRef(new Map<string, string>());
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    if (state.kind === 'ready') pruneRequestIds(ids.current, state.view.reports);
  }, [state]);

  const setError = (fingerprint: string, text: string | null) =>
    setErrors((prev) => {
      const rest = Object.fromEntries(Object.entries(prev).filter(([k]) => k !== fingerprint));
      return text === null ? rest : { ...rest, [fingerprint]: text };
    });

  const windowAction = async (open: boolean) => {
    setWindowError(null);
    setWindowBusy(true);
    const result = await changeWindow(actions, ask, open);
    if (result.kind === 'failed') {
      if (result.refused) onRefused();
      if (alive.current) setWindowError(result.text);
    }
    await refresh();
    if (alive.current) setWindowBusy(false);
  };

  const rowAction = async (report: SetupReport, run: () => Promise<FlowResult>) => {
    setError(report.fingerprint, null);
    setBusy(report.fingerprint);
    const result = await run();
    if (result.kind === 'failed') {
      if (result.refused) onRefused();
      if (alive.current) setError(report.fingerprint, result.text);
    }
    if (result.kind !== 'cancelled') {
      if (alive.current) setConfirming(null);
      await refresh();
    }
    if (alive.current) setBusy(null);
  };

  const handlersOf = (report: SetupReport): RowHandlers => ({
    onAsk: () => setConfirming(report.fingerprint),
    onDismissAsk: () => setConfirming(null),
    onConfirm: () => void rowAction(report, () => allowOneRetry(actions, ask, ids.current, report)),
    onCancelRetry: () =>
      void rowAction(report, () => plainCall(() => actions.cancelRetry(report.fingerprint))),
    onDismiss: () =>
      void rowAction(report, () => plainCall(() => actions.dismiss(report.fingerprint))),
  });

  return (
    <SetupReportsView
      state={state}
      busy={busy}
      windowBusy={windowBusy}
      windowError={windowError}
      confirming={confirming}
      errors={errors}
      onOpen={() => void windowAction(true)}
      onClose={() => void windowAction(false)}
      handlersOf={handlersOf}
    />
  );
}
