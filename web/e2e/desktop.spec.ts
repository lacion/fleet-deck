// DL-140: the desktop layout (1440x900): the session sidebar, the session pane
// with its tabs, the keyboard (j/k, Ctrl+K, ?, arrows on the tabs).
import { expect, type Page, test } from '@playwright/test';
import { logIn } from './fake-deck';
import { ALPHA, BETA, threeSessionDeck, threeSessions } from './scenes';

async function setUp(page: Page) {
  const deck = threeSessionDeck();
  await deck.install(page);
  await logIn(page);
  const sidebar = page.getByRole('complementary', { name: 'Sessions' });
  await expect(sidebar).toBeVisible();
  return { deck, sidebar };
}

test('the sidebar lists the sessions under their stations and a needs-you chip', async ({
  page,
}) => {
  const { sidebar } = await setUp(page);
  // no group of its own: a chip links to the first session that needs you
  await expect(sidebar.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  const chip = sidebar.getByRole('link', { name: '1 needs you' });
  await expect(chip).toHaveAttribute('href', `#/s/${ALPHA}/s1`);

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

test('the Files side panel lists the root at once, opens folders, and its toggle closes it', async ({
  page,
}) => {
  const deck = threeSessionDeck();
  const lists: string[] = [];
  deck.on(`/api/stations/${ALPHA}/files/list`, (_body, query) => {
    const path = query.get('path') ?? '';
    lists.push(path);
    const entries =
      path === ''
        ? [
            { name: 'docs', kind: 'dir', size: null },
            { name: 'a.txt', kind: 'file', size: 4 },
          ]
        : [{ name: 'guide.md', kind: 'file', size: 6 }];
    return { status: 200, body: { root: { session: 's2' }, path, entries, truncated: false } };
  });
  await deck.install(page);
  await logIn(page);
  await page
    .getByRole('complementary', { name: 'Sessions' })
    .getByRole('region', { name: 'Station Alpha' })
    .getByRole('link', { name: /Write the docs/ })
    .click();
  const pane = page.getByRole('region', { name: 'Session' });
  const toggle = pane.getByRole('button', { name: 'Files' });
  const side = pane.getByRole('complementary', { name: 'Files panel' });
  // open by default; the Files tab is not in the tab list while the panel is open
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(side).toBeVisible();
  await expect(pane.getByRole('tablist', { name: 'Session views' }).getByRole('tab')).toHaveText([
    'Chat',
    'Changes',
  ]);
  // the root listing loads at once, once
  await expect(side.getByRole('button', { name: 'docs/' })).toBeVisible();
  expect(lists).toEqual(['']);
  // a new summary of the session re-renders the pane but does not list again
  const docs = threeSessions()[1];
  if (docs === undefined) throw new Error('the scene has no second session');
  deck.setSession({
    ...docs,
    summary: { ...docs.summary, title: 'Write the docs now', updated_at: 2 },
  });
  await expect(pane.getByRole('heading', { level: 2 })).toHaveText('Write the docs now');
  expect(lists).toEqual(['']);

  await side.getByRole('button', { name: 'docs/' }).click();
  await expect(side.getByRole('button', { name: /^guide\.md/ })).toBeVisible();
  const crumbs = side.getByRole('navigation', { name: 'Folder path' });
  await expect(crumbs.getByRole('button', { name: 'docs' })).toBeVisible();
  await crumbs.getByRole('button', { name: 'root' }).click();
  await expect(side.getByRole('button', { name: 'docs/' })).toBeVisible();
  expect(lists).toEqual(['', 'docs', '']);

  // the toggle closes the panel, the Files tab takes its place, and the choice is kept
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-pressed', 'false');
  await expect(side).toHaveCount(0);
  await expect(pane.getByRole('tab', { name: 'Files' })).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem('fd-files-panel'))).toBe('0');
  await page.reload();
  await expect(pane.getByRole('button', { name: 'Files' })).toHaveAttribute(
    'aria-pressed',
    'false',
  );
  await expect(pane.getByRole('complementary', { name: 'Files panel' })).toHaveCount(0);
  await pane.getByRole('button', { name: 'Files' }).click();
  await expect(pane.getByRole('complementary', { name: 'Files panel' })).toBeVisible();
});
