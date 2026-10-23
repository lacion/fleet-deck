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
    completion_reason: null,
    question: null,
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
  // A grant is a row; its Change… opens the Fleet access dialog, where Revoke asks to confirm.
  const grants = page.getByRole('list', { name: 'Coordinator grants' });
  await expect(grants).toContainText('Coordinator · active');
  await grants.getByRole('button', { name: 'Change…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Fleet access' });
  await expect(dialog).toContainText('read');
  await dialog.getByRole('button', { name: 'Revoke' }).click();
  await dialog.getByRole('button', { name: 'Revoke' }).click();
  await expect(dialog.getByRole('status')).toHaveText('Grant revoked.');
  expect(await fitsWidth(page)).toBe(true);
  await dialog.locator('.fd-dialog-head').getByRole('button', { name: 'Close' }).click();
  // A revoked grant leaves the list.
  await expect(grants).toHaveCount(0);
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

test('a retry after a failed mail sends only the failed target, with its first mail_id', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [sessionIn('a', 'Alpha one'), sessionIn('b', 'Alpha two')];
  const mails: { mail_id: string; session_id: string }[] = [];
  let first = true;
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) => {
    mails.push(body as { mail_id: string; session_id: string });
    if (first) {
      first = false;
      return { status: 500, body: { code: 'internal', message: 'The deck failed.' } };
    }
    return { status: 200, body: {} };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByRole('button', { name: 'Everyone' }).click();
  await panel.getByLabel('Text').fill('hello');
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByText('Sent to 1 of 2.')).toBeVisible();
  expect(mails).toHaveLength(2);
  const failedTry = mails[0];
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByText('Sent to 2 of 2.')).toBeVisible();
  expect(mails).toHaveLength(3);
  expect(mails[2]).toEqual(failedTry);
  await expect(panel.getByLabel('Text')).toHaveValue('');
});

test('a retry keeps its first recipients when the fleet changes; Start over adopts it', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [sessionIn('a', 'Alpha one'), sessionIn('b', 'Alpha two')];
  const mails: { mail_id: string; session_id: string }[] = [];
  let first = true;
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) => {
    mails.push(body as { mail_id: string; session_id: string });
    if (first) {
      first = false;
      return { status: 500, body: { code: 'internal', message: 'The deck failed.' } };
    }
    return { status: 200, body: {} };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByRole('button', { name: 'Everyone' }).click();
  await panel.getByLabel('Text').fill('hello');
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByText('Sent to 1 of 2.')).toBeVisible();
  await expect(
    panel.getByText('Send again retries the 1 failed of the first 2 recipients.'),
  ).toBeVisible();
  expect(mails).toHaveLength(2);
  const failedTry = mails[0];
  deck.setSession(sessionIn('c', 'Alpha three'));
  await expect(panel.getByRole('button', { name: 'Send to 3' })).toBeVisible();
  await panel.getByRole('button', { name: 'Send to 3' }).click();
  await expect(panel.getByText('Sent to 2 of 2.')).toBeVisible();
  expect(mails).toHaveLength(3);
  expect(mails[2]).toEqual(failedTry);
  await expect(panel.getByText(/Send again retries/)).toHaveCount(0);
});

test('a retry stays possible when every first recipient has ended', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [sessionIn('a', 'Alpha one'), sessionIn('b', 'Alpha two')];
  const mails: { mail_id: string; session_id: string }[] = [];
  let first = true;
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) => {
    mails.push(body as { mail_id: string; session_id: string });
    if (first) {
      first = false;
      return { status: 500, body: { code: 'internal', message: 'The deck failed.' } };
    }
    return { status: 200, body: {} };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByRole('button', { name: 'Station' }).click();
  await panel.getByLabel('Text').fill('hello');
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByText('Sent to 1 of 2.')).toBeVisible();
  expect(mails).toHaveLength(2);
  const failedTry = mails[0];
  deck.setSession({ ...sessionIn('a', 'Alpha one'), status: 'ended' });
  deck.setSession({ ...sessionIn('b', 'Alpha two'), status: 'ended' });
  await expect(panel.getByText('No live session matches.')).toHaveCount(0);
  const again = panel.getByRole('button', { name: 'Send again' });
  await expect(again).toBeEnabled();
  await again.click();
  await expect(panel.getByText('Sent to 2 of 2.')).toBeVisible();
  expect(mails).toHaveLength(3);
  expect(mails[2]).toEqual(failedTry);
});

test('the phone keeps the bottom bar on the Files page', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [session()];
  await deck.install(page);
  await logIn(page);
  await page.goto('/#/files');
  await expect(
    page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }),
  ).toBeVisible();
});

test('Start over sends to all current targets with new mail ids', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [sessionIn('a', 'Alpha one'), sessionIn('b', 'Alpha two')];
  const mails: { mail_id: string; session_id: string }[] = [];
  let first = true;
  deck.on('/api/tasks', () => ({ status: 200, body: [] }));
  deck.on('/api/mail', (body) => {
    mails.push(body as { mail_id: string; session_id: string });
    if (first) {
      first = false;
      return { status: 500, body: { code: 'internal', message: 'The deck failed.' } };
    }
    return { status: 200, body: {} };
  });
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Mail & tasks');
  const panel = page.getByRole('region', { name: 'Mail and tasks' });
  await panel.getByRole('button', { name: 'Everyone' }).click();
  await panel.getByLabel('Text').fill('hello');
  await panel.getByRole('button', { name: 'Send to 2' }).click();
  await expect(panel.getByText('Sent to 1 of 2.')).toBeVisible();
  const firstIds = mails.map((m) => m.mail_id);
  deck.setSession(sessionIn('c', 'Alpha three'));
  await expect(panel.getByRole('button', { name: 'Send to 3' })).toBeVisible();
  await panel.getByRole('button', { name: 'Start over' }).click();
  await expect(panel.getByRole('button', { name: 'Start over' })).toHaveCount(0);
  await expect(panel.getByText('Sent to 1 of 2.')).toHaveCount(0);
  await panel.getByRole('button', { name: 'Send to 3' }).click();
  await expect(panel.getByText('Sent to 3 of 3.')).toBeVisible();
  const second = mails.slice(2);
  expect(second.map((m) => m.session_id).sort()).toEqual(['a', 'b', 'c']);
  for (const m of second) expect(firstIds).not.toContain(m.mail_id);
});

test('Archive hides an ended session and the Show archived chip shows it again', async ({
  page,
}) => {
  const openFilters = () => page.getByText('Filters', { exact: true }).click();
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
  const card = page.locator('a.fd-strip[data-session="e"]');
  // The lane folds its ended strips into a collapsed Ended row (DESIGN-SYSTEM §4.2).
  const ended = page.locator('.fd-lane-ended > summary');
  await ended.click();
  await expect(card).toBeVisible();
  await page.getByRole('button', { name: 'Archive 1' }).click();
  await expect(card).toHaveCount(0);
  await openFilters();
  const chip = page.getByRole('group', { name: 'Fleet filters' }).getByRole('button', {
    name: 'Show archived 1',
  });
  await chip.click();
  await expect(chip).toHaveAttribute('aria-pressed', 'true');
  await page.keyboard.press('Escape');
  await ended.click();
  await expect(card).toBeVisible();
  await expect(card).toHaveAttribute('data-archived', 'true');
  await expect(card.locator('[data-ico="archived"]')).toHaveAttribute('title', 'Archived');
  // The active-filter chip clears the filter and hides the session again
  await expect(chip).toBeHidden();
  await page.getByRole('button', { name: 'Archived shown' }).click();
  await expect(card).toHaveCount(0);
  await openFilters();
  await chip.click();
  await page.keyboard.press('Escape');
  await ended.click();
  await expect(card).toBeVisible();
  await page.getByRole('button', { name: 'Restore all' }).click();
  await expect(card).toBeVisible();
  await expect(card).not.toHaveAttribute('data-archived', 'true');
});

test('the strips show the bypass mode, the last error and the conflict', async ({ page }) => {
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
  const a = page.locator('a.fd-strip[data-session="a"]');
  await expect(a.locator('[data-ico="risky"]')).toHaveAttribute(
    'title',
    'Permission mode: bypassPermissions',
  );
  await expect(a).toHaveAttribute('data-contested', 'true');
  await expect(a.locator('[data-ico="contested"]')).toHaveAttribute(
    'title',
    'Contested with Alpha two',
  );
  await expect(page.locator('a.fd-strip[data-session="i"]')).toContainText('Try later.');
  await expect(page.getByText('1 conflict', { exact: true })).toBeVisible();
});

test('a strip shows the mail icon and the touched files from the extras', async ({ page }) => {
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
  const a = page.locator('a.fd-strip[data-session="a"]');
  await expect(a.locator('[data-ico="mail"]')).toHaveText('2');
  await expect(a.locator('.fd-strip-now')).toHaveAttribute('title', 'Touched: main.rs, NOTES.md');
  await expect(page.locator('a.fd-strip[data-session="b"] [data-ico="mail"]')).toHaveCount(0);
});

test.describe('wide screen', () => {
  test.use({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });

  test('Group by repository makes one lane for each repository and the choice stays', async ({
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
    await page.getByLabel('Group by').selectOption('repo');
    await expect(board.getByRole('region', { name: 'Repository org/app' })).toBeVisible();
    await expect(board.getByRole('region', { name: 'Repository lib' })).toBeVisible();
    await expect(
      board.getByRole('region', { name: 'Repository Outside a repository' }),
    ).toBeVisible();
    await expect(board.getByRole('region', { name: 'Station Alpha' })).toHaveCount(0);
    // the strips show their station in this grouping
    await expect(page.locator('a.fd-strip[data-session="a"]')).toContainText('Alpha');
    await page.reload();
    await expect(board.getByRole('region', { name: 'Repository org/app' })).toBeVisible();
    // the repo filter, in the Filters menu
    await page.getByText('Filters', { exact: true }).click();
    await filters.getByRole('button', { name: 'lib', exact: true }).click();
    await expect(page.locator('a.fd-strip')).toHaveCount(1);
    await expect(page.getByText('1 of 3')).toBeVisible();
    await expect(page.locator('.fd-filters-menu > summary')).toHaveText('Filters 1');
    // the active-filter chip clears the repo filter
    await page.getByRole('button', { name: 'Repo: lib' }).click();
    await expect(page.locator('a.fd-strip')).toHaveCount(3);
    await expect(page.getByRole('button', { name: 'Repo: lib' })).toHaveCount(0);
  });
});
