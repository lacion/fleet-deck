// M3 6-11: the human shell of a station in a phone browser (PLAN §6.8, DL-404).
import { expect, test } from '@playwright/test';
import { CSRF, FakeDeck, fitsWidth, logIn, openMore, stationView } from './fake-deck';

type ShellRow = {
  shell_id: string;
  cwd: string;
  state: 'running' | 'exited';
  cols: number;
  rows: number;
  created_at: number;
  exit_code: number | null;
};

test('an admin opens a station shell, types into it, reattaches, and closes it', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'alpha')];
  let shells: ShellRow[] = [];
  deck.on('/api/stations/alpha/shells', (body) => {
    if (body === null) return { status: 200, body: shells };
    const change = (body as { change: { op: string; shell_id: string; cwd?: string } }).change;
    if (change.op === 'open' && change.cwd === '/etc') {
      return {
        status: 400,
        body: { code: 'invalid_command', message: '`/etc` is outside the allowed roots' },
      };
    }
    if (change.op === 'open') {
      shells = [
        {
          shell_id: change.shell_id,
          cwd: change.cwd ?? '',
          state: 'running',
          cols: 80,
          rows: 24,
          created_at: 1,
          exit_code: null,
        },
      ];
    }
    if (change.op === 'close') shells = [];
    return { status: 200, body: { state: 'accepted' } };
  });
  // The fake station echoes the keystrokes and keeps them as scrollback.
  let scrollback = 'welcome to alpha\r\n';
  let attaches = 0;
  await page.routeWebSocket('**/shells/*/attach', (ws) => {
    attaches += 1;
    ws.send(Buffer.from(scrollback));
    ws.onMessage((data) => {
      const text = typeof data === 'string' ? data : data.toString('utf8');
      scrollback += text;
      ws.send(Buffer.from(text));
    });
  });
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  await page.locator('details.fd-station-sec > summary').click();
  const panel = page.getByRole('region', { name: 'Shells on alpha' });
  await expect(panel).toContainText('No open shells.');

  // A directory outside the allowed roots is refused with the station's reason.
  await panel.getByLabel('Directory').fill('/etc');
  await panel.getByRole('button', { name: 'Open shell' }).click();
  await expect(panel.getByRole('alert')).toContainText('outside the allowed roots');

  await panel.getByLabel('Directory').fill('/work/repo');
  await panel.getByRole('button', { name: 'Open shell' }).click();
  await expect(panel).toContainText('/work/repo · running');
  const terminal = panel.locator('.fd-terminal');
  // xterm.js renders only while the terminal is visible.
  await terminal.scrollIntoViewIfNeeded();
  await expect(terminal).toContainText('welcome to alpha');
  await terminal.click();
  await page.keyboard.type('pwd');
  await expect(terminal).toContainText('welcome to alphapwd');
  expect(await fitsWidth(page)).toBe(true);

  // Detach and attach again: the scrollback comes back.
  await panel.getByRole('button', { name: 'Detach' }).click();
  await expect(panel.locator('.fd-terminal')).toHaveCount(0);
  await panel.getByRole('button', { name: 'Attach' }).click();
  await panel.locator('.fd-terminal').scrollIntoViewIfNeeded();
  await expect(panel.locator('.fd-terminal')).toContainText('welcome to alphapwd');
  expect(attaches).toBe(2);

  await panel.getByRole('button', { name: 'Close shell' }).click();
  await expect(panel).toContainText('No open shells.');
  const posts = deck.calls.filter(
    (c) => c.path === '/api/stations/alpha/shells' && c.body !== null,
  );
  const ops = posts.map((c) => (c.body as { change: { op: string } }).change.op);
  expect(ops[0]).toBe('open');
  expect(ops).toContain('resize');
  expect(ops.at(-1)).toBe('close');
  for (const c of posts) {
    expect(c.csrf).toBe(CSRF);
    expect((c.body as { command_id: string }).command_id).toMatch(/^sh-[0-9a-z]{16}$/);
  }
});

test('an unreachable station cannot open a shell', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'alpha', { reachable: false })];
  deck.on('/api/stations/alpha/shells', () => ({ status: 200, body: [] }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  await page.locator('details.fd-station-sec > summary').click();
  const panel = page.getByRole('region', { name: 'Shells on alpha' });
  await panel.getByLabel('Directory').fill('/work/repo');
  await expect(panel.getByRole('button', { name: 'Open shell' })).toBeDisabled();
});

test('a login without the admin scope sees no shell panel', async ({ page }) => {
  const viewer = new FakeDeck();
  viewer.scopes = ['read', 'drive'];
  viewer.stations = [stationView('st-alpha', 'alpha')];
  await viewer.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  await expect(page.getByRole('region', { name: 'Shells on alpha' })).toHaveCount(0);
});
