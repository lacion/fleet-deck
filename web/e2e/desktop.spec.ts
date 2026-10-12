// DL-140: the desktop layout (1440x900): the session sidebar, the session pane
// with its tabs, the keyboard (j/k, Ctrl+K, ?, arrows on the tabs).
import { expect, type Page, test } from '@playwright/test';
import { logIn } from './fake-deck';
import { ALPHA, BETA, threeSessionDeck } from './scenes';

async function setUp(page: Page) {
  const deck = threeSessionDeck();
  await deck.install(page);
  await logIn(page);
  const sidebar = page.getByRole('complementary', { name: 'Sessions' });
  await expect(sidebar).toBeVisible();
  return { deck, sidebar };
}

test('the sidebar lists the sessions under their stations and a Needs you group', async ({
  page,
}) => {
  const { sidebar } = await setUp(page);
  const needs = sidebar.getByRole('region', { name: 'Needs you' });
  await expect(needs.getByRole('heading', { level: 3 })).toHaveText('Needs you · 1');
  await expect(needs.getByRole('link')).toHaveCount(1);
  await expect(needs.getByRole('link')).toContainText('Review the repo');

  const alpha = sidebar.getByRole('region', { name: 'Station Alpha' });
  await expect(alpha.getByRole('heading', { level: 3 })).toContainText('Alpha');
  await expect(alpha.getByRole('link')).toHaveCount(2);
  await expect(alpha).toContainText('Review the repo');
  await expect(alpha).toContainText('Write the docs');
  const beta = sidebar.getByRole('region', { name: 'Station Beta' });
  await expect(beta.getByRole('heading', { level: 3 })).toContainText('Beta');
  await expect(beta.getByRole('link')).toHaveCount(1);
  await expect(beta).toContainText('Fix the build');
});

test('a click on a session opens its pane on the Chat tab', async ({ page }) => {
  const { sidebar } = await setUp(page);
  await sidebar.getByRole('region', { name: 'Station Beta' }).getByRole('link').click();
  await expect(page).toHaveURL(new RegExp(`#/s/${BETA}/s3$`));
  const pane = page.getByRole('region', { name: 'Session' });
  await expect(pane.getByRole('heading', { level: 2 })).toHaveText('Fix the build');
  const tabs = pane.getByRole('tablist', { name: 'Session views' });
  await expect(tabs.getByRole('tab', { name: 'Chat' })).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.getByRole('tab', { name: 'Changes' })).toHaveAttribute(
    'aria-selected',
    'false',
  );
});

test('j and k step through the sessions in the sidebar order', async ({ page }) => {
  const { sidebar } = await setUp(page);
  const title = page.getByRole('region', { name: 'Session' }).getByRole('heading', { level: 2 });
  await sidebar.getByRole('region', { name: 'Station Alpha' }).getByRole('link').first().click();
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s1$`));
  await expect(title).toHaveText('Review the repo');
  // Focus is on a link, outside an input: the keys move between sessions.
  await page.keyboard.press('j');
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s2$`));
  await expect(title).toHaveText('Write the docs');
  await page.keyboard.press('j');
  await expect(page).toHaveURL(new RegExp(`#/s/${BETA}/s3$`));
  await page.keyboard.press('k');
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s2$`));
  await page.keyboard.press('k');
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s1$`));
});

test('j and k are plain text in an input', async ({ page }) => {
  const { sidebar } = await setUp(page);
  await sidebar.getByRole('region', { name: 'Station Alpha' }).getByRole('link').first().click();
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s1$`));
  const search = sidebar.getByRole('searchbox', { name: 'Search sessions' });
  await search.click();
  await page.keyboard.type('jk');
  await expect(search).toHaveValue('jk');
  await expect(page).toHaveURL(new RegExp(`#/s/${ALPHA}/s1$`));
});

test('Ctrl+K opens the palette, typing filters and Enter opens the match', async ({ page }) => {
  await setUp(page);
  await page.keyboard.press('Control+k');
  const palette = page.getByRole('dialog', { name: 'Go to session' });
  await expect(palette).toBeVisible();
  const box = palette.getByRole('combobox', { name: 'Session name' });
  await expect(box).toBeFocused();
  await expect(palette.getByRole('option')).toHaveCount(3);
  await page.keyboard.type('build');
  await expect(palette.getByRole('option')).toHaveCount(1);
  await expect(palette.getByRole('option')).toContainText('Fix the build');
  await expect(palette.getByRole('option')).toContainText('Beta');
  await page.keyboard.press('Enter');
  await expect(palette).toHaveCount(0);
  await expect(page).toHaveURL(new RegExp(`#/s/${BETA}/s3$`));
  await expect(
    page.getByRole('region', { name: 'Session' }).getByRole('heading', { level: 2 }),
  ).toHaveText('Fix the build');
});

test('? shows the keyboard shortcuts and Escape closes them', async ({ page }) => {
  await setUp(page);
  await page.keyboard.press('?');
  const help = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
  await expect(help).toBeVisible();
  await expect(help).toContainText('Next session');
  await page.keyboard.press('Escape');
  await expect(help).toHaveCount(0);
});

test('the Changes tab is reached with the arrow keys', async ({ page }) => {
  const { sidebar } = await setUp(page);
  await sidebar.getByRole('region', { name: 'Station Beta' }).getByRole('link').click();
  const tabs = page.getByRole('tablist', { name: 'Session views' });
  const chat = tabs.getByRole('tab', { name: 'Chat' });
  await chat.focus();
  await page.keyboard.press('ArrowRight');
  const changes = tabs.getByRole('tab', { name: 'Changes' });
  await expect(changes).toBeFocused();
  await expect(changes).toHaveAttribute('aria-selected', 'true');
  await expect(page).toHaveURL(new RegExp(`#/s/${BETA}/s3\\?tab=changes$`));
  await expect(page.getByText('No finished turns yet.')).toBeVisible();
  await page.keyboard.press('ArrowLeft');
  await expect(chat).toBeFocused();
  await expect(chat).toHaveAttribute('aria-selected', 'true');
});
