// The phone navigation: every board page can be left, Back of a session returns to where the
// person came from, the session menu fits the screen, the tablet drawer backdrop covers the
// screen, and the pairing dialog closes on Escape and on an outside click.
import { expect, type Page, test } from '@playwright/test';
import { logIn, openFleet, quietShells } from './fake-deck';
import { ALPHA, threeSessionDeck } from './scenes';

async function start(page: Page) {
  const deck = threeSessionDeck();
  deck.on('/api/artifacts', () => ({ status: 200, body: [] }));
  deck.on('/api/projects', () => ({ status: 200, body: { projects: [] } }));
  deck.on('/api/schedules', () => ({ status: 200, body: [] }));
  await quietShells(page);
  await deck.install(page);
  await logIn(page);
  await page.locator('a.fd-srow').first().waitFor();
  return deck;
}

async function go(page: Page, hash: string) {
  await page.evaluate((h) => {
    window.location.hash = h;
  }, hash);
  await expect(page).toHaveURL(new RegExp(`${hash.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')}$`));
}

const PAGES: readonly (readonly [string, string])[] = [
  ['artifacts', '#/artifacts'],
  ['an artifact', `#/artifact/${ALPHA}/a1`],
  ['settings', '#/settings'],
  ['automations', '#/automations'],
  ['projects', '#/projects'],
  ['a project', '#/project/p1'],
  ['a compare page', '#/compare/b1'],
];

for (const [name, hash] of PAGES) {
  test(`${name} shows the bottom bar and Sessions leaves it`, async ({ page }) => {
    await start(page);
    await go(page, hash);
    const bar = page.getByRole('navigation', { name: 'Board' });
    await expect(bar).toBeVisible();
    await expect(bar.getByRole('link', { name: 'More' })).toHaveAttribute('aria-current', 'page');
    await bar.getByRole('link', { name: 'Sessions' }).click();
    await expect(page).toHaveURL(/#\/$/);
    await expect(page.locator('a.fd-srow').first()).toBeVisible();
  });
}

test('More opens the help and the palette', async ({ page }) => {
  await start(page);
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  const links = page.getByRole('navigation', { name: 'More pages' });
  await links.getByRole('button', { name: 'Help' }).click();
  const help = page.getByRole('dialog').first();
  await expect(help).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await links.getByRole('button', { name: 'Search sessions' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('Back of a session returns to the Fleet page, or home on a deep link', async ({ page }) => {
  await start(page);
  await openFleet(page);
  await page.locator('a.fd-card').first().click();
  await expect(page).toHaveURL(/#\/s\//);
  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page).toHaveURL(/#\/fleet$/);

  // After a reload the page has no navigation of its own: Back goes home.
  await page.reload();
  await expect(page).toHaveURL(/#\/fleet$/);
  await go(page, `#/s/${ALPHA}/s2`);
  await page.reload();
  await expect(page.getByRole('button', { name: 'Back' })).toBeVisible();
  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page).toHaveURL(/#\/$/);
});

test('Back of a session goes home after the browser Back returned to the first entry', async ({
  page,
}) => {
  await start(page);
  await go(page, `#/s/${ALPHA}/s2`);
  // A reload makes the session the first entry of this board start.
  await page.reload();
  await expect(page.getByRole('button', { name: 'Back' })).toBeVisible();
  await go(page, '#/fleet');
  await page.goBack();
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s2$`));
  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page).toHaveURL(/#\/$/);
  await expect(page.locator('a.fd-srow').first()).toBeVisible();
});

test('the session menu fits the screen', async ({ page }) => {
  await start(page);
  await go(page, `#/s/${ALPHA}/s2`);
  const summary = page.locator('summary[aria-label="Session menu"]');
  await summary.click();
  const body = page.locator('.fd-menu[open] > .fd-menu-body');
  await expect(body).toBeVisible();
  const box = await body.boundingBox();
  const height = page.viewportSize()?.height ?? 0;
  expect(box).not.toBeNull();
  expect((box?.y ?? 0) + (box?.height ?? 0)).toBeLessThanOrEqual(height);
  const overflow = await body.evaluate((el) => getComputedStyle(el).overflowY);
  expect(overflow).toBe('auto');
});

test('the tablet drawer backdrop covers the screen and a tap closes the drawer', async ({
  page,
}) => {
  await start(page);
  await page.setViewportSize({ width: 800, height: 1000 });
  await go(page, '#/fleet');
  await page.getByRole('button', { name: 'Sessions', exact: true }).first().click();
  await expect(page.getByRole('dialog', { name: 'Sessions' })).toBeVisible();
  const backdrop = page.locator('.fd-drawer-backdrop');
  const box = await backdrop.boundingBox();
  expect(box?.height).toBe(1000);
  expect(box?.width).toBe(800);
  await page.mouse.click(790, 900);
  await expect(page.getByRole('dialog', { name: 'Sessions' })).toHaveCount(0);
});

test('the pairing dialog closes on Escape and on an outside click', async ({ page }) => {
  await start(page);
  await go(page, '#/stations');
  const add = page.getByRole('button', { name: 'Add station' });
  await add.click();
  await expect(page.getByRole('dialog', { name: 'Add station' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);

  await add.click();
  await expect(page.getByRole('dialog', { name: 'Add station' })).toBeVisible();
  await page.mouse.click(5, 5);
  await expect(page.getByRole('dialog')).toHaveCount(0);
});
