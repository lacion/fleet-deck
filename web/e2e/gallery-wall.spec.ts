// The wall gallery (opt-in: FD_GALLERY=1): screenshots of the wall (`#/wall`, R13 P1), the wall
// display page (`/display`, R13 P2c), the Displays settings and the Desktop panel of a station,
// with the three-station scene of gallery-scene.ts, in the dark and the light theme. Each shot is
// test-results/gallery/<project>-<NN>-<name>-<theme>.png. The wide wall shots set their own
// viewport and run in the desktop project only.
import { expect, type Page, test } from '@playwright/test';
import { logIn } from './fake-deck';
import { galleryDeck, LIGHTING, OTTER } from './gallery-scene';

test.skip(!process.env['FD_GALLERY'], 'FD_GALLERY is not set');

const desktopOnly = () =>
  test.skip(test.info().project.name !== 'desktop', 'the desktop layout only');

const MIN = 60_000;

async function setUp(page: Page) {
  const deck = galleryDeck();
  const now = Date.now();
  deck.on('/api/displays', () => ({
    status: 200,
    body: {
      displays: [
        {
          display_id: 'D'.repeat(26),
          name: 'Office wall',
          ceiling: [LIGHTING],
          show_diffs: false,
          wake_on_needs_you: true,
          quiet_hours: '22:00-07:00',
          binding: null,
          created_at: now - 3 * 24 * 60 * MIN,
          last_seen: now - 2 * MIN,
        },
      ],
      pending: [
        {
          pairing_id: 'p1',
          code: '482913',
          source: '10.0.0.7',
          user_agent: 'Mozilla/5.0 (X11; Linux x86_64) Chromium/140.0',
          created_at: now - MIN,
          expires_at: now + 4 * MIN,
        },
      ],
    },
  }));
  await deck.install(page);
  await logIn(page);
  await page.locator('a.fd-srow').first().waitFor();
  return deck;
}

/** The page as the person sees it: fonts loaded, nothing moving, the viewport only. */
async function shot(page: Page, nn: string, name: string, theme: string) {
  await page.evaluate(() => document.fonts.ready);
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

async function wall(page: Page, hash = '#/wall?pin=1') {
  await goTo(page, hash);
  const root = page.getByTestId('wall');
  await expect(root).toBeVisible();
  return root;
}

test('41 wall', async ({ page }) => {
  await setUp(page);
  const root = await wall(page);
  await expect(root.locator('[data-area="needs"] li').first()).toBeVisible();
  await themed(page, '41', 'wall');
});

test('42 wall 1920', async ({ page }) => {
  desktopOnly();
  await page.setViewportSize({ width: 1920, height: 1080 });
  await setUp(page);
  const root = await wall(page);
  await expect(root).toHaveAttribute('data-shape', 'wide');
  await expect(root.locator('[data-area="stream"] li').first()).toBeVisible();
  await themed(page, '42', 'wall-1920');
});

test('43 wall ultra', async ({ page }) => {
  desktopOnly();
  await page.setViewportSize({ width: 2580, height: 1080 });
  await setUp(page);
  const root = await wall(page);
  await expect(root).toHaveAttribute('data-shape', 'ultra');
  await expect(root.locator('[data-area="stream"] li').first()).toBeVisible();
  await themed(page, '43', 'wall-ultra');
});

test('44 wall standard', async ({ page }) => {
  desktopOnly();
  await page.setViewportSize({ width: 1280, height: 960 });
  await setUp(page);
  const root = await wall(page);
  await expect(root).toHaveAttribute('data-shape', 'standard');
  await themed(page, '44', 'wall-standard');
});

test('45 wall focus', async ({ page }) => {
  await setUp(page);
  const root = await wall(page, `#/wall?pin=1&mode=focus&session=${LIGHTING}/${OTTER}`);
  await expect(root).toHaveAttribute('data-mode', 'focus');
  await expect(root.locator('.fd-diff')).toBeVisible();
  await themed(page, '45', 'wall-focus');
});

test('46 wall ambient', async ({ page }) => {
  await setUp(page);
  const root = await wall(page, '#/wall?pin=1&mode=ambient');
  await expect(root).toContainText('Running');
  await themed(page, '46', 'wall-ambient');
});

test('47 wall pair', async ({ page }) => {
  const deck = galleryDeck();
  deck.on('/api/displays/pair', () => ({
    status: 200,
    body: { pairing_id: 'p1', code: '482913', expires_at: Date.now() + 272_000 },
  }));
  deck.on('/api/displays/pair/p1', () => ({ status: 200, body: { state: 'pending' } }));
  await deck.install(page);
  await page.goto('/display#/display-pair');
  await expect(page.getByLabel('Pairing code')).toHaveText('482 913');
  await themed(page, '47', 'wall-pair');
});

test('48 settings displays', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/settings/displays');
  await expect(page.getByRole('heading', { name: 'Office wall' })).toBeVisible();
  await themed(page, '48', 'settings-displays');
  // The lower part of the page: the waiting pairing and the display.
  await page
    .getByRole('heading', { name: 'Waiting for a code' })
    .evaluate((el) => el.scrollIntoView({ block: 'start' }));
  await themed(page, '48', 'settings-displays-lower');
});

test('49 desktop panel', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/stations');
  const panel = page.locator('details.fd-desk').first();
  await panel.locator('summary').click();
  await expect(panel.getByRole('list', { name: 'Monitors' })).toBeVisible();
  await panel.scrollIntoViewIfNeeded();
  await themed(page, '49', 'desktop-panel');
});
