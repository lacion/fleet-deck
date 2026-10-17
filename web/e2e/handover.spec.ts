// DL-140: the terminal handover from the session header, against the scripted
// deck: open the provider TUI, see the hold, return to board control, and the
// stop that comes first for a running session. Runs at the phone and the desktop.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import { claudeCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { FakeDeck, logIn, openSession, quietShells, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 's1';
const TITLE = 'Review the repo';
const SHELL = 'tui-e2e0000000000001';
const HOLD = { shell_id: SHELL, pid: 4242, since: 1_760_000_000_000, reason: 'handover requested' };

type Summary = DeckSession['summary'];

function session(extra: Partial<Summary> = {}, status: DeckSession['status'] = 'ended') {
  const ended = status === 'ended';
  return {
    station_id: ST,
    status,
    summary: summary(ID, ended ? 'idle' : 'working', 1, {
      title: TITLE,
      capabilities: claudeCaps(),
      state: ended ? 'exited' : 'running',
      current_run: ended ? null : 'run-1',
      ...extra,
    }),
  } satisfies DeckSession;
}

async function setUp(page: Page, deck: FakeDeck) {
  deck.stations = [stationView(ST, 'Alpha')];
  await quietShells(page);
  await deck.install(page);
  await logIn(page);
  await openSession(page, TITLE);
}

const header = (page: Page) => page.getByLabel('Session header');

const onPhone = () => test.info().project.name === 'phone';
const sessionMenu = (page: Page) => header(page).locator('summary[aria-label="Session menu"]');

/** The phone keeps the terminal actions in the session menu; the desktop has them in the header. */
async function openPhoneMenu(page: Page) {
  if (onPhone()) await sessionMenu(page).click();
}

async function confirmOpen(page: Page, text: string) {
  if (onPhone()) {
    await sessionMenu(page).click();
    await header(page).getByRole('button', { name: 'Open in terminal…' }).click();
  } else {
    await header(page).locator('summary[aria-label="Terminal"]').click();
    await header(page).getByRole('button', { name: 'Open in terminal…' }).click();
  }
  const confirm = header(page).getByRole('group', { name: 'Open in terminal' });
  await expect(confirm).toContainText(text);
  await confirm.getByRole('button', { name: 'Open in terminal' }).click();
}

/** No way to open a terminal: no item in the Terminal menu on the desktop, none in the session menu. */
async function expectNoOpenInTerminal(page: Page) {
  await expect(header(page).locator('button', { hasText: 'Open in terminal…' })).toHaveCount(0);
  await sessionMenu(page).click();
  await expect(header(page).getByRole('button', { name: 'Open in terminal…' })).toHaveCount(0);
}

const WARNING =
  'In the terminal, the board does not see approvals or prompts. The station holds this session until you return it.';

test('an ended session opens in the terminal with one handover and no stop', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  const shells = deck.shells(ST);
  await setUp(page, deck);
  await confirmOpen(page, WARNING);
  await expect.poll(() => shells.posts.length).toBe(1);
  const post = shells.posts[0];
  expect(post?.command_id).toMatch(/^sh-[0-9a-z]{16}$/);
  expect(post?.change).toEqual({
    op: 'handover',
    shell_id: expect.stringMatching(/^tui-/),
    session_id: ID,
    cols: expect.any(Number),
    rows: expect.any(Number),
  });
  expect(typeof post?.change.cols).toBe('number');
  expect(typeof post?.change.rows).toBe('number');
  expect(deck.calls.filter((c) => c.path === '/api/commands/session.stop')).toHaveLength(0);
  expect(shells.posts).toHaveLength(1);
});

test('a held session shows In terminal, keeps the composer off and offers no Resume', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  const shells = deck.shells(ST);
  await setUp(page, deck);
  await confirmOpen(page, WARNING);
  await expect.poll(() => shells.posts.length).toBe(1);
  if (onPhone()) {
    // The phone shows the terminal on its own page; the session is one step back.
    await page.getByRole('button', { name: 'Back' }).click();
  }
  const shellId = shells.posts[0]?.change.shell_id ?? SHELL;
  deck.setSession(session({ terminal: { ...HOLD, shell_id: shellId } }));
  await openPhoneMenu(page);
  await expect(header(page).getByText('In terminal', { exact: true })).toBeVisible();
  await expect(page.getByRole('form', { name: 'Composer' })).toContainText(
    'A terminal holds this session. Return it to board control to send prompts.',
  );
  await expect(header(page).getByRole('button', { name: 'Resume', exact: true })).toHaveCount(0);
  await expect(header(page).locator('button', { hasText: 'Open in terminal…' })).toHaveCount(0);
  await expect(header(page).getByRole('button', { name: 'Open in terminal…' })).toHaveCount(0);
});

test('Return to board control closes the shell, then resumes once the hold is gone', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session({ terminal: HOLD })];
  const shells = deck.shells(ST, (post) => {
    if (post.change.op === 'close') {
      shells.rows = [];
      deck.setSession(session());
    }
  });
  shells.rows = [
    {
      shell_id: SHELL,
      cwd: '/work/repo',
      state: 'running',
      cols: 120,
      rows: 32,
      created_at: 1,
      exit_code: null,
      session_id: ID,
    },
  ];
  const resumes: unknown[] = [];
  deck.accept('session.resume', ST, (command) => resumes.push(command));
  const release = deck.hold('/api/commands/session.resume');
  await setUp(page, deck);
  await openPhoneMenu(page);
  await header(page).getByRole('button', { name: 'Return to board control' }).click();
  // while the flow runs, the header says what happens
  await expect(
    page.getByRole('status').filter({ hasText: 'Ending the terminal, then resuming the session…' }),
  ).toBeVisible();
  release();
  await expect.poll(() => resumes.length).toBe(1);
  expect(resumes).toEqual([{ op: 'resume', session_id: ID }]);
  expect(shells.posts).toHaveLength(1);
  expect(shells.posts[0]?.change).toMatchObject({ op: 'close', shell_id: SHELL });
  expect(
    deck.calls.filter((c) => c.path === '/api/commands/session.resume' && c.body !== null),
  ).toHaveLength(1);
});

test('a running session is stopped gracefully before the handover', async ({ page }) => {
  const deck = new FakeDeck();
  deck.sessions = [session({}, 'working')];
  const shells = deck.shells(ST);
  const stops: unknown[] = [];
  deck.accept('session.stop', ST, (command) => {
    stops.push(command);
    queueMicrotask(() => deck.setSession(session({ state: 'exited', current_run: null })));
  });
  const release = deck.hold('/api/commands/session.stop');
  await setUp(page, deck);
  await confirmOpen(page, `${WARNING} The session stops first.`);
  // while the session stops, the header says what happens
  await expect(
    page
      .getByRole('status')
      .filter({ hasText: 'Stopping the session, then starting its terminal…' }),
  ).toBeVisible();
  release();
  await expect.poll(() => shells.posts.length).toBe(1);
  expect(stops).toEqual([{ op: 'stop', session_id: ID, how: 'graceful' }]);
  expect(shells.posts[0]?.change).toMatchObject({ op: 'handover', session_id: ID });
  const paths = deck.calls
    .filter((c) => c.path === '/api/commands/session.stop' || c.path.endsWith('/shells'))
    .map((c) => c.path);
  expect(paths).toEqual(['/api/commands/session.stop', `/api/stations/${ST}/shells`]);
});

test('a login without the admin scope has no Open in terminal', async ({ page }) => {
  const deck = new FakeDeck();
  deck.scopes = ['read', 'drive', 'approve'];
  deck.sessions = [session()];
  await setUp(page, deck);
  await expect(header(page).getByRole('button', { name: 'Resume', exact: true })).toBeVisible();
  await expectNoOpenInTerminal(page);
});
