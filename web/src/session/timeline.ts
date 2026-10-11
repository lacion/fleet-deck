// M2 P4-3: the session timeline as a tree (PLAN D15, §6.3).
//
// A subagent is a `tool.started` with `tool_kind: subagent`. Every event with
// a `parent_node_id` belongs to the timeline of that subagent, so children
// nest to any depth. A subagent that the provider moves to the background
// completes its tool call at once; the next unbound subagent task of
// `background.updated` belongs to it until the task leaves the list (DL-060).
import type { ApprovalOption, Event, EventEnvelope, ToolKind, TurnStatus } from '../gen';

export type SubagentStatus = 'running' | 'background' | 'done' | 'failed' | 'ended';

export type Item =
  | { type: 'run'; key: string; run_id: string; input: string; origin: string }
  | { type: 'message'; key: string; text: string; done: boolean }
  | {
      type: 'tool';
      key: string;
      node_id: string;
      tool_kind: ToolKind;
      title: string;
      input: string;
      output: string;
      status: 'running' | 'ok' | 'failed' | 'denied' | 'ended';
    }
  | Subagent
  | {
      type: 'approval';
      key: string;
      request_id: string;
      title: string;
      details: string | null;
      options: readonly ApprovalOption[];
      state: 'open' | 'answered' | 'expired';
      outcome: string | null;
    }
  | { type: 'turn'; key: string; status: TurnStatus }
  | { type: 'notice'; key: string; text: string; tone: 'info' | 'error' };

export type Subagent = {
  type: 'subagent';
  key: string;
  node_id: string;
  title: string;
  input: string;
  status: SubagentStatus;
  task_id: string | null;
  items: Item[];
};

export type SessionTimeline = {
  readonly items: readonly Item[];
  /** Every subagent of the tree, outermost first. */
  readonly subagents: readonly Subagent[];
  /** Subagents that run now, in the foreground or the background. */
  readonly running: number;
};

/** Background task kinds that are subagents (`SUBAGENT_TASK_KINDS` in Rust). */
export const SUBAGENT_TASK_KINDS: readonly string[] = ['local_agent', 'remote_agent'];

type Open = { item: Extract<Item, { type: 'message' }>; turn: string };

/** Builds the tree from a timeline in `session_seq` order. */
export function buildTimeline(events: readonly EventEnvelope[]): SessionTimeline {
  const root: Item[] = [];
  const nodes = new Map<string, Extract<Item, { type: 'tool' }> | Subagent>();
  const where = new Map<string, Item[]>(); // node id → the list it lives in
  const subagents: Subagent[] = [];
  const messages = new Map<string, Open>(); // container node id ('' = root) → open message
  const approvals = new Map<string, Extract<Item, { type: 'approval' }>>();
  const allMessages: Extract<Item, { type: 'message' }>[] = [];
  let tasks: string[] = []; // the current subagent task ids
  // Background subagents that the previous event finished by an empty task
  // list. An old station recovery wrote that list just before the loss, so
  // a loss that follows at once ends them instead (M2 gate correction).
  let justFinished: Subagent[] = [];

  // A lost or ended process runs nothing more: every running node ends,
  // open messages close with their text, open requests expire. Terminal
  // statuses stay as they are.
  const endAll = (requestOutcome: string) => {
    for (const node of nodes.values()) {
      if (node.type === 'tool' && node.status === 'running') node.status = 'ended';
      if (node.type === 'subagent' && (node.status === 'running' || node.status === 'background'))
        node.status = 'ended';
    }
    for (const message of allMessages) message.done = true;
    messages.clear();
    for (const a of approvals.values()) {
      if (a.state === 'open') Object.assign(a, { state: 'expired', outcome: requestOutcome });
    }
    tasks = [];
  };

  const list = (parent: string | null): Item[] => {
    if (parent === null) return root;
    const node = nodes.get(parent);
    return node?.type === 'subagent' ? node.items : root;
  };

  for (const env of events) {
    const event = env.event as Event | { kind: string };
    const key = `${env.session_seq}`;
    const previousFinished = justFinished;
    justFinished = [];
    switch (event.kind) {
      case 'run.started': {
        const e = event as Extract<Event, { kind: 'run.started' }>;
        root.push({
          type: 'run',
          key,
          run_id: e.run_id,
          input: e.input_summary,
          origin: e.origin,
        });
        messages.delete('');
        break;
      }
      case 'message.delta':
      case 'message.completed': {
        const e = event as Extract<Event, { kind: 'message.delta' | 'message.completed' }>;
        const container = e.parent_node_id ?? '';
        let open = messages.get(container);
        if (!open || open.turn !== e.turn_id || open.item.done) {
          const item = { type: 'message' as const, key, text: '', done: false };
          list(e.parent_node_id).push(item);
          allMessages.push(item);
          open = { item, turn: e.turn_id };
          messages.set(container, open);
        }
        if (e.kind === 'message.completed') {
          open.item.text = e.text;
          open.item.done = true;
        } else {
          open.item.text += e.text;
        }
        break;
      }
      case 'tool.started': {
        const e = event as Extract<Event, { kind: 'tool.started' }>;
        const parent = list(e.parent_node_id);
        messages.delete(e.parent_node_id ?? '');
        if (e.tool_kind === 'subagent') {
          const sub: Subagent = {
            type: 'subagent',
            key,
            node_id: e.node_id,
            title: e.title,
            input: e.input_summary,
            status: 'running',
            task_id: null,
            items: [],
          };
          nodes.set(e.node_id, sub);
          subagents.push(sub);
          parent.push(sub);
        } else {
          const tool = {
            type: 'tool' as const,
            key,
            node_id: e.node_id,
            tool_kind: e.tool_kind,
            title: e.title,
            input: e.input_summary,
            output: '',
            status: 'running' as const,
          };
          nodes.set(e.node_id, tool);
          parent.push(tool);
        }
        where.set(e.node_id, parent);
        break;
      }
      case 'tool.output': {
        const e = event as Extract<Event, { kind: 'tool.output' }>;
        const node = nodes.get(e.node_id);
        if (node?.type === 'tool') node.output = (node.output + e.chunk).slice(-4000);
        break;
      }
      case 'tool.completed': {
        const e = event as Extract<Event, { kind: 'tool.completed' }>;
        const node = nodes.get(e.node_id);
        if (node?.type === 'tool') node.status = e.status;
        if (node?.type === 'subagent' && node.status === 'running') {
          const bound = new Set(subagents.map((s) => s.task_id));
          const task = e.status === 'ok' ? tasks.find((t) => !bound.has(t)) : undefined;
          if (task !== undefined) {
            node.status = 'background';
            node.task_id = task;
          } else {
            node.status = e.status === 'ok' ? 'done' : 'failed';
          }
        }
        break;
      }
      case 'background.updated': {
        const e = event as Extract<Event, { kind: 'background.updated' }>;
        tasks = e.tasks
          .filter((t) => SUBAGENT_TASK_KINDS.includes(t.task_kind))
          .map((t) => t.task_id);
        for (const sub of subagents) {
          if (sub.status === 'background' && sub.task_id && !tasks.includes(sub.task_id)) {
            sub.status = 'done';
            if (e.tasks.length === 0) justFinished.push(sub);
          }
        }
        break;
      }
      // `session.state` is the wire kind of Rust `SessionStateChanged`. The
      // variant name is read too, so a hand-written sequence that uses it
      // gets the same cleanup (DL-070).
      case 'session.state':
      case 'session.state_changed': {
        const e = event as Extract<Event, { kind: 'session.state' }>;
        if (e.state !== 'lost' && e.state !== 'exited') break;
        for (const sub of previousFinished) if (sub.status === 'done') sub.status = 'ended';
        endAll(e.state === 'lost' ? 'session lost' : 'session ended');
        break;
      }
      case 'approval.requested': {
        const e = event as Extract<Event, { kind: 'approval.requested' }>;
        const parentList = (e.node_id && where.get(e.node_id)) || root;
        const item = {
          type: 'approval' as const,
          key,
          request_id: e.request_id,
          title: e.title,
          details: e.details,
          options: e.options,
          state: 'open' as const,
          outcome: null,
        };
        approvals.set(e.request_id, item);
        parentList.push(item);
        break;
      }
      case 'approval.resolved': {
        const e = event as Extract<Event, { kind: 'approval.resolved' }>;
        const item = approvals.get(e.request_id);
        if (item) {
          const label = item.options.find((o) => o.option_id === e.option_id)?.label;
          Object.assign(item, {
            state: 'answered',
            outcome: `${label ?? e.option_id} (by ${e.by})`,
          });
        }
        break;
      }
      case 'approval.expired': {
        const e = event as Extract<Event, { kind: 'approval.expired' }>;
        const item = approvals.get(e.request_id);
        if (item) Object.assign(item, { state: 'expired', outcome: `expired: ${e.reason}` });
        break;
      }
      case 'turn.completed': {
        const e = event as Extract<Event, { kind: 'turn.completed' }>;
        if (e.status !== 'done') root.push({ type: 'turn', key, status: e.status });
        break;
      }
      case 'mode.changed': {
        const e = event as Extract<Event, { kind: 'mode.changed' }>;
        root.push({ type: 'notice', key, text: `Mode: ${e.mode}`, tone: 'info' });
        break;
      }
      case 'interrupt.result': {
        const e = event as Extract<Event, { kind: 'interrupt.result' }>;
        const text = e.accepted
          ? 'Interrupt accepted.'
          : `Interrupt refused${e.message ? `: ${e.message}` : '.'}`;
        root.push({ type: 'notice', key, text, tone: e.accepted ? 'info' : 'error' });
        break;
      }
      case 'error': {
        const e = event as Extract<Event, { kind: 'error' }>;
        root.push({ type: 'notice', key, text: `${e.code}: ${e.message}`, tone: 'error' });
        break;
      }
      case 'session.ended': {
        endAll('session ended');
        root.push({ type: 'notice', key, text: 'The session ended.', tone: 'info' });
        break;
      }
      default:
        break;
    }
  }
  const running = subagents.filter((s) => s.status === 'running' || s.status === 'background');
  return { items: root, subagents, running: running.length };
}

/** The status word of a subagent for the tree and for screen readers. */
export function subagentLabel(status: SubagentStatus): string {
  switch (status) {
    case 'running':
      return 'running';
    case 'background':
      return 'running in the background';
    case 'done':
      return 'done';
    case 'failed':
      return 'failed';
    case 'ended':
      return 'stopped';
  }
}

/** Running subagents of a card: foreground subagent tools plus background agents. */
export function runningSubagents(summary: {
  running_tools: readonly { tool_kind: ToolKind }[];
  background_agents: number;
}): number {
  return (
    summary.running_tools.filter((t) => t.tool_kind === 'subagent').length +
    summary.background_agents
  );
}
