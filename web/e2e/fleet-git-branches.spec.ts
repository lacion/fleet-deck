// FG-10 (design §13.1, T4.5a): the group "Other stations" of the Branches view, at the phone
// viewport. The board talks to a scripted deck.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import type { OtherBranch } from '../src/integrations/actions';
import { FakeDeck, fitsWidth, logIn, openSession, stationView } from './fake-deck';
import {
  branchesAnswer,
  gitProject,
  gitSession,
  installFleetGit,
  installFleetGitBranches,
} from './fake-deck-git';

const ST = 'st-alpha';

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await page.routeWebSocket('**/shells/*/attach', () => {});
  deck.on('/api/artifacts', () => ({ status: 200, body: [] }));
  deck.on('/api/schedules', () => ({ status: 200, body: [] }));
}

const syncedProject = () =>
  gitProject({ git: { ...gitProject().git, sync: true, default_branch: 'main' } });

const landSession = (id: string, extra: Partial<DeckSession['summary']> = {}): DeckSession => ({
  ...gitSession(id, 'Fix the build', undefined, { worktree: '/work/atlas-wt', ...extra }),
  project_id: 'p1',
});

async function openBranches(page: Page) {
  const view = page.getByRole('region', { name: 'Session' });
  await view.getByRole('tab', { name: /^Changes/ }).click();
  await view.getByRole('button', { name: 'Branches', exact: true }).click();
  return view;
}

const otherRow = (extra: Partial<OtherBranch>): OtherBranch => {
  const row: OtherBranch = {
    name: 'fd/bravo/bea',
    slug: 'bravo',
    callsign: 'bea',
    head: 'b'.repeat(40),
    ahead: 3,
    behind: 1,
    at: Date.now() - 7_200_000,
    subject: 'Fix it',
    legacy: false,
    conflict: 'none',
    files: [],
    ...extra,
  };
  // A legacy branch has no slug.
  if (row.legacy) delete (row as { slug?: string }).slug;
  return row;
};

test('Other stations lists the branches of other stations, fetches and refreshes', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [landSession('g1')];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const git = installFleetGitBranches(deck, 'p1', {
    list: branchesAnswer(
      [
        otherRow({ name: 'fd/alpha/ada', callsign: 'ada', station_id: ST, station_name: 'Alpha' }),
        otherRow({
          station_id: 'st-bravo',
          station_name: 'Bravo',
          session: { station_id: 'st-bravo', session_id: 'b1', callsign: 'bea' },
          conflict: 'will_conflict',
          files: ['src/a.rs', 'src/b.rs'],
        }),
        otherRow({ name: 'fd/cai', callsign: 'cai', legacy: true, ahead: 2 }),
      ],
      { fetched_at: Date.now() - 5 * 60_000 },
    ),
  });
  await logIn(page);
  await openSession(page, 'Fix the build');
  const view = await openBranches(page);
  const group = view.getByRole('region', { name: 'Other stations' });
  const rows = group.getByRole('listitem');
  await expect(rows).toHaveCount(2);
  await expect(group).not.toContainText('fd/alpha/ada');
  await expect(rows.first()).toContainText('fd/bravo/bea');
  await expect(rows.first()).toContainText('Bravo');
  await expect(rows.first()).toContainText('↑3 ↓1');
  await expect(rows.first()).toContainText('Will conflict (2 files)');
  await expect(rows.first().getByRole('link', { name: 'bea' })).toHaveAttribute(
    'href',
    '#/s/st-bravo/b1',
  );
  await expect(rows.nth(1)).toContainText('legacy branch');
  await expect(rows.nth(1)).toContainText('no session');
  await expect(group).toContainText('Fetched 5m ago');
  expect(await fitsWidth(page)).toBe(true);

  git.fetch = { status: 200, body: { fetched_at: Date.now() } };
  git.list = branchesAnswer([], { fetched_at: Date.now() });
  await group.getByRole('button', { name: 'Fetch', exact: true }).click();
  await expect(group).toContainText('Fetched just now');
  await expect(group).toContainText(
    'No branches of other stations. Push is off, or no other station pushed yet.',
  );
  expect(git.calls.at(-2)).toBe('POST /api/projects/p1/fetch');
  expect(git.calls.at(-1)).toBe('GET /api/projects/p1/branches');
});

test('Other stations shows a failed fetch and a refusal of the deck', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [landSession('g1')];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const git = installFleetGitBranches(deck, 'p1', {
    list: branchesAnswer([], { fetched_at: Date.now() }),
    fetch: { status: 200, body: { code: 'fetch_failed', text: 'No route to host.' } },
  });
  await logIn(page);
  await openSession(page, 'Fix the build');
  const view = await openBranches(page);
  const group = view.getByRole('region', { name: 'Other stations' });
  await group.getByRole('button', { name: 'Fetch', exact: true }).click();
  await expect(group.getByRole('alert')).toHaveText('No route to host.');

  git.fetch = { status: 200, body: { fetched_at: Date.now() } };
  git.list = {
    status: 409,
    body: {
      error: 'station_unreachable',
      message: 'station `Alpha` has no live link to this deck',
    },
  };
  await group.getByRole('button', { name: 'Fetch', exact: true }).click();
  await expect(group.getByRole('alert')).toHaveText(
    'station `Alpha` has no live link to this deck',
  );
  expect(await fitsWidth(page)).toBe(true);
});

test('Other stations is hidden for an older deck', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [landSession('g1')];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const git = installFleetGitBranches(deck, 'p1', {
    list: { status: 404, body: { error: 'not_found', message: 'No such route.' } },
  });
  await logIn(page);
  await openSession(page, 'Fix the build');
  const view = await openBranches(page);
  await expect(view.locator('[data-view="branches"]')).toBeVisible();
  await expect.poll(() => git.calls.length).toBe(1);
  await expect(view.getByRole('region', { name: 'Other stations' })).toHaveCount(0);
});

test('Other stations is hidden for a project with sync off', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [landSession('g1')];
  await deck.install(page);
  await installFleetGit(page, deck, [gitProject()]);
  const git = installFleetGitBranches(deck, 'p1');
  await logIn(page);
  await openSession(page, 'Fix the build');
  const view = await openBranches(page);
  await expect(view.locator('[data-view="branches"]')).toBeVisible();
  await expect(view.getByRole('region', { name: 'Other stations' })).toHaveCount(0);
  expect(git.calls).toEqual([]);
});
