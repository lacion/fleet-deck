// BG-1 (design `docs/v2/design/board-gaps.md` §2.3, §2.4, §2.7, §2.9): the "Desktop tools" card on
// the station tab of Settings. It shows the facts of the desktop tools, reviews an install plan,
// opens the installation terminal (with the elevation) and lists the earlier installations.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { DesktopToolsPlan, MaintenanceInfo, StationTools } from '../gen';
import {
  type DesktopToolsActions,
  keyboardLine,
  LOADING_TEXT,
  makeRequestIds,
  openInstall,
  PACKAGE_MISMATCH_TEXT,
  PLAN_LOADING_TEXT,
  type PlanView,
  planErrorText,
  planView,
  pluginLine,
  READ_ERROR_TEXT,
  shortState,
  SUDO_TEXT,
  toolLine,
  toolName,
  CONFIRM_TEXT,
} from './desktopTools';
import type { ElevationAsk } from './elevation';
import { DeckError } from './http';
import { MaintenancePanel } from './MaintenancePanel';
import { guarded } from './OmarchyCard';

export type ToolsState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'hidden' }
  | { readonly kind: 'error' }
  | { readonly kind: 'ready'; readonly tools: StationTools };

export type RunsState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'hidden' }
  | { readonly kind: 'error' }
  | { readonly kind: 'ready'; readonly runs: readonly MaintenanceInfo[] };

/** The install panel: reading the plan, a failed read, or a plan with its last notice. */
export type InstallState =
  | { readonly kind: 'reading' }
  | { readonly kind: 'error'; readonly text: string }
  | {
      readonly kind: 'plan';
      readonly plan: DesktopToolsPlan;
      /** The failure of the last open, as plain text. */
      readonly notice: string | null;
      readonly busy: boolean;
    };

/** The request gate of the card: a promise chain that never rejects. */
export type DesktopGate = { current: Promise<void> };

/**
 * The calls of `actions` through `gate`: every read and the open of the card are sent after the
 * one before it has ended. `resize` and `socketPath` stay outside: the terminal must not wait.
 */
export function gateDesktopTools(
  actions: DesktopToolsActions,
  gate: DesktopGate,
): DesktopToolsActions {
  const through = <T,>(call: () => Promise<T>): Promise<T> => {
    const result = gate.current.then(call);
    gate.current = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  };
  return {
    tools: (station) => through(() => actions.tools(station)),
    plan: (station) => through(() => actions.plan(station)),
    open: (station, planId, requestId, cols, rows, elevation) =>
      through(() => actions.open(station, planId, requestId, cols, rows, elevation)),
    list: (station) => through(() => actions.list(station)),
    resize: actions.resize,
    socketPath: actions.socketPath,
  };
}

const unsupported = (e: unknown): boolean => e instanceof DeckError && e.status === 501;

/** The facts of the station: one line for each tool, the keyboard, the plugin, the sudo note. */
export function FactsView({ tools }: { readonly tools: StationTools }) {
  return (
    <>
      <ul className="fd-dt-tools">
        {tools.tools.map((fact) => (
          <li key={fact.id}>
            <span>{toolLine(fact)}</span>
            {fact.package_matches_executable === false ? (
              <span className="fd-note">{PACKAGE_MISMATCH_TEXT}</span>
            ) : null}
          </li>
        ))}
      </ul>
      <p className="fd-note">{keyboardLine(tools.keyboard)}</p>
      <p className="fd-note">{pluginLine(tools.plugin.status)}</p>
      {tools.passwordless_sudo === 'on' ? (
        <p className="fd-note" role="note">
          {SUDO_TEXT}
        </p>
      ) : null}
    </>
  );
}

type InstallProps = {
  readonly state: InstallState;
  readonly onOpen: () => void;
  readonly onClose: () => void;
};

/** The install dialog (an inline panel): the plan states of the design. */
export function InstallView({ state, onOpen, onClose }: InstallProps) {
  const close = (label: string) => (
    <button type="button" className="fd-sm" onClick={onClose}>
      {label}
    </button>
  );
  let body: React.ReactNode;
  if (state.kind === 'reading') {
    body = <p role="status">{PLAN_LOADING_TEXT}</p>;
  } else if (state.kind === 'error') {
    body = (
      <>
        <p role="alert">{state.text}</p>
        <div className="fd-set-bar-actions">{close('Close')}</div>
      </>
    );
  } else {
    const view: PlanView = planView(state.plan);
    body = (
      <>
        {view.lines.map((line) => (
          <p key={line}>{line}</p>
        ))}
        {view.command === null ? null : <pre>{view.command}</pre>}
        {view.transaction.length === 0 ? null : (
          <>
            <p>Packages in the transaction:</p>
            <ul className="fd-dt-transaction">
              {view.transaction.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </>
        )}
        {view.open ? <p>{CONFIRM_TEXT}</p> : null}
        {state.notice === null ? null : <p role="alert">{state.notice}</p>}
        <div className="fd-set-bar-actions">
          {view.open ? (
            <>
              <button type="button" className="fd-sm" disabled={state.busy} onClick={onOpen}>
                Open installation terminal
              </button>
              {close('Cancel')}
            </>
          ) : (
            close('Close')
          )}
        </div>
      </>
    );
  }
  return (
    <section className="fd-dt-install" aria-label="Install desktop tools">
      <h3>Install desktop tools</h3>
      {body}
    </section>
  );
}

/** The date of a run, in the language of the browser. */
const dateOf = (run: MaintenanceInfo): string => new Date(run.created_at).toLocaleString();

/** The Installations list; nothing for the state `hidden`. */
export function InstallationsView({
  state,
  onOpen,
}: {
  readonly state: RunsState;
  readonly onOpen: (id: string) => void;
}) {
  if (state.kind === 'hidden') return null;
  let body: React.ReactNode;
  if (state.kind === 'loading') body = <p role="status">Loading…</p>;
  else if (state.kind === 'error') {
    body = <p role="alert">The installations could not be read.</p>;
  } else if (state.runs.length === 0) body = <p className="fd-note">No installations yet.</p>;
  else {
    body = (
      <ul className="fd-dt-runs">
        {state.runs.map((run) => (
          <li key={run.maintenance_id}>
            <span>{dateOf(run)}</span>
            <span>{run.tools.map(toolName).join(', ')}</span>
            <span>{shortState(run)}</span>
            <button type="button" className="fd-sm" onClick={() => onOpen(run.maintenance_id)}>
              {run.state === 'admitted' || run.state === 'running' ? 'Reattach' : 'Show output'}
            </button>
          </li>
        ))}
      </ul>
    );
  }
  return (
    <section className="fd-dt-installations" aria-label="Installations">
      <h3>Installations</h3>
      {body}
    </section>
  );
}

type ViewProps = {
  readonly tools: ToolsState;
  readonly runs: RunsState;
  readonly install: InstallState | null;
  /** A read or an open runs: the buttons are off. */
  readonly busy: boolean;
  readonly onRefresh: () => void;
  readonly onInstall: () => void;
  readonly onOpenTerminal: () => void;
  readonly onCloseInstall: () => void;
  readonly onOpenRun: (id: string) => void;
  /** The installation panel, when one is open. */
  readonly panel?: React.ReactNode;
};

/** The card for a state. A 501 (`hidden`) shows nothing. */
export function DesktopToolsView(props: ViewProps) {
  const { tools, busy } = props;
  if (tools.kind === 'hidden') return null;
  let body: React.ReactNode;
  if (tools.kind === 'loading') {
    body = <p role="status">{LOADING_TEXT}</p>;
  } else if (tools.kind === 'error') {
    body = (
      <div className="fd-set-notice" data-tone="error" role="alert">
        <span>{READ_ERROR_TEXT}</span>
        <button type="button" className="fd-sm" disabled={busy} onClick={props.onRefresh}>
          Refresh diagnostics
        </button>
      </div>
    );
  } else {
    body = (
      <>
        <FactsView tools={tools.tools} />
        <div className="fd-set-bar-actions">
          <button type="button" className="fd-sm" disabled={busy} onClick={props.onRefresh}>
            Refresh diagnostics
          </button>
          <button type="button" className="fd-sm" disabled={busy} onClick={props.onInstall}>
            Install desktop tools…
          </button>
        </div>
        {props.install === null ? null : (
          <InstallView
            state={props.install}
            onOpen={props.onOpenTerminal}
            onClose={props.onCloseInstall}
          />
        )}
        {props.panel ?? null}
        <InstallationsView state={props.runs} onOpen={props.onOpenRun} />
      </>
    );
  }
  return (
    <section className="fd-set-sec fd-desktop-tools" aria-label="Desktop tools">
      <h2>Desktop tools</h2>
      {body}
    </section>
  );
}

type Props = {
  /** The station name, for the path of the calls. */
  readonly station: string;
  readonly actions: DesktopToolsActions;
  readonly ask: ElevationAsk;
};

export function DesktopToolsCard({ station, actions: direct, ask }: Props) {
  const [tools, setTools] = useState<ToolsState>({ kind: 'loading' });
  const [runs, setRuns] = useState<RunsState>({ kind: 'loading' });
  const [install, setInstall] = useState<InstallState | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);
  const acting = useRef(false);
  const gate = useRef<DesktopGate['current']>(Promise.resolve());
  const requestId = useRef(makeRequestIds());
  // Every read and the open go through the one gate.
  const actions = useMemo(() => gateDesktopTools(direct, gate), [direct]);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const loadRuns = useCallback(async () => {
    try {
      const list = await actions.list(station);
      if (alive.current) setRuns({ kind: 'ready', runs: list.runs });
    } catch (e) {
      if (alive.current) setRuns(unsupported(e) ? { kind: 'hidden' } : { kind: 'error' });
    }
  }, [actions, station]);

  // The facts: read when the card mounts, and again at **Refresh diagnostics**. The list of
  // installations is read after the first successful read.
  const first = useRef(true);
  const loadTools = useCallback(async () => {
    setBusy(true);
    try {
      const read = await actions.tools(station);
      if (!alive.current) return;
      setTools({ kind: 'ready', tools: read });
      if (first.current) {
        first.current = false;
        void loadRuns();
      }
    } catch (e) {
      if (alive.current) setTools(unsupported(e) ? { kind: 'hidden' } : { kind: 'error' });
    } finally {
      if (alive.current) setBusy(false);
    }
  }, [actions, station, loadRuns]);
  useEffect(() => {
    first.current = true;
    setTools({ kind: 'loading' });
    setRuns({ kind: 'loading' });
    setInstall(null);
    setOpen(null);
    void loadTools();
  }, [loadTools]);

  const readPlan = guarded(acting, async () => {
    setInstall({ kind: 'reading' });
    try {
      const plan = await actions.plan(station);
      if (alive.current) setInstall({ kind: 'plan', plan, notice: null, busy: false });
    } catch (e) {
      if (alive.current) setInstall({ kind: 'error', text: planErrorText(e) });
    }
  });

  const openTerminal = guarded(acting, async () => {
    if (install?.kind !== 'plan' || install.plan.plan_id === null) return;
    const { plan } = install;
    const planId = plan.plan_id as string;
    setInstall({ kind: 'plan', plan, notice: null, busy: true });
    const out = await openInstall(actions, ask, station, planId, requestId.current(planId));
    if (!alive.current) return;
    if (out.kind === 'opened') {
      setInstall(null);
      setOpen(out.run.maintenance_id);
      void loadRuns();
    } else {
      setInstall({
        kind: 'plan',
        plan,
        notice: out.kind === 'failed' ? out.text : null,
        busy: false,
      });
    }
  });

  const panel =
    open === null ? null : (
      <MaintenancePanel
        key={open}
        station={station}
        runId={open}
        actions={actions}
        onDetach={() => setOpen(null)}
        onRuns={(list) => setRuns({ kind: 'ready', runs: list })}
        onTools={(read) => setTools({ kind: 'ready', tools: read })}
      />
    );

  return (
    <DesktopToolsView
      tools={tools}
      runs={runs}
      install={install}
      busy={busy}
      onRefresh={() => void loadTools()}
      onInstall={() => void readPlan()}
      onOpenTerminal={() => void openTerminal()}
      onCloseInstall={() => setInstall(null)}
      onOpenRun={setOpen}
      panel={panel}
    />
  );
}
