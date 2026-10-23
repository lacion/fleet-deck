// B17 §3.2 (design `docs/v2/design/wall-control.md`, B3): wall pages against real processes (one deck
// and one station with the offline fake agents) and the production bundle. The owner adds pages in
// Settings, Displays, and shows one on a wall; the wall shows it in a credentialless, sandboxed frame.
// The wall has its own browser context (a kiosk profile). `pages.ts` starts the page servers P and N.
import { type BrowserContext, expect, type Page, test } from '@playwright/test';
import { Deck, Station } from './fleet';
import { N_TEXT, type Pages, P_TEXT, startPages } from './pages';

/** Long enough for the deck; set with `fleetdeck deck password`. */
const PASSWORD = 'correct horse battery staple e2e';

test.describe.configure({ mode: 'serial' });

let deck: Deck;
let station: Station;
let origin: string;
let owner: BrowserContext;
let ownerPage: Page;
let wallContext: BrowserContext;
let wall: Page;
let pages: Pages;
let displayId = '';

test.beforeAll(async ({ browser }) => {
  pages = await startPages();
  deck = await Deck.create();
  station = new Station('alpha');
  await deck.start();
  await station.start();
  station.cli(['station', 'join', '--stdin'], deck.cli(['deck', 'invite']));
  deck.cli(['deck', 'password', '--stdin'], `${PASSWORD}\n`);
  const link = deck.loginLink();
  origin = new URL(link).origin;
  owner = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    ignoreHTTPSErrors: true,
  });
  await owner.addInitScript(() => localStorage.setItem('fd-help-seen', '1'));
  ownerPage = await owner.newPage();
  await ownerPage.goto(link);
  wallContext = await browser.newContext({
    viewport: { width: 2560, height: 1440 },
    ignoreHTTPSErrors: true,
  });
  wall = await wallContext.newPage();
});

test.afterAll(async () => {
  for (const context of [wallContext, owner]) await context?.close();
  const errors: unknown[] = [];
  for (const node of [station, deck]) {
    try {
      await node?.dispose();
    } catch (error) {
      errors.push(error);
    }
  }
  await pages?.close();
  if (errors.length > 0) throw errors[0];
});

const wallOf = (page: Page) => page.getByTestId('wall');

/** The owner page at Settings, Displays. */
async function displaysPage(): Promise<void> {
  await ownerPage.evaluate(() => {
    window.location.hash = '#/settings/displays';
  });
  await expect(ownerPage.getByRole('heading', { name: 'Wall displays' })).toBeVisible();
  await expect(ownerPage.getByRole('heading', { name: 'Wall pages' })).toBeVisible();
}

/** Types the owner password into the "Confirm it is you" dialog, when it shows. */
async function confirmIfAsked(): Promise<void> {
  const dialog = ownerPage.getByRole('dialog', { name: 'Confirm it is you' });
  const asked = await dialog.waitFor({ state: 'visible', timeout: 2_000 }).then(
    () => true,
    () => false,
  );
  if (!asked) return;
  await dialog.getByLabel('Password').fill(PASSWORD);
  await dialog.getByRole('button', { name: 'Confirm' }).click();
}

const pagesPanel = () => ownerPage.getByRole('region', { name: 'Wall pages' });

/** Fills the form of the Wall pages section and presses "Add the page". */
async function submitPage(name: string, url: string, fit: 'screen' | 'long' = 'screen') {
  const panel = pagesPanel();
  await panel.getByLabel('Name').fill(name);
  await panel.getByLabel('Address').fill(url);
  await panel.getByLabel(fit === 'screen' ? 'Fits one screen' : 'Long page (scrolls)').check();
  await panel.getByRole('button', { name: 'Add the page' }).click();
}

/** Adds a page that the deck accepts: the list shows its name. */
async function addPage(name: string, url: string): Promise<void> {
  await submitPage(name, url);
  await confirmIfAsked();
  await expect(pagesPanel().locator('.fd-wp-row', { hasText: name })).toBeVisible();
}

/** "Show a page…" from the display row menu, then the dialog. */
async function showPage(name: string): Promise<void> {
  const row = ownerPage.locator(`[data-display="${displayId}"]`);
  await row.locator('summary[aria-label="Display actions"]').click();
  await row.getByRole('button', { name: 'Show a page…' }).click();
  const dialog = ownerPage.getByRole('dialog', { name: /^Show a page on / });
  await dialog.getByLabel('Page', { exact: true }).selectOption({ label: name });
  await dialog.getByRole('button', { name: 'Show the page' }).click();
  await expect(dialog.getByText('The wall shows the page.')).toBeVisible();
  await dialog.getByRole('button', { name: 'Close' }).click();
}

const frameOf = (page: Page) => page.frameLocator('iframe.fd-wall-pageframe');

test('setup: a display link signs the wall in', async () => {
  await displaysPage();
  await ownerPage.getByRole('button', { name: 'Add a wall display' }).first().click();
  const add = ownerPage.getByRole('region', { name: 'Add a wall display' });
  await add.getByLabel('Name').fill('Wall one');
  await add.getByLabel('alpha').check();
  await add.getByRole('button', { name: 'Make a pairing link' }).click();
  await confirmIfAsked();
  const url = await add.locator('.fd-disp-url').innerText();
  await wall.goto(url);
  await expect(wallOf(wall)).toBeVisible();
  const id = await ownerPage.locator('[data-display]').first().getAttribute('data-display');
  if (id === null) throw new Error('no display row');
  displayId = id;
  await displaysPage();
});

test('an_https_page_shows_in_the_wall_frame', async () => {
  await addPage('Page P', pages.p.httpsUrl);
  await showPage('Page P');
  await expect(wallOf(wall)).toHaveAttribute('data-mode', 'page', { timeout: 20_000 });
  // The document was loaded again with the generation of the directive (the new document has the
  // origin of the page in its policy).
  await expect(wall).toHaveURL(/\/display\?page_gen=\d+/, { timeout: 20_000 });
  await expect(frameOf(wall).getByText(P_TEXT)).toBeVisible({ timeout: 20_000 });
});

test('an_http_loopback_page_shows_in_the_wall_frame', async () => {
  const http = pages.p.httpUrl;
  if (http === null) throw new Error('no http server');
  await displaysPage();
  await addPage('Page P http', http);
  await showPage('Page P http');
  await expect(frameOf(wall).getByText(P_TEXT)).toBeVisible({ timeout: 20_000 });
});

test('the_deck_refuses_http_for_a_private_tailnet_or_local_host', async () => {
  await displaysPage();
  const panel = pagesPanel();
  for (const address of ['http://192.168.1.20:3000', 'http://100.100.1.1/', 'http://nas.local/']) {
    await panel.getByLabel('Name').fill('Private');
    await panel.getByLabel('Address').fill(address);
    await panel.getByRole('button', { name: 'Add the page' }).click();
    await expect(
      panel.getByText(
        'Use https for this address. A wall blocks plain http pages, except pages of its own machine (localhost).',
      ),
    ).toBeVisible();
  }
  await expect(panel.locator('.fd-wp-row', { hasText: 'Private' })).toHaveCount(0);
});

test('the_page_and_its_nested_frame_get_no_deck_cookie', async () => {
  // The wall holds the display login for this host, so the check means something.
  const held = (await wallContext.cookies(origin)).map((c) => c.name);
  expect(held).toContain('fd_display');
  await expect(frameOf(wall).getByText(P_TEXT)).toBeVisible();
  expect(pages.p.requests()).toBeGreaterThan(0);
  expect(pages.n.requests()).toBeGreaterThan(0);
  for (const cookie of [...pages.p.cookies, ...pages.n.cookies]) {
    expect(cookie).not.toContain('fd_display');
    expect(cookie).not.toContain('fd_session');
  }
});

test('a_nested_frame_of_another_origin_cannot_open_a_window_or_move_the_wall', async () => {
  await expect.poll(() => pages.n.reports.length, { timeout: 30_000 }).toBeGreaterThan(0);
  for (const report of pages.n.reports) {
    expect(report).toEqual({ open: 'null', top: 'blocked' });
  }
  // The nested page is visible inside the page, and the wall is still the wall.
  await expect(
    frameOf(wall).frameLocator('iframe[title="nested"]').getByText(N_TEXT),
  ).toBeVisible();
  expect(new URL(wall.url()).pathname).toBe('/display');
  await expect(wallOf(wall)).toBeVisible();
  expect(wallContext.pages()).toHaveLength(1);
});

test('the_anonymous_display_document_has_no_page_origin', async ({ playwright }) => {
  const anonymous = await playwright.request.newContext({ ignoreHTTPSErrors: true });
  try {
    const res = await anonymous.get(`${origin}/display`);
    expect(res.status()).toBe(200);
    const csp = res.headers()['content-security-policy'] ?? '';
    expect(csp).not.toBe('');
    for (const url of [pages.p.httpsUrl, pages.p.httpUrl ?? '', pages.n.httpsUrl]) {
      expect(csp).not.toContain(new URL(url).origin);
    }
  } finally {
    await anonymous.dispose();
  }
});
