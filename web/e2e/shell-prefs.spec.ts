// DL-141 wave 6: the theme and compact switches, the `c` key and the first-run help.
import { expect, test } from '@playwright/test';
import { FakeDeck, logIn, stationView } from './fake-deck';

async function openMorePage(page: import('@playwright/test').Page) {
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  await expect(page.getByRole('region', { name: 'More' })).toBeVisible();
}

test('the theme buttons switch the page and the choice survives a reload', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  await deck.install(page);
  await logIn(page);
  await openMorePage(page);
  const theme = page.getByRole('group', { name: 'Theme' });
  const html = page.locator('html');

  await theme.getByRole('button', { name: 'Light theme' }).click();
  await expect(html).toHaveAttribute('data-theme', 'light');
  await expect(theme.getByRole('button', { name: 'Light theme' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await theme.getByRole('button', { name: 'Dark theme' }).click();
  await expect(html).toHaveAttribute('data-theme', 'dark');

  await theme.getByRole('button', { name: 'Light theme' }).click();
  await page.reload();
  await expect(html).toHaveAttribute('data-theme', 'light');
  expect(await page.evaluate(() => localStorage.getItem('fd-theme'))).toBe('light');

  await openMorePage(page);
  await page
    .getByRole('group', { name: 'Theme' })
    .getByRole('button', { name: 'System theme' })
    .click();
  expect(await page.evaluate(() => localStorage.getItem('fd-theme'))).toBe('system');
  await expect(html).toHaveAttribute('data-theme', /^(light|dark)$/);
});

test('the compact switch sets the density and survives a reload', async ({ page }) => {
  const deck = new FakeDeck();
  await deck.install(page);
  await logIn(page);
  await openMorePage(page);
  const compact = page.getByRole('button', { name: 'Compact cards' });
  await expect(compact).toHaveAttribute('aria-pressed', 'false');
  await compact.click();
  await expect(compact).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('html')).toHaveAttribute('data-density', 'compact');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-density', 'compact');
  await openMorePage(page);
  await page.getByRole('button', { name: 'Compact cards' }).click();
  await expect(page.locator('html')).not.toHaveAttribute('data-density', 'compact');
});

test('c opens the fleet page', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  await deck.install(page);
  await logIn(page);
  // The key listener lives in the board: wait for it before the key press.
  await expect(page.getByRole('navigation', { name: 'Board' })).toBeVisible();
  await expect(page).not.toHaveURL(/#\/fleet$/);
  await page.keyboard.press('c');
  await expect(page).toHaveURL(/#\/fleet$/);
});

test('a first visit shows the help notice once; the help dialog does not open by itself', async ({
  page,
}) => {
  const deck = new FakeDeck();
  await deck.install(page, { firstRun: true });
  await logIn(page);
  const notice = page.getByText('Press ? for the keys and a short guide.');
  await expect(notice).toBeVisible();
  const help = page.getByRole('dialog', { name: 'Keyboard and help' });
  await expect(help).toHaveCount(0);
  await page.getByRole('button', { name: 'Dismiss the help notice' }).click();
  await expect(notice).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem('fd-help-seen'))).toBe('1');
  await page.reload();
  await expect(page.getByRole('navigation', { name: 'Board' })).toBeVisible();
  await expect(notice).toHaveCount(0);
  await expect(help).toHaveCount(0);
});
