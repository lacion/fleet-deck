// DL-140: screenshots of the board for the visual check (opt-in: FD_SCREENS=1).
// Each test writes test-results/screens/<project>-<name>.png at the viewport of
// its project (phone, desktop, tablet). The scripted deck feeds the same three
// sessions on two stations to every shot.
import { expect, type Page, test } from '@playwright/test';
import type { Event } from '../src/gen';
import { envelopes } from '../src/testing/timeline';
import { logIn, quietShells } from './fake-deck';
import { ALPHA, bashRequest, threeSessionDeck } from './scenes';

test.skip(
  process.env['FD_SCREENS'] === undefined || process.env['FD_SCREENS'] === '',
  'FD_SCREENS is not set',
);

const ID = 's1';
const refs = {};

const REPLY = [
  'The build fails in two places. Here is what I found:',
  '',
  '```ts',
  'export function parse(input: string): number {',
  '  return Number.parseInt(input, 10);',
  '}',
  '```',
  '',
  '| File | Problem | Fix |',
  '| --- | --- | --- |',
  '| `src/parse.ts` | no radix | pass `10` |',
  '| `src/main.ts` | unused import | remove it |',
  '',
  'I will clean `build/` and run the tests again.',
].join('\n');

const finalized = (turn: string): Event => ({
  kind: 'turn.finalized',
  turn_id: turn,
  checkpoint: {
    worktree_id: 'wt',
    scope: 'tracked',
    start: { state: 'available', commit: `${turn}-start` },
    end: { state: 'available', commit: `${turn}-end` },
  },
  diff_summary: { files_changed: 2, insertions: 3, deletions: 1 },
});

const tool = (
  node_id: string,
  tool_kind: 'read' | 'shell',
  title: string,
  input: string,
): Event => ({
  kind: 'tool.started',
  node_id,
  parent_node_id: null,
  tool_kind,
  title,
  input_summary: input,
  files: [],
  provider_refs: refs,
});

const toolDone = (node_id: string): Event => ({
  kind: 'tool.completed',
  node_id,
  status: 'ok',
  files: [],
  exit_code: null,
});

const TIMELINE: Event[] = [
  // The first turn is over and has a diff.
  { kind: 'run.started', run_id: 'run-0', input_summary: 'fix the typo', origin: 'board' },
  { kind: 'turn.started', run_id: 'run-0', turn_id: 'turn-0', provider_refs: refs },
  { kind: 'message.completed', turn_id: 'turn-0', parent_node_id: null, text: 'Fixed the typo.' },
  { kind: 'turn.completed', turn_id: 'turn-0', status: 'done', usage: null },
  finalized('turn-0'),
  // The second turn reasons, reads, answers and waits for an approval.
  { kind: 'run.started', run_id: 'run-1', input_summary: 'review the repo', origin: 'board' },
  { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: refs },
  {
    kind: 'reasoning.delta',
    turn_id: 'turn-1',
    text: 'The failing step is the build. Read the sources, then clear the output folder.',
  },
  tool('node-1', 'read', 'Read', 'src/parse.ts'),
  toolDone('node-1'),
  tool('node-2', 'shell', 'Bash', 'cargo build'),
  toolDone('node-2'),
  { kind: 'message.completed', turn_id: 'turn-1', parent_node_id: null, text: REPLY },
  tool('node-9', 'shell', 'Bash', 'rm -rf build'),
  {
    kind: 'approval.requested',
    request_id: bashRequest.request_id,
    node_id: 'node-9',
    request_kind: 'command',
    title: 'Bash',
    details: 'rm -rf build',
    input: { command: 'rm -rf build' },
    options: bashRequest.options,
    response: 'rpc_reply',
    provider_refs: refs,
  },
];

async function setUp(page: Page) {
  const deck = threeSessionDeck();
  deck.emit(ALPHA, ID, envelopes(ID, TIMELINE));
  deck.on(`/api/stations/${ALPHA}/sessions/${ID}/turns/turn-0/diff`, () => ({
    status: 200,
    body: {
      session_id: ID,
      turn_id: 'turn-0',
      from_commit: 'turn-0-start',
      to_commit: 'turn-0-end',
      summary: { files_changed: 2, insertions: 3, deletions: 1 },
      patch: [
        'diff --git a/src/parse.ts b/src/parse.ts',
        '--- a/src/parse.ts',
        '+++ b/src/parse.ts',
        '@@ -1,3 +1,3 @@',
        ' export function parse(input: string): number {',
        '-  return Number.parseInt(input);',
        '+  return Number.parseInt(input, 10);',
        ' }',
      ].join('\n'),
      truncated: false,
    },
  }));
  await quietShells(page);
  await deck.install(page);
  await logIn(page);
  return deck;
}

/** The board as the person sees it: fonts loaded, nothing moving. */
async function shot(page: Page, name: string) {
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: `test-results/screens/${test.info().project.name}-${name}.png`,
    fullPage: true,
    animations: 'disabled',
  });
}

async function toSession(page: Page) {
  await page.evaluate(
    ([st, id]) => {
      window.location.hash = `#/s/${st}/${id}`;
    },
    [ALPHA, ID] as const,
  );
  const pane = page.getByRole('region', { name: 'Session' });
  await expect(pane.getByRole('heading', { level: 2 })).toHaveText('Review the repo');
  return pane;
}

test('home with three sessions on two stations, one needs you', async ({ page }) => {
  await setUp(page);
  // The phone and the desktop list the sessions; the tablet shows the Fleet board.
  if (test.info().project.name === 'tablet') {
    await expect(page.getByText('3 sessions · 1 needs you')).toBeVisible();
  } else {
    await expect(page.locator('a.fd-srow').first()).toBeVisible();
  }
  await shot(page, 'home');
});

test('a session with chat, tool calls, a reasoning item and an open approval', async ({ page }) => {
  await setUp(page);
  const pane = await toSession(page);
  await expect(pane.locator('table')).toBeVisible();
  await expect(pane.locator('pre').filter({ hasText: 'Number.parseInt' })).toBeVisible();
  await expect(pane).toContainText('rm -rf build');
  await expect(pane.getByRole('button', { name: 'Allow' }).first()).toBeVisible();
  await shot(page, 'session-chat');
});

test('the same session on the Changes tab', async ({ page }) => {
  await setUp(page);
  const pane = await toSession(page);
  await pane
    .getByRole('tablist', { name: 'Session views' })
    .getByRole('tab', { name: /^Changes/ })
    .click();
  const diff = pane.getByRole('button', { name: 'Show the diff' });
  await diff.click();
  await expect(pane.locator('[data-diff="turn-0"]')).toContainText('parseInt(input, 10)');
  await shot(page, 'session-changes');
});

test('the terminal dock with one shell tab', async ({ page }) => {
  test.skip(test.info().project.name !== 'desktop', 'the dock is the desktop layout');
  const deck = await setUp(page);
  deck.shells(ALPHA);
  const pane = await toSession(page);
  // DL-141: the dock has no bar without tabs; the session menu opens a shell.
  await pane.locator('summary[aria-label="Session menu"]').click();
  await pane.getByRole('button', { name: 'Open shell in workspace' }).click();
  const dock = page.getByRole('region', { name: 'Terminals' });
  await expect(dock.getByRole('tablist', { name: 'Open terminals' }).getByRole('tab')).toHaveCount(
    1,
  );
  await expect(dock.getByRole('button', { name: 'Hide terminals' })).toBeVisible();
  await shot(page, 'terminal-dock');
});
