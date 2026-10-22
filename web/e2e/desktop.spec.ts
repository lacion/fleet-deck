// DL-140: the desktop layout (1440x900): the session sidebar, the session pane
// with its tabs, the keyboard (j/k, Ctrl+K, ?, arrows on the tabs).
import { expect, type Page, test } from '@playwright/test';
import type { Event } from '../src/gen';
import { envelopes } from '../src/testing/timeline';
import { logIn } from './fake-deck';
import { ALPHA, BETA, NOW, threeSessionDeck, threeSessions } from './scenes';
import { summary } from '../src/testing/summary';

async function setUp(page: Page) {
  const deck = threeSessionDeck();
  await deck.install(page);
  await logIn(page);
  const sidebar = page.getByRole('complementary', { name: 'Sessions' });
  await expect(sidebar).toBeVisible();
  return { deck, sidebar };
}

test('the sidebar lists the needs-you group first, then the sessions under their stations', async ({
  page,
}) => {
  const { sidebar } = await setUp(page);
  await expect(sidebar.getByRole('link', { name: /^Fleet/ })).toContainText('1');
  const needs = sidebar.getByRole('region', { name: 'Needs you' });
  await expect(needs.getByRole('link')).toHaveCount(1);
  await expect(needs.getByRole('link')).toHaveAttribute('href', `#/s/${ALPHA}/s1`);

  const alpha = sidebar.getByRole('region', { name: 'Station Alpha' });
  await expect(alpha.getByRole('heading', { level: 3 })).toContainText('Alpha');
  // a session that needs you lists once, in "Needs you"; the station count is the rows shown
  // under it, and its title still counts every session of the station
  const alphaCount = alpha.getByRole('heading', { level: 3 }).locator('.fd-count');
  await expect(alphaCount).toHaveText('1');
  await expect(alphaCount).toHaveAttribute('title', '2 sessions on Alpha');
  await expect(alpha.getByRole('link')).toHaveCount(1);
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
  await sidebar.getByRole('region', { name: 'Needs you' }).getByRole('link').click();
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
  await sidebar.getByRole('region', { name: 'Needs you' }).getByRole('link').click();
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

test('? shows the keyboard and help dialog and Escape closes it', async ({ page }) => {
  await setUp(page);
  await page.keyboard.press('?');
  const help = page.getByRole('dialog', { name: 'Keyboard and help' });
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
  // closed by default: the Files tab is in the tab list, and the toggle opens the panel
  await expect(toggle).toHaveAttribute('aria-pressed', 'false');
  await expect(side).toHaveCount(0);
  await expect(pane.getByRole('tab', { name: 'Files' })).toBeVisible();
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(side).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem('fd-files-panel'))).toBe('1');
  await expect(pane.getByRole('tablist', { name: 'Session views' }).getByRole('tab')).toHaveText([
    'Chat',
    'Changes',
    'Artifacts',
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

test('a file name of a diff opens the file in the Files side panel, rendered or raw', async ({
  page,
}) => {
  const deck = threeSessionDeck();
  deck.on(`/api/stations/${ALPHA}/files/list`, (_body, query) => ({
    status: 200,
    body: { root: { session: 's2' }, path: query.get('path') ?? '', entries: [], truncated: false },
  }));
  deck.on(`/api/stations/${ALPHA}/sessions/s2/turns/t1/diff`, () => ({
    status: 200,
    body: {
      session_id: 's2',
      turn_id: 't1',
      from_commit: 't1-start',
      to_commit: 't1-end',
      summary: { files_changed: 1, insertions: 1, deletions: 0 },
      patch: 'diff --git a/docs/guide.md b/docs/guide.md\n+# Guide',
      truncated: false,
    },
  }));
  const reads: string[] = [];
  deck.on(`/api/stations/${ALPHA}/files/read`, (_body, query) => {
    const path = query.get('path') ?? '';
    reads.push(path);
    return {
      status: 200,
      body: { path, size: 7, binary: false, text: '# Guide', truncated: false },
    };
  });
  const finalized: Event = {
    kind: 'turn.finalized',
    turn_id: 't1',
    checkpoint: {
      worktree_id: 'wt',
      scope: 'tracked',
      start: { state: 'available', commit: 't1-start' },
      end: { state: 'available', commit: 't1-end' },
    },
    diff_summary: { files_changed: 1, insertions: 1, deletions: 0 },
  };
  await deck.install(page);
  await logIn(page);
  deck.emit(ALPHA, 's2', envelopes('s2', [finalized]));
  await page
    .getByRole('complementary', { name: 'Sessions' })
    .getByRole('region', { name: 'Station Alpha' })
    .getByRole('link', { name: /Write the docs/ })
    .click();
  const pane = page.getByRole('region', { name: 'Session' });
  const side = pane.getByRole('complementary', { name: 'Files panel' });
  // the panel is closed by default: open it
  await pane.getByRole('button', { name: 'Files' }).click();
  await pane.getByRole('tab', { name: /^Changes/ }).click();
  await pane.getByRole('button', { name: 'Show the diff' }).click();
  await pane.locator('[data-diff="t1"]').getByRole('button', { name: 'docs/guide.md' }).click();
  // the file opens in the side panel, in its folder
  await expect(side.locator('[data-file="docs/guide.md"]')).toBeVisible();
  await expect(side.locator('.fd-files-md h1')).toHaveText('Guide');
  await expect(side.getByRole('navigation', { name: 'Folder path' })).toContainText('docs');
  await expect(side.getByRole('button', { name: 'Copy the path' })).toBeVisible();
  expect(reads).toEqual(['docs/guide.md']);
  await side.getByRole('button', { name: 'Raw' }).click();
  await expect(side.locator('.fd-file-text')).toHaveText('# Guide');
  // the Changes tab stays: the click did not change the tab
  await expect(pane.getByRole('tab', { name: /^Changes/ })).toHaveAttribute(
    'aria-selected',
    'true',
  );
});

test('the sidebar footer has the theme and compact switches', async ({ page }) => {
  const { sidebar } = await setUp(page);
  const theme = sidebar.getByRole('group', { name: 'Theme' });
  await theme.getByRole('button', { name: 'Light theme' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await theme.getByRole('button', { name: 'Dark theme' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await sidebar.getByRole('button', { name: 'Compact cards' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-density', 'compact');
  await expect(sidebar.locator('.fd-srow-meta').first()).toBeHidden();
});

// The panel request (`panelRequest`, tab Mail & tasks) is read by the fleet home of worker H;
// this spec asserts the route that the key opens.
test('c opens the fleet page', async ({ page }) => {
  await setUp(page);
  await page.keyboard.press('c');
  await expect(page).toHaveURL(/#\/fleet$/);
});

test('a starred session sorts first and ended sessions are cleared into Archived', async ({
  page,
}) => {
  const deck = threeSessionDeck();
  const done = (id: string, title: string, status: 'ended' | 'lost') => ({
    station_id: ALPHA,
    status,
    summary: summary(id, 'idle', NOW - 5, { title, state: 'exited' }),
  });
  deck.sessions = [
    ...threeSessions(),
    done('s4', 'Old job', 'ended'),
    done('s5', 'Lost job', 'lost'),
  ];
  await page.addInitScript(() => {
    localStorage.setItem('fd-priority', JSON.stringify(['st-alpha/s2']));
  });
  await deck.install(page);
  await logIn(page);
  const sidebar = page.getByRole('complementary', { name: 'Sessions' });
  const alpha = sidebar.getByRole('region', { name: 'Station Alpha' });
  // The needs-you session ranks first by status unless starred; the star puts s2 above it.
  await expect(alpha.locator('a.fd-srow').first()).toContainText('Write the docs');
  await expect(
    alpha.locator('a.fd-srow').first().getByRole('img', { name: 'Starred' }),
  ).toBeVisible();

  await alpha.locator('details.fd-sended > summary').click();
  await alpha.getByRole('button', { name: 'Archive the ended sessions of Alpha' }).click();
  await expect(alpha.locator('details.fd-sended')).toHaveCount(0);
  const toggle = sidebar.getByRole('button', { name: /^Archived \(2\)/ });
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  const archived = sidebar.getByRole('region', { name: 'Archived' });
  await expect(archived).toContainText('Old job');
  await expect(archived).toContainText('Lost job');
  expect(await page.evaluate(() => localStorage.getItem('fd-archived-cache'))).toContain(
    'st-alpha/s4',
  );
});
