// FG-1..FG-4 (design §16.2, phase 1): the project Git settings form, the "Not pushed" badge with
// "Push now", the "Stale base" chip and the timeline lines of the Fleet Git events, at the phone
// viewport. The board talks to a scripted deck.
import { expect, type Page, test } from '@playwright/test';
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
  await expect(alpha.getByText('linux · x86_64 · gpu, always_on')).toBeVisible();
  await expect(bravo.getByText('macos · aarch64 · on battery')).toBeVisible();
  await expect(alpha.getByText('Config: Reference')).toBeVisible();
  await expect(bravo.getByText('Config: Differs: agents, skills')).toBeVisible();
  await expect(old.getByText('Config: Unknown')).toBeVisible();
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
  await expect(page.getByText('Pick a reference station for the config report.')).toBeVisible();
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
  await page.getByRole('button', { name: 'New session' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('combobox', { name: 'Station' }).selectOption('auto');
  await form.getByLabel('Directory path').fill('/work/repo');
  await form.getByLabel('Needs').fill('gpu');
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect(form.getByText('No station matches: gpu.')).toBeVisible();
  expect(deck.calls.some((c) => c.path === '/api/commands/session.start')).toBe(false);
});
