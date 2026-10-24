// DL-191: "Show its work" on a subagent card and the Branches view of the Changes tab,
// against the scripted deck (phone viewport).
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession, Event, HistoryItem } from '../src/gen';
import { claudeCapabilities } from '../src/testing/timeline';
import { summary } from '../src/testing/summary';
import { envelopes } from '../src/testing/timeline';
import { FakeDeck, fitsWidth, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 's1';
const API = `/api/sessions/${ST}/${ID}`;
const NOW = Math.floor(Date.now() / 1000);
const HEAD_A = 'a'.repeat(40);
const HEAD_B = 'b'.repeat(40);

const session = (): DeckSession => ({
  station_id: ST,
  status: 'idle',
  summary: summary(ID, 'idle', 1, {
    title: 'Fix the docs',
    capabilities: claudeCapabilities(),
    cwd: '/home/me/app',
  }),
});

const agent: Event = {
  kind: 'tool.started',
  node_id: 'agent-1',
  parent_node_id: null,
  tool_kind: 'subagent',
  title: 'Agent',
  input_summary: 'Read the docs',
  files: [],
  provider_refs: { tool_use_id: 'toolu_01' } as never,
};

const items: HistoryItem[] = [
  { id: 'u1', at: 0, kind: 'user', text: 'Read the docs folder', tool: null },
  {
    id: 't1',
    at: 0,
    kind: 'tool',
    text: '',
    tool: { name: 'Bash', summary: 'ls docs', status: 'ok' },
  },
  { id: 'a1', at: 0, kind: 'assistant', text: 'There are **two** files.', tool: null },
];

const PATCH = [
  'diff --git a/docs/a.md b/docs/a.md',
  'index 1111111..2222222 100644',
  '--- a/docs/a.md',
  '+++ b/docs/a.md',
  '@@ -1,2 +1,2 @@',
  ' one',
  '-two',
  '+TWO',
  '',
].join('\n');

async function setUp(page: Page) {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  deck.stations = [stationView(ST, 'Alpha')];
  deck.on(`${API}/subagents/toolu_01`, () => ({
    status: 200,
    body: { items, state: { kind: 'complete' } },
  }));
  deck.on(`${API}/branches`, () => ({
    status: 200,
    body: {
      head: HEAD_A,
      branch: 'main',
      branches: [
        {
          name: 'feat/docs',
          head: HEAD_B,
          ahead: 2,
          behind: 1,
          at: NOW - 2 * 3600,
          subject: 'Rewrite the docs',
        },
        {
          name: 'old/done',
          head: 'c'.repeat(40),
          ahead: 0,
          behind: 4,
          at: NOW - 3 * 86400,
          subject: 'Already merged work',
        },
      ],
      truncated: false,
    },
  }));
  deck.on(`${API}/branches/${HEAD_B}/diff`, () => ({
    status: 200,
    body: {
      session_id: ID,
      base_oid: HEAD_A,
      summary: { files_changed: 1, insertions: 1, deletions: 1 },
      patch: PATCH,
      truncated: false,
    },
  }));
  await deck.install(page);
  await logIn(page);
  deck.emit(ST, ID, envelopes(ID, [agent]));
  await openSession(page, 'Fix the docs');
  return { deck, view: page.getByRole('region', { name: 'Session' }) };
}

test('a subagent card shows its work and refreshes it', async ({ page }) => {
  const { deck, view } = await setUp(page);
  const card = view.locator('[data-subagent="agent-1"]');
  await card.getByRole('button', { name: 'Show its work' }).click();
  const log = card.getByRole('log', { name: 'Subagent work' });
  await expect(log).toContainText('Read the docs folder');
  await expect(log).toContainText('ls docs');
  await expect(log.locator('strong')).toHaveText('two');
  await card.getByRole('button', { name: 'Refresh' }).click();
  await expect(card.getByRole('button', { name: 'Refresh' })).toBeVisible();
  expect(deck.calls.filter((c) => c.path === `${API}/subagents/toolu_01`)).toHaveLength(2);
  expect(await fitsWidth(page)).toBe(true);
});

test('the Changes tab lists the branches and shows the diff of one', async ({ page }) => {
  const { view } = await setUp(page);
  await view.getByRole('tab', { name: /^Changes/ }).click();
  await view.getByRole('button', { name: 'Branches', exact: true }).click();
  await expect(view).toContainText('Branches of this repository, newest first.');
  await expect(view).toContainText('This session is on main.');
  await expect(view).toContainText('Uncommitted changes in other folders are not shown.');
  const rows = view.locator('.fd-branch');
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toContainText('feat/docs');
  await expect(rows.first()).toContainText('2 ahead');
  await expect(rows.first()).toContainText('1 behind');
  await expect(rows.first()).toContainText('2 h');
  await expect(rows.nth(1)).toContainText('merged');
  expect(await fitsWidth(page)).toBe(true);

  await rows.first().click();
  await expect(view).toContainText('feat/docs: 1 file +1 −1 since it left main');
  await expect(view.locator('[data-diff="branch-' + HEAD_B + '"]')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
  await view.getByRole('button', { name: 'Back to branches' }).click();
  await expect(view.locator('.fd-branch')).toHaveCount(2);
});
