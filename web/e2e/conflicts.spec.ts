// M3 6-7 (DL-306): the conflict radar on the fleet screen at a phone
// viewport. A fleet change loads the radar again; a session of a conflict
// opens from the radar.
import { expect, test } from '@playwright/test';
import type { DeckSession, FileConflict } from '../src/gen';
import { summary } from '../src/testing/summary';
import { FakeDeck, fitsWidth, logIn, openFleet, stationView } from './fake-deck';

function session(station: string, id: string, title: string, at: number): DeckSession {
  return { station_id: station, status: 'working', summary: summary(id, 'working', at, { title }) };
}

test('two sessions on two stations that change one file show as a conflict', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha'), stationView('st-beta', 'Beta')];
  deck.sessions = [
    session('st-alpha', 's1', 'Alpha fix', 1_760_000_000_000),
    session('st-beta', 's2', 'Beta fix', 1_760_000_000_000),
  ];
  let answer: FileConflict[] = [];
  deck.on('/api/conflicts', () => ({ status: 200, body: answer }));
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  const radar = page.getByRole('region', { name: 'Conflict radar' });
  // without a conflict the radar takes no room
  await expect(page.getByRole('region', { name: 'Fleet panel' })).toBeVisible();
  await expect(radar).toHaveCount(0);

  const now = Date.now();
  answer = [
    {
      repo: 'github.com/o/app',
      file: 'src/lib.rs',
      touches: [
        {
          station_id: 'st-beta',
          session_id: 's2',
          session_title: 'Beta fix',
          path: 'lib.rs',
          touched_at: now - 60_000,
        },
        {
          station_id: 'st-alpha',
          session_id: 's1',
          session_title: 'Alpha fix',
          path: '/home/x/app/src/lib.rs',
          touched_at: now - 3 * 60_000,
        },
      ],
    },
  ];
  // A change of the fleet loads the radar again.
  deck.sessions = [
    session('st-alpha', 's1', 'Alpha fix', 1_760_000_000_500),
    session('st-beta', 's2', 'Beta fix', 1_760_000_000_000),
  ];
  deck.publish();
  const item = radar.getByRole('listitem', { name: 'Conflict src/lib.rs' });
  await expect(item).toContainText('CONFLICT');
  await expect(item).toContainText('src/lib.rs');
  await expect(item).toContainText('github.com/o/app');
  await expect(item).toContainText('Beta fix × Alpha fix');
  // the strip shows the age of the newest touch
  await expect(item).toContainText('1 min ago');
  expect(await fitsWidth(page)).toBe(true);

  await item.getByRole('button', { name: 'Beta fix' }).click();
  await expect(page.getByRole('form', { name: 'Composer' })).toBeVisible();
});

test('a radar that cannot load says so in one line', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  deck.sessions = [session('st-alpha', 's1', 'Alpha fix', 1_760_000_000_000)];
  deck.on('/api/conflicts', () => ({
    status: 500,
    body: { code: 'internal', message: 'The conflict index is down.' },
  }));
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  const radar = page.getByRole('region', { name: 'Conflict radar' });
  await expect(radar).toContainText('Conflict radar unavailable');
  await expect(radar.getByText('Conflict radar unavailable')).toHaveAttribute(
    'title',
    'The conflict index is down.',
  );
});
