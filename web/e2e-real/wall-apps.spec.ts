// Wall apps (design `docs/v2/design/wall-control.md` §3.4, C2): a wall display runs an HTML artifact
// as an app against real processes (one deck and one station with the offline fake agents) and the
// production bundle. With "Allow apps on this wall" on and `app: true`, the frame runs the script
// and the wall host answers `ui/initialize`. Without the setting, the same file stays inert.
// The wall has its own browser context (a kiosk profile).
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { type BrowserContext, expect, type Page, test } from '@playwright/test';
import { Deck, Station } from './fleet';

/** Long enough for the deck; set with `fleetdeck deck password`. */
const PASSWORD = 'correct horse battery staple e2e';

/** The app: its script writes "app ran", asks `ui/initialize` and writes " initialized" after the
 * answer. Without a script the body stays "static". */
const APP_HTML = `<!doctype html>
<html><body><p>static</p>
<script>
document.body.textContent = 'app ran';
window.addEventListener('message', (ev) => {
  const m = ev.data;
  if (m && m.id === 1 && m.result) document.body.textContent += ' initialized';
});
window.parent.postMessage({ jsonrpc: '2.0', id: 1, method: 'ui/initialize', params: {} }, '*');
</script></body></html>
`;

test.describe.configure({ mode: 'serial' });

let deck: Deck;
let station: Station;
let owner: BrowserContext;
let ownerPage: Page;
let wallContext: BrowserContext;
let wall: Page;
let displayId = '';
let artifact: { station_id: string; artifact_id: string; version: number };

test.beforeAll(async ({ browser }) => {
  deck = await Deck.create();
  station = new Station('alpha');
  await deck.start();
  await station.start();
  station.cli(['station', 'join', '--stdin'], deck.cli(['deck', 'invite']));
  deck.cli(['deck', 'password', '--stdin'], `${PASSWORD}\n`);
  const link = deck.loginLink();
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
  if (errors.length > 0) throw errors[0];
});

const wallOf = (page: Page) => page.getByTestId('wall');

/** The owner page at Settings, Displays. */
async function displaysPage(): Promise<void> {
  await ownerPage.evaluate(() => {
    window.location.hash = '#/settings/displays';
  });
  await expect(ownerPage.getByRole('heading', { name: 'Wall displays' })).toBeVisible();
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

/** Turns "Allow apps on this wall" on or off in the display row and saves. */
async function allowApps(on: boolean): Promise<void> {
  await displaysPage();
  const row = ownerPage.locator(`[data-display="${displayId}"]`);
  const toggle = row.getByRole('switch', { name: 'Allow apps on this wall' });
  if (on) await toggle.check();
  else await toggle.uncheck();
  await row.getByRole('button', { name: 'Save' }).click();
  await confirmIfAsked();
  await expect(row.getByText('Saved.')).toBeVisible();
}

/** The owner's artifact directive for the app file (a setting change resets the directive). */
async function sendAppDirective(): Promise<number> {
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
    {
      id: displayId,
      directive: { mode: 'artifact', artifact, app: true, pin: true, filter: null },
    },
  );
}

test('setup: a display signs in and the app file is published', async () => {
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
  // The file sits in the station workspace (inside `allowed_roots`).
  const path = join(station.workspace, 'wall-app.html');
  writeFileSync(path, APP_HTML);
  const published = await ownerPage.evaluate(async (file) => {
    const session = await (await fetch('/api/auth/session')).json();
    const res = await fetch('/api/artifacts/publish', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrf },
      body: JSON.stringify({
        station: 'alpha',
        session_id: null,
        path: file,
        title: 'Wall app',
        artifact_id: null,
      }),
    });
    return { status: res.status, body: await res.json() };
  }, path);
  expect(published.status).toBe(200);
  artifact = {
    station_id: published.body.artifact.station_id,
    artifact_id: published.body.artifact.artifact_id,
    version: published.body.version.version,
  };
});

test('an_app_runs_and_initializes_in_the_wall_frame', async () => {
  await allowApps(true);
  expect(await sendAppDirective()).toBe(200);
  await expect(wallOf(wall)).toHaveAttribute('data-mode', 'artifact', { timeout: 20_000 });
  const frame = wall.frameLocator('iframe[title="Wall app"]');
  await expect(frame.locator('body')).toHaveText('app ran initialized', { timeout: 20_000 });
});

test('without_allow_apps_the_same_file_stays_inert', async () => {
  await allowApps(false);
  expect(await sendAppDirective()).toBe(200);
  await expect(wallOf(wall)).toHaveAttribute('data-mode', 'artifact', { timeout: 20_000 });
  const frame = wall.frameLocator('iframe[title="Artifact"]');
  await expect(frame.locator('body')).toContainText('static', { timeout: 20_000 });
  await expect(frame.locator('body')).not.toContainText('app ran');
});
