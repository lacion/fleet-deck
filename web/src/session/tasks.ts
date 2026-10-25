// B6: the tasks of a session, read from the `task.updated` events: a subagent, a background
// command. The station deduplicates the events; the board keeps the last state of each task.
import type { Event, EventEnvelope } from '../gen';
import { type Item, type Subagent, type SubagentStatus, subagentLabel } from './timeline';
import type { WorkflowView } from './workflows';

export type TaskState = {
  task_id: string;
  node_id: string | null;
  kind: string;
  status: string;
  description: string | null;
  summary: string | null;
};

export type ShownStatus = { label: string; tone: 'ok' | 'info' | 'hazard' | 'muted' };

export const isRunningStatus = (status: string): boolean => status === 'running';

/** The last `task.updated` of each task wins; the description and the summary keep the last non-null value. */
export function taskStates(events: readonly EventEnvelope[]): Map<string, TaskState> {
  const tasks = new Map<string, TaskState>();
  for (const { event } of events) {
    // A lost or ended process runs nothing more: its running tasks stop (as the timeline ends its nodes).
    const state = (event as { state?: unknown }).state;
    if (
      event.kind === 'session.ended' ||
      ((event.kind === 'session.state' || (event.kind as string) === 'session.state_changed') &&
        (state === 'lost' || state === 'exited'))
    ) {
      for (const [id, task] of tasks)
        if (isRunningStatus(task.status)) tasks.set(id, { ...task, status: 'stopped' });
      continue;
    }
    if (event.kind !== 'task.updated') continue;
    const e = event as Extract<Event, { kind: 'task.updated' }>;
    const before = tasks.get(e.task_id);
    tasks.set(e.task_id, {
      task_id: e.task_id,
      node_id: e.node_id ?? before?.node_id ?? null,
      // A notification of a task that the current process did not start has no kind: it keeps the earlier one.
      kind: e.task_kind === 'unknown' ? (before?.kind ?? e.task_kind) : e.task_kind,
      status: e.status,
      description: e.description ?? before?.description ?? null,
      summary: e.summary ?? before?.summary ?? null,
    });
  }
  return tasks;
}

/** True for the shown statuses "running" and "running in the background". */
export const isLive = (status: ShownStatus): boolean =>
  status.label === 'running' || status.label === subagentLabel('background');

/** The tone of a subagent status. */
export const subagentTone = (status: SubagentStatus): ShownStatus['tone'] => {
  if (status === 'running') return 'ok';
  if (status === 'background') return 'info';
  if (status === 'failed') return 'hazard';
  return 'muted';
};

/** The chip of a task status: `running` is ok, `failed` and `killed` are hazard, the rest is muted. */
export function taskStatus(status: string): ShownStatus {
  switch (status) {
    case 'running':
      return { label: 'running', tone: 'ok' };
    case 'completed':
      return { label: 'done', tone: 'muted' };
    case 'failed':
    case 'killed':
      return { label: 'failed', tone: 'hazard' };
    case 'stopped':
    case 'cancelled':
      return { label: 'stopped', tone: 'muted' };
    default:
      return { label: status, tone: 'muted' };
  }
}

/** The task of a subagent: among the tasks of its node, a running one wins, else the last in the map. */
export function taskOf(
  sub: Pick<Subagent, 'node_id'>,
  tasks: ReadonlyMap<string, TaskState>,
): TaskState | null {
  let last: TaskState | null = null;
  for (const task of tasks.values()) {
    if (task.node_id !== sub.node_id) continue;
    if (isRunningStatus(task.status)) return task;
    last = task;
  }
  return last;
}

/** The status of a subagent: from its task when there is one, else from the card. */
export function shownStatus(sub: Subagent, tasks: ReadonlyMap<string, TaskState>): ShownStatus {
  const task = taskOf(sub, tasks);
  // A running task of a subagent that the provider moved to the background keeps that word.
  if (task !== null && isRunningStatus(task.status) && sub.status === 'background') {
    return { label: subagentLabel('background'), tone: subagentTone('background') };
  }
  if (task !== null) return taskStatus(task.status);
  return { label: subagentLabel(sub.status), tone: subagentTone(sub.status) };
}

/** The background commands of the session. */
export function commandTasks(tasks: ReadonlyMap<string, TaskState>): TaskState[] {
  return [...tasks.values()].filter((t) => t.kind === 'local_bash');
}

/** True when the switcher has something to list (a workflow counts, B12). */
export function hasAgents(
  subagents: readonly Subagent[],
  tasks: ReadonlyMap<string, TaskState>,
  workflows?: ReadonlyMap<string, WorkflowView>,
): boolean {
  return subagents.length > 0 || commandTasks(tasks).length > 0 || (workflows?.size ?? 0) > 0;
}

/**
 * Subagents that run (by their shown status, or in the background without a task), running
 * commands and the running agents of the running workflows (B12).
 */
export function runningCount(
  subagents: readonly Subagent[],
  tasks: ReadonlyMap<string, TaskState>,
  workflows?: ReadonlyMap<string, WorkflowView>,
): number {
  const agents = subagents.filter((sub) =>
    taskOf(sub, tasks) === null
      ? sub.status === 'running' || sub.status === 'background'
      : isLive(shownStatus(sub, tasks)),
  ).length;
  let inWorkflows = 0;
  for (const w of workflows?.values() ?? []) {
    if (isRunningStatus(w.status)) inWorkflows += w.running_agents;
  }
  return agents + inWorkflows + commandTasks(tasks).filter((t) => isRunningStatus(t.status)).length;
}

/** The input summary of every tool card of the tree (also inside subagents), by node id. */
export function toolTitles(items: readonly Item[]): Map<string, string> {
  const titles = new Map<string, string>();
  const walk = (list: readonly Item[]) => {
    for (const item of list) {
      if (item.type === 'tool') titles.set(item.node_id, item.input);
      else if (item.type === 'subagent') walk(item.items);
    }
  };
  walk(items);
  return titles;
}
