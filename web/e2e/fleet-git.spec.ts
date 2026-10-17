// FG-1..FG-4 (design §16.2, phase 1): the project Git settings form, the "Not pushed" badge with
// "Push now", the "Stale base" chip and the timeline lines of the Fleet Git events, at the phone
// viewport. The board talks to a scripted deck.
import { expect, type Page, test } from '@playwright/test';
import { FakeDeck, fitsWidth, logIn, openFleet, openSession, stationView } from './fake-deck';
import { gitEvents, gitProject, gitSession, installFleetGit } from './fake-deck-git';

const ST = 'st-alpha';

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await page.routeWebSocket('**/shells/*/attach', () => {});
  deck.on('/api/artifacts', () => ({ status: 200, body: [] }));
  deck.on('/api/schedules', () => ({ status: 200, body: [] }));
}

test('the project Git settings save with their revision and show the deck refusal', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  const projects = [gitProject({ git_revision: 3 })];
  await deck.install(page);
  const git = await installFleetGit(page, deck, projects);
  await logIn(page);
  await page.evaluate(() => {
    window.location.hash = '#/projects';
  });
  await page
    .getByRole('row', { name: 'Project Atlas' })
    .getByRole('button', { name: 'Edit' })
    .click();
  const form = page.getByRole('form', { name: 'Git sync' });
  await expect(form).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);

  // The board refuses "push without sync" before it calls the deck.
  await form.getByLabel('Push branches').selectOption('on_turn');
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  await expect(form.getByText('Turn on sync first.')).toBeVisible();
  expect(git.puts).toHaveLength(0);

  await form.getByLabel('Sync this project across stations').check();
  await form.getByLabel('Remote', { exact: true }).fill('origin');
  await form.getByLabel('WIP remote URL (optional)').fill('ssh://bare.test/wip.git');
  await form.getByLabel('Copy ignored files into new worktrees').fill('.env\nconfig/*.local');
  await form.getByLabel('Environment for agents and setup').fill('RUSTC_WRAPPER=sccache');
  // The deck refuses one value: its message shows under its field.
  git.refuse = {
    status: 400,
    body: {
      error: 'invalid_git_settings',
      field: 'wip_remote',
      message: 'The WIP remote must be an https, ssh or user@host:path URL without a password.',
    },
  };
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  await expect(
    form.getByText(
      'The WIP remote must be an https, ssh or user@host:path URL without a password.',
    ),
  ).toBeVisible();

  await form.getByLabel('WIP remote URL (optional)').fill('ssh://bare.test/wip2.git');
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  await expect(form.getByText('Saved.')).toBeVisible();
  expect(git.puts).toHaveLength(2);
  expect(git.puts[1]).toMatchObject({
    revision: 3,
    settings: {
      sync: true,
      push: 'on_turn',
      push_remote: 'origin',
      wip_remote: 'ssh://bare.test/wip2.git',
      worktree_include: ['.env', 'config/*.local'],
      worktree_env: { RUSTC_WRAPPER: 'sccache' },
    },
  });
  expect(git.puts[1]).not.toHaveProperty('remote_url');
});

test('a stale revision shows its message and a Reload button', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  await deck.install(page);
  const git = await installFleetGit(page, deck, [gitProject({ git_revision: 1 })]);
  await logIn(page);
  await page.evaluate(() => {
    window.location.hash = '#/projects';
  });
  await page
    .getByRole('row', { name: 'Project Atlas' })
    .getByRole('button', { name: 'Edit' })
    .click();
  const form = page.getByRole('form', { name: 'Git sync' });
  git.refuse = {
    status: 409,
    body: {
      error: 'stale_revision',
      message: 'The Git settings changed. Reload and try again.',
    },
  };
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  await expect(form.getByText('The Git settings changed. Reload and try again.')).toBeVisible();
  await expect(form.getByRole('button', { name: 'Reload' })).toBeVisible();
});

test('a failing push shows "Not pushed" with its title; Push now posts and keeps the board', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [
    gitSession(
      'g1',
      'Fix the build',
      {
        state: 'failing',
        ref_name: 'fd/alpha/ada',
        attempts: 2,
        text: 'Push was rejected: stale info',
        next_attempt_at: Date.now() + 5 * 60_000,
      },
      { base_stale: true },
    ),
  ];
  await deck.install(page);
  const git = await installFleetGit(page, deck, []);
  await logIn(page);
  await openFleet(page);
  const card = page.locator('[data-session="g1"]');
  const badge = card.getByText('Not pushed');
  await expect(badge).toBeVisible();
  await expect(badge).toHaveAttribute(
    'title',
    /^Push failed 2 times: Push was rejected: stale info\. Next try in \d+ min\.$/,
  );
  await expect(card.getByText('Stale base')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);

  await card.getByRole('button', { name: 'Push now' }).click();
  await expect.poll(() => git.pushes).toEqual(['/api/sessions/st-alpha/g1/push']);
  // The button did not open the session.
  await expect(page).toHaveURL(/#\/fleet$/);
});

test('the chat shows the notices of the new events', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [gitSession('g2', 'Write the docs', undefined)];
  deck.emit(
    ST,
    'g2',
    gitEvents('g2', [
      {
        kind: 'workspace.base_stale',
        code: 'fetch_failed',
        text: 'Fetch failed; this session started from the local HEAD: no route to host',
      },
      {
        kind: 'workspace.include_skipped',
        files: [{ path: '.env.local', reason: 'too_large' }],
      },
      {
        kind: 'session.push',
        state: 'failing',
        ref_name: 'fd/alpha/ada',
        attempts: 1,
        code: 'push_timeout',
        text: 'Push took more than 60 s.',
      },
      {
        kind: 'session.push',
        state: 'pushed',
        ref_name: 'fd/alpha/ada',
        attempts: 0,
        oid: 'abcdef1234567',
      },
      { kind: 'session.cleanup', state: 'kept', reason: 'not_pushed' },
    ]),
  );
  await deck.install(page);
  await installFleetGit(page, deck, []);
  await logIn(page);
  await openSession(page, 'Write the docs');
  const view = page.getByRole('region', { name: 'Session' });
  await expect(
    view.getByText('Fetch failed; this session started from the local HEAD: no route to host'),
  ).toBeVisible();
  await expect(view.getByText('Not copied: .env.local (larger than 1 MiB)')).toBeVisible();
  await expect(view.getByText('Push took more than 60 s.')).toBeVisible();
  await expect(view.getByText('Pushed abcdef1')).toBeVisible();
  await expect(view.getByText('Kept: the last commit is not pushed.')).toBeVisible();
});
