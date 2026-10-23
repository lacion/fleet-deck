// DL-144, DL-146, DL-148: the click flows of the spawn form's branch, access
// check and setup command, and the "Add a device" dialog. `FD_SHOTS=<dir>`
// also saves a screenshot of each screen.
import { expect, type Page, test } from '@playwright/test';
import type { WorkspaceListing } from '../src/gen';
import { claudeCaps } from '../src/testing/capabilities';
import { FakeDeck, fitsWidth, logIn, openAddDevice, stationView } from './fake-deck';

const ROOT = '/home/me/projects';
const alpha = stationView('st-alpha', 'Alpha', {
  info: {
    name: 'alpha',
    incarnation: 'inc-1',
    head: 3,
    live_sessions: 0,
    version: '0.2.0',
    max_sessions: 4,
    allow_shell: true,
    allow_setup_cmd: true,
    providers: [
      {
        instance: 'personal',
        provider: 'claude',
        version: '2.1.5',
        auth: 'signed_in',
        version_status: 'unknown',
        capabilities: claudeCaps(),
      },
    ],
  },
  info_at: 1_760_000_000_000,
});

const LISTINGS: Record<string, WorkspaceListing> = {
  '': {
    path: null,
    parent: null,
    entries: [{ name: 'projects', path: ROOT, repository: false }],
    truncated: false,
  },
  [ROOT]: {
    path: ROOT,
    parent: null,
    entries: [{ name: 'app', path: `${ROOT}/app`, repository: true }],
    truncated: false,
  },
  [`${ROOT}/app`]: { path: `${ROOT}/app`, parent: ROOT, entries: [], truncated: false },
};

async function shot(page: Page, name: string): Promise<void> {
  const dir = process.env['FD_SHOTS'];
  if (dir) await page.screenshot({ path: `${dir}/${test.info().project.name}-${name}.png` });
}

function deckWithStation(): FakeDeck {
  const deck = new FakeDeck();
  deck.stations = [alpha];
  deck.on('/api/stations/st-alpha/workspaces', (_body, query) => {
    const listing = LISTINGS[query.get('path') ?? ''];
    return listing === undefined
      ? { status: 400, body: { code: 'outside_roots', message: 'outside' } }
      : { status: 200, body: listing };
  });
  return deck;
}

async function chooseApp(page: Page): Promise<void> {
  await page.getByRole('link', { name: 'New', exact: true }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('button', { name: 'Browse…' }).click();
  // The station has one allowed root (projects): Browse… opens inside it.
  await form.getByRole('button', { name: 'Open app' }).click();
  await form.getByRole('button', { name: 'Use this folder' }).click();
  await expect(form.getByLabel('Directory path')).toHaveValue(`${ROOT}/app`);
}

test('a worktree start sends the branch, the base and the setup command', async ({ page }) => {
  const deck = deckWithStation();
  deck.on('/api/commands/session.start', () => ({
    status: 200,
    body: {
      state: 'done',
      receipt: { state: 'accepted', result: { session_id: 's-new' }, reject: null },
    },
  }));
  await deck.install(page);
  await logIn(page);
  await chooseApp(page);
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('radiogroup', { name: 'Workspace' }).getByText('New worktree').click();
  await expect(form.getByRole('textbox', { name: 'Base', exact: true })).toHaveCount(0);
  await form.getByRole('textbox', { name: 'Branch', exact: true }).fill('a..b');
  await expect(form).toContainText('The branch name is not valid.');
  await form.getByRole('textbox', { name: 'Branch', exact: true }).fill('feature/PROJ-123-login');
  await form.getByRole('textbox', { name: 'Base', exact: true }).fill('main');
  await form.getByText('More options', { exact: true }).click();
  await form.getByLabel('Setup command (optional)').fill('bun install');
  await form.getByLabel('What should the agent do? (optional)').fill('Fix the login');
  await shot(page, 'spawn-worktree');
  expect(await fitsWidth(page)).toBe(true);
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect
    .poll(() => deck.calls.some((c) => c.path === '/api/commands/session.start'))
    .toBe(true);
  const call = deck.calls.find((c) => c.path === '/api/commands/session.start');
  const body = call?.body as { command?: Record<string, unknown> } | undefined;
  const command = body?.command ?? {};
  expect(command['workspace']).toEqual({
    kind: 'worktree',
    branch: 'feature/PROJ-123-login',
    base: 'main',
  });
  expect(command['setup_cmd']).toBe('bun install');
});

test('the access check of a clone URL shows the hint and what Git said', async ({ page }) => {
  const deck = deckWithStation();
  deck.on('/api/stations/st-alpha/workspace/preflight', () => ({
    status: 200,
    body: {
      ok: false,
      message: 'git@github.com: Permission denied (publickey).',
      hint: "The station's SSH key has no access. Add the station user's SSH key to the Git host, or use an https:// URL.",
    },
  }));
  await deck.install(page);
  await logIn(page);
  await chooseApp(page);
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('radiogroup', { name: 'Workspace' }).getByText('Clone repository').click();
  await form.getByLabel('Repository', { exact: true }).fill('git@github.com:me/private.git');
  await form.getByRole('button', { name: 'Check access' }).click();
  await expect(form).toContainText("The station's SSH key has no access.");
  await form.getByText('Git said').click();
  await expect(form).toContainText('Permission denied (publickey).');
  await shot(page, 'spawn-clone-check');
  expect(await fitsWidth(page)).toBe(true);
  await form.getByLabel('Repository', { exact: true }).fill('git@github.com:me/other.git');
  await expect(form).not.toContainText("The station's SSH key has no access.");
});

test('Add a device shows a QR code, the link and its end', async ({ page }) => {
  const deck = deckWithStation();
  deck.on('/api/auth/device-link', () => ({
    status: 200,
    body: {
      links: [
        {
          label: 'Tailnet deck.tail1234.ts.net',
          url: 'https://deck.tail1234.ts.net:7420/#login=tok',
          certificate_sha256: 'ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12',
          trust: 'self_signed',
        },
        {
          label: 'This computer',
          url: 'https://127.0.0.1:7420/#login=tok',
          certificate_sha256: 'ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12',
          trust: 'self_signed',
        },
      ],
      expires_at: Date.now() + 5 * 60_000,
      certificate_sha256: 'ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12',
      self_signed: true,
    },
  }));
  await deck.install(page);
  await logIn(page);
  if (test.info().project.name === 'phone') {
    await page
      .getByRole('navigation', { name: 'Board' })
      .getByRole('link', { name: 'More' })
      .click();
  }
  await openAddDevice(page);
  const dialog = page.getByRole('dialog', { name: 'Add a device' });
  await expect(dialog.getByRole('img', { name: 'QR code of the login link' })).toBeVisible();
  await expect(dialog.getByRole('textbox', { name: 'Login link' })).toHaveValue(
    'https://deck.tail1234.ts.net:7420/#login=tok',
  );
  await expect(dialog).toContainText('Ends in');
  await expect(dialog).toContainText('AB12 CD34 EF56 AB12');
  await shot(page, 'device-link');
  expect(await fitsWidth(page)).toBe(true);
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
});
