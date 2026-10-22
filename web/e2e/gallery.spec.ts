// The UI gallery (opt-in: FD_GALLERY=1): a screenshot of every board page with the three-station
// scene of gallery-scene.ts, in the dark and the light theme. Each shot is
// test-results/gallery/<project>-<NN>-<name>-<theme>.png at the viewport of its project (phone
// and desktop). The tablet project does not run this spec.
import { expect, type Page, test } from '@playwright/test';
import { logIn } from './fake-deck';
import { galleryDeck, LIGHTING, OTTER } from './gallery-scene';

test.skip(!process.env['FD_GALLERY'], 'FD_GALLERY is not set');

const desktopOnly = () =>
  test.skip(test.info().project.name !== 'desktop', 'the desktop layout only');
const phoneOnly = () => test.skip(test.info().project.name !== 'phone', 'the phone layout only');

/** A shell that shows a prompt and the last command, and stays open. */
async function shellScreen(page: Page) {
  const screen = [
    '\x1b[32mme@lighting\x1b[0m:\x1b[34m~/projects/fleet-deck\x1b[0m$ git status -sb',
    '## fleet-sync...origin/fleet-sync',
    ' M crates/fleetdeck-station/src/shim/reaper.rs',
    '\x1b[32mme@lighting\x1b[0m:\x1b[34m~/projects/fleet-deck\x1b[0m$ ',
  ].join('\r\n');
  await page.routeWebSocket('**/shells/*/attach', (ws) => ws.send(Buffer.from(screen)));
}

async function setUp(page: Page, options: { needsYou?: boolean } = {}) {
  const deck = galleryDeck(options);
  // FD_GALLERY_DEBUG=1 lists the API calls that the scene does not answer (status 400 and up).
  if (process.env['FD_GALLERY_DEBUG']) {
    page.on('response', (r) => {
      if (r.status() >= 400 && r.url().includes('/api/'))
        console.log(
          `UNANSWERED ${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`,
        );
    });
  }
  await shellScreen(page);
  await deck.install(page);
  await logIn(page);
  await page.locator('a.fd-srow').first().waitFor();
  return deck;
}

/** The board as the person sees it: fonts loaded, nothing moving, the viewport only. */
async function shot(page: Page, nn: string, name: string, theme: string) {
  await page.evaluate(() => document.fonts.ready);
  // No hover from the last click: a hovered row or button would look selected.
  await page.mouse.move(0, 0);
  await page.screenshot({
    path: `test-results/gallery/${test.info().project.name}-${nn}-${name}-${theme}.png`,
    fullPage: false,
    animations: 'disabled',
  });
}

/** One shot in each theme. */
async function themed(page: Page, nn: string, name: string) {
  for (const theme of ['dark', 'light']) {
    await page.evaluate((t) => {
      document.documentElement.dataset['theme'] = t;
    }, theme);
    await shot(page, nn, name, theme);
  }
}

async function goTo(page: Page, hash: string) {
  await page.evaluate((h) => {
    window.location.hash = h;
  }, hash);
}

/** Opens `otter-n1rr` on a tab and returns the session region. */
async function toOtter(page: Page, tab: 'chat' | 'changes' | 'files' | 'artifacts') {
  await goTo(page, `#/s/${LIGHTING}/${OTTER}${tab === 'chat' ? '' : `?tab=${tab}`}`);
  const pane = page.getByRole('region', { name: 'Session' });
  await expect(pane.getByRole('heading', { level: 2 })).toHaveText('Fix flaky shim test');
  return pane;
}

/** The home: the session list on the phone, the Fleet board on the desktop. */
async function homeReady(page: Page, needsYou: boolean) {
  if (test.info().project.name === 'phone') {
    const list = page.getByRole('complementary', { name: 'Sessions' });
    await expect(list.getByRole('region', { name: 'Station Lighting' })).toBeVisible();
    if (needsYou) await expect(list.getByRole('region', { name: 'Needs you' })).toBeVisible();
  } else {
    await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
  }
}

test('01 home', async ({ page }) => {
  await setUp(page);
  await homeReady(page, true);
  await themed(page, '01', 'home');
});

test('02 fleet', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/fleet');
  await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
  await themed(page, '02', 'fleet');
});

test('03 session chat', async ({ page }) => {
  await setUp(page);
  const pane = await toOtter(page, 'chat');
  await expect(pane.locator('table')).toBeVisible();
  await expect(pane.locator('pre').filter({ hasText: 'block_sigchld' })).toBeVisible();
  await expect(pane.getByRole('button', { name: 'Allow', exact: true }).first()).toBeVisible();
  await themed(page, '03', 'session-chat');
});

test('04 session changes', async ({ page }) => {
  await setUp(page);
  const pane = await toOtter(page, 'changes');
  await pane.getByRole('button', { name: 'Show the diff' }).click();
  await expect(pane.locator('[data-diff="turn-1"]')).toContainText('block_sigchld');
  await themed(page, '04', 'session-changes');
});

test('05 session files', async ({ page }) => {
  await setUp(page);
  const pane = await toOtter(page, 'files');
  await expect(pane.getByRole('tabpanel')).toBeVisible();
  await themed(page, '05', 'session-files');
});

test('06 session artifacts', async ({ page }) => {
  await setUp(page);
  const pane = await toOtter(page, 'artifacts');
  await expect(pane.getByRole('tabpanel')).toBeVisible();
  await themed(page, '06', 'session-artifacts');
});

/** Opens a shell in the workspace of `otter-n1rr`: the terminal dock gets one tab. */
async function openShell(page: Page) {
  const pane = await toOtter(page, 'chat');
  await pane.locator('summary[aria-label="Session menu"]').click();
  await pane.getByRole('button', { name: 'Open shell in workspace' }).click();
}

test('07 terminal dock', async ({ page }) => {
  desktopOnly();
  await setUp(page);
  await openShell(page);
  const dock = page.getByRole('region', { name: 'Terminals' });
  await expect(dock.getByRole('tablist', { name: 'Open terminals' }).getByRole('tab')).toHaveCount(
    1,
  );
  await themed(page, '07', 'terminal-dock');
});

test('08 new session', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/new');
  await expect(page.getByRole('region', { name: 'New session' })).toBeVisible();
  await themed(page, '08', 'new-session');
});

test('09 stations', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/stations');
  await expect(page.locator('.fd-station').first()).toBeVisible();
  await themed(page, '09', 'stations');
});

test('10 add station dialog', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/stations');
  await page.getByRole('button', { name: 'Add station' }).click();
  await expect(page.getByRole('dialog', { name: 'Add station' })).toBeVisible();
  await themed(page, '10', 'add-station-dialog');
});

test('11 settings deck', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/settings');
  await expect(page.getByRole('region', { name: 'Tailnet' })).toBeVisible();
  await themed(page, '11', 'settings-deck');
});

test('12 settings station', async ({ page }) => {
  await setUp(page);
  await goTo(page, `#/settings/station/${LIGHTING}`);
  await expect(page.getByRole('tabpanel')).toBeVisible();
  await themed(page, '12', 'settings-station');
});

test('13 activity', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/activity');
  await expect(page.getByRole('heading', { level: 1, name: 'Activity' })).toBeVisible();
  await expect(page.locator('li.fd-arow').first()).toBeVisible();
  await themed(page, '13', 'activity');
});

test('14 watch', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/watch');
  await expect(page.getByRole('heading', { level: 1, name: 'Watch' })).toBeVisible();
  await themed(page, '14', 'watch');
});

test('15 plans', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/plans');
  const library = page.getByRole('region', { name: 'Plan library' });
  await expect(library.getByRole('article').first()).toBeVisible();
  await themed(page, '15', 'plans');
});

test('16 automations', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/automations');
  await expect(page.getByRole('heading', { level: 1, name: 'Automations' })).toBeVisible();
  await expect(page.getByRole('article', { name: /^Automation / }).first()).toBeVisible();
  await themed(page, '16', 'automations');
});

test('17 artifacts', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/artifacts');
  await expect(page.getByRole('heading', { level: 1, name: 'Artifacts' })).toBeVisible();
  await themed(page, '17', 'artifacts');
});

test('18 projects', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/projects');
  await expect(page.getByRole('heading', { level: 1, name: 'Projects' })).toBeVisible();
  await themed(page, '18', 'projects');
});

test('19 files', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/files');
  await expect(page.getByRole('heading', { level: 1, name: 'Files' })).toBeVisible();
  const folder = page.getByRole('form', { name: 'Open a folder' });
  await folder.getByLabel('Folder').fill('/home/me/projects/fleet-deck');
  await folder.getByRole('button', { name: 'Open' }).click();
  await expect(page.getByText('crates/')).toBeVisible();
  await themed(page, '19', 'files');
});

test('20 shares', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/shares');
  await expect(page.getByRole('heading', { level: 1, name: 'Shares' })).toBeVisible();
  await themed(page, '20', 'shares');
});

test('21 terminals', async ({ page }) => {
  await setUp(page);
  await openShell(page);
  await goTo(page, '#/terminals');
  if (test.info().project.name === 'phone') {
    await expect(page.getByRole('heading', { level: 1, name: 'Terminals' })).toBeVisible();
  } else {
    // The desktop has no Terminals page: the route shows the home with the dock open.
    await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
  }
  await expect(
    page.getByRole('region', { name: 'Terminals' }).getByRole('tab').first(),
  ).toBeVisible();
  await themed(page, '21', 'terminals');
});

test('22 more', async ({ page }) => {
  phoneOnly();
  await setUp(page);
  await goTo(page, '#/more');
  await expect(page.getByRole('heading', { level: 1, name: 'More' })).toBeVisible();
  await themed(page, '22', 'more');
});

test('23 help overlay', async ({ page }) => {
  desktopOnly();
  await setUp(page);
  await page.locator('body').press('?');
  await expect(page.getByRole('dialog', { name: 'Keyboard and help' })).toBeVisible();
  await themed(page, '23', 'help-overlay');
});

test('24 palette', async ({ page }) => {
  desktopOnly();
  await setUp(page);
  await page.locator('body').press('Control+k');
  await expect(page.getByRole('dialog', { name: 'Go to session' })).toBeVisible();
  await themed(page, '24', 'palette');
});

test('25 home all clear', async ({ page }) => {
  await setUp(page, { needsYou: false });
  await homeReady(page, false);
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  await themed(page, '25', 'home-all-clear');
});
