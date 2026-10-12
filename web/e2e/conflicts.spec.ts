// M3 6-7 (DL-306): the conflict radar on the fleet screen at a phone
// viewport. A fleet change loads the radar again; a session of a conflict
// opens from the radar.
import { expect, test } from '@playwright/test';
import type { DeckSession, FileConflict } from '../src/gen';
import { summary } from '../src/testing/summary';
import { FakeDeck, fitsWidth, logIn, stationView } from './fake-deck';

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
  const radar = page.getByRole('region', { name: 'Conflict radar' });
  await expect(radar).toContainText('No file was changed by two sessions');

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
  await expect(item).toContainText('in github.com/o/app');
  await expect(item).toContainText('on Beta, 1 min ago');
  await expect(item).toContainText('on Alpha, 3 min ago');
  expect(await fitsWidth(page)).toBe(true);

  await item.getByRole('button', { name: 'Beta fix' }).click();
  await expect(page.getByRole('form', { name: 'Composer' })).toBeVisible();
});
