import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Event, EventEnvelope } from '../gen';
import { hasAgents, taskStates, toolTitles } from '../session/tasks';
import { buildTimeline } from '../session/timeline';
import { envelopes, recorded } from '../testing/timeline';
import { AgentHead, AgentsMenu } from './AgentsMenu';

function task(
  task_id: string,
  status: string,
  more: Partial<Extract<Event, { kind: 'task.updated' }>> = {},
): Event {
  return { kind: 'task.updated', task_id, task_kind: 'local_bash', status, ...more };
}

function menu(events: readonly EventEnvelope[], agent: string | null = null) {
  const tree = buildTimeline(events);
  const tasks = taskStates(events);
  return {
    html: renderToStaticMarkup(
      <AgentsMenu
        subagents={tree.subagents}
        tasks={tasks}
        toolTitles={toolTitles(tree.items)}
        agent={agent}
        onChoose={() => {}}
      />,
    ),
    tree,
    tasks,
  };
}

describe('the agents menu', () => {
  test('the Claude fixture lists the main chat and the subagent with its status', () => {
    const { html, tree, tasks } = menu(recorded('subagent'));
    expect(hasAgents(tree.subagents, tasks)).toBe(true);
    expect(html).toContain('Main chat');
    expect(html).toContain('Test subagent');
    expect(html).toContain('data-agent="node-2"');
    expect(html).toContain('data-tone="muted">done<');
    expect(html).not.toContain('running</span>');
    // Nothing runs: the summary says how many agents the menu lists.
    expect(html).toContain('Agents <span class="fd-count">1</span>');
  });

  test('a running subagent shows the running badge', () => {
    // Up to the first `task.updated`: the subagent runs.
    const events = recorded('subagent').filter((e) => e.session_seq <= 11);
    const { html } = menu(events);
    expect(html).toContain('1 running');
    expect(html).toContain('data-tone="ok">running<');
    expect(html).not.toContain('fd-count');
  });

  test('the Codex fixture lists its subagents', () => {
    const { html } = menu(recorded('codex-subagent'));
    expect(html).toContain('spawnAgent');
    expect(html).toContain('data-tone="muted">done<');
  });

  test('the chosen subagent is marked', () => {
    const picked = menu(recorded('subagent'), 'node-2').html;
    expect(picked).toContain('aria-current="true"');
    expect(picked).toContain('data-agent="node-2"');
    expect(menu(recorded('subagent')).html.match(/aria-current="true"/g)).toHaveLength(1);
  });

  test('a background command shows its description, its status and its summary when done', () => {
    const events = envelopes('s', [
      {
        kind: 'tool.started',
        node_id: 'b1',
        parent_node_id: null,
        tool_kind: 'shell',
        title: 'Bash',
        input_summary: 'sleep 60',
        files: [],
        provider_refs: {},
      },
      task('t1', 'running', { node_id: 'b1', description: 'Sleep a minute' }),
      task('t2', 'completed', { node_id: 'nowhere', summary: 'all good' }),
    ]);
    const { html } = menu(events);
    expect(html).toContain('Background command');
    expect(html).toContain('Sleep a minute');
    expect(html).toContain('1 running');
    // The second task has no card: its row is disabled, with its summary.
    expect(html).toMatch(/<button[^>]*data-task="t2"[^>]*disabled/);
    expect(html).toContain('all good');
    expect(html).not.toMatch(/<button[^>]*data-task="t1"[^>]*disabled/);
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
