// M2 P4-3: the session view, the needs-you rail and the D15 subagent tree in
// a phone browser. The board talks to a scripted deck (DL-059); P4-5 runs the
// same flows against real processes.
import { expect, type Locator, type Page, test } from '@playwright/test';
import type { DeckCommand, DeckSession, OpenRequest } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities, nestedSubagents } from '../src/testing/timeline';
import { CSRF, FakeDeck, fitsWidth, logIn, openFleet, openSession, stationView } from './fake-deck';

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
  await openFleet(page);
  const card = page.locator(`[data-session="${ID}"]`);
  await expect(card.locator('[data-running-subagents="4"]')).toBeVisible();
  await expect(card.locator('[title="Subagents running"]')).toHaveText('4');
  expect(await fitsWidth(page)).toBe(true);

  // the whole card is the link to the session
  await card.click();
  const view = page.getByRole('region', { name: 'Session' });
  const explore = view.locator('[data-subagent="node-1"]');
  const docs = explore.locator('[data-subagent="node-4"]');
  const tests = view.locator('[data-subagent="node-2"]');
  const chip = (card: Locator) => card.locator('> .fd-chat-card-head .fd-badge');
  const task = (card: Locator) => card.locator('> .fd-chat-subagent-task');
  await expect(view.locator('.fd-chat-subagent[data-status="running"]')).toHaveCount(3);
  await expect(task(explore)).toHaveText('Explore the code');
  await expect(chip(explore)).toHaveText('running');
  await expect(explore).toContainText('main is small');
  await expect(task(docs)).toHaveText('Read the docs');
  await expect(chip(docs)).toHaveText('running');
  await expect(docs).toContainText('Bash');
  await expect(docs).toContainText('Waiting for your answer · Bash');
  // The child cards stay inside their parent: two cards at the top, one nested.
  await expect(
    view.locator('.fd-chat-subagent:not(.fd-chat-nested .fd-chat-subagent)'),
  ).toHaveCount(2);
  await expect(explore.locator('.fd-chat-subagent')).toHaveCount(1);
  await expect(tests.locator('.fd-chat-subagent')).toHaveCount(0);
  expect(await fitsWidth(page)).toBe(true);

  deck.emit(ST, ID, nestedSubagents().slice(11));
  await expect(view.locator('.fd-chat-subagent[data-status="running"]')).toHaveCount(0);
  await expect(task(docs)).toHaveText('Read the docs');
  await expect(chip(docs)).toHaveText('done');
  await expect(task(tests)).toHaveText('Check the tests');
  await expect(chip(tests)).toHaveText('failed');
  await expect(task(explore)).toHaveText('Explore the code');
  await expect(chip(explore)).toHaveText('done');
  // The board is the owner: the outcome names no one.
  await expect(docs).toContainText('Bash · Allow');
  await expect(docs).not.toContainText('(by board)');
  await expect(view).toContainText('All subagents reported.');
});

test('an approval is answered from the needs-you rail with one command', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ open_requests: [request], activity: 'needs_you' }, 'needs_you')];
  deck.on('/api/commands/approval.answer', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'done'),
  }));
  const release = deck.hold('/api/commands/approval.answer');
  await setUp(page, deck);
  await openFleet(page);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toContainText('Review the repo');
  await expect(rail).toContainText('Alpha');
  await expect(rail).toContainText('Bash');
  await expect(rail).toContainText('ls docs');
  const allow = rail.getByRole('button', { name: 'Allow: Bash in Review the repo' });
  const deny = rail.getByRole('button', { name: 'Deny: Bash in Review the repo' });
  await allow.click();
  // the buttons stay disabled while the command is pending
  await expect(allow).toBeDisabled();
  await expect(deny).toBeDisabled();
  release();
  deck.sessions = [session()];
  deck.publish();
  // the request is answered: on a phone the strip has nothing left to show
  await expect(rail).toHaveCount(0);
  // the rail answers in place: the page does not change
  await expect(page).toHaveURL(/#\/fleet$/);
  const calls = deck.calls.filter((c) => c.path === '/api/commands/approval.answer');
  expect(calls).toHaveLength(1);
  expect(calls[0]?.csrf).toBe(CSRF);
  expect(calls[0]?.body).toEqual({
    station: ST,
    command_id: expect.stringMatching(/^b[0-9a-z]{24}$/),
    command: {
      op: 'answer',
      session_id: ID,
      request_id: 'req-6',
      decision: { option_id: 'allow', updated_input: null, text: null },
    },
  });
});

test('a refused answer from the rail shows its text and the buttons work again', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ open_requests: [request], activity: 'needs_you' }, 'needs_you')];
  deck.on('/api/commands/approval.answer', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'done', 'The request is not open any more.'),
  }));
  await setUp(page, deck);
  await openFleet(page);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await rail.getByRole('button', { name: 'Deny: Bash in Review the repo' }).click();
  await expect(rail.getByRole('alert')).toHaveText('Refused: The request is not open any more.');
  await expect(rail.getByRole('button', { name: 'Deny: Bash in Review the repo' })).toBeEnabled();
  await expect(rail.getByRole('button', { name: 'Allow: Bash in Review the repo' })).toBeEnabled();
  const calls = deck.calls.filter((c) => c.path === '/api/commands/approval.answer');
  expect(calls).toHaveLength(1);
  expect(calls[0]?.body).toMatchObject({
    command: { decision: { option_id: 'deny', updated_input: null, text: null } },
  });
});

/** Two waiting sessions on the fleet screen; the older request is first in the rail. */
function twoWaiting(): DeckSession[] {
  const first = session({ open_requests: [request], activity: 'needs_you' }, 'needs_you');
  const second: DeckSession = {
    station_id: ST,
    status: 'needs_you',
    summary: {
      ...first.summary,
      session_id: 'second',
      title: 'Write the docs',
      open_requests: [
        {
          ...request,
          request_id: 'req-7',
          title: 'Edit',
          requested_at: request.requested_at + 1,
        },
      ],
    },
  };
  return [first, second];
}

test('the a key answers the selected request of the rail with its first option', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = twoWaiting();
  deck.on('/api/commands/approval.answer', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'done'),
  }));
  await setUp(page, deck);
  await openFleet(page);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail.locator('[data-request="req-6"]')).toHaveAttribute('data-selected', 'true');
  await page.keyboard.press('a');
  await expect
    .poll(() => deck.calls.filter((c) => c.path === '/api/commands/approval.answer').length)
    .toBe(1);
  const call = deck.calls.find((c) => c.path === '/api/commands/approval.answer');
  expect(call?.body).toMatchObject({
    command: {
      session_id: ID,
      request_id: 'req-6',
      decision: { option_id: 'allow' },
    },
  });
});

test('the rail keys do nothing behind the Help dialog', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = twoWaiting();
  deck.on('/api/commands/approval.answer', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'done'),
  }));
  await setUp(page, deck);
  await openFleet(page);
  const answers = () => deck.calls.filter((c) => c.path === '/api/commands/approval.answer');
  await page.keyboard.press('?');
  const help = page.getByRole('dialog', { name: 'Keyboard and help' });
  await expect(help).toBeVisible();
  await page.keyboard.press('a');
  await page.keyboard.press('Escape');
  await expect(help).toHaveCount(0);
  expect(answers()).toHaveLength(0);
  await page.keyboard.press('a');
  await expect.poll(() => answers().length).toBe(1);
});

test('the bracket keys move the selection and d answers with the last option', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = twoWaiting();
  deck.on('/api/commands/approval.answer', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'done'),
  }));
  await setUp(page, deck);
  await openFleet(page);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  const first = rail.locator('[data-request="req-6"]');
  const second = rail.locator('[data-request="req-7"]');
  await expect(first).toHaveAttribute('data-selected', 'true');
  await page.keyboard.press(']');
  await expect(second).toHaveAttribute('data-selected', 'true');
  await expect(first).not.toHaveAttribute('data-selected', 'true');
  await page.keyboard.press(']');
  await expect(second).toHaveAttribute('data-selected', 'true');
  await page.keyboard.press('[');
  await expect(first).toHaveAttribute('data-selected', 'true');
  await page.keyboard.press(']');
  await page.keyboard.press('d');
  await expect
    .poll(() => deck.calls.filter((c) => c.path === '/api/commands/approval.answer').length)
    .toBe(1);
  const call = deck.calls.find((c) => c.path === '/api/commands/approval.answer');
  expect(call?.body).toMatchObject({
    command: { session_id: 'second', request_id: 'req-7', decision: { option_id: 'deny' } },
  });
});

test('an approval is answered from the dock of its session', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ open_requests: [request], activity: 'needs_you' }, 'needs_you')];
  deck.on('/api/commands/approval.answer', (body) => {
    deck.sessions = [session()];
    queueMicrotask(() => deck.publish());
    return { status: 200, body: receipt(idOf(body), 'done') };
  });
  await setUp(page, deck);
  await openSession(page, 'Review the repo');
  const needs = page.getByRole('region', { name: 'Needs you' });
  await expect(needs).toContainText('ls docs');
  await needs.getByRole('button', { name: 'Allow' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  const call = deck.calls.find((c) => c.path === '/api/commands/approval.answer');
  expect(call?.csrf).toBe(CSRF);
  expect(call?.body).toMatchObject({
    command: { decision: { option_id: 'allow', updated_input: null, text: null } },
  });
  await page.getByRole('button', { name: 'Back' }).click();
  await openFleet(page);
  await expect(page.getByRole('region', { name: 'Needs you rail' })).toHaveCount(0);
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
  await openSession(page, 'Review the repo');
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
  await expect(page.getByRole('alert').locator('span')).toHaveText('Refused: No run is active.');
  await page.getByRole('button', { name: 'Interrupt' }).click();
  await expect(page.getByRole('alert')).toContainText('Delivery is unknown');
  await page.locator('summary[aria-label="Session menu"]').click();
  // The order of the rows says "most restrictive first": the legend is "Mode" ("Model" is the
  // legend of the model rows, so the name is exact).
  const ladder = page.getByRole('group', { name: 'Mode', exact: true });
  // The modes are menu rows with their sentence-case names (the title keeps the raw id).
  await expect(ladder.getByRole('button')).toHaveText([
    'Default',
    'Plan',
    'Accept edits',
    "Don't ask",
    'Auto',
  ]);
  await expect(ladder.getByRole('button', { name: 'Default' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await ladder.getByRole('button', { name: 'Plan' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  // The phone has no composer model chip: its session menu holds the model rows (no list here:
  // the id field only).
  const model = page.getByRole('group', { name: 'Model', exact: true });
  await model.getByLabel('Model id or alias').fill(' claude-haiku-4-5 ');
  await model.getByRole('button', { name: 'Set model' }).click();
  // A pick closes the menu (the same as a model row).
  await expect(model).toBeHidden();
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
  await openSession(page, 'Review the repo');
  const composer = page.getByRole('form', { name: 'Composer' });
  await expect(composer.getByLabel('Message')).toBeDisabled();
  await expect(composer.getByRole('button', { name: 'Send' })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Interrupt' })).toHaveCount(0);
  // Stop is an item of the session menu: open the menu, then look for it.
  await page.locator('summary[aria-label="Session menu"]').click();
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Force stop…' })).toHaveCount(0);
  await expect(page.locator('summary[aria-label="Open in terminal"]')).toHaveCount(0);
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
  await openSession(page, 'Review the repo');
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

test('the menu lists the session facts, asks for a status update, and stars the session', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [
    session({
      callsign: 'falcon',
      worktree: '/wt/review',
      session_seq: 42,
      updated_at: Date.now(),
    }),
  ];
  deck.on('/api/commands/session.follow_up', (body) => ({
    status: 200,
    body: receipt(idOf(body), 'done'),
  }));
  await setUp(page, deck);
  await openSession(page, 'Review the repo');
  const view = page.getByRole('region', { name: 'Session' });
  const menu = view.locator('summary[aria-label="Session menu"]');
  await menu.click();
  const details = view.locator('.fd-sh-details');
  await expect(details).toContainText('Session');
  await expect(details).toContainText(ID);
  // The header shows the callsign: the details do not repeat it.
  await expect(details).not.toContainText('falcon');
  await expect(details).toContainText('/wt/review');
  await expect(details).toContainText('42');
  await expect(details.getByRole('button', { name: 'Copy the session id' })).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
  // Star fills the star of the title; the item then reads Unstar
  await expect(view.locator('.fd-sh-star')).toHaveCount(0);
  await view.getByRole('button', { name: 'Star', exact: true }).click();
  await expect(view.locator('.fd-sh-star')).toHaveCount(1);
  await expect(view.getByRole('button', { name: 'Unstar' })).toBeVisible();
  await view.getByRole('button', { name: 'Unstar' }).click();
  await expect(view.locator('.fd-sh-star')).toHaveCount(0);
  // a running session gets the nudge as a follow-up
  await view.getByRole('button', { name: 'Ask for a status update' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const call = deck.calls.find((c) => c.path === '/api/commands/session.follow_up');
  expect((call?.body as { command: unknown } | undefined)?.command).toEqual({
    op: 'follow_up',
    session_id: ID,
    input: {
      text: 'Give a short status update: what you finished, what you do now, and what blocks you.',
    },
  });
});

test('the last error shows under the header of an ended session, with the retry time', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [
    session(
      {
        state: 'exited',
        current_run: null,
        last_error: {
          code: 'usage_limit',
          message: 'The limit is reached',
          retry_at: Date.now() + 3 * 60_000,
        },
      },
      'ended',
    ),
  ];
  await setUp(page, deck);
  await openSession(page, 'Review the repo');
  const strip = page
    .getByRole('region', { name: 'Session' })
    .locator('.fd-sh-strip[data-tone="hazard"]');
  // The words of the error (the code only in the title); the retry is a sentence of its own.
  await expect(strip).toContainText('The limit is reached. Retry in');
  await expect(strip).toHaveAttribute('title', 'usage_limit: The limit is reached');
  expect(await fitsWidth(page)).toBe(true);
});

test('an ended session can be archived from the menu', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ state: 'exited', current_run: null }, 'ended')];
  await setUp(page, deck);
  await openSession(page, 'Review the repo');
  const view = page.getByRole('region', { name: 'Session' });
  await view.locator('summary[aria-label="Session menu"]').click();
  await expect(view.getByRole('button', { name: 'Ask for a status update' })).toHaveCount(0);
  await view.getByRole('button', { name: 'Archive', exact: true }).click();
  await expect(view.getByRole('button', { name: 'Unarchive' })).toBeVisible();
  await view.getByRole('button', { name: 'Unarchive' }).click();
  await expect(view.getByRole('button', { name: 'Archive', exact: true })).toBeVisible();
});
