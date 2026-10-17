// R13 P2c (design §5F item 3, §5G item 3): the wall display page `/display` of the production
// bundle, against real processes (one deck and one station with the offline fake agents). Four
// proofs: a display link signs the wall in; the wall follows the directive of the owner and its own
// keys; a kiosk that shows a pairing code signs in when the owner types the code; and a reload at
// `/display` keeps the wall. The last step revokes a display and its page asks for a new pairing.
//
// The owner uses the board in one context. Each wall has its own context, so the cookies stay
// apart, as the kiosk profile does on a desktop.
import { type BrowserContext, expect, type Page, test } from '@playwright/test';
import { Deck, Station } from './fleet';

/** Long enough for the deck; set with `fleetdeck deck password`. */
const PASSWORD = 'correct horse battery staple e2e';

test.describe.configure({ mode: 'serial' });

let deck: Deck;
let station: Station;
let origin: string;
let owner: BrowserContext;
let ownerPage: Page;
const walls: BrowserContext[] = [];
let firstWall: Page;
let displayId = '';

test.beforeAll(async ({ browser }) => {
  deck = await Deck.create();
  station = new Station('alpha');
  await deck.start();
  await station.start();
  // The station dials the deck with a one-time invitation.
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
});

test.afterAll(async () => {
  for (const context of [...walls, owner]) await context?.close();
  const errors: unknown[] = [];
  for (const node of [station, deck]) {
    try {
      await node?.dispose();
    } catch (error) {
      errors.push(error);
    }
  }
  if (errors.length > 0) throw errors[0];
});

async function newWall(browser: import('@playwright/test').Browser): Promise<Page> {
  const context = await browser.newContext({
    viewport: { width: 2560, height: 1440 },
    ignoreHTTPSErrors: true,
  });
  walls.push(context);
  return context.newPage();
}

const wallOf = (page: Page) => page.getByTestId('wall');

/** The owner page at Settings, Displays. */
async function displaysPage(): Promise<void> {
  await ownerPage.evaluate(() => {
    window.location.hash = '#/settings/displays';
  });
  await expect(ownerPage.getByRole('heading', { name: 'Add a wall display' })).toBeVisible();
}

/** Types the owner password into the "Confirm it is you" dialog. */
async function confirmIt(): Promise<void> {
  const dialog = ownerPage.getByRole('dialog', { name: 'Confirm it is you' });
  await dialog.getByLabel('Password').fill(PASSWORD);
  await dialog.getByRole('button', { name: 'Confirm' }).click();
}

/** The owner's directive without a UI control (the board sends one from a session menu). */
async function sendDirective(body: Record<string, unknown>): Promise<number> {
  return ownerPage.evaluate(
    async ({ id, directive }) => {
      const session = await (await fetch('/api/auth/session')).json();
      const res = await fetch(`/api/displays/${id}/directive`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrf },
        body: JSON.stringify(directive),
      });
      return res.status;
    },
    { id: displayId, directive: body },
  );
}

test('a display link signs the wall in, and a reload at /display keeps it', async ({ browser }) => {
  await displaysPage();
  const add = ownerPage.getByRole('region', { name: 'Add a wall display' });
  await add.getByLabel('Name').fill('Wall one');
  await add.getByLabel('alpha').check();
  await add.getByRole('button', { name: 'Make a link' }).click();
  await confirmIt();
  const url = await add.locator('.fd-disp-url').innerText();
  expect(url).toContain('/display#display=');
  firstWall = await newWall(browser);
  await firstWall.goto(url);
  await expect(wallOf(firstWall)).toBeVisible();
  await expect(wallOf(firstWall)).toHaveAttribute('data-mode', 'mission');
  await expect(wallOf(firstWall)).toContainText('Fleet Deck wall');
  await expect(wallOf(firstWall).getByRole('status')).toHaveText('Live');
  await expect(wallOf(firstWall).locator('button, textarea, input, select, a')).toHaveCount(0);
  // The token is not in the address bar any more, and a reload keeps the login.
  expect(firstWall.url()).not.toContain('display=');
  await firstWall.reload();
  await expect(wallOf(firstWall)).toBeVisible();
  expect(new URL(firstWall.url()).pathname).toBe('/display');
  // The owner's list shows the display and the time of its first use.
  await expect(ownerPage.getByRole('heading', { name: 'Wall one' })).toBeVisible();
  const id = await ownerPage.locator('[data-display]').first().getAttribute('data-display');
  if (id === null) throw new Error('no display row');
  displayId = id;
});

test('the wall follows the directive and its own keys', async () => {
  expect(await sendDirective({ mode: 'ambient', pin: false, filter: null })).toBe(200);
  await expect(wallOf(firstWall)).toHaveAttribute('data-mode', 'ambient', { timeout: 15_000 });
  await ownerPage
    .locator(`[data-display="${displayId}"]`)
    .getByRole('button', { name: 'Show Mission' })
    .click();
  await expect(wallOf(firstWall)).toHaveAttribute('data-mode', 'mission', { timeout: 15_000 });
  // The keys of the wall only navigate: the next mode with a target is ambient.
  await firstWall.keyboard.press('ArrowRight');
  await expect(wallOf(firstWall)).toHaveAttribute('data-mode', 'ambient');
  await firstWall.keyboard.press('Escape');
  await expect(wallOf(firstWall)).toHaveAttribute('data-mode', 'mission');
});

test('a pairing code that the owner types signs a second wall in', async ({ browser }) => {
  const kiosk = await newWall(browser);
  await kiosk.goto(`${origin}/display#/display-pair`);
  const code = kiosk.getByLabel('Pairing code');
  await expect(code).toHaveText(/^\d{3} \d{3}$/);
  const digits = (await code.innerText()).replace(' ', '');
  await expect(kiosk.locator('button, input, textarea, select, a')).toHaveCount(0);
  await displaysPage();
  const pending = ownerPage.getByRole('region', { name: 'Waiting for a code' });
  // The list shows where the request came from, never the code itself.
  await expect(pending).toContainText('127.0.0.1', { timeout: 15_000 });
  await expect(pending).not.toContainText(digits);
  await pending.getByLabel('Name').fill('Wall two');
  await pending.getByLabel('alpha').check();
  await pending.getByLabel('Code on the wall screen').fill(digits);
  await pending.getByRole('button', { name: 'Approve' }).click();
  // The elevation of the first step may have ended; the dialog shows only then.
  const dialog = ownerPage.getByRole('dialog', { name: 'Confirm it is you' });
  if (await dialog.isVisible({ timeout: 2_000 }).catch(() => false)) await confirmIt();
  await expect(pending.getByText('Approved. The wall display signs in')).toBeVisible();
  await expect(wallOf(kiosk)).toBeVisible({ timeout: 20_000 });
  await kiosk.reload();
  await expect(wallOf(kiosk)).toBeVisible();
  expect(new URL(kiosk.url()).pathname).toBe('/display');
});

test('a revoked display loses its wall and asks for a new pairing', async () => {
  await displaysPage();
  const row = ownerPage.locator(`[data-display="${displayId}"]`);
  await row.getByRole('button', { name: 'Revoke' }).click();
  await row.getByRole('button', { name: 'Revoke' }).click();
  await expect(firstWall.getByLabel('Pairing code')).toBeVisible({ timeout: 30_000 });
  await expect(wallOf(firstWall)).toHaveCount(0);
});
