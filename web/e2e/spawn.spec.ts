// M2 P4-4: the spawn form and the directory picker in a phone browser.
import { expect, test } from '@playwright/test';
import type { WorkspaceListing } from '../src/gen';
import { claudeCaps, codexCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { CSRF, FakeDeck, fitsWidth, logIn, stationView } from './fake-deck';

const alpha = stationView('st-alpha', 'Alpha', {
  info: {
    name: 'alpha',
    incarnation: 'inc-1',
    head: 3,
    live_sessions: 0,
    version: '0.2.0',
    max_sessions: 4,
    providers: [
      {
        instance: 'personal',
        provider: 'claude',
        version: '2.1.5',
        auth: 'signed_in',
        capabilities: claudeCaps(),
      },
      {
        instance: 'work',
        provider: 'codex',
        version: '0.160.0',
        auth: 'signed_in',
        capabilities: codexCaps(),
      },
    ],
  },
  info_at: 1_760_000_000_000,
});

const ROOT = '/home/me/projects';
const LISTINGS: Record<string, WorkspaceListing> = {
  '': {
    path: null,
    parent: null,
    entries: [{ name: 'projects', path: ROOT, repository: false }],
    truncated: false,
  },
  [ROOT]: {
    path: ROOT,
    parent: null,
    entries: [
      { name: 'app', path: `${ROOT}/app`, repository: true },
      { name: 'notes', path: `${ROOT}/notes`, repository: false },
    ],
    truncated: false,
  },
  [`${ROOT}/app`]: { path: `${ROOT}/app`, parent: ROOT, entries: [], truncated: false },
};

function picker(deck: FakeDeck): void {
  deck.on('/api/stations/st-alpha/workspaces', (_body, query) => {
    const listing = LISTINGS[query.get('path') ?? ''];
    return listing === undefined
      ? { status: 400, body: { code: 'outside_roots', message: 'The path is outside the roots.' } }
      : { status: 200, body: listing };
  });
}

async function chooseApp(page: import('@playwright/test').Page): Promise<void> {
  await page.getByRole('button', { name: 'New session' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('button', { name: 'Browse the station' }).click();
  await form.getByRole('button', { name: 'Open projects' }).click();
  await expect(form.getByRole('button', { name: 'Open app' })).toContainText('git');
  await form.getByRole('button', { name: 'Open app' }).click();
  await expect(form.getByRole('button', { name: 'Up one level' })).toBeVisible();
  await form.getByRole('button', { name: 'Use this directory' }).click();
  await expect(form).toContainText(`Chosen: ${ROOT}/app`);
}

test('a session starts from the form with a picked directory and opens', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  picker(deck);
  deck.on('/api/commands/session.start', () => {
    deck.sessions = [
      {
        station_id: 'st-alpha',
        status: 'working',
        summary: summary('s-new', 'working', 1, { title: 'Fix the login', cwd: `${ROOT}/app` }),
      },
    ];
    deck.publish();
    return {
      status: 200,
      body: {
        state: 'done',
        receipt: {
          state: 'accepted',
          result: { session_id: 's-new', input_id: 'i-1' },
          reject: null,
        },
      },
    };
  });
  await deck.install(page);
  await logIn(page);
  await chooseApp(page);
  const form = page.getByRole('region', { name: 'New session' });
  await expect(form.getByLabel('Provider')).toContainText('personal · claude · 2.1.5');
  await form.getByLabel('Permission mode').selectOption('plan');
  await form.getByLabel('Model (optional)').fill('sonnet');
  await form.getByLabel('Title (optional)').fill('Fix the login');
  await form.getByLabel('First prompt (optional)').fill('Find the bug in login.ts');
  expect(await fitsWidth(page)).toBe(true);
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();

  const starts = deck.calls.filter((c) => c.path === '/api/commands/session.start');
  expect(starts).toHaveLength(1);
  expect(starts[0]?.csrf).toBe(CSRF);
  expect(starts[0]?.body).toMatchObject({
    station: 'st-alpha',
    command: {
      op: 'start',
      spec: {
        provider_instance: 'personal',
        cwd: `${ROOT}/app`,
        mode: 'plan',
        model: 'sonnet',
        title: 'Fix the login',
      },
      prompt: { text: 'Find the bug in login.ts' },
    },
  });
  await page.getByRole('button', { name: 'Open the new session' }).click();
  await expect(page.getByRole('heading', { name: 'Fix the login' })).toBeVisible();
});

test('the mode list follows the capabilities of the chosen provider (P5-3)', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  picker(deck);
  deck.on('/api/commands/session.start', () => ({
    status: 200,
    body: {
      state: 'done',
      receipt: { state: 'accepted', result: { session_id: 's-cx', input_id: null }, reject: null },
    },
  }));
  await deck.install(page);
  await logIn(page);
  await chooseApp(page);
  const form = page.getByRole('region', { name: 'New session' });
  const mode = form.getByLabel('Permission mode');
  await expect(mode.locator('option')).toHaveText([
    'Station default',
    'default',
    'plan',
    'acceptEdits',
    'dontAsk',
    'auto',
  ]);
  await mode.selectOption('plan');
  // Another provider resets the mode and offers its own native modes.
  await form.getByLabel('Provider').selectOption('work');
  await expect(mode).toHaveValue('');
  await expect(mode.locator('option')).toHaveText(['Station default', ...codexCaps().modes]);
  await mode.selectOption('untrusted/read-only');
  expect(await fitsWidth(page)).toBe(true);
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const starts = deck.calls.filter((c) => c.path === '/api/commands/session.start');
  expect(starts).toHaveLength(1);
  expect(starts[0]?.body).toMatchObject({
    command: {
      op: 'start',
      spec: { provider_instance: 'work', cwd: `${ROOT}/app`, mode: 'untrusted/read-only' },
    },
  });
});

test('a refused start shows the station reason and keeps the form', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  picker(deck);
  deck.on('/api/commands/session.start', () => ({
    status: 200,
    body: {
      state: 'done',
      receipt: {
        state: 'rejected',
        result: null,
        reject: { code: 'capacity', message: 'The station runs 4 of 4 sessions.' },
      },
    },
  }));
  await deck.install(page);
  await logIn(page);
  await chooseApp(page);
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByLabel('First prompt (optional)').fill('hello');
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect(page.getByRole('alert')).toContainText('Refused: The station runs 4 of 4 sessions.');
  await expect(form.getByLabel('First prompt (optional)')).toHaveValue('hello');
  await expect(page.getByRole('button', { name: 'Open the new session' })).toHaveCount(0);
});

test('a read-only login cannot browse or start', async ({ page }) => {
  const deck = new FakeDeck();
  deck.scopes = ['read'];
  deck.stations = [alpha];
  picker(deck);
  await deck.install(page);
  await logIn(page);
  await page.getByRole('button', { name: 'New session' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await expect(form).toContainText('Starting sessions needs the drive scope.');
  await expect(form.getByRole('button', { name: 'Browse the station' })).toBeDisabled();
  await expect(form.getByRole('button', { name: 'Start session' })).toBeDisabled();
  expect(deck.calls.filter((c) => c.path.includes('/workspaces'))).toHaveLength(0);
});

test('a terminal CLI session is imported by its id as a driven session (6-12)', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  picker(deck);
  deck.on('/api/commands/session.import', () => ({
    status: 200,
    body: {
      state: 'done',
      receipt: { state: 'accepted', result: { session_id: 's-imp', input_id: null }, reject: null },
    },
  }));
  await deck.install(page);
  await logIn(page);
  await chooseApp(page);
  const form = page.getByRole('region', { name: 'New session' });
  const id = form.getByLabel('Import CLI session id (optional)');
  await id.fill('--resume');
  await expect(form.getByRole('alert')).toContainText('starts with a letter or a digit');
  await expect(form.getByRole('button', { name: 'Import session' })).toBeDisabled();
  await id.fill('0b6c1d2e-aaaa-4bbb-8ccc-123456789abc');
  await form.getByLabel('First prompt (optional)').fill('Go on');
  expect(await fitsWidth(page)).toBe(true);
  await form.getByRole('button', { name: 'Import session' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Done.' })).toBeVisible();
  const imports = deck.calls.filter((c) => c.path === '/api/commands/session.import');
  expect(imports).toHaveLength(1);
  expect(imports[0]?.csrf).toBe(CSRF);
  expect(imports[0]?.body).toMatchObject({
    station: 'st-alpha',
    command: {
      op: 'import',
      spec: { provider_instance: 'personal', cwd: `${ROOT}/app` },
      native_session_id: '0b6c1d2e-aaaa-4bbb-8ccc-123456789abc',
      prompt: { text: 'Go on' },
    },
  });
  expect(deck.calls.filter((c) => c.path === '/api/commands/session.start')).toHaveLength(0);
});
