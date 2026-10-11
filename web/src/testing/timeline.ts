// Timeline fixtures for the session view (M2 P4-3, PLAN D15).
//
// `recorded(...)` loads the canonical events of a recorded Claude or Codex
// fixture; the Rust tests `the_board_timeline_fixtures_equal_the_canonical_replay`
// (claude_core_replay, codex_core_replay) keep those files equal to the
// production cores' replays. `nestedSubagents()`
// is a typed synthetic timeline with foreground subagents two levels deep,
// child tools, child text, a child approval and a failed child: the shape the
// Claude core produces from `parent_tool_use_id` frames (§6.3).
import type { Capabilities, Event, EventEnvelope } from '../gen';
import approve from './fixtures/claude-tool_call_approve.events.json' with { type: 'json' };
import subagent from './fixtures/claude-subagent.events.json' with { type: 'json' };
import codexSubagent from './fixtures/codex-subagent.events.json' with { type: 'json' };
import codexApprove from './fixtures/codex-tool_call_approve.events.json' with { type: 'json' };

const RECORDED = {
  subagent,
  tool_call_approve: approve,
  'codex-subagent': codexSubagent,
  'codex-tool_call_approve': codexApprove,
};

export function recorded(name: keyof typeof RECORDED): EventEnvelope[] {
  // SAFETY: the Rust tests above write these files from typed `EventEnvelope`
  // values; JSON imports widen string unions, so TypeScript cannot see it.
  return RECORDED[name] as unknown as EventEnvelope[];
}

export function envelopes(sessionId: string, events: readonly Event[]): EventEnvelope[] {
  return events.map((event, i) => ({
    station_seq: 500 + i + 1,
    session_id: sessionId,
    session_seq: i + 1,
    at: 1_760_000_000_000 + 1000 * (i + 1),
    proto_version: 1,
    raw_ref: null,
    event,
  }));
}

const refs = {};
const tool = (
  node_id: string,
  parent_node_id: string | null,
  tool_kind: Extract<Event, { kind: 'tool.started' }>['tool_kind'],
  title: string,
  input_summary: string,
): Event => ({
  kind: 'tool.started',
  node_id,
  parent_node_id,
  tool_kind,
  title,
  input_summary,
  files: [],
  provider_refs: refs,
});
const done = (node_id: string, status: 'ok' | 'failed' = 'ok'): Event => ({
  kind: 'tool.completed',
  node_id,
  status,
  files: [],
  exit_code: null,
});
const text = (parent: string | null, value: string): Event => ({
  kind: 'message.completed',
  turn_id: 'turn-1',
  parent_node_id: parent,
  text: value,
});

/** Events up to and including `upTo` (all by default). */
export function nestedSubagents(upTo?: number): EventEnvelope[] {
  const events: Event[] = [
    { kind: 'run.started', run_id: 'run-1', input_summary: 'review the repo', origin: 'board' },
    { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: refs },
    tool('node-1', null, 'subagent', 'Agent', 'Explore the code'),
    tool('node-2', null, 'subagent', 'Agent', 'Check the tests'),
    tool('node-3', 'node-1', 'read', 'Read', 'src/main.rs'),
    { kind: 'message.delta', turn_id: 'turn-1', parent_node_id: 'node-1', text: 'main ' },
    { kind: 'message.delta', turn_id: 'turn-1', parent_node_id: 'node-1', text: 'is small' },
    done('node-3'),
    tool('node-4', 'node-1', 'subagent', 'Agent', 'Read the docs'),
    tool('node-5', 'node-4', 'shell', 'Bash', 'ls docs'),
    {
      kind: 'approval.requested',
      request_id: 'req-6',
      node_id: 'node-5',
      request_kind: 'command',
      title: 'Bash',
      details: 'ls docs',
      input: { command: 'ls docs' },
      options: [
        { option_id: 'allow', label: 'Allow' },
        { option_id: 'deny', label: 'Deny' },
      ],
      response: 'rpc_reply',
      provider_refs: refs,
    },
    // Up to index 10: three subagents run; a grandchild approval is open.
    {
      kind: 'approval.resolved',
      request_id: 'req-6',
      option_id: 'allow',
      by: 'board',
    },
    { kind: 'tool.output', node_id: 'node-5', chunk: 'PLAN.md\n', truncated: false },
    done('node-5'),
    text('node-4', 'docs are fine'),
    done('node-4'),
    done('node-2', 'failed'),
    // Up to index 17: only node-1 still runs.
    text('node-1', 'main is small and tested'),
    done('node-1'),
    text(null, 'All subagents reported.'),
  ];
  return envelopes('nested', upTo === undefined ? events : events.slice(0, upTo + 1));
}

/** The capabilities of a recorded session (no bypass mode, no full access). */
export function recordedCapabilities(name: keyof typeof RECORDED): Capabilities {
  const first = recorded(name)[0]?.event as Event | undefined;
  if (first?.kind !== 'session.started') throw new Error('the fixture starts a session');
  return first.capabilities;
}

/** The capabilities of the recorded Claude session (no bypass mode). */
export function claudeCapabilities(): Capabilities {
  return recordedCapabilities('subagent');
}
