// M3 6-8 (DL-095): rename a session from the board and follow the fleet in
// the event feed, at a phone viewport against the scripted deck.
import { expect, type Page, test } from '@playwright/test';
import type { DeckCommand, DeckSession } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities } from '../src/testing/timeline';
import { CSRF, FakeDeck, fitsWidth, logIn, openFleet, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';

function session(
  id: string,
  seq: number,
  extra: Partial<DeckSession['summary']> = {},
  status: DeckSession['status'] = 'idle',
): DeckSession {
  return {
    station_id: ST,
    status,
    summary: summary(id, 'idle', 1_760_000_000_000 + seq * 1000, {
      title: 'Review the repo',
      capabilities: claudeCapabilities(),
      cwd: '/home/me/projects/app',
      session_seq: seq,
      ...extra,
    }),
  };
}

function done(command_id: string, reject: string | null = null): DeckCommand {
  return {
    command_id,
    station_id: ST,
    method: 'session.rename',
    origin: 'board',
    envelope: null,
    retry_until: 0,
    state: 'done',
    attempts: 1,
    sent_at: 1,
    receipt: {
      command_id,
      state: reject === null ? 'accepted' : 'rejected',
      station_seq: null,
      result: null,
      reject:
        reject === null ? null : { code: 'invalid_command', message: reject, unsupported: null },
      at: 1,
    },
    reason: null,
    created_at: 1,
    updated_at: 1,
  };
}

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  await logIn(page);
}

test('a session is renamed from the board, also after it ended', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session('s1', 1, { state: 'exited' }, 'ended')];
  deck.on('/api/commands/session.rename', (body) => {
    const b = body as { command_id: string; command: { title: string | null } };
    if (b.command.title === 'bad') {
      return { status: 200, body: done(b.command_id, 'the title has a control character') };
    }
    deck.sessions = [session('s1', 2, { state: 'exited', title: b.command.title }, 'ended')];
    queueMicrotask(() => deck.publish());
    return { status: 200, body: done(b.command_id) };
  });
  await setUp(page, deck);
  await openSession(page, 'Review the repo');
  const view = page.getByRole('region', { name: 'Session' });
  const rename = async () => {
    // the phone header has no pencil: Rename is an item of the session menu
    await view.locator('summary[aria-label="Session menu"]').click();
    await view.getByRole('button', { name: 'Rename' }).click();
  };
  await rename();
  const form = view.getByRole('form', { name: 'Rename the session' });
  await form.getByLabel('Title').fill('x'.repeat(81));
  await expect(form.getByRole('alert')).toContainText('longer than 80 characters');
  await expect(form.getByRole('button', { name: 'Save' })).toBeDisabled();
  await form.getByLabel('Title').fill('bad');
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(view.getByRole('alert')).toContainText('Refused: the title has a control character');
  await rename();
  await form.getByLabel('Title').fill('  Docs pass  ');
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(view.getByRole('heading', { level: 2 })).toHaveText('Docs pass');
  expect(await fitsWidth(page)).toBe(true);
  const calls = deck.calls.filter((c) => c.path === '/api/commands/session.rename');
  expect(calls.map((c) => c.csrf)).toEqual([CSRF, CSRF]);
  expect(calls[1]?.body).toMatchObject({
    station: ST,
    command: { op: 'rename', session_id: 's1', title: 'Docs pass' },
  });
  // An empty title clears it: the card falls back to the session id.
  await rename();
  await form.getByLabel('Title').fill('');
  await form.getByRole('button', { name: 'Clear the title' }).click();
  await expect(view.getByRole('heading', { level: 2 })).toHaveText('s1');
});

test('the event feed shows fleet changes newest first, filters them and opens a session', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session('s1', 1)];
  await setUp(page, deck);
  await openFleet(page);
  const feed = page.getByRole('region', { name: 'Event feed' });
  await expect(feed).toContainText('Quiet so far.');
  deck.sessions = [
    session('s1', 2, { current_turn: 't1', activity: 'working' }, 'working'),
    session('s2', 1, { title: 'Write the docs' }),
  ];
  deck.publish();
  await expect(feed.locator('li')).toHaveCount(1);
  await expect(feed.locator('li').first()).toContainText('Write the docs');
  await expect(feed.locator('li').first()).toContainText('joined in');
  deck.sessions = [
    session('s1', 3, {
      current_turn: null,
      open_requests: [
        {
          request_id: 'req-1',
          node_id: null,
          request_kind: 'command',
          title: 'Bash',
          details: 'ls',
          input: null,
          options: [{ option_id: 'allow', label: 'Allow' }],
          response: 'rpc_reply',
          requested_at: 1,
        },
      ],
    }),
    session('s2', 2, { title: 'Docs', state: 'exited' }, 'ended'),
  ];
  deck.publish();
  await expect(feed.locator('li')).toHaveCount(5);
  await expect(feed.locator('li').first()).toContainText('Docs');
  await expect(feed.locator('li').first()).toContainText('ended');
  await expect(feed.locator('li').last()).toContainText('joined in');
  expect(await fitsWidth(page)).toBe(true);
  await feed.getByRole('button', { name: 'Asks' }).click();
  await expect(feed.getByRole('button', { name: 'Asks' })).toHaveAttribute('aria-pressed', 'true');
  await expect(feed.locator('li')).toHaveCount(1);
  await expect(feed.locator('li')).toContainText('Review the repo');
  await expect(feed.locator('li')).toContainText('asks: Bash');
  await feed.getByRole('button', { name: 'Ends' }).click();
  await feed.getByRole('button', { name: 'Docs' }).click();
  await expect(page.getByRole('region', { name: 'Session' })).toContainText('Docs');
});
