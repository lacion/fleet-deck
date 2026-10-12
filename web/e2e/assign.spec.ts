// M3 6-3 (DL-204): `assign auto` from the fleet screen in a phone browser.
import { expect, test } from '@playwright/test';
import { FakeDeck, fitsWidth, logIn, stationView } from './fake-deck';

test('assign auto sends the task once and names the session', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  const bodies: unknown[] = [];
  deck.on('/api/assign', (body) => {
    bodies.push(body);
    return {
      status: 200,
      body: {
        outcome: 'assigned',
        station_id: 'st-alpha',
        session_id: 's1',
        callsign: 'otter-ab12',
        command_id: (body as { command_id: string }).command_id,
        state: 'done',
      },
    };
  });
  await deck.install(page);
  await logIn(page);
  const form = page.getByRole('form', { name: 'Assign a task' });
  await form.getByLabel('Task').fill('fix the login');
  await form.getByLabel('Repository').fill('/home/me/projects/app');
  await form.getByRole('button', { name: 'Assign auto' }).click();
  await expect(form.getByRole('status')).toHaveText('Assigned to otter-ab12.');
  expect(bodies).toHaveLength(1);
  expect(bodies[0]).toMatchObject({ text: 'fix the login', repo: '/home/me/projects/app' });
  expect(await fitsWidth(page)).toBe(true);
});

test('an unrouted task offers a spawn on the best station', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  deck.on('/api/assign', () => ({
    status: 200,
    body: {
      outcome: 'unrouted',
      reason: 'no connected session fits the task',
      spawn_on: { station_id: 'st-alpha', name: 'Alpha' },
    },
  }));
  await deck.install(page);
  await logIn(page);
  const form = page.getByRole('form', { name: 'Assign a task' });
  await form.getByLabel('Task').fill('write the docs');
  await form.getByRole('button', { name: 'Assign auto' }).click();
  await expect(form.getByRole('status')).toContainText('No session fits.');
  await form.getByRole('button', { name: 'Spawn on Alpha' }).click();
  await expect(page.getByRole('button', { name: 'New session' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
});
