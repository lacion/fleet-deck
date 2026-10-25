// B12: the workflow projection of one session, read from the station in pages (design
// `workflow-agents.md` §4.6 Loading). The pages are read one after the other until `next` is
// absent: the events of earlier runs can be gone from the deck timeline (500 rows) and from the
// browser window (1,000 events). Each page is merged at once and not kept.
import { useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import type {
  DeckSession,
  EventEnvelope,
  WorkflowAgent,
  WorkflowPhase,
  WorkflowRun,
  WorkflowsCursor,
  WorkflowsPage,
  WorkflowUpdate,
} from '../gen';
import { sessionKey } from '../runtime/state';
import type { SessionActions } from './commands';
import { type WorkflowsSnapshot, type WorkflowView, workflowStates } from './workflows';

export type WorkflowsLoad = (after: WorkflowsCursor | null) => Promise<WorkflowsPage>;

export type WorkflowsState = {
  /** The merged pages; `null`: an older station, no answer, or the first load is not done. */
  readonly snapshot: WorkflowsSnapshot | null;
  readonly loading: boolean;
};

const IDLE: WorkflowsState = { snapshot: null, loading: false };
const FIRST: WorkflowsState = { snapshot: null, loading: true };

/** One run while its pages are merged: the fields of its last page and its entries by index. */
type Merging = {
  run_order: number;
  state: WorkflowUpdate;
  phases: Map<number, WorkflowPhase>;
  agents: Map<number, WorkflowAgent>;
};

/** R1 with the complete entries of a page: a later page of the same run adds its agents. */
function merge(runs: Map<number, Merging>, run: WorkflowRun): void {
  let into = runs.get(run.run_order);
  if (into === undefined) {
    into = { run_order: run.run_order, state: run.state, phases: new Map(), agents: new Map() };
    runs.set(run.run_order, into);
  }
  into.state = run.state;
  for (const p of run.state.phases) into.phases.set(p.index, p);
  for (const a of run.state.agents) into.agents.set(a.index, a);
}

const ascending = <T extends { index: number }>(map: Map<number, T>): T[] =>
  [...map.values()].sort((a, b) => a.index - b.index);

function built(runs: Map<number, Merging>): WorkflowRun[] {
  return [...runs.values()]
    .sort((a, b) => a.run_order - b.run_order)
    .map((m) => ({
      run_order: m.run_order,
      state: { ...m.state, phases: ascending(m.phases), agents: ascending(m.agents) },
    }));
}

/** True when `a` is strictly after `b` (`run_order`, then `agent_index`). */
const isAfter = (a: WorkflowsCursor, b: WorkflowsCursor): boolean =>
  a.run_order > b.run_order || (a.run_order === b.run_order && a.agent_index > b.agent_index);

/**
 * True when the browser window has dropped events after the snapshot: the oldest retained event
 * has `session_seq > as_of + 1`. The pages are then read again.
 */
export function windowPassed(
  snapshot: WorkflowsSnapshot | null,
  events: readonly EventEnvelope[],
): boolean {
  const oldest = events[0];
  return snapshot !== null && oldest !== undefined && oldest.session_seq > snapshot.as_of + 1;
}

/**
 * The pages of one session. One load runs at a time; the snapshot is replaced only when a load
 * ends, so a half-merged snapshot is never shown. Answers that arrive after `dispose` are dropped.
 */
export class WorkflowsLoader {
  private state: WorkflowsState = FIRST;
  private readonly listeners = new Set<() => void>();
  private started = false;
  private running = false;
  private disposed = false;

  constructor(
    private readonly load: WorkflowsLoad,
    private readonly name: string,
  ) {}

  getSnapshot = (): WorkflowsState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** The first load, once. A start after `dispose` takes the loader back (a remount in StrictMode). */
  start(): void {
    this.disposed = false;
    if (this.started) return;
    this.started = true;
    void this.run();
  }

  /** Read the pages again. Nothing while a load runs: that load ends first. */
  reload(): void {
    if (this.disposed || !this.started || this.running) return;
    void this.run();
  }

  dispose(): void {
    this.disposed = true;
  }

  private set(next: WorkflowsState): void {
    this.state = next;
    for (const listener of [...this.listeners]) listener();
  }

  private async run(): Promise<void> {
    this.running = true;
    this.set({ snapshot: this.state.snapshot, loading: true });
    const runs = new Map<number, Merging>();
    let asOf = Number.POSITIVE_INFINITY;
    let cursor: WorkflowsCursor | null = null;
    let incomplete = false;
    try {
      for (;;) {
        const page: WorkflowsPage = await this.load(cursor);
        if (this.disposed) return;
        asOf = Math.min(asOf, page.as_of);
        for (const run of page.workflows) merge(runs, run);
        const next: WorkflowsCursor | null = page.next ?? null;
        if (next === null) break;
        // The cursor contract guard (not a page cap): a `next` that does not advance, or a page
        // with `next` and no runs, ends the load.
        if (page.workflows.length === 0 || (cursor !== null && !isAfter(next, cursor))) {
          incomplete = true;
          console.warn(`Fleet Deck could not read every workflow of session ${this.name}.`);
          break;
        }
        cursor = next;
      }
    } catch {
      // An older station (409), a 404 or another error: the view of the timeline events only.
      if (!this.disposed) this.set({ snapshot: null, loading: false });
      return;
    } finally {
      this.running = false;
    }
    if (this.disposed) return;
    this.set({ snapshot: { as_of: asOf, runs: built(runs), incomplete }, loading: false });
  }
}

const NEVER = (): (() => void) => () => {};
const NONE = (): WorkflowsState => IDLE;

export type UseWorkflows = {
  readonly workflows: ReadonlyMap<string, WorkflowView>;
  readonly snapshot: WorkflowsSnapshot | null;
  readonly loading: boolean;
};

/**
 * The workflows of a Claude session. The pages are read when the view opens, when the action
 * exists, and again once the event window has passed `as_of`. Without the action (a provider
 * that has no workflows, an older board API) the view comes from the events alone.
 */
export function useWorkflows(
  session: DeckSession,
  events: readonly EventEnvelope[],
  actions: SessionActions | null | undefined,
): UseWorkflows {
  const load = actions?.workflows ?? null;
  const enabled = session.summary.provider === 'claude' && load !== null;
  const id = sessionKey({ station_id: session.station_id, session_id: session.summary.session_id });
  // `actions` is a new object on every render: the loader reads the latest `load`.
  const latest = useRef(load);
  latest.current = load;
  const loader = useMemo(
    () =>
      enabled
        ? new WorkflowsLoader((cursor) => {
            const current = latest.current;
            return current === null ? Promise.reject(new Error('No reader.')) : current(cursor);
          }, id)
        : null,
    [id, enabled],
  );
  useEffect(() => {
    loader?.start();
    return () => loader?.dispose();
  }, [loader]);
  const state = useSyncExternalStore(
    loader === null ? NEVER : loader.subscribe,
    loader === null ? NONE : loader.getSnapshot,
    loader === null ? NONE : loader.getSnapshot,
  );
  const { snapshot, loading } = state;
  const passed = !loading && windowPassed(snapshot, events);
  useEffect(() => {
    if (passed) loader?.reload();
  }, [passed, snapshot, loader]);
  const workflows = useMemo(() => workflowStates(snapshot, events), [snapshot, events]);
  return { workflows, snapshot, loading };
}
