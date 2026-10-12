// M3 6-2 / 6-4 (DL-205, DL-206): fleet mail and tasks from the fleet screen
// in a phone browser, against the scripted deck.
import { expect, test } from '@playwright/test';
import type { DeckSession, TaskView } from '../src/gen';
import { summary } from '../src/testing/summary';
import { FakeDeck, fitsWidth, logIn, stationView } from './fake-deck';

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
  const grants = page.getByRole('list', { name: 'Coordinator grants' });
  await expect(grants).toContainText('read, mail: active');
  await grants.getByRole('button', { name: 'Revoke' }).click();
  await expect(grants).toContainText('read, mail: revoked');
  await expect(grants.getByRole('button', { name: 'Revoke' })).toHaveCount(0);
  expect(await fitsWidth(page)).toBe(true);
});
