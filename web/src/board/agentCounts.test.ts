import { describe, expect, test } from 'bun:test';
import { card } from '../testing/fleet';
import { summary } from '../testing/summary';
import { agentsPartial, atWork, backgroundCommands, runningAgents } from './agentCounts';

const tool = (id: string) => ({
  node_id: id,
  tool_kind: 'subagent' as const,
  title: 'Agent',
  input_summary: 'Explore',
});

describe('agent counts', () => {
  test('runningAgents prefers running_agents and falls back', () => {
    // an older deck: no running_agents, the sum of the subagent tools and the background agents
    const older = summary('a', 'working', 1, { running_tools: [tool('n1')], background_agents: 2 });
    expect(runningAgents(older)).toBe(3);
    // a newer deck: the summary's count, also when it is larger than the older sum
    expect(runningAgents({ ...older, running_agents: 5 })).toBe(5);
    expect(runningAgents(summary('b', 'working', 1, { running_agents: 4 }))).toBe(4);
    expect(runningAgents(summary('c', 'idle', 1))).toBe(0);
  });

  test('agentsPartial reads agents_partial', () => {
    expect(agentsPartial(summary('a', 'working', 1))).toBe(false);
    expect(agentsPartial(summary('a', 'working', 1, { agents_partial: false }))).toBe(false);
    expect(agentsPartial(summary('a', 'working', 1, { agents_partial: true }))).toBe(true);
  });

  test('backgroundCommands leaves out agents and workflows', () => {
    const tasks = ['t1', 't2', 't3', 't4', 't5'];
    expect(backgroundCommands(summary('a', 'working', 1, { background_tasks: tasks }))).toBe(5);
    expect(
      backgroundCommands(
        summary('a', 'working', 1, {
          background_tasks: tasks,
          background_agents: 2,
          background_workflows: 1,
        }),
      ),
    ).toBe(2);
    // never below 0 when the counts disagree
    expect(
      backgroundCommands(
        summary('a', 'working', 1, { background_tasks: ['t1'], background_agents: 3 }),
      ),
    ).toBe(0);
  });

  test('atWork counts working and background sessions and the agents of live cards, not unreachable ones', () => {
    const cards = [
      card('w', 'working', 'station-a', { activity: 'working', running_agents: 2 }),
      card('b', 'working', 'station-a', { activity: 'background', background_agents: 1 }),
      // idle: not a working session, but its running agents count
      card('i', 'idle', 'station-a', { activity: 'idle', running_agents: 3 }),
      // needs you: neither working nor background
      card('n', 'needs_you', 'station-a', { activity: 'needs_you' }),
      // not live: not counted at all
      card('u', 'unreachable', 'station-b', { activity: 'working', running_agents: 7 }),
      card('e', 'ended', 'station-a', { activity: 'ended', running_agents: 7 }),
      card('l', 'lost', 'station-a', { activity: 'working', running_agents: 7 }),
    ];
    expect(atWork(cards)).toEqual({ sessions: 2, agents: 6, partial: false });
    expect(atWork([])).toEqual({ sessions: 0, agents: 0, partial: false });
  });

  test('atWork is partial when one live card is partial', () => {
    const live = card('a', 'working', 'station-a', { agents_partial: true, running_agents: 1 });
    expect(atWork([card('b'), live]).partial).toBe(true);
    // a partial card that is not live does not make the total partial
    const away = card('u', 'unreachable', 'station-b', { agents_partial: true });
    expect(atWork([card('b'), away]).partial).toBe(false);
  });
});
