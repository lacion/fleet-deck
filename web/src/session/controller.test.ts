// B12: what the owner may do while only a workflow runs (design `workflow-agents.md` §4.8).
import { describe, expect, test } from 'bun:test';
import type { DeckSession } from '../gen';
import { summary } from '../testing/summary';
import { claudeCapabilities } from '../testing/timeline';
import { sessionActions } from './commands';
import { sessionFlags } from './controller';

const key = { station_id: 'station-a', session_id: 's1' };
const actions = sessionActions(async () => ({}), key, ['read', 'drive', 'approve']);

const card = (extra: Parameters<typeof summary>[3]): DeckSession => ({
  station_id: 'station-a',
  status: 'working',
  summary: summary('s1', 'background', 1, { capabilities: claudeCapabilities(), ...extra }),
});

describe('the controls of a session with a running workflow (B12)', () => {
  test('controller: with only a running workflow, Interrupt is off and Stop is on', () => {
    const idle = card({
      current_run: null,
      background_tasks: ['w1'],
      background_workflows: 1,
      running_workflows: [{ task_id: 'w1', running_agents: 3, agents_total: 5 }],
      running_agents: 3,
    });
    const flags = sessionFlags(idle, 'live', actions);
    // No turn runs: the station and the core refuse an interrupt, so the board does not offer it.
    expect(flags.working).toBe(false);
    expect(flags.canInterrupt).toBe(false);
    // Stop ends the session process, and with it the workflow.
    expect(flags.canStop).toBe(true);
  });

  test('a main turn that runs still allows Interrupt', () => {
    const turn = card({ current_run: 'run-1', running_agents: 3 });
    expect(sessionFlags(turn, 'live', actions).canInterrupt).toBe(true);
  });
});
