// B8: `!command` in the composer, the shell card in the chat, and the Claude result chip that
// goes with the next prompt, against the scripted deck.
import { expect, type Page, test } from '@playwright/test';
import type { DeckCommand, DeckSession, Event } from '../src/gen';
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

const idOf = (body: unknown) => (body as { command_id: string }).command_id;
const accepted = (method: string) => (body: unknown) => ({
  status: 200,
  body: {
    command_id: idOf(body),
    station_id: ST,
    method,
    origin: 'board',
    envelope: null,
    retry_until: 0,
    state: 'done',
    receipt: { state: 'accepted', reject: null },
    reason: null,
  },
});

const shellEvents = (command: string, output: string): Event[] => [
  {
    kind: 'tool.started',
    node_id: 'shell-e1',
    parent_node_id: null,
    tool_kind: 'shell',
    title: 'Shell command',
    input_summary: command,
    files: [],
    provider_refs: {},
  },
  { kind: 'tool.output', node_id: 'shell-e1', chunk: output, truncated: false },
  {
    kind: 'tool.completed',
    node_id: 'shell-e1',
    status: 'ok',
    files: [],
    exit_code: 0,
    provider_refs: { shell: 'done' },
  },
];

async function setUp(page: Page) {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  deck.stations = [stationView(ST, 'Alpha')];
  deck.on('/api/commands/session.shell_command', accepted('session.shell_command'));
  deck.on('/api/commands/session.prompt', accepted('session.prompt'));
  await deck.install(page);
  await logIn(page);
  await openSession(page, 'Fix the docs');
  return { deck, view: page.getByRole('region', { name: 'Session' }) };
}

test('!pwd runs a shell command; its card shows the output; a chip goes with the next prompt', async ({
  page,
}) => {
  const { deck, view } = await setUp(page);
  const composer = view.getByRole('form', { name: 'Composer' });
  const box = composer.getByLabel('Message');

  await box.fill('!pwd');
  await expect(composer.getByRole('img', { name: 'Shell command' })).toBeVisible();
  await expect(box).toHaveAttribute('placeholder', 'Shell command (runs in the session folder)');
  expect(await fitsWidth(page)).toBe(true);
  await composer.getByRole('button', { name: 'Run' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const sent = deck.calls.filter((c) => c.path === '/api/commands/session.shell_command');
  expect(sent.map((c) => (c.body as { command: unknown }).command)).toEqual([
    { op: 'shell_command', session_id: ID, command: 'pwd' },
  ]);

  // The station answers with the shell item.
  deck.emit(ST, ID, envelopes(ID, shellEvents('pwd', '/home/me/app\n')));
  const card = view.locator('[data-node="shell-e1"]');
  await expect(card).toContainText('Shell command');
  await expect(card).toContainText('pwd');
  await expect(card).toContainText('/home/me/app');
  await expect(card).toContainText('exit 0');
  expect(await fitsWidth(page)).toBe(true);

  // The finished command is a pending result of the next prompt.
  await expect(composer.getByText('Includes the output of')).toBeVisible();
  await box.fill('what did it print?');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(composer.getByText('Includes the output of')).toHaveCount(0);
  const prompts = deck.calls.filter((c) => c.path === '/api/commands/session.prompt');
  expect(prompts.map((c) => (c.body as { command: unknown }).command)).toEqual([
    {
      op: 'prompt',
      session_id: ID,
      input: { text: 'what did it print?', shell_results: ['e1'] },
    },
  ]);
});

test('a removed chip stays out of the prompt; \\! sends a literal prompt', async ({ page }) => {
  const { deck, view } = await setUp(page);
  deck.emit(ST, ID, envelopes(ID, shellEvents('ls', 'a\n')));
  const composer = view.getByRole('form', { name: 'Composer' });
  const box = composer.getByLabel('Message');
  await composer.getByRole('button', { name: 'Remove the output of !ls' }).click();
  await expect(composer.getByText('Includes the output of')).toHaveCount(0);
  await box.fill('\\!important: keep going');
  await expect(composer.getByRole('img', { name: 'Shell command' })).toHaveCount(0);
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const prompts = deck.calls.filter((c) => c.path === '/api/commands/session.prompt');
  expect(prompts.map((c) => (c.body as { command: unknown }).command)).toEqual([
    { op: 'prompt', session_id: ID, input: { text: '!important: keep going' } },
  ]);
});

const running = (): Event[] => [
  {
    kind: 'tool.started',
    node_id: 'shell-e2',
    parent_node_id: null,
    tool_kind: 'shell',
    title: 'Shell command',
    input_summary: 'sleep 600',
    files: [],
    provider_refs: {},
  },
];

test('a running card has Stop, which interrupts the session', async ({ page }) => {
  const { deck, view } = await setUp(page);
  deck.on('/api/commands/session.interrupt', accepted('session.interrupt'));
  deck.emit(ST, ID, envelopes(ID, running()));
  const card = view.locator('[data-node="shell-e2"]');
  await expect(card).toContainText('Running…');
  await expect(card.getByRole('button', { name: 'Release the folder' })).toHaveCount(0);
  await card.getByRole('button', { name: 'Stop' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const sent = deck.calls.filter((c) => c.path === '/api/commands/session.interrupt');
  expect(sent.map((c) => (c.body as { command: unknown }).command)).toEqual([
    { op: 'interrupt', session_id: ID },
  ]);
});

test('on an ended session the owner releases the folder after a question', async ({ page }) => {
  const deck = new FakeDeck();
  const ended = session();
  ended.summary.state = 'exited';
  deck.sessions = [ended];
  deck.stations = [stationView(ST, 'Alpha')];
  deck.on('/api/commands/session.release_shell', accepted('session.release_shell'));
  await deck.install(page);
  await logIn(page);
  await openSession(page, 'Fix the docs');
  deck.emit(ST, ID, envelopes(ID, running()));
  const view = page.getByRole('region', { name: 'Session' });
  const card = view.locator('[data-node="shell-e2"]');
  await card.getByRole('button', { name: 'Release the folder' }).click();
  const dialog = page.getByRole('dialog', { name: 'Release this folder' });
  await expect(dialog).toContainText(
    'Only do this when you are sure that !sleep 600 is no longer running',
  );
  expect(await fitsWidth(page)).toBe(true);
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
  expect(deck.calls.filter((c) => c.path === '/api/commands/session.release_shell')).toHaveLength(
    0,
  );
  await card.getByRole('button', { name: 'Release the folder' }).click();
  await page
    .getByRole('dialog', { name: 'Release this folder' })
    .getByRole('button', { name: 'Release', exact: true })
    .click();
  await expect(dialog).toHaveCount(0);
  const sent = deck.calls.filter((c) => c.path === '/api/commands/session.release_shell');
  expect(sent.map((c) => (c.body as { command: unknown }).command)).toEqual([
    { op: 'release_shell', session_id: ID, effect_id: 'e2' },
  ]);
});

/** A command as the deck answers it (`done` with a refusal text, or `pending`). */
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
    reason: null,
    created_at: 1,
    updated_at: 1,
  };
}

test('a pending prompt that the station refuses keeps its shell chip', async ({ page }) => {
  const { deck, view } = await setUp(page);
  deck.on('/api/commands/session.prompt', (b) => ({
    status: 200,
    body: receipt(idOf(b), 'pending'),
  }));
  deck.route('GET', /^\/api\/commands\/([^/]+)$/, (m) => ({
    status: 200,
    body: receipt(m[1] ?? '', 'done', 'the session is busy'),
  }));
  deck.emit(ST, ID, envelopes(ID, shellEvents('pwd', '/home/me/app\n')));
  const composer = view.getByRole('form', { name: 'Composer' });
  await expect(composer.getByText('Includes the output of')).toBeVisible();
  await composer.getByLabel('Message').fill('what did it print?');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(view.getByRole('alert')).toContainText('Refused: the session is busy', {
    timeout: 5_000,
  });
  await expect(composer.getByText('Includes the output of')).toBeVisible();
  deck.on('/api/commands/session.prompt', accepted('session.prompt'));
  await composer.getByLabel('Message').fill('again');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(composer.getByText('Includes the output of')).toHaveCount(0);
  const prompts = deck.calls.filter((c) => c.path === '/api/commands/session.prompt');
  const last = prompts[prompts.length - 1];
  expect((last?.body as { command: unknown } | undefined)?.command).toEqual({
    op: 'prompt',
    session_id: ID,
    input: { text: 'again', shell_results: ['e1'] },
  });
});

test('a pending release that the station refuses stays in the dialog', async ({ page }) => {
  const deck = new FakeDeck();
  const ended = session();
  ended.summary.state = 'exited';
  deck.sessions = [ended];
  deck.stations = [stationView(ST, 'Alpha')];
  deck.on('/api/commands/session.release_shell', (b) => ({
    status: 200,
    body: receipt(idOf(b), 'pending'),
  }));
  deck.route('GET', /^\/api\/commands\/([^/]+)$/, (m) => ({
    status: 200,
    body: receipt(m[1] ?? '', 'done', 'the command still runs'),
  }));
  await deck.install(page);
  await logIn(page);
  await openSession(page, 'Fix the docs');
  deck.emit(ST, ID, envelopes(ID, running()));
  const view = page.getByRole('region', { name: 'Session' });
  const card = view.locator('[data-node="shell-e2"]');
  await card.getByRole('button', { name: 'Release the folder' }).click();
  const dialog = page.getByRole('dialog', { name: 'Release this folder' });
  await dialog.getByRole('button', { name: 'Release', exact: true }).click();
  await expect(dialog).toContainText('Sent. The station has not confirmed it yet.');
  await expect(dialog).toContainText('Refused: the command still runs', { timeout: 5_000 });
  await expect(dialog).toHaveCount(1);
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
});
