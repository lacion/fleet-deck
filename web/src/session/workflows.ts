// B12: the workflows of a session (Claude `Workflow` tool runs) and their agents. The state comes
// from two sources that follow the same rules (design `workflow-agents.md` §4.3a): the pages of
// `session.workflows` (the station projection, complete) and the `workflow.updated` events that
// came after the pages. An older station gives no pages: then only `task.updated` events show
// the workflow itself (`task_only`), and no agents.
import type { EventEnvelope, WorkflowAgent, WorkflowRun, WorkflowUpdate } from '../gen';
import type { ShownStatus } from './tasks';

/** R1: a run keeps at most this many phases; a new index beyond is ignored. */
export const MAX_PHASES = 100;
/** R1: a run keeps at most this many agents; a new index beyond is ignored. */
export const MAX_AGENTS = 1000;

export type WorkflowAgentView = {
  index: number;
  label: string;
  state: string;
  phase_index: number | null;
  agent_id: string | null;
  model: string | null;
  attempt: number | null;
  tool_calls: number | null;
  tokens: number | null;
  last_tool: string | null;
  prompt_preview: string | null;
  result_preview: string | null;
  error: string | null;
  started_at: number | null;
};

export type WorkflowView = {
  task_id: string;
  node_id: string | null;
  name: string | null;
  description: string | null;
  /** running | completed | failed | stopped | the provider's text. */
  status: string;
  untracked: boolean;
  /** Index to title. */
  phases: Map<number, string>;
  agents: Map<number, WorkflowAgentView>;
  running_agents: number;
  agents_total: number;
  /** 'station': from the page or a `workflow.updated`; 'task_only': only `task.updated` events. */
  source: 'station' | 'task_only';
};

/**
 * The merged pages of `session.workflows`: `as_of` is the smallest `as_of` of the pages.
 * `incomplete` is true only when the station broke the cursor contract (Loading).
 */
export type WorkflowsSnapshot = { as_of: number; runs: WorkflowRun[]; incomplete: boolean };

const isRunning = (status: string): boolean => status === 'running';

const agentView = (a: WorkflowAgent): WorkflowAgentView => ({
  index: a.index,
  label: a.label,
  state: a.state,
  phase_index: a.phase_index ?? null,
  agent_id: a.agent_id ?? null,
  model: a.model ?? null,
  attempt: a.attempt ?? null,
  tool_calls: a.tool_calls ?? null,
  tokens: a.tokens ?? null,
  last_tool: a.last_tool ?? null,
  prompt_preview: a.prompt_preview ?? null,
  result_preview: a.result_preview ?? null,
  error: a.error ?? null,
  started_at: a.started_at ?? null,
});

const blank = (task_id: string, source: WorkflowView['source']): WorkflowView => ({
  task_id,
  node_id: null,
  name: null,
  description: null,
  status: 'running',
  untracked: false,
  phases: new Map(),
  agents: new Map(),
  running_agents: 0,
  agents_total: 0,
  source,
});

/** R2: a run that is not running has no queued or running agent. */
function stopRun(w: WorkflowView): void {
  for (const [index, a] of w.agents) {
    if (a.state === 'queued' || a.state === 'running')
      w.agents.set(index, { ...a, state: 'stopped' });
  }
  w.running_agents = 0;
}

/** R1: one `workflow.updated` (or one run of a page). */
function applyUpdate(views: Map<string, WorkflowView>, u: WorkflowUpdate): void {
  let w = views.get(u.task_id);
  if (w === undefined) {
    w = blank(u.task_id, 'station');
    views.set(u.task_id, w);
  }
  w.source = 'station';
  w.status = u.status;
  w.untracked = u.untracked === true;
  w.running_agents = u.running_agents;
  w.agents_total = u.agents_total;
  if (u.node_id !== undefined) w.node_id = u.node_id;
  if (u.name !== undefined) w.name = u.name;
  for (const p of u.phases) {
    if (w.phases.has(p.index) || w.phases.size < MAX_PHASES) w.phases.set(p.index, p.title);
  }
  for (const a of u.agents) {
    if (w.agents.has(a.index) || w.agents.size < MAX_AGENTS) w.agents.set(a.index, agentView(a));
  }
  if (!isRunning(w.status)) stopRun(w);
}

/** The `task.updated` status of a workflow task, in the words of a workflow status. */
function taskWorkflowStatus(status: string): string {
  switch (status) {
    case 'failed':
    case 'killed':
      return 'failed';
    case 'cancelled':
      return 'stopped';
    default:
      return status;
  }
}

/** R3 and R4: every running run that `keep` does not name stops. */
function stopRunning(views: Map<string, WorkflowView>, keep?: ReadonlySet<string>): void {
  for (const w of views.values()) {
    if (!isRunning(w.status) || keep?.has(w.task_id) === true) continue;
    w.status = 'stopped';
    stopRun(w);
  }
}

/**
 * Starts from `snapshot` (null: an older station or no answer) and applies, in order, every
 * event with `session_seq > snapshot.as_of` (every event when null) with the rules R1 to R5
 * (design §4.3a). `task.updated` with kind `local_workflow` creates a `task_only` workflow or
 * updates its description and node (status: running→running, completed→completed,
 * failed/killed→failed, stopped/cancelled→stopped, for a `task_only` workflow only). The map is
 * in the order of the runs: the pages first, then the runs that the events start.
 */
export function workflowStates(
  snapshot: WorkflowsSnapshot | null,
  events: readonly EventEnvelope[],
): Map<string, WorkflowView> {
  const views = new Map<string, WorkflowView>();
  if (snapshot !== null) for (const run of snapshot.runs) applyUpdate(views, run.state);
  const from = snapshot?.as_of ?? Number.NEGATIVE_INFINITY;
  for (const { event, session_seq } of events) {
    if (session_seq <= from) continue;
    switch (event.kind) {
      case 'workflow.updated':
        applyUpdate(views, event);
        break;
      case 'background.updated':
        stopRunning(views, new Set(event.tasks.map((t) => t.task_id)));
        break;
      case 'session.ended':
        stopRunning(views);
        break;
      case 'task.updated': {
        const known = views.get(event.task_id);
        if (event.task_kind !== 'local_workflow' && known === undefined) break;
        const w = known ?? blank(event.task_id, 'task_only');
        views.set(event.task_id, w);
        if (event.description !== undefined) w.description = event.description;
        if (event.node_id !== undefined) w.node_id = event.node_id;
        if (w.source === 'task_only') {
          w.status = taskWorkflowStatus(event.status);
          if (!isRunning(w.status)) stopRun(w);
        }
        break;
      }
      default: {
        // The kind may be one that this board does not know (`session.state_changed`).
        const state = (event as { state?: unknown }).state;
        const kind = event.kind as string;
        if (
          (kind === 'session.state' || kind === 'session.state_changed') &&
          (state === 'lost' || state === 'exited')
        ) {
          stopRunning(views);
        }
      }
    }
  }
  return views;
}

/** request_id to agent_id, from `approval.requested.provider_refs.agent_id`. */
export function requestAgents(events: readonly EventEnvelope[]): Map<string, string> {
  const agents = new Map<string, string>();
  for (const { event } of events) {
    if (event.kind !== 'approval.requested') continue;
    const id = event.provider_refs['agent_id'];
    if (typeof id === 'string' && id !== '') agents.set(event.request_id, id);
  }
  return agents;
}

/** The agent with this id in any workflow, or null. */
export function agentById(
  workflows: ReadonlyMap<string, WorkflowView>,
  agentId: string,
): WorkflowAgentView | null {
  for (const w of workflows.values()) {
    for (const a of w.agents.values()) if (a.agent_id === agentId) return a;
  }
  return null;
}

/** The status chip of an agent state: running ok; queued info; failed hazard; done, stopped and other text muted. */
export function agentStatus(state: string): ShownStatus {
  switch (state) {
    case 'running':
      return { label: 'running', tone: 'ok' };
    case 'queued':
      return { label: 'queued', tone: 'info' };
    case 'failed':
      return { label: 'failed', tone: 'hazard' };
    default:
      return { label: state, tone: 'muted' };
  }
}

/** The agents of all workflows that the switcher counts. */
export function workflowAgentTotal(workflows: ReadonlyMap<string, WorkflowView>): number {
  let n = 0;
  for (const w of workflows.values()) n += w.agents_total;
  return n;
}

/** The name of a workflow in the switcher: its script name, else its description. */
export const workflowTitle = (w: Pick<WorkflowView, 'name' | 'description'>): string =>
  firstLine(w.name ?? '') || firstLine(w.description ?? '');

/** The first line of a text that has words, trimmed; '' when there is none. */
export const firstLine = (text: string): string =>
  text
    .split('\n')
    .map((line) => line.trim())
    .find((line) => line !== '') ?? '';

/** The `?agent=` value of a workflow agent: `wf:<task id>:<index>`. */
export const workflowAgentParam = (task_id: string, index: number): string =>
  `wf:${task_id}:${index}`;

/** The workflow agent that an `?agent=` value names; null for any other value. */
export function parseWorkflowAgentParam(
  param: string | undefined,
): { task_id: string; index: number } | null {
  if (param === undefined || !param.startsWith('wf:')) return null;
  const rest = param.slice(3);
  const cut = rest.lastIndexOf(':');
  if (cut <= 0) return null;
  const digits = rest.slice(cut + 1);
  if (!/^\d{1,9}$/.test(digits)) return null;
  return { task_id: rest.slice(0, cut), index: Number(digits) };
}
