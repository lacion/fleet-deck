// FG-1..FG-4 (design §16.2, phase 1): the project Git settings form, the "Not pushed" badge with
// "Push now", the "Stale base" chip and the timeline lines of the Fleet Git events, at the phone
// viewport. The board talks to a scripted deck.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import {
  FakeDeck,
  fitsWidth,
  logIn,
  openFleet,
  openMore,
  openSession,
  stationView,
} from './fake-deck';
import {
  gitEvents,
  gitHome,
  gitProject,
  gitSession,
  gitStation,
  installFleetGit,
  installFleetGitHomes,
  installFleetGitLands,
  installFleetGitMoves,
  installFleetGitPlacement,
} from './fake-deck-git';

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

// Phase 2 (T2.4): homes, placement, station traits, config drift, Taildrive and artifacts.

const goto = (page: Page, hash: string) =>
  page.evaluate((h) => {
    window.location.hash = h;
  }, hash);

async function twoStations(page: Page, deck: FakeDeck) {
  deck.stations = [gitStation('st-alpha', 'Alpha'), gitStation('st-bravo', 'Bravo')];
  await page.routeWebSocket('**/shells/*/attach', () => {});
  deck.on('/api/artifacts', () => ({ status: 200, body: [] }));
  deck.on('/api/schedules', () => ({ status: 200, body: [] }));
}

test('the Homes section adds a home that clones then is ready, retries a failed one and removes one', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await twoStations(page, deck);
  const atlas = gitProject({
    remote_url: 'https://git.test/acme/atlas.git',
    repo: 'git.test/acme/atlas',
    homes: [gitHome('st-alpha', { path: '/work/atlas' })],
  });
  await deck.install(page);
  await installFleetGit(page, deck, [atlas]);
  const homes = await installFleetGitHomes(page, deck, [atlas]);
  await logIn(page);
  await goto(page, '#/project/p1');
  const section = page.getByRole('region', { name: 'Homes' });
  await expect(section.getByText('Alpha · /work/atlas · Ready')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
  // The last home cannot be removed.
  await expect(section.getByRole('button', { name: /^Remove/ })).toHaveCount(0);

  await section.getByRole('button', { name: 'Add home' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add a home on another station' });
  await expect(
    dialog.getByText('The station clones https://git.test/acme/atlas.git into'),
  ).toBeVisible();
  await expect(dialog).toContainText('/srv/st-bravo/projects/Atlas');
  await expect(dialog).toContainText('uses it.');
  await dialog.getByRole('button', { name: 'Add home' }).click();
  expect(homes.calls[0]?.call).toBe('POST /api/projects/p1/homes');
  expect(homes.calls[0]?.body).toMatchObject({ station: 'st-bravo' });
  await expect(
    section.getByText('Bravo · Cloning into /srv/st-bravo/projects/Atlas…'),
  ).toBeVisible();
  // The board reads the list again while a home clones.
  homes.settle('p1', 'st-bravo');
  await expect(section.getByText('Bravo · /srv/st-bravo/projects/Atlas · Ready')).toBeVisible({
    timeout: 12_000,
  });

  // A failed home shows its text and Retry.
  const bravo = atlas.homes.find((h) => h.station_id === 'st-bravo');
  if (bravo === undefined) throw new Error('no home');
  bravo.state = 'failed';
  bravo.message = 'The clone failed: no space left.';
  await page.reload();
  await expect(section.getByText('Bravo · Failed: The clone failed: no space left.')).toBeVisible();
  await section.getByRole('button', { name: 'Retry the home on Bravo' }).click();
  await expect(section.getByText(/^Bravo · Cloning into/)).toBeVisible();
  expect(homes.calls.at(-1)?.call).toBe('POST /api/projects/p1/homes/st-bravo/retry');

  // A refusal of the deck shows under the list.
  homes.refuse = {
    status: 409,
    body: { error: 'home_busy', message: 'A clone runs for this home.' },
  };
  await section.getByRole('button', { name: 'Remove the home on Bravo' }).click();
  await expect(section.getByRole('alert')).toHaveText('A clone runs for this home.');
  await section.getByRole('button', { name: 'Remove the home on Bravo' }).click();
  await expect(section.getByText(/^Bravo/)).toHaveCount(0);
});

test('a changed remote URL asks for the rebind before it saves', async ({ page }) => {
  const deck = new FakeDeck();
  await twoStations(page, deck);
  const atlas = gitProject({
    remote_url: 'https://git.test/acme/atlas.git',
    remote_ssh_user: 'git',
    repo: 'git.test/acme/atlas',
    git_revision: 3,
  });
  await deck.install(page);
  const git = await installFleetGit(page, deck, [atlas]);
  const homes = await installFleetGitHomes(page, deck, [atlas], {
    rebindFrom: 'git.test/acme/atlas',
  });
  await logIn(page);
  await goto(page, '#/projects');
  await page
    .getByRole('row', { name: 'Project Atlas' })
    .getByRole('button', { name: 'Edit' })
    .click();
  const form = page.getByRole('form', { name: 'Git sync' });
  // Fleet Sync: the remote URL is in the closed "Advanced" part.
  await form.getByText('Advanced', { exact: true }).click();
  await expect(form.getByText('SSH login: git')).toBeVisible();
  await expect(form.getByLabel('Remote URL', { exact: true })).toHaveValue(
    'https://git.test/acme/atlas.git',
  );
  await form.getByLabel('Remote URL', { exact: true }).fill('https://git.test/acme/other.git');
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  const dialog = page.getByRole('dialog', {
    name: 'Rebind the project to https://git.test/acme/other.git?',
  });
  await expect(dialog).toContainText('This URL names another repository than git.test/acme/atlas.');
  // Cancel keeps everything as it was.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  expect(git.puts).toHaveLength(0);
  await form.getByRole('button', { name: 'Save Git settings' }).click();
  await page.getByRole('button', { name: 'Rebind' }).click();
  await expect(form.getByText('Saved.')).toBeVisible();
  expect(homes.calls.map((c) => c.body)).toMatchObject([
    { remote_url: 'https://git.test/acme/other.git', rebind: false },
    { remote_url: 'https://git.test/acme/other.git', rebind: false },
    { remote_url: 'https://git.test/acme/other.git', rebind: true },
  ]);
  // The settings go after the remote, with the revision that the remote route made.
  expect(git.puts).toHaveLength(1);
  expect(git.puts[0]).toMatchObject({ revision: 4 });
  expect(git.puts[0]).not.toHaveProperty('remote_url');
});

test('the Stations page shows traits, the config drift and the Taildrive shares', async ({
  page,
}) => {
  const deck = new FakeDeck();
  const digest = (skills: string, agents = 'a'.repeat(64)) => ({
    claude_version: '2.1.5',
    codex_version: '0.9.0',
    computed_at: 1,
    items: [
      { name: 'claude_md' as const, hash: 'a'.repeat(64) },
      { name: 'claude_agents' as const, hash: agents },
      { name: 'claude_skills' as const, hash: skills },
      { name: 'codex_agents_md' as const, hash: 'a'.repeat(64) },
    ],
  });
  deck.stations = [
    gitStation('st-alpha', 'Alpha', {
      traits: { os: 'linux', arch: 'x86_64', labels: ['gpu', 'always_on'] },
      config: digest('b'.repeat(64)),
      taildrive: {
        state: 'ok',
        machine: 'alpha',
        checked_at: 1,
        inventory_revision: 1,
        shares: [
          {
            name: 'out',
            path: '/srv/out',
            candidates: ['/srv/out'],
            as_user: 'ada',
            exposed: ['ssh', 'project_home'],
            exposure_state: 'exposed',
            path_state: 'ok',
          },
          {
            name: 'gone',
            path: '/srv/gone',
            candidates: ['/srv/gone'],
            exposed: [],
            exposure_state: 'clear',
            path_state: 'missing',
          },
        ],
      },
    }),
    gitStation('st-bravo', 'Bravo', {
      traits: { os: 'macos', arch: 'aarch64', on_battery: true, labels: [] },
      config: digest('c'.repeat(64), 'd'.repeat(64)),
    }),
    gitStation('st-old', 'Old'),
  ];
  await deck.install(page);
  const prefs = await installFleetGitPlacement(page, deck, { reference: 'st-alpha' });
  await logIn(page);
  await openMore(page, 'Stations');
  const alpha = page.locator('[data-station="st-alpha"]');
  const bravo = page.locator('[data-station="st-bravo"]');
  const old = page.locator('[data-station="st-old"]');
  await expect(page.getByText('Config reference: Alpha')).toBeVisible();
  await expect(alpha.getByText('Linux · x86_64 · gpu, always_on')).toBeVisible();
  await expect(bravo.getByText('macOS · aarch64 · on battery')).toBeVisible();
  await expect(alpha.getByText('Config: Reference')).toBeVisible();
  await expect(bravo.getByText('Config: Differs: agents, skills')).toBeVisible();
  await expect(old.getByText(/^Config:/)).toHaveCount(0);
  await expect(old.getByText('linux')).toHaveCount(0);

  // The Taildrive shares and the warning of O37.
  await expect(alpha.getByText('out · /srv/out · as ada')).toBeVisible();
  await expect(
    alpha.getByText(
      "This Taildrive share exposes ~/.ssh, a project home. Devices in your tailnet policy can read it, outside Fleet Deck's file rules.",
    ),
  ).toBeVisible();
  await expect(alpha.getByText('gone: the path does not exist on this station.')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);

  // "Use as config reference" in the menu of another station.
  await bravo.getByLabel('Station actions').click();
  await bravo.getByRole('button', { name: 'Use as config reference' }).click();
  await expect(page.getByText('Config reference: Bravo')).toBeVisible();
  expect(prefs.references).toEqual([{ station: 'st-bravo' }]);
  await expect(bravo.getByText('Config: Reference')).toBeVisible();
  await expect(alpha.getByText('Config: Differs: agents, skills')).toBeVisible();
});

test('the Stations page asks for a config reference when the deck has none', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [gitStation('st-alpha', 'Alpha')];
  await deck.install(page);
  await installFleetGitPlacement(page, deck);
  await logIn(page);
  await openMore(page, 'Stations');
  await expect(page.getByText('Config reference…')).toBeVisible();
});

test('the session Artifacts link opens the folder, or shows the address', async ({ page }) => {
  const deck = new FakeDeck();
  await twoStations(page, deck);
  deck.sessions = [
    gitSession('a1', 'Local artifacts', undefined, {
      artifacts_dir: { path: '/mnt/out/Atlas/ada/' },
    }),
    gitSession('a2', 'Remote artifacts', undefined, {
      artifacts_dir: { url: 'http://100.100.100.100:8080/t.ts.net/nas/out/Atlas/ada/' },
    }),
    gitSession('a3', 'Revoked artifacts', undefined),
  ];
  deck.emit(
    ST,
    'a3',
    gitEvents('a3', [
      { kind: 'artifacts.ready', path: '/mnt/out/Atlas/ada/' },
      {
        kind: 'artifacts.revoked',
        code: 'share_unverified',
        text: 'The artifact share is not verified now. Fleet Deck stops showing the artifact folder.',
      },
    ]),
  );
  const listed: string[] = [];
  deck.on(`/api/stations/${ST}/files/list`, (_body, query) => {
    listed.push(query.toString());
    return {
      status: 200,
      body: {
        root: 'artifacts',
        path: query.get('path') ?? '',
        entries: [{ name: 'report.md', kind: 'file', size: 12 }],
        truncated: false,
      },
    };
  });
  await deck.install(page);
  await installFleetGit(page, deck, []);
  await logIn(page);

  await openSession(page, 'Local artifacts');
  const view = page.getByRole('region', { name: 'Session' });
  await view.getByRole('button', { name: 'Artifacts', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Artifacts' });
  await expect(dialog.getByText('report.md')).toBeVisible();
  expect(listed.some((q) => q.includes('artifacts=a1'))).toBe(true);
  expect(await fitsWidth(page)).toBe(true);
  await dialog.getByRole('button', { name: 'Close' }).click();

  await goto(page, '#/');
  await openSession(page, 'Remote artifacts');
  await expect(
    page.getByText('The artifacts are on nas. Open this address on a tailnet device:'),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Copy address' })).toBeVisible();

  await goto(page, '#/');
  await openSession(page, 'Revoked artifacts');
  const revoked = page.getByRole('region', { name: 'Session' });
  await expect(
    revoked.getByRole('status').filter({ hasText: 'The artifact share is not verified now.' }),
  ).toBeVisible();
  await expect(revoked.getByRole('button', { name: 'Artifacts', exact: true })).toHaveCount(0);
});

test('a project start picks a home or Auto, and the placement decides the station', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await twoStations(page, deck);
  const traits = { os: 'linux', arch: 'x86_64', labels: ['gpu'] };
  deck.stations = [
    gitStation('st-alpha', 'Alpha', { traits }),
    gitStation('st-bravo', 'Bravo', { traits }),
  ];
  const atlas = gitProject({
    homes: [gitHome('st-alpha'), gitHome('st-bravo')],
    path: '/srv/st-alpha/atlas',
  });
  deck.accept('session.start', 'st-bravo');
  await deck.install(page);
  await installFleetGit(page, deck, [atlas]);
  const placement = await installFleetGitPlacement(page, deck, { answer: ['st-bravo'] });
  await logIn(page);
  await goto(page, '#/project/p1');
  await page.getByRole('button', { name: 'New session in this project' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await expect(form.getByLabel('Home')).toHaveValue('st-alpha');
  await expect(form.getByLabel('Home')).toContainText('Bravo · /srv/st-bravo/atlas');
  await form.getByLabel('Home').selectOption('auto');
  await form.getByLabel('Needs').fill('gpu, os:linux');
  await form.getByLabel('Idle station').check();
  await form.getByLabel('What should the agent do? (optional)').fill('Fix it');
  expect(await fitsWidth(page)).toBe(true);
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect.poll(() => placement.placements.length).toBe(1);
  expect(placement.placements[0]).toEqual({
    count: 1,
    requires: ['gpu', 'os:linux'],
    prefer: ['idle'],
    spread: false,
    provider: 'claude',
    project_id: 'p1',
  });
  await expect
    .poll(() => deck.calls.some((c) => c.path === '/api/commands/session.start'))
    .toBe(true);
  const start = deck.calls.find((c) => c.path === '/api/commands/session.start');
  expect(start?.body).toMatchObject({
    station: 'st-bravo',
    command: { op: 'start', spec: { cwd: '/srv/st-bravo/atlas', provider_instance: 'personal' } },
  });
});

test('a refusal of the placement shows in the form and starts nothing', async ({ page }) => {
  const deck = new FakeDeck();
  const traits = { os: 'linux', arch: 'x86_64', labels: [] };
  deck.stations = [
    gitStation('st-alpha', 'Alpha', { traits }),
    gitStation('st-bravo', 'Bravo', { traits }),
  ];
  await page.routeWebSocket('**/shells/*/attach', () => {});
  deck.on('/api/schedules', () => ({ status: 200, body: [] }));
  deck.on('/api/artifacts', () => ({ status: 200, body: [] }));
  await deck.install(page);
  await installFleetGit(page, deck, []);
  await installFleetGitPlacement(page, deck, {
    answer: { status: 409, body: { error: 'no_station', message: 'No station matches: gpu.' } },
  });
  await logIn(page);
  await page.getByRole('link', { name: 'New', exact: true }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('radiogroup', { name: 'Station' }).getByText('Auto').click();
  await form.getByLabel('Directory path').fill('/work/repo');
  await form.getByLabel('Needs').fill('gpu');
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect(form.getByText('No station matches: gpu.')).toBeVisible();
  expect(deck.calls.some((c) => c.path === '/api/commands/session.start')).toBe(false);
});

// Phase 3 (T3.4): the radar groups, the land panel and the land queue.

const syncedProject = () =>
  gitProject({ git: { ...gitProject().git, sync: true, default_branch: 'main' } });

const landSession = (id: string, extra: Partial<DeckSession['summary']> = {}): DeckSession => ({
  ...gitSession(id, 'Fix the build', undefined, { worktree: '/work/atlas-wt', ...extra }),
  project_id: 'p1',
});

test('the radar groups the rows by project with a Will conflict row and a footer notice', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.on('/api/conflicts', () => ({ status: 200, body: [] }));
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  installFleetGitLands(deck, 'p1', {
    radar: {
      groups: [
        {
          project_id: 'p1',
          group_key: 'origin:github.com/o/atlas',
          will_conflict: [
            {
              a: { station_id: ST, session_id: 'g1', callsign: 'ada' },
              b: 'default',
              files: ['src/a.rs', 'src/b.rs', 'src/c.rs', 'src/d.rs'],
              checked_at: Date.now() - 4 * 60_000,
            },
          ],
          same_files: [],
        },
      ],
      notices: [{ project_id: 'p1', code: 'station_offline', text: 'Radar: Alpha is offline.' }],
    },
  });
  await logIn(page);
  await openFleet(page);
  const radar = page.getByRole('region', { name: 'Conflict radar' });
  await expect(radar.getByText('Atlas', { exact: true })).toBeVisible();
  await expect(radar.getByText('Will conflict')).toBeVisible();
  await expect(radar.getByText('src/a.rs, src/b.rs, src/c.rs')).toBeVisible();
  await expect(radar.getByText('+1', { exact: true })).toBeVisible();
  await expect(radar.getByText('4 min ago')).toBeVisible();
  await expect(radar.getByText('Radar: Alpha is offline.')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
});

test('the radar of an older deck leaves the panel as it was', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.on('/api/conflicts', () => ({ status: 200, body: [] }));
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  installFleetGitLands(deck, 'p1');
  await logIn(page);
  await openFleet(page);
  await expect(page.getByRole('region', { name: 'Fleet panel' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Conflict radar' })).toHaveCount(0);
});

test('Land asks, queues the land, shows its position, and cancels it', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [landSession('g1')];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const lands = installFleetGitLands(deck, 'p1');
  await logIn(page);
  await openSession(page, 'Fix the build');
  const panel = page.getByRole('region', { name: 'Land', exact: true });
  await panel.getByRole('button', { name: 'Land', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Land fd/alpha/ada?' });
  await expect(dialog).toContainText(
    'Fleet Deck rebases the branch onto origin/main in a separate folder, runs the check command, pushes the branch, then pushes the result to origin/main.',
  );
  expect(await fitsWidth(page)).toBe(true);
  lands.refuse = {
    status: 409,
    body: { error: 'land_not_ready', message: 'This session has no Fleet Git branch.' },
  };
  await dialog.getByRole('button', { name: 'Land', exact: true }).click();
  await expect(dialog.getByRole('alert')).toHaveText('This session has no Fleet Git branch.');
  await dialog.getByRole('button', { name: 'Land', exact: true }).click();
  await expect(panel.getByText('Queued (0 ahead)')).toBeVisible();
  expect(lands.calls.map((c) => c.call)).toEqual([
    'POST /api/sessions/st-alpha/g1/land',
    'POST /api/sessions/st-alpha/g1/land',
  ]);
  await panel.getByRole('button', { name: 'Cancel land' }).click();
  await expect(panel.getByText('Cancelled.')).toBeVisible();
  expect(lands.calls.at(-1)?.call).toBe('POST /api/lands/land-1/cancel');
});

test('an uncertain land has Stop checking; a conflict has Send to session; the project lists the lands', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.sessions = [landSession('g1')];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const lands = installFleetGitLands(deck, 'p1', {
    lands: {
      p1: {
        queue: [
          {
            land_id: 'land-7',
            session_id: 'g1',
            station_id: ST,
            branch: 'fd/alpha/ada',
            requested_by: 'owner',
            state: 'running',
            phase: 'lost',
            reconciling: false,
            attempt: 1,
            files: [],
            created_at: 20,
          },
        ],
        recent: [
          {
            land_id: 'land-3',
            session_id: 'g1',
            station_id: ST,
            branch: 'fd/alpha/ada',
            requested_by: 'owner',
            state: 'conflict',
            reconciling: false,
            attempt: 1,
            files: ['src/a.rs', 'src/b.rs'],
            base_oid: 'abc1234def',
            message: 'Conflict in 2 files with origin/main (abc1234).',
            created_at: 10,
            ended_at: 11,
          },
        ],
      },
    },
  });
  await logIn(page);
  await openSession(page, 'Fix the build');
  const panel = page.getByRole('region', { name: 'Land', exact: true });
  await expect(
    panel.getByText(
      'The station has no record of this land. Check the remote, then press Stop checking.',
    ),
  ).toBeVisible();
  await panel.getByRole('button', { name: 'Stop checking' }).click();
  const dialog = page.getByRole('dialog', { name: 'Stop checking this land?' });
  await expect(dialog).toContainText(
    'Fleet Deck frees the queue after Alpha confirms that it stopped. Fleet Deck does not know if the land changed the remote.',
  );
  await dialog.getByRole('button', { name: 'Stop checking', exact: true }).click();
  await expect(panel.getByText('Stopping… waiting for Alpha to confirm.')).toBeVisible();
  expect(lands.calls.at(-1)?.call).toBe('POST /api/lands/land-7/abandon');

  // The project page lists the queue and the recent ends; the conflict has Send to session there.
  await goto(page, '#/project/p1');
  const list = page.getByRole('region', { name: 'Lands' });
  await expect(list.getByText('Stopping… waiting for Alpha to confirm.')).toBeVisible();
  await expect(list.getByText('Conflict in 2 files with origin/main (abc1234).')).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
  await list.getByRole('button', { name: 'Send to session' }).click();
  await expect.poll(() => lands.calls.at(-1)?.call).toBe('POST /api/lands/land-3/send');
});

// Phase 4 (T4.5b): the move of a session to another station (FG-9).

const MOVE = { station: ST, session: 'g1' };
const moveTargets = [
  { station_id: 'st-beta', name: 'Beta', kind: 'native', ready: true },
  {
    station_id: 'st-gamma',
    name: 'Gamma',
    kind: 'branch_only',
    ready: false,
    failed: { code: 'move_not_ready', text: 'The branch is not pushed.' },
  },
];
const moveStations = () => [
  stationView(ST, 'Alpha'),
  stationView('st-beta', 'Beta'),
  stationView('st-gamma', 'Gamma'),
];

test('Move to station checks the conditions, starts the move and shows its progress', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.stations = moveStations();
  deck.sessions = [landSession('g1', { callsign: 'ada' })];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const moves = installFleetGitMoves(deck, MOVE, { read: { move: null, targets: moveTargets } });
  await logIn(page);
  await openSession(page, 'Fix the build');
  await page.locator('summary[aria-label="Session menu"]').click();
  await page.getByRole('button', { name: 'Move to station…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Move ada to another station' });
  await expect(dialog.getByText('The conversation moves with its history.')).toBeVisible();
  await expect(dialog.getByRole('img', { name: 'Passed' })).toHaveCount(6);
  expect(await fitsWidth(page)).toBe(true);

  // A station that is not ready: one failed item with the text of the deck, the rest not checked.
  await dialog.getByLabel('Station').selectOption('st-gamma');
  await expect(dialog.getByRole('img', { name: 'Failed' })).toHaveCount(1);
  await expect(dialog.getByRole('img', { name: 'Not checked' })).toHaveCount(3);
  await expect(dialog.getByText('The branch is pushed: The branch is not pushed.')).toBeVisible();
  await expect(
    dialog.getByText('Only the branch moves; a new session continues from the last summary.'),
  ).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Move', exact: true })).toBeDisabled();

  // The deck refuses the first try; the second starts the move and the dialog closes.
  await dialog.getByLabel('Station').selectOption('st-beta');
  moves.refuse = {
    status: 409,
    body: { error: 'move_not_ready', message: 'A turn is running.' },
  };
  await dialog.getByRole('button', { name: 'Move', exact: true }).click();
  await expect(dialog.getByRole('alert')).toHaveText('A turn is running.');
  await dialog.getByRole('button', { name: 'Move', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(moves.calls.map((c) => c.call)).toEqual([
    'POST /api/sessions/st-alpha/g1/move',
    'POST /api/sessions/st-alpha/g1/move',
  ]);
  expect(moves.calls[1]?.body).toMatchObject({ to_station: 'st-beta' });
  await expect(page.getByText('Moving: holding')).toBeVisible();

  // The progress follows the move: the board reads it every two seconds while it is open.
  const open = moves.read.move;
  if (open === null) throw new Error('the scripted deck has no move');
  moves.read.move = { ...open, state: 'exporting', bytes: 2_097_152 };
  await expect(page.getByText('Moving: copying the conversation (2.0 MiB)')).toBeVisible();
  moves.read.move = { ...open, state: 'starting' };
  await expect(page.getByText('Moving: starting on Beta')).toBeVisible();
  moves.read.move = { ...open, state: 'finishing', code: 'source_running' };
  await expect(
    page.getByText('The source session runs again. Stop it, then Fleet Deck finishes the move.'),
  ).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
});

test('a source that moved shows Moved to with Open; the new session arrives without a composer', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.stations = moveStations();
  deck.sessions = [
    landSession('g1', {
      moved_to: { move_id: 'mv-1', station_id: 'st-beta', session_id: 'g2' },
    }),
    {
      ...landSession('g2', { move_in: { move_id: 'mv-1', state: 'resumed' } }),
      station_id: 'st-beta',
      summary: {
        ...landSession('g2').summary,
        title: 'Arriving work',
        move_in: { move_id: 'mv-1', state: 'resumed' },
      },
    },
  ];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const done = {
    move_id: 'mv-1',
    state: 'done' as const,
    kind: 'native',
    src_station: ST,
    src_session: 'g1',
    dst_station: 'st-beta',
    dst_session: 'g2',
    created_at: 1,
  };
  installFleetGitMoves(deck, MOVE, { read: { move: done, targets: [] } });
  installFleetGitMoves(
    deck,
    { station: 'st-beta', session: 'g2' },
    {
      read: { move: { ...done, state: 'releasing' }, targets: [] },
    },
  );
  await logIn(page);
  await openSession(page, 'Fix the build');
  await expect(page.getByText('Moved to Beta')).toBeVisible();
  await expect(page.getByRole('link', { name: 'Open', exact: true })).toHaveAttribute(
    'href',
    '#/s/st-beta/g2',
  );
  await expect(page.getByRole('form', { name: 'Composer' })).toHaveCount(0);
  expect(await fitsWidth(page)).toBe(true);
  await page.getByRole('link', { name: 'Open', exact: true }).click();
  await expect(page.getByText('Arriving from Alpha…')).toBeVisible();
  await expect(page.getByRole('form', { name: 'Composer' })).toHaveCount(0);
});

test('a released Claude session asks for one prompt to confirm the moved conversation', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.stations = moveStations();
  deck.sessions = [landSession('g1', { move_in: { move_id: 'mv-1', state: 'released' } })];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  installFleetGitMoves(deck, MOVE);
  await logIn(page);
  await openSession(page, 'Fix the build');
  await expect(page.getByText('Send one prompt to confirm the moved conversation.')).toBeVisible();
  await expect(page.getByRole('form', { name: 'Composer' })).toBeVisible();
});

test('Undo move asks, posts the undo and shows Undoing the move', async ({ page }) => {
  const deck = new FakeDeck();
  await setUp(page, deck);
  deck.stations = moveStations();
  deck.sessions = [
    landSession('g1', {
      moved_to: { move_id: 'mv-1', station_id: 'st-beta', session_id: 'g2' },
    }),
  ];
  await deck.install(page);
  await installFleetGit(page, deck, [syncedProject()]);
  const moves = installFleetGitMoves(deck, MOVE, {
    read: {
      move: {
        move_id: 'mv-1',
        state: 'resume_failed',
        kind: 'native',
        src_station: ST,
        src_session: 'g1',
        dst_station: 'st-beta',
        dst_session: 'g2',
        created_at: 1,
      },
      targets: [],
    },
  });
  await logIn(page);
  await openSession(page, 'Fix the build');
  await expect(page.getByText('Moved to Beta. The new session could not resume.')).toBeVisible();
  await page.getByRole('button', { name: 'Undo move' }).click();
  const dialog = page.getByRole('dialog', { name: 'Undo the move?' });
  await expect(dialog).toContainText(
    'The session on Alpha becomes resumable again. The new session on Beta stays stopped and read only.',
  );
  expect(await fitsWidth(page)).toBe(true);
  moves.refuse = {
    status: 409,
    body: {
      error: 'move_not_undoable',
      message: 'Only a move whose new session could not resume can be undone.',
    },
  };
  await dialog.getByRole('button', { name: 'Undo move' }).click();
  await expect(dialog.getByRole('alert')).toHaveText(
    'Only a move whose new session could not resume can be undone.',
  );
  await dialog.getByRole('button', { name: 'Undo move' }).click();
  await expect(dialog).toHaveCount(0);
  expect(moves.calls.at(-1)?.call).toBe('POST /api/moves/mv-1/undo');
  await expect(page.getByText('Undoing the move…')).toBeVisible();
});
