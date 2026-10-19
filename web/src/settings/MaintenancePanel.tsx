// BG-1 (design `docs/v2/design/board-gaps.md` §2.6, §2.9): the installation terminal. It shows the
// terminal of one maintenance run and the state of the run. There is no close or stop control:
// **Detach** only unmounts the panel, and the installation goes on.
import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react';
import type { MaintenanceInfo, StationTools } from '../gen';
import { Terminal } from '../terminal/Terminal';
import {
  CONNECTING_TEXT,
  type DesktopToolsActions,
  GONE_TEXT,
  maintenanceTerminalActions,
  postExitLines,
  stateLines,
} from './desktopTools';

/** The read of the state of the run while the panel is open: every 5 s. */
export const PANEL_POLL_MS = 5_000;

/** What the state line shows: nothing read yet, a run, or a run that the list lacks. */
export type LineState =
  | { readonly kind: 'connecting' }
  | { readonly kind: 'run'; readonly run: MaintenanceInfo }
  | { readonly kind: 'gone' };

/** The text lines of a state line. */
export function lineTexts(state: LineState): string[] {
  if (state.kind === 'connecting') return [CONNECTING_TEXT];
  if (state.kind === 'gone') return [GONE_TEXT];
  return stateLines(state.run);
}

type ViewProps = {
  readonly state: LineState;
  /** The lines after the exit: the tools that are still missing, or installed. */
  readonly post: readonly string[];
  readonly onDetach: () => void;
  /** The terminal. */
  readonly children?: ReactNode;
};

/** The panel without its terminal logic: the title, the state lines, the terminal, **Detach**. */
export function MaintenanceView({ state, post, onDetach, children }: ViewProps) {
  return (
    <section className="fd-panel fd-maint" aria-label="Installation terminal">
      <h3>Installation terminal</h3>
      <div className="fd-maint-state" role="status">
        {lineTexts(state).map((line) => (
          <p key={line}>{line}</p>
        ))}
        {post.map((line) => (
          <p key={line}>{line}</p>
        ))}
      </div>
      <div className="fd-maint-term">{children}</div>
      <div className="fd-set-bar-actions">
        <button type="button" className="fd-sm" onClick={onDetach}>
          Detach
        </button>
      </div>
    </section>
  );
}

type Props = {
  readonly station: string;
  readonly runId: string;
  /** The calls of the card (through its gate, except `resize`). */
  readonly actions: DesktopToolsActions;
  readonly onDetach: () => void;
  /** Every list that the panel reads also updates the Installations list. */
  readonly onRuns: (runs: readonly MaintenanceInfo[]) => void;
  /** The fresh tool facts after the exit: the tool rows of the card are updated. */
  readonly onTools: (tools: StationTools) => void;
};

export function MaintenancePanel({ station, runId, actions, onDetach, onRuns, onTools }: Props) {
  const [state, setState] = useState<LineState>({ kind: 'connecting' });
  const [post, setPost] = useState<readonly string[]>([]);
  const terminal = useMemo(() => maintenanceTerminalActions(actions, station), [actions, station]);
  const latest = useRef({ actions, onRuns, onTools });
  latest.current = { actions, onRuns, onTools };

  useEffect(() => {
    let dead = false;
    let reading = false;
    let toolsRead = false;
    setState({ kind: 'connecting' });
    setPost([]);
    const afterExit = async (run: MaintenanceInfo) => {
      toolsRead = true;
      try {
        const tools = await latest.current.actions.tools(station);
        if (dead) return;
        latest.current.onTools(tools);
        setPost(postExitLines(run.tools, tools));
      } catch {
        // The tool rows stay as they were.
      }
    };
    const tick = async () => {
      if (reading) return;
      reading = true;
      try {
        const { runs } = await latest.current.actions.list(station);
        if (dead) return;
        latest.current.onRuns(runs);
        const run = runs.find((r) => r.maintenance_id === runId);
        setState(run === undefined ? { kind: 'gone' } : { kind: 'run', run });
        if (run?.state === 'exited' && !toolsRead) await afterExit(run);
      } catch {
        // A failed read keeps the last state line.
      } finally {
        reading = false;
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), PANEL_POLL_MS);
    return () => {
      dead = true;
      clearInterval(timer);
    };
  }, [station, runId]);

  return (
    <MaintenanceView state={state} post={post} onDetach={onDetach}>
      <Terminal station={station} shellId={runId} actions={terminal} active touch={false} />
    </MaintenanceView>
  );
}
