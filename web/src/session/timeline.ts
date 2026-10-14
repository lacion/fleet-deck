// M2 P4-3: the session timeline as a tree (PLAN D15, §6.3).
//
// A subagent is a `tool.started` with `tool_kind: subagent`. Every event with
// a `parent_node_id` belongs to the timeline of that subagent, so children
// nest to any depth. A subagent that the provider moves to the background
// completes its tool call at once; the next unbound subagent task of
// `background.updated` belongs to it until the task leaves the list (DL-060).
import type {
  ApprovalOption,
  ArtifactKind,
  DiffSummary,
  Event,
  EventEnvelope,
  ModelChoice,
  ToolKind,
  TurnStatus,
} from '../gen';

export type SubagentStatus = 'running' | 'background' | 'done' | 'failed' | 'ended';

export type Item =
  | {
      type: 'run';
      key: string;
      run_id: string;
      input: string;
      origin: string;
      /** `member:<name>` when a member typed the input (R11.5); the owner has none. */
      author?: string;
      /** Unix ms of the run start; set with `author`. */
      at?: number;
      /** The input of the station that started the run (DL-179: it ties a run to a review). */
      input_id?: string;
    }
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
      /** Files of the call: deduplicated, first-seen order. */
      files: string[];
      exit_code: number | null;
    }
  | { type: 'reasoning'; key: string; turn_id: string; text: string; done: boolean }
  | { type: 'plan'; key: string; plan_id: string; markdown: string }
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
      /** `member:<name>` when a member answered (R11.5); the owner has none. */
      answered_by?: string;
    }
  | { type: 'turn'; key: string; status: TurnStatus; turn_id: string; diff: DiffSummary | null }
  | { type: 'notice'; key: string; text: string; tone: 'info' | 'error' }
  // R12 (DL-165): a published artifact, and a file that the station suggests to keep.
  | {
      type: 'artifact';
      key: string;
      artifact_id: string;
      version: number;
      title: string;
      artifact_kind: ArtifactKind;
    }
  | { type: 'suggestion'; key: string; path: string };

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

/** The commands of the newest `session.slash_commands` event, or null. */
export function slashCommands(events: readonly EventEnvelope[]): readonly string[] | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i]?.event as Event | { kind: string } | undefined;
    if (event?.kind === 'session.slash_commands') {
      return (event as Extract<Event, { kind: 'session.slash_commands' }>).commands;
    }
  }
  return null;
}

/** The models of the newest `session.models` event, or null. */
export function sessionModels(events: readonly EventEnvelope[]): readonly ModelChoice[] | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i]?.event as Event | { kind: string } | undefined;
    if (event?.kind === 'session.models') {
      return (event as Extract<Event, { kind: 'session.models' }>).models;
    }
  }
  return null;
}

/** Builds the tree from a timeline in `session_seq` order. */
export function buildTimeline(events: readonly EventEnvelope[]): SessionTimeline {
  const root: Item[] = [];
  const nodes = new Map<string, Extract<Item, { type: 'tool' }> | Subagent>();
  const where = new Map<string, Item[]>(); // node id → the list it lives in
  const subagents: Subagent[] = [];
  const messages = new Map<string, Open>(); // container node id ('' = root) → open message
  const approvals = new Map<string, Extract<Item, { type: 'approval' }>>();
  const allMessages: Extract<Item, { type: 'message' }>[] = [];
  const allReasoning: Extract<Item, { type: 'reasoning' }>[] = [];
  const openReasoning = new Map<Item[], Extract<Item, { type: 'reasoning' }>>(); // list → open item
  const turns = new Map<string, Extract<Item, { type: 'turn' }>>();
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
    for (const reasoning of allReasoning) reasoning.done = true;
    messages.clear();
    openReasoning.clear();
    for (const a of approvals.values()) {
      if (a.state === 'open') Object.assign(a, { state: 'expired', outcome: requestOutcome });
    }
    tasks = [];
  };

  // Any item that joins a list after open reasoning ends that reasoning.
  const add = (target: Item[], item: Item) => {
    const open = openReasoning.get(target);
    if (open) {
      open.done = true;
      openReasoning.delete(target);
    }
    target.push(item);
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
        add(root, {
          type: 'run',
          key,
          run_id: e.run_id,
          input: e.input_summary,
          origin: e.origin,
          ...(e.author === undefined ? {} : { author: e.author, at: env.at }),
          ...(e.input_id === undefined ? {} : { input_id: e.input_id }),
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
          add(list(e.parent_node_id), item);
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
          add(parent, sub);
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
            files: [...new Set(e.files)],
            exit_code: null,
          };
          nodes.set(e.node_id, tool);
          add(parent, tool);
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
        if (node?.type === 'tool') {
          node.status = e.status;
          node.exit_code = e.exit_code;
          node.files = [...new Set([...node.files, ...e.files])];
        }
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
      // ADR-007 §2.5: a skipped restart continuation shows its reason.
      case 'session.auto_continue_skipped': {
        const e = event as Extract<Event, { kind: 'session.auto_continue_skipped' }>;
        add(root, {
          type: 'notice',
          key,
          text: `Auto-continue skipped: ${e.reason.replaceAll('_', ' ')}`,
          tone: 'info',
        });
        break;
      }
      // R6.6: the station stopped an idle session; the next prompt resumes it.
      case 'session.hibernated': {
        const e = event as Extract<Event, { kind: 'session.hibernated' }>;
        add(root, {
          type: 'notice',
          key,
          text: `Hibernated after ${e.after_min} min idle. Send a prompt to resume it.`,
          tone: 'info',
        });
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
        add(parentList, item);
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
            ...(e.answered_by === undefined ? {} : { answered_by: e.answered_by }),
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
        const known = turns.get(e.turn_id);
        if (known) {
          known.status = e.status;
        } else if (e.status !== 'done') {
          const item = {
            type: 'turn' as const,
            key,
            status: e.status,
            turn_id: e.turn_id,
            diff: null,
          };
          turns.set(e.turn_id, item);
          add(root, item);
        }
        break;
      }
      case 'turn.finalized': {
        const e = event as Extract<Event, { kind: 'turn.finalized' }>;
        const known = turns.get(e.turn_id);
        if (known) {
          known.diff = e.diff_summary;
        } else {
          const item = {
            type: 'turn' as const,
            key,
            status: 'done' as const,
            turn_id: e.turn_id,
            diff: e.diff_summary,
          };
          turns.set(e.turn_id, item);
          add(root, item);
        }
        break;
      }
      case 'reasoning.delta': {
        const e = event as Extract<Event, { kind: 'reasoning.delta' }>;
        const open = openReasoning.get(root);
        if (open && open.turn_id === e.turn_id) {
          open.text += e.text;
        } else {
          const item = {
            type: 'reasoning' as const,
            key,
            turn_id: e.turn_id,
            text: e.text,
            done: false,
          };
          add(root, item);
          allReasoning.push(item);
          openReasoning.set(root, item);
        }
        break;
      }
      case 'plan.proposed': {
        const e = event as Extract<Event, { kind: 'plan.proposed' }>;
        add(root, { type: 'plan', key, plan_id: e.plan_id, markdown: e.markdown });
        break;
      }
      case 'file.changed': {
        const e = event as Extract<Event, { kind: 'file.changed' }>;
        const node = e.node_id === null ? undefined : nodes.get(e.node_id);
        if (node?.type === 'tool') {
          if (!node.files.includes(e.path)) node.files.push(e.path);
        } else {
          add(root, { type: 'notice', key, text: `${e.path} ${e.change}`, tone: 'info' });
        }
        break;
      }
      case 'artifact.published': {
        const e = event as Extract<Event, { kind: 'artifact.published' }>;
        add(root, {
          type: 'artifact',
          key,
          artifact_id: e.artifact_id,
          version: e.version,
          title: e.title,
          artifact_kind: e.artifact_kind,
        });
        break;
      }
      case 'artifact.suggested': {
        const e = event as Extract<Event, { kind: 'artifact.suggested' }>;
        add(root, { type: 'suggestion', key, path: e.path });
        break;
      }
      case 'model.changed': {
        const e = event as Extract<Event, { kind: 'model.changed' }>;
        add(root, { type: 'notice', key, text: `Model changed to ${e.model}`, tone: 'info' });
        break;
      }
      case 'session.output_gap': {
        const e = event as Extract<Event, { kind: 'session.output_gap' }>;
        const text = `Some output is missing here (events ${e.from}–${e.to}).`;
        add(root, { type: 'notice', key, text, tone: 'info' });
        break;
      }
      case 'session.terminal': {
        const e = event as Extract<Event, { kind: 'session.terminal' }>;
        const text =
          e.shell_id == null
            ? `Returned to board control${e.reason ? `: ${e.reason}` : ''}`
            : 'Opened in a terminal.';
        add(root, { type: 'notice', key, text, tone: 'info' });
        break;
      }
      case 'session.notice': {
        const e = event as Extract<Event, { kind: 'session.notice' }>;
        add(root, { type: 'notice', key, text: e.text, tone: 'info' });
        break;
      }
      case 'mode.changed': {
        const e = event as Extract<Event, { kind: 'mode.changed' }>;
        add(root, { type: 'notice', key, text: `Mode: ${e.mode}`, tone: 'info' });
        break;
      }
      case 'interrupt.result': {
        const e = event as Extract<Event, { kind: 'interrupt.result' }>;
        const text = e.accepted
          ? 'Interrupt accepted.'
          : `Interrupt refused${e.message ? `: ${e.message}` : '.'}`;
        add(root, { type: 'notice', key, text, tone: e.accepted ? 'info' : 'error' });
        break;
      }
      case 'error': {
        const e = event as Extract<Event, { kind: 'error' }>;
        add(root, { type: 'notice', key, text: `${e.code}: ${e.message}`, tone: 'error' });
        break;
      }
      case 'session.ended': {
        endAll('session ended');
        add(root, { type: 'notice', key, text: 'The session ended.', tone: 'info' });
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
