// R13 P1: the wall route `#/wall`: the four areas, a QR code per needs-you item, auto-focus,
// the keys, the portrait shape, and no write control.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import { claudeCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { type FakeDeck, fitsWidth, logIn } from './fake-deck';
import { ALPHA, NOW, bashRequest, threeSessionDeck } from './scenes';

// The wall is for a wide screen: this file sets its own viewport and ignores the phone defaults.
test.use({ isMobile: false, hasTouch: false });

const patch = [
  'diff --git a/src/a.ts b/src/a.ts',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1,2 +1,2 @@',
  ' keep',
  '-old',
  '+new',
  '',
].join('\n');

async function open(page: Page, size: { width: number; height: number }, hash = '#/wall') {
  const deck = threeSessionDeck();
  deck.on('/api/activity', () => ({
    status: 200,
    body: {
      items: [
        {
          station_id: ALPHA,
          session_id: 's2',
          session_title: 'Write the docs',
          repo: null,
          kind: 'message',
          text: 'Finished the intro',
          at: NOW,
          station_seq: 5,
        },
      ],
    },
  }));
  deck.on(`/api/stations/${ALPHA}/sessions/s2/diff`, () => ({
    status: 200,
    body: {
      session_id: 's2',
      base_oid: 'abc',
      summary: { files_changed: 1, insertions: 1, deletions: 1 },
      patch,
      truncated: false,
      scope: 'repo',
    },
  }));
  await page.setViewportSize(size);
  await deck.install(page);
  await logIn(page);
  await expect(page.getByRole('complementary', { name: 'Sessions' }))
    .toBeVisible({
      timeout: 10_000,
    })
    .catch(() => undefined);
  await page.evaluate((h) => {
    window.location.hash = h;
  }, hash);
  await expect(page.getByTestId('wall')).toBeVisible();
  return deck;
}

const needsMe = (id: string, title: string, at: number): DeckSession => ({
  station_id: ALPHA,
  status: 'needs_you',
  summary: summary(id, 'needs_you', at, {
    title,
    cwd: '/work/repo',
    capabilities: claudeCaps(),
    open_requests: [{ ...bashRequest, request_id: `req-${id}`, requested_at: at }],
  }),
});

async function noWriteControl(page: Page) {
  await expect(page.getByTestId('wall').locator('button, textarea, input, select, a')).toHaveCount(
    0,
  );
}

test('3440×1440 shows the four areas and a QR code for each needs-you item', async ({ page }) => {
  await open(page, { width: 3440, height: 1440 });
  const wall = page.getByTestId('wall');
  await expect(wall).toHaveAttribute('data-shape', 'ultra');
  for (const area of ['needs', 'tiles', 'side', 'stream']) {
    await expect(wall.locator(`[data-area="${area}"]`)).toBeVisible();
  }
  await expect(wall).toContainText('Fleet Deck wall');
  await expect(wall.getByRole('status')).toHaveText('Live');
  const needs = wall.locator('[data-area="needs"]');
  await expect(needs.getByRole('img', { name: /^QR code for / })).toHaveCount(1);
  await expect(needs.getByRole('img', { name: /^QR code for / })).toHaveAttribute(
    'aria-label',
    new RegExp(`/#/s/${ALPHA}/s1\\?request=req-1$`),
  );
  await expect(needs).toContainText('Scan to answer on your phone');
  await expect(wall.locator('[data-area="stream"]')).toContainText('Finished the intro');
  await expect(wall.locator('[data-area="tiles"] li')).toHaveCount(3);
  await noWriteControl(page);
  expect(await fitsWidth(page)).toBe(true);
});

test('a new request takes the wall to focus on its session, and Esc returns to mission', async ({
  page,
}) => {
  const deck: FakeDeck = await open(page, { width: 3440, height: 1440 });
  const wall = page.getByTestId('wall');
  await expect(wall).toHaveAttribute('data-mode', 'mission');
  deck.sessions = [...deck.sessions, needsMe('s9', 'Deploy the site', NOW + 5000)];
  deck.publish();
  await expect(wall).toHaveAttribute('data-mode', 'focus');
  await expect(wall.locator('[data-session="s9"]')).toBeVisible();
  expect(page.url()).toContain('mode=focus');
  await noWriteControl(page);
  await page.keyboard.press('Escape');
  await expect(wall).toHaveAttribute('data-mode', 'mission');
  expect(page.url()).toContain('mode=mission');
  expect(await fitsWidth(page)).toBe(true);
});

test('pin=1 turns auto-focus off', async ({ page }) => {
  const deck = await open(page, { width: 3440, height: 1440 }, '#/wall?pin=1');
  const wall = page.getByTestId('wall');
  deck.sessions = [...deck.sessions, needsMe('s9', 'Deploy the site', NOW + 5000)];
  deck.publish();
  await expect(wall.locator('[data-area="needs"] li')).toHaveCount(2);
  await expect(wall).toHaveAttribute('data-mode', 'mission');
});

test('the keys only navigate: arrows cycle the modes, Enter focuses a tile, n does nothing', async ({
  page,
}) => {
  await open(page, { width: 3440, height: 1440 });
  const wall = page.getByTestId('wall');
  await page.keyboard.press('n');
  await expect(wall).toBeVisible();
  expect(page.url()).toContain('#/wall');
  await page.keyboard.press('ArrowRight');
  await expect(wall).toHaveAttribute('data-mode', 'focus');
  await noWriteControl(page);
  await page.keyboard.press('ArrowRight');
  await expect(wall).toHaveAttribute('data-mode', 'review');
  await noWriteControl(page);
  // race and artifact have no target: the next stop is ambient
  await page.keyboard.press('ArrowRight');
  await expect(wall).toHaveAttribute('data-mode', 'ambient');
  await expect(wall).toContainText('Running');
  await noWriteControl(page);
  await page.keyboard.press('Escape');
  await expect(wall).toHaveAttribute('data-mode', 'mission');
  // Tab moves between tiles; Enter focuses the tile that has the focus.
  await page.keyboard.press('Tab');
  const tile = wall.locator('[data-area="tiles"] li:focus');
  await expect(tile).toHaveCount(1);
  const id = await tile.getAttribute('data-session');
  await page.keyboard.press('Enter');
  await expect(wall).toHaveAttribute('data-mode', 'focus');
  await expect(wall.locator(`[data-session="${id}"]`)).toBeVisible();
});

test('the focus mode shows the chat and the diff of one session, read-only', async ({ page }) => {
  await open(page, { width: 3440, height: 1440 }, `#/wall?mode=focus&session=${ALPHA}/s2`);
  const wall = page.getByTestId('wall');
  await expect(wall).toHaveAttribute('data-mode', 'focus');
  await expect(wall).toContainText('Since the session started');
  await expect(wall.locator('.fd-diff')).toContainText('+new');
  await noWriteControl(page);
});

test('1080×1920 gives the portrait layout with no sideways scroll', async ({ page }) => {
  await open(page, { width: 1080, height: 1920 });
  const wall = page.getByTestId('wall');
  await expect(wall).toHaveAttribute('data-shape', 'portrait');
  await expect(wall.locator('[data-area="needs"]')).toBeVisible();
  await expect(wall.locator('[data-area="tiles"]')).toBeVisible();
  await expect(wall.locator('[data-area="stream"]')).toBeVisible();
  await expect(wall.locator('[data-area="side"]')).toHaveCount(0);
  expect(await fitsWidth(page)).toBe(true);
  const box = await wall.boundingBox();
  expect(box?.width).toBeLessThanOrEqual(1080);
  expect(box?.height).toBeLessThanOrEqual(1920);
});
