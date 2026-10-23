import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Event, EventEnvelope } from '../gen';
import { hasAgents, taskStates, toolTitles } from '../session/tasks';
import { buildTimeline } from '../session/timeline';
import { envelopes, recorded } from '../testing/timeline';
import {
  AgentHead,
  AgentsMenu,
  commandName,
  commandsText,
  subagentName,
  summaryNote,
} from './AgentsMenu';

function task(
  task_id: string,
  status: string,
  more: Partial<Extract<Event, { kind: 'task.updated' }>> = {},
): Event {
  return { kind: 'task.updated', task_id, task_kind: 'local_bash', status, ...more };
}

function shell(node_id: string, command: string): Event {
  return {
    kind: 'tool.started',
    node_id,
    parent_node_id: null,
    tool_kind: 'shell',
    title: 'Bash',
    input_summary: command,
    files: [],
    provider_refs: {},
  };
}

function agent(node_id: string, input: string): Event {
  return {
    kind: 'tool.started',
    node_id,
    parent_node_id: null,
    tool_kind: 'subagent',
    title: 'Agent',
    input_summary: input,
    files: [],
    provider_refs: {},
  };
}

function menu(events: readonly EventEnvelope[], agentId: string | null = null) {
  const tree = buildTimeline(events);
  const tasks = taskStates(events);
  return {
    html: renderToStaticMarkup(
      <AgentsMenu
        subagents={tree.subagents}
        tasks={tasks}
        toolTitles={toolTitles(tree.items)}
        agent={agentId}
        onChoose={() => {}}
      />,
    ),
    tree,
    tasks,
  };
}

/** The `data-agent` and `data-task` ids of the rows, in their order. */
const rowIds = (html: string): string[] =>
  [...html.matchAll(/data-(?:agent|task)="([^"]+)"/g)].map((m) => m[1] ?? '');

describe('the agents menu', () => {
  test('the Claude fixture lists the main chat and the subagent by its task, with its status', () => {
    const { html, tree, tasks } = menu(recorded('subagent'));
    expect(hasAgents(tree.subagents, tasks)).toBe(true);
    expect(html).toContain('Main chat');
    // The row is named by its task, not by the tool name "Agent"; the summary of the done
    // agent is its second line.
    expect(html).toContain('title="Test subagent">Test subagent</span>');
    expect(html).toContain('<span class="fd-agent-dim">sub-ok</span>');
    expect(html).toContain('data-agent="node-2"');
    expect(html).toContain('data-tone="muted">done<');
    expect(html).not.toContain('running</span>');
    // Nothing runs: the summary says how many subagents the menu lists.
    expect(html).toContain('Agents <span class="fd-count">1</span>');
    expect(html).toContain(
      '<p class="fd-label fd-agent-group">Agents <span class="fd-count">1</span></p>',
    );
    expect(html).not.toContain('Background commands');
  });

  test('a running subagent: the summary keeps the count and adds the working glyph', () => {
    // Up to the first `task.updated`: the subagent runs.
    const events = recorded('subagent').filter((e) => e.session_seq <= 11);
    const { html } = menu(events);
    expect(html).toContain('Agents <span class="fd-count">1</span>');
    expect(html).toContain(
      '<span class="fd-agents-running"><span class="fd-dot" data-status="working" aria-hidden="true"></span>1 running</span>',
    );
    // Green is the state of the row, not the count of the summary.
    expect(html).not.toContain('<span class="fd-badge" data-tone="ok">1 running');
    expect(html).toContain('data-tone="ok">running<');
    // A running agent has no summary line yet.
    expect(html).not.toContain('fd-agent-dim');
  });

  test('the Codex fixture names its subagents by their prompt, else by their tool', () => {
    const { html } = menu(recorded('codex-subagent'));
    expect(html).toContain('>Do not use tools. Reply with exactly: sub-ok</span>');
    // The `wait` call has no input: its tool name names it.
    expect(html).toContain('title="wait">wait</span>');
    expect(html).toContain('data-tone="muted">done<');
  });

  test('the chosen subagent is marked', () => {
    const picked = menu(recorded('subagent'), 'node-2').html;
    expect(picked).toContain('aria-current="true"');
    expect(picked).toContain('data-agent="node-2"');
    expect(menu(recorded('subagent')).html.match(/aria-current="true"/g)).toHaveLength(1);
  });

  test('a background command is named by its description, else by its command', () => {
    const events = envelopes('s', [
      shell('b1', 'sleep 60'),
      task('t1', 'running', { node_id: 'b1', description: 'Sleep a minute' }),
      shell('b2', 'cargo test -p fleetdeck-station --manifest-path /home/me/app/Cargo.toml'),
      task('t2', 'running', { node_id: 'b2' }),
      task('t3', 'completed', { node_id: 'nowhere', summary: 'all good' }),
    ]);
    const { html } = menu(events);
    expect(html).toContain('title="Sleep a minute">Sleep a minute</span>');
    // A command without a description reads as code, its home folder as `~`.
    expect(html).toContain(
      'data-code="" title="cargo test -p fleetdeck-station --manifest-path ~/app/Cargo.toml">',
    );
    // A task with neither keeps the generic name.
    expect(html).toContain('>Background command</span>');
    // The third task has no card: its row is disabled, with its summary.
    expect(html).toMatch(/<button[^>]*data-task="t3"[^>]*disabled/);
    expect(html).toContain('<span class="fd-agent-dim">all good</span>');
    expect(html).not.toMatch(/<button[^>]*data-task="t1"[^>]*disabled/);
    // Commands only: the summary names them, and what runs.
    expect(html).toContain(
      '<span class="fd-agents-part">Commands <span class="fd-count">3</span></span>',
    );
    expect(html).not.toContain('Agents <span class="fd-count">');
    expect(html).toContain('2 running</span>');
    expect(html).toContain('Background commands <span class="fd-count">3</span></p>');
    expect(html).not.toContain('fd-agents-more');
  });

  test('subagents and commands: two groups, and two label-and-pill counts in the summary', () => {
    const events = envelopes('s', [
      agent('a1', 'Read the docs'),
      shell('b1', 'sleep 60'),
      task('t1', 'completed', { node_id: 'b1' }),
      shell('b2', 'sleep 70'),
      task('t2', 'completed', { node_id: 'b2' }),
    ]);
    const { html } = menu(events);
    // §5: a count after a label is a pill, for the commands too (never "· 2 commands").
    expect(html).toContain(
      '<span class="fd-agents-part">Agents <span class="fd-count">1</span></span><span class="fd-agents-part">Commands <span class="fd-count">2</span></span>',
    );
    expect(html).not.toContain('fd-agents-more');
    const agents = html.indexOf('fd-agent-group">Agents');
    const commands = html.indexOf('fd-agent-group">Background commands');
    expect(agents).toBeGreaterThan(html.indexOf('Main chat'));
    expect(commands).toBeGreaterThan(html.indexOf('data-agent="a1"'));
    expect(html.indexOf('data-task="t1"')).toBeGreaterThan(commands);
  });

  test('in each group, what runs comes first, then what failed, then what is done', () => {
    const events = envelopes('s', [
      agent('a1', 'First agent'),
      agent('a2', 'Second agent'),
      agent('a3', 'Third agent'),
      {
        kind: 'task.updated',
        task_id: 'ta1',
        node_id: 'a1',
        task_kind: 'local_agent',
        status: 'completed',
      },
      {
        kind: 'task.updated',
        task_id: 'ta2',
        node_id: 'a2',
        task_kind: 'local_agent',
        status: 'failed',
      },
      {
        kind: 'task.updated',
        task_id: 'ta3',
        node_id: 'a3',
        task_kind: 'local_agent',
        status: 'running',
      },
      shell('b1', 'echo 1'),
      task('t1', 'completed', { node_id: 'b1' }),
      shell('b2', 'echo 2'),
      task('t2', 'running', { node_id: 'b2' }),
      shell('b3', 'echo 3'),
      task('t3', 'killed', { node_id: 'b3' }),
      shell('b4', 'echo 4'),
      task('t4', 'completed', { node_id: 'b4' }),
      shell('b5', 'echo 5'),
      task('t5', 'running', { node_id: 'b5' }),
    ]);
    expect(rowIds(menu(events).html)).toEqual(['a3', 'a2', 'a1', 't2', 't5', 't3', 't1', 't4']);
  });

  test('the words of the counts', () => {
    expect(commandsText(1)).toBe('1 command');
    expect(commandsText(33)).toBe('33 commands');
    expect(commandsText(1286)).toBe('1,286 commands');
    expect(subagentName({ input: '\n  Fix the shim\nmore', title: 'Agent' })).toBe('Fix the shim');
    expect(subagentName({ input: '', title: 'Agent' })).toBe('Agent');
  });

  test('a description that repeats the command reads as the command, in code', () => {
    const titles = new Map([
      ['b1', 'cd /home/me/app && cargo test'],
      ['b2', `cd /home/me/app && ${'x'.repeat(10)}…`],
    ]);
    // Claude names a command without a description by the command itself.
    expect(
      commandName({ node_id: 'b1', description: 'cd /home/me/app && cargo test' }, titles),
    ).toEqual({
      name: 'cd ~/app && cargo test',
      code: true,
      raw: 'cd /home/me/app && cargo test',
    });
    // The card keeps a cut input; the description is the whole command.
    expect(
      commandName({ node_id: 'b2', description: `cd /home/me/app && ${'x'.repeat(30)}` }, titles)
        .code,
    ).toBe(true);
    expect(commandName({ node_id: 'b1', description: 'Run the tests' }, titles)).toEqual({
      name: 'Run the tests',
      code: false,
      raw: 'Run the tests',
    });
    expect(commandName({ node_id: 'b1', description: null }, titles).code).toBe(true);
    expect(commandName({ node_id: null, description: null }, titles)).toEqual({
      name: '',
      code: false,
      raw: '',
    });
  });

  test('a summary shows only what the row does not say', () => {
    expect(summaryNote('VERDICT: FAIL', 'Adversary round 3')).toBe('VERDICT: FAIL');
    expect(summaryNote('Run the tests', 'Run the tests')).toBeNull();
    expect(summaryNote(null, 'Run the tests')).toBeNull();
    // Claude's sentence: the outcome counts, unless the done chip already says it.
    expect(
      summaryNote('Background command "Run the tests" completed (exit code 0)', 'x'),
    ).toBeNull();
    expect(summaryNote('Background command "Run the tests" failed with exit code 1', 'x')).toBe(
      'Failed with exit code 1',
    );
    // A summary cut inside the quote has no outcome.
    expect(summaryNote('Background command "Render the screenshots, demo vid…', 'x')).toBeNull();
    // The station cut the summary that repeats a long name; a longer summary says more.
    expect(summaryNote('cd /home/me/app && cargo te…', 'cd /home/me/app && cargo test')).toBeNull();
    expect(summaryNote('Run the tests: 3 failed', 'Run the tests')).toBe('Run the tests: 3 failed');
  });
});

describe('the agent header', () => {
  test('it shows the way back, the title and the status', () => {
    const html = renderToStaticMarkup(
      <AgentHead title="Explore" status={{ label: 'running', tone: 'ok' }} onBack={() => {}} />,
    );
    expect(html).toContain('‹ Main chat');
    expect(html).toContain('Explore');
    expect(html).toContain('data-tone="ok">running<');
  });
});
