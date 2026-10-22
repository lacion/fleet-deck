// B7: browse the sessions of a station, preview one, and continue it in the New session form.
import { expect, test } from '@playwright/test';
import type { NativePreview, NativeSession, NativeSessionList } from '../src/gen';
import { claudeCaps, codexCaps } from '../src/testing/capabilities';
import { FakeDeck, fitsWidth, logIn, stationView } from './fake-deck';

const alpha = stationView('st-alpha', 'Alpha', {
  info: {
    name: 'alpha',
    incarnation: 'inc-1',
    head: 3,
    live_sessions: 0,
    version: '0.2.0',
    max_sessions: 4,
    allow_shell: null,
    providers: [
      {
        instance: 'personal',
        provider: 'claude',
        version: '2.1.5',
        auth: 'signed_in',
        version_status: 'unknown',
        capabilities: claudeCaps(),
      },
      {
        instance: 'work',
        provider: 'codex',
        version: '0.160.0',
        auth: 'signed_in',
        version_status: 'unknown',
        capabilities: codexCaps(),
      },
    ],
  },
  info_at: 1_760_000_000_000,
});

const PIN = { home: '/home/me/.claude', dev: 7, ino: 99 };
const NOW = Date.now();
const native = (id: string, extra: Partial<NativeSession> = {}): NativeSession => ({
  provider: 'claude',
  native_id: id,
  instances: ['personal'],
  pin: PIN,
  cwd: '/home/me/projects/app',
  title: null,
  first_prompt: null,
  modified_at: NOW - 5 * 60_000,
  size: 20_480,
  parent_native_id: null,
  known: null,
  import: { state: 'ok' },
  verified: true,
  ...extra,
});

const LIST: NativeSessionList = {
  homes: [
    {
      provider: 'codex',
      instances: ['work'],
      state: { state: 'unavailable', reason: 'not_readable' },
    },
  ],
  sessions: [
    native('0b6c1d2e-aaaa-4bbb-8ccc-123456789abc', {
      title: 'Fix the login bug',
      cwd: '/home/me/projects/some/very/long/folder/name/that/keeps/going/app',
    }),
    native('11111111-aaaa-4bbb-8ccc-123456789abc', {
      first_prompt: 'Add a retry to the uploader',
      known: { session_id: 's-known', state: 'running' },
      import: { state: 'in_use', session_id: 's-known' },
      modified_at: NOW - 3 * 3_600_000,
    }),
    native('22222222-aaaa-4bbb-8ccc-123456789abc', {
      title: 'Old notes',
      import: { state: 'too_large' },
      size: 80_000_000,
      modified_at: NOW - 2 * 86_400_000,
    }),
  ],
  truncated: true,
  timed_out: false,
};

const PREVIEW: NativePreview = {
  items: [
    { id: 'i1', at: 0, kind: 'user', text: 'Why does login fail?', tool: null },
    { id: 'i2', at: 0, kind: 'assistant', text: 'The token expires too soon.', tool: null },
  ],
  state: { kind: 'complete' },
  cwd: '/home/me/projects/app',
  size: 20_480,
  modified_at: NOW,
  earlier_omitted: true,
};

test('browse the sessions of a station, preview one, continue it', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  const native = deck.nativeSessions('st-alpha', LIST, PREVIEW);
  await deck.install(page);
  await logIn(page);
  await page.getByRole('link', { name: 'New', exact: true }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByText('More options').click();
  await form.getByRole('button', { name: 'Browse sessions on this station' }).click();

  const dialog = page.getByRole('dialog', { name: 'Sessions on Alpha' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText('personal · Claude')).toBeVisible();
  await expect(dialog.getByText('Fix the login bug')).toBeVisible();
  await expect(dialog.getByText('Add a retry to the uploader')).toBeVisible();
  await expect(dialog.getByText('A Fleet Deck session uses it.')).toBeVisible();
  await expect(dialog.getByText('The transcript is too large to import.')).toBeVisible();
  await expect(dialog.getByText('work: the home folder cannot be read.')).toBeVisible();
  await expect(dialog.getByText('Showing the newest 3 sessions.')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);

  await dialog.getByRole('button', { name: /Fix the login bug/ }).click();
  await expect(dialog.getByText('Why does login fail?')).toBeVisible();
  await expect(dialog.getByText('Earlier messages are not shown.')).toBeVisible();
  expect(native.previews).toEqual([
    {
      provider_instance: 'personal',
      native_id: '0b6c1d2e-aaaa-4bbb-8ccc-123456789abc',
      pin: PIN,
    },
  ]);
  // Escape goes back to the list first.
  await page.keyboard.press('Escape');
  await expect(dialog.getByText('Fix the login bug')).toBeVisible();
  await expect(dialog.getByText('Why does login fail?')).toHaveCount(0);

  await dialog.getByRole('button', { name: /Fix the login bug/ }).click();
  await dialog.getByRole('button', { name: 'Continue it' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(form.getByLabel('Import CLI session id (optional)')).toHaveValue(
    '0b6c1d2e-aaaa-4bbb-8ccc-123456789abc',
  );
  await expect(form.getByLabel('Directory path')).toHaveValue(
    '/home/me/projects/some/very/long/folder/name/that/keeps/going/app',
  );
  await expect(form.getByLabel('Provider')).toContainText('Claude (personal)');
  await expect(form.getByRole('button', { name: 'Import session' })).toBeEnabled();
});

test('a station that does not list its sessions shows the text without a retry', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  deck.on('/api/stations/st-alpha/native-sessions', () => ({
    status: 409,
    body: {
      error: 'native_sessions_unsupported',
      message: 'This station does not list its sessions yet. Update it.',
    },
  }));
  await deck.install(page);
  await logIn(page);
  await page.getByRole('link', { name: 'New', exact: true }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByText('More options').click();
  await form.getByRole('button', { name: 'Browse sessions on this station' }).click();
  const dialog = page.getByRole('dialog', { name: 'Sessions on Alpha' });
  await expect(dialog.getByRole('alert')).toHaveText(
    'This station does not list its sessions yet. Update it.',
  );
  await expect(dialog.getByRole('button', { name: 'Try again' })).toHaveCount(0);
});
