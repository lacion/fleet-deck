// M2 P4-3: the session view, the needs-you rail and the D15 subagent tree in
// a phone browser. The board talks to a scripted deck (DL-059); P4-5 runs the
// same flows against real processes.
import { expect, type Page, test } from '@playwright/test';
import type { DeckCommand, DeckSession, OpenRequest } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities, nestedSubagents } from '../src/testing/timeline';
import { CSRF, FakeDeck, fitsWidth, logIn, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 'nested';

function session(
  extra: Partial<DeckSession['summary']> = {},
  status: DeckSession['status'] = 'working',
): DeckSession {
  return {
    station_id: ST,
    status,
    summary: summary(ID, 'working', 1, {
      title: 'Review the repo',
      capabilities: claudeCapabilities(),
      mode: 'default',
      current_run: 'run-1',
      ...extra,
    }),
  };
}

const request: OpenRequest = {
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
  requested_at: 1_760_000_000_000,
};

function receipt(
  command_id: string,
  state: DeckCommand['state'],
  reject: string | null = null,
): DeckCommand {
  return {
    command_id,
    station_id: ST,
    method: 'session.x',
    origin: 'board',
    envelope: null,
    retry_until: 0,
    state,
    attempts: 1,
    sent_at: 1,
    receipt:
      state === 'done'
        ? {
            command_id,
            state: reject === null ? 'accepted' : 'rejected',
            station_seq: null,
            result: null,
            reject:
              reject === null
                ? null
                : { code: 'invalid_state', message: reject, unsupported: null },
            at: 1,
          }
        : null,
    reason: state === 'failed' ? 'station refused the link' : null,
    created_at: 1,
    updated_at: 1,
  };
}

const idOf = (body: unknown) => (body as { command_id: string }).command_id;

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  await logIn(page);
}

test('the subagent tree shows each child timeline and status, and the card counts running subagents', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [
    session({
      running_tools: [
        {
          node_id: 'node-1',
          tool_kind: 'subagent',
          title: 'Agent',
          input_summary: 'Explore the code',
        },
        {
          node_id: 'node-2',
          tool_kind: 'subagent',
          title: 'Agent',
          input_summary: 'Check the tests',
        },
        {
          node_id: 'node-4',
          tool_kind: 'subagent',
          title: 'Agent',
          input_summary: 'Read the docs',
        },
      ],
      background_agents: 1,
    }),
  ];
  deck.emit(ST, ID, nestedSubagents(10));
  await setUp(page, deck);
  const card = page.locator(`[data-session="${ID}"]`);
  await expect(card).toContainText('Subagents running: 4');
  expect(await fitsWidth(page)).toBe(true);

  await page.getByRole('button', { name: 'Open session Review the repo' }).click();
  const view = page.getByRole('main', { name: 'Session' });
  const explore = view.locator('[data-subagent="node-1"]');
  const docs = explore.locator('[data-subagent="node-4"]');
  await expect(view.locator('[data-running-subagents]')).toHaveText('3');
  await expect(explore.locator('> summary')).toContainText('Explore the code · running');
  await expect(explore).toContainText('main is small');
  await expect(docs.locator('> summary')).toContainText('Read the docs · running');
  await expect(docs).toContainText('Approval: Bash · waiting for an answer');
  // The child events stay out of the top-level timeline.
  await expect(view.locator('.fd-timeline > .fd-items > li')).toHaveCount(3);
  expect(await fitsWidth(page)).toBe(true);

  deck.emit(ST, ID, nestedSubagents().slice(11));
  await expect(view.locator('[data-running-subagents]')).toHaveText('0');
  await expect(docs.locator('> summary')).toContainText('Read the docs · done');
  await expect(view.locator('[data-subagent="node-2"] > summary')).toContainText(
    'Check the tests · failed',
  );
  await expect(explore.locator('> summary')).toContainText('Explore the code · done');
  await expect(docs).toContainText('Approval: Bash · Allow (by board)');
  await expect(view).toContainText('All subagents reported.');
});

test('an approval is answered from the needs-you rail', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ open_requests: [request], activity: 'needs_you' }, 'needs_you')];
  deck.on('/api/commands/approval.answer', (body) => {
    deck.sessions = [session()];
    queueMicrotask(() => deck.publish());
    return { status: 200, body: receipt(idOf(body), 'done') };
  });
  await setUp(page, deck);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toContainText('Review the repo · Alpha: Bash');
  await rail.getByRole('button', { name: 'Answer Bash in Review the repo' }).click();
  const needs = page.getByRole('region', { name: 'Needs you' });
  await expect(needs).toContainText('ls docs');
  await needs.getByRole('button', { name: 'Allow' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  const call = deck.calls.find((c) => c.path === '/api/commands/approval.answer');
  expect(call?.csrf).toBe(CSRF);
  expect(call?.body).toEqual({
    station: ST,
    command_id: expect.stringMatching(/^b[0-9a-z]{24}$/),
    command: {
      op: 'answer',
      session_id: ID,
      request_id: 'req-6',
      decision: { option_id: 'allow', updated_input: null, text: null },
    },
  });
  await page.getByRole('button', { name: 'Back to fleet' }).click();
  await expect(page.getByRole('region', { name: 'Needs you rail' })).toContainText(
    'Nothing waits for you.',
  );
});

test('steer, follow-up, interrupt and the mode ladder send one command each', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  for (const m of ['session.steer', 'session.follow_up', 'session.set_mode', 'session.set_model']) {
    deck.on(`/api/commands/${m}`, (body) => ({ status: 200, body: receipt(idOf(body), 'done') }));
  }
  let interrupts = 0;
  deck.on('/api/commands/session.interrupt', (body) => {
    interrupts += 1;
    return interrupts === 1
      ? { status: 200, body: receipt(idOf(body), 'done', 'No run is active.') }
      : { status: 200, body: receipt(idOf(body), 'ambiguous') };
  });
  await setUp(page, deck);
  await page.getByRole('button', { name: 'Open session Review the repo' }).click();
  const composer = page.getByRole('form', { name: 'Composer' });
  const text = composer.getByLabel('Message');
  await expect(composer.getByRole('button', { name: 'Steer now' })).toBeDisabled();
  await text.fill('focus on the tests');
  await composer.getByRole('button', { name: 'Steer now' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  await expect(text).toHaveValue('');
  await text.fill('then summarize');
  await composer.getByRole('button', { name: 'Queue follow-up' }).click();
  await page.getByRole('button', { name: 'Interrupt' }).click();
  await expect(page.getByRole('alert')).toHaveText('Refused: No run is active.');
  await page.getByRole('button', { name: 'Interrupt' }).click();
  await expect(page.getByRole('alert')).toContainText('Delivery is unknown');
  const ladder = page.getByRole('group', { name: 'Mode (most restrictive first)' });
  await expect(ladder.getByRole('button')).toHaveText([
    'default',
    'plan',
    'acceptEdits',
    'dontAsk',
    'auto',
  ]);
  await expect(ladder.getByRole('button', { name: 'default' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await ladder.getByRole('button', { name: 'plan' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const model = page.getByRole('form', { name: 'Model' });
  await model.getByLabel('Model (now unknown)').fill(' claude-haiku-4-5 ');
  await model.getByRole('button', { name: 'Set model' }).click();
  await expect(model.getByRole('button', { name: 'Set model' })).toBeDisabled();
  expect(await fitsWidth(page)).toBe(true);

  const sent = deck.calls.filter((c) => c.path.startsWith('/api/commands/'));
  expect(sent.map((c) => [c.path, (c.body as { command: unknown }).command])).toEqual([
    [
      '/api/commands/session.steer',
      { op: 'steer', session_id: ID, input: { text: 'focus on the tests' } },
    ],
    [
      '/api/commands/session.follow_up',
      { op: 'follow_up', session_id: ID, input: { text: 'then summarize' } },
    ],
    ['/api/commands/session.interrupt', { op: 'interrupt', session_id: ID }],
    ['/api/commands/session.interrupt', { op: 'interrupt', session_id: ID }],
    ['/api/commands/session.set_mode', { op: 'set_mode', session_id: ID, mode: 'plan' }],
    [
      '/api/commands/session.set_model',
      { op: 'set_model', session_id: ID, model: 'claude-haiku-4-5' },
    ],
  ]);
  // Each action has its own command id; the board never resends one.
  expect(new Set(sent.map((c) => idOf(c.body))).size).toBe(sent.length);
});

test('a read-only login sees the session but cannot drive or answer', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [
    session({ current_run: null, activity: 'idle', open_requests: [request] }, 'needs_you'),
  ];
  deck.scopes = ['read'];
  await setUp(page, deck);
  await page.getByRole('button', { name: 'Open session Review the repo' }).click();
  const composer = page.getByRole('form', { name: 'Composer' });
  await expect(composer.getByLabel('Message')).toBeDisabled();
  await expect(composer.getByRole('button', { name: 'Send' })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Interrupt' })).toBeDisabled();
  await expect(
    page.getByRole('region', { name: 'Needs you' }).getByRole('button', { name: 'Allow' }),
  ).toBeDisabled();
  await expect(page.getByText('Answering needs the approve scope.')).toBeVisible();
  expect(deck.calls.some((c) => c.path.startsWith('/api/commands/'))).toBe(false);
});

test('an idle session with the drive scope sends a prompt', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ current_run: null, activity: 'idle' }, 'idle')];
  deck.on('/api/commands/session.prompt', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'pending'),
  }));
  await setUp(page, deck);
  await page.getByRole('button', { name: 'Open session Review the repo' }).click();
  const composer = page.getByRole('form', { name: 'Composer' });
  await expect(composer.getByRole('button', { name: 'Steer now' })).toHaveCount(0);
  await composer.getByLabel('Message').fill('hello');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(
    page.getByRole('status').filter({ hasText: 'Sent. The station has not confirmed it yet.' }),
  ).toBeVisible();
  const call = deck.calls.find((c) => c.path === '/api/commands/session.prompt');
  expect(call?.body).toMatchObject({ station: ST });
  expect((call?.body as { command: unknown } | undefined)?.command).toEqual({
    op: 'prompt',
    session_id: ID,
    input: { text: 'hello' },
  });
});
