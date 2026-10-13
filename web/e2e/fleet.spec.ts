// M3 6-2 / 6-4 (DL-205, DL-206): fleet mail and tasks from the fleet screen
// in a phone browser, against the scripted deck.
import { expect, test } from '@playwright/test';
import type { DeckSession, TaskView } from '../src/gen';
import { summary } from '../src/testing/summary';
import { FakeDeck, fitsWidth, logIn, openFleet, openPanel, stationView } from './fake-deck';

const ST = 'st-alpha';

function session(): DeckSession {
  return {
    station_id: ST,
    status: 'idle',
    summary: summary('s1', 'idle', 1_760_000_000_000, {
      title: 'Edit the docs',
      callsign: 'otter-ab12',
      cwd: '/home/me/projects/app',
    }),
  };
}

function task(title: string, extra: Partial<TaskView> = {}): TaskView {
  return {
    task_id: 't1',
    title,
    creator: 'owner',
    station_id: ST,
    session_id: 's1',
    state: 'open',
    result: null,
    delivery: 'delivered',
    reason: null,
    created_at: 1,
    updated_at: 1,
    completed_at: null,
    ...extra,
  };
}

test('a mail goes to the chosen session once, as a steer when asked', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [session()];
  const mails: unknown[] = [];
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) => {
    mails.push(body);
    return { status: 200, body: {} };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await expect(panel.getByLabel('Session')).toHaveValue(`${ST}/s1`);
  await panel.getByLabel('Text').fill('please rebase');
  await panel.getByLabel('Steer now (mail only)').check();
  await panel.getByRole('button', { name: 'Send mail' }).click();
  await expect(panel.getByRole('status')).toHaveText('Mail queued.');
  expect(mails).toHaveLength(1);
  expect(mails[0]).toMatchObject({
    station: ST,
    session_id: 's1',
    text: 'please rebase',
    delivery: 'steer',
  });
  expect(await fitsWidth(page)).toBe(true);
});

test('a task is created and its completion shows in the task list', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [session()];
  let tasks: TaskView[] = [];
  deck.on('/api/tasks', (body) => {
    if (body !== null) {
      tasks = [task((body as { title: string }).title)];
      return { status: 200, body: tasks[0] };
    }
    return { status: 200, body: tasks };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByLabel('Text').fill('write the docs');
  await panel.getByRole('button', { name: 'Create task' }).click();
  await expect(panel.getByRole('status')).toHaveText('Task created.');
  await expect(panel.getByRole('list', { name: 'Tasks' })).toContainText('write the docs Open');
  tasks = [task('write the docs', { state: 'done', result: 'merged' })];
  await page.reload();
  await expect(
    page.getByRole('region', { name: 'Mail and tasks' }).getByRole('list', { name: 'Tasks' }),
  ).toContainText('Done: merged');
  expect(await fitsWidth(page)).toBe(true);
});

test('the owner revokes a coordinator grant from the phone', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [session()];
  let state = 'active';
  const grant = () => ({
    grant_id: 'g1',
    station_id: ST,
    session_id: 'coord-1',
    classes: ['read', 'mail'],
    stations: [ST],
    spawn_cap: 3,
    spawn_used: 0,
    state,
    revoked_reason: state === 'revoked' ? 'owner' : null,
    generation: 1,
    expires_at: 1_760_000_000_000,
    last_used_at: null,
  });
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/coordinator/grants', () => ({ status: 200, body: [grant()] }));
  deck.on('/api/coordinator/grants/g1/revoke', () => {
    state = 'revoked';
    return { status: 200, body: grant() };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const grants = page.getByRole('list', { name: 'Coordinator grants' });
  await expect(grants).toContainText('read, mail: active');
  await grants.getByRole('button', { name: 'Revoke' }).click();
  await expect(grants).toContainText('read, mail: revoked');
  await expect(grants.getByRole('button', { name: 'Revoke' })).toHaveCount(0);
  expect(await fitsWidth(page)).toBe(true);
});

function sessionIn(id: string, title: string, extra: Partial<DeckSession['summary']> = {}) {
  const s: DeckSession = {
    station_id: ST,
    status: 'working',
    summary: summary(id, 'working', 1_760_000_000_000, { title, ...extra }),
  };
  return s;
}

test('mail to Everyone sends one mail for each live session', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [
    sessionIn('a', 'Alpha one'),
    sessionIn('b', 'Alpha two'),
    { ...sessionIn('c', 'Done one'), status: 'ended' },
    { ...sessionIn('d', 'Lost one'), status: 'lost' },
  ];
  const mails: { session_id: string; text: string }[] = [];
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) => {
    mails.push(body as { session_id: string; text: string });
    return { status: 200, body: {} };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByRole('button', { name: 'Everyone' }).click();
  await panel.getByLabel('Text').fill('stand up now');
  await expect(panel.getByRole('button', { name: 'Create task' })).toBeDisabled();
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByRole('status')).toHaveText('Sent to 2 of 2.');
  expect(mails.map((m) => m.session_id).sort()).toEqual(['a', 'b']);
  expect(mails.every((m) => m.text === 'stand up now')).toBe(true);
});

test('a mail that fails for one session is listed by name', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [sessionIn('a', 'Alpha one'), sessionIn('b', 'Alpha two')];
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) =>
    (body as { session_id: string }).session_id === 'b'
      ? { status: 409, body: { code: 'conflict', message: 'The mailbox is full.' } }
      : { status: 200, body: {} },
  );
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByRole('button', { name: 'Everyone' }).click();
  await panel.getByLabel('Text').fill('hello');
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByText('Sent to 1 of 2.')).toBeVisible();
  await expect(panel.getByRole('alert')).toContainText('Alpha two: ');
});

test('Clear hides an ended session and the Archived chip shows it again', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [
    sessionIn('a', 'Alpha one'),
    {
      ...sessionIn('e', 'Finished work', { state: 'exited' }),
      status: 'ended',
    },
  ];
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  const card = page.locator('a.fd-card[data-session="e"]');
  await expect(card).toBeVisible();
  await page.getByRole('button', { name: 'Clear (1)' }).click();
  await expect(card).toHaveCount(0);
  const chip = page.getByRole('button', { name: 'Archived (1)' });
  await chip.click();
  await expect(chip).toHaveAttribute('aria-pressed', 'true');
  await expect(card).toBeVisible();
  await expect(card).toContainText('Archived');
  await page.getByRole('button', { name: 'Restore all' }).click();
  await expect(chip).toHaveCount(0);
  await expect(card).toBeVisible();
  await expect(card).not.toContainText('Archived');
});

test('the cards show the bypass mode, the last error and the conflict', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [
    sessionIn('a', 'Alpha one', { mode: 'bypassPermissions' }),
    {
      ...sessionIn('i', 'Idle one', {
        last_error: { code: 'rate_limit', message: 'Try later', retry_at: null },
      }),
      status: 'idle',
    },
    sessionIn('b', 'Alpha two'),
  ];
  deck.on('/api/conflicts', () => ({
    status: 200,
    body: [
      {
        repo: null,
        file: 'src/lib.rs',
        touches: [
          {
            station_id: ST,
            session_id: 'a',
            session_title: 'Alpha one',
            path: 'lib.rs',
            touched_at: 1,
          },
          {
            station_id: ST,
            session_id: 'b',
            session_title: 'Alpha two',
            path: 'lib.rs',
            touched_at: 2,
          },
        ],
      },
    ],
  }));
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  const a = page.locator('a.fd-card[data-session="a"]');
  await expect(a).toContainText('bypassPermissions');
  await expect(a).toHaveAttribute('data-contested', 'true');
  await expect(a).toContainText('contested with Alpha two');
  await expect(page.locator('a.fd-card[data-session="i"]')).toContainText('rate_limit: Try later');
  await expect(page.getByText('1 conflict', { exact: true })).toBeVisible();
});

test('a card shows the mail badge and the touched-file chips from the extras', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [sessionIn('a', 'Alpha one'), sessionIn('b', 'Alpha two')];
  deck.on('/api/fleet/extras', () => ({
    status: 200,
    body: {
      sessions: [
        {
          station_id: ST,
          session_id: 'a',
          mail_pending: 2,
          files: ['src/app/main.rs', 'docs/NOTES.md'],
        },
      ],
    },
  }));
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  const a = page.locator('a.fd-card[data-session="a"]');
  await expect(a.locator('.fd-card-mail')).toHaveText('2');
  await expect(a.locator('.fd-card-files .fd-filechip')).toHaveText(['main.rs', 'NOTES.md']);
  await expect(page.locator('a.fd-card[data-session="b"] .fd-card-mail')).toHaveCount(0);
});

test.describe('wide screen', () => {
  test.use({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });

  test('Group by Repo makes one lane for each repository and the choice stays', async ({
    page,
  }) => {
    const deck = new FakeDeck();
    deck.stations = [stationView(ST, 'Alpha')];
    deck.sessions = [
      sessionIn('a', 'Alpha one', {
        repo: { root: '/w/app', origin: 'git@github.com:org/app.git' },
      }),
      sessionIn('b', 'Alpha two', { repo: { root: '/w/lib', origin: null } }),
      sessionIn('c', 'No repo here'),
    ];
    await deck.install(page);
    await logIn(page);
    await page.evaluate(() => {
      window.location.hash = '#/fleet';
    });
    const filters = page.getByRole('group', { name: 'Fleet filters' });
    const board = page.getByRole('main', { name: 'Fleet board' });
    await expect(board.getByRole('region', { name: 'Station Alpha' })).toBeVisible();
    await filters.getByRole('button', { name: 'Repo', exact: true }).last().click();
    await expect(board.getByRole('region', { name: 'Repository org/app' })).toBeVisible();
    await expect(board.getByRole('region', { name: 'Repository lib' })).toBeVisible();
    await expect(board.getByRole('region', { name: 'Repository No repository' })).toBeVisible();
    await expect(board.getByRole('region', { name: 'Station Alpha' })).toHaveCount(0);
    // the cards show their station in this grouping
    await expect(page.locator('a.fd-card[data-session="a"]')).toContainText('Alpha');
    await page.reload();
    await expect(board.getByRole('region', { name: 'Repository org/app' })).toBeVisible();
    // the repo filter
    await filters.getByRole('button', { name: 'lib', exact: true }).click();
    await expect(page.locator('a.fd-card')).toHaveCount(1);
    await expect(filters.getByText('1 of 3')).toBeVisible();
  });
});
