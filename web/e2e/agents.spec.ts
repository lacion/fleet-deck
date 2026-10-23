// B6: the agents switcher of the chat tab, against the scripted deck: choose a subagent, see its
// own chat with the way back and no composer, go back; choose a background command.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession, Event } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities, envelopes } from '../src/testing/timeline';
import { FakeDeck, fitsWidth, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 's1';

const session = (): DeckSession => ({
  station_id: ST,
  status: 'idle',
  summary: summary(ID, 'idle', 1, {
    title: 'Fix the docs',
    capabilities: claudeCapabilities(),
    cwd: '/home/me/app',
  }),
});

const events: Event[] = [
  { kind: 'run.started', run_id: 'run-1', input_summary: 'go', origin: 'board' },
  { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: {} },
  {
    kind: 'tool.started',
    node_id: 'agent-1',
    parent_node_id: null,
    tool_kind: 'subagent',
    title: 'Agent',
    input_summary: 'Read the docs',
    files: [],
    provider_refs: { tool_use_id: 'toolu_01' } as never,
  },
  {
    kind: 'task.updated',
    task_id: 'ta',
    node_id: 'agent-1',
    task_kind: 'local_agent',
    status: 'running',
  },
  {
    kind: 'message.completed',
    turn_id: 'turn-1',
    parent_node_id: 'agent-1',
    text: 'The docs folder has two files.',
  },
  {
    kind: 'tool.started',
    node_id: 'bash-1',
    parent_node_id: null,
    tool_kind: 'shell',
    title: 'Bash',
    input_summary: 'sleep 600',
    files: [],
    provider_refs: {},
  },
  {
    kind: 'task.updated',
    task_id: 'tb',
    node_id: 'bash-1',
    task_kind: 'local_bash',
    status: 'running',
    description: 'Wait ten minutes',
  },
];

async function setUp(page: Page) {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  await logIn(page);
  deck.emit(ST, ID, envelopes(ID, events));
  await openSession(page, 'Fix the docs');
  return { deck, view: page.getByRole('region', { name: 'Session' }) };
}

test('the Agents menu lists the agents; a subagent shows its own chat and the way back', async ({
  page,
}) => {
  const { view } = await setUp(page);
  const summaryEl = view.locator('.fd-chat-toolbar summary');
  await expect(summaryEl).toContainText('Agents');
  await expect(summaryEl).toContainText('· 1 command');
  await expect(summaryEl).toContainText('2 running');
  await summaryEl.click();
  const menu = view.locator('.fd-chat-toolbar .fd-menu-body');
  await expect(menu.getByRole('button', { name: /^Main chat/ })).toBeVisible();
  await expect(menu.getByText('Read the docs')).toBeVisible();
  await expect(menu.locator('.fd-agent-group')).toHaveText(['Agents 1', 'Background commands 1']);
  await expect(menu.getByText('Wait ten minutes')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);

  await menu.locator('[data-agent="agent-1"]').click();
  await expect(page).toHaveURL(/agent=agent-1/);
  const back = view.getByRole('button', { name: '‹ Main chat', exact: true });
  await expect(back).toBeVisible();
  await expect(view.locator('.fd-chat-column')).toContainText('The docs folder has two files.');
  await expect(view.getByText('A subagent takes no input. Write in the main chat.')).toBeVisible();
  await expect(view.locator('#fd-composer-text')).toHaveCount(0);
  expect(await fitsWidth(page)).toBe(true);

  await back.click();
  await expect(page).not.toHaveURL(/agent=/);
  await expect(back).toHaveCount(0);
  await expect(view.locator('#fd-composer-text')).toBeVisible();
});

test('a background command is scrolled to and marked in the main chat', async ({ page }) => {
  const { view } = await setUp(page);
  await view.locator('.fd-chat-toolbar summary').click();
  await view.locator('[data-task="tb"]').click();
  await expect(view.locator('[data-node="bash-1"]')).toHaveAttribute('data-highlight', '');
  await expect(page).not.toHaveURL(/agent=/);
});
