// M2 P4-5: the MVP flows on a phone viewport against real processes. One
// `fleetdeck deck serve` and two `fleetdeck station serve` processes run on
// temporary nodes; `alpha` listens and the deck dials it, `bravo` dials the
// deck. Both stations run the offline `fake-claude sim` agent.
//
// The steps share one fleet and one phone page, in order: pairing from the
// board, spawn on each station, an approval from the needs-you rail, steer and
// interrupt, a deck restart, a phone network drop, and a station link drop.
import {
  type BrowserContext,
  expect,
  type Locator,
  type Page,
  test,
  type WebSocketRoute,
} from '@playwright/test';
import { BRANCH, Deck, PROVIDER, Station } from './fleet';

test.describe.configure({ mode: 'serial' });

let deck: Deck;
let alpha: Station;
let bravo: Station;
let context: BrowserContext;
let page: Page;
/** The phone's board sockets. Chromium's offline mode stops new requests but
 * keeps open sockets, so the network-drop step closes them itself. */
const sockets = new Set<WebSocketRoute>();

test.beforeAll(async ({ browser }) => {
  deck = await Deck.create();
  alpha = new Station('alpha');
  bravo = new Station('bravo');
  await deck.start();
  await alpha.start();
  await bravo.start();
  context = await browser.newContext({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    ignoreHTTPSErrors: true,
  });
  await context.routeWebSocket('**/api/ws', (ws) => {
    const server = ws.connectToServer();
    sockets.add(ws);
    ws.onClose(() => {
      sockets.delete(ws);
      void server.close();
    });
    server.onClose(() => {
      sockets.delete(ws);
      void ws.close();
    });
  });
  page = await context.newPage();
});

test.afterAll(async () => {
  await context?.close();
  for (const node of [bravo, alpha, deck]) await node?.dispose();
});

const status = (text: string) => page.getByRole('status').filter({ hasText: text });
const live = () => expect(status('Connected · Live fleet')).toBeVisible();
const card = (sessionId: string) => page.locator(`.fd-card[data-session="${sessionId}"]`);
const stationCard = (name: string) =>
  page.locator('.fd-station[data-station]').filter({ has: page.getByRole('heading', { name }) });

/** The page fits the phone width: no horizontal scroll. */
async function fitsWidth(): Promise<boolean> {
  return page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
}

async function fleet() {
  await page.getByRole('button', { name: 'Fleet', exact: true }).click();
}

/** Starts a session from the spawn form and returns its id. */
async function spawn(station: Station, title: string, prompt: string): Promise<string> {
  await page.getByRole('button', { name: 'New session' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form
    .getByRole('combobox', { name: 'Station', exact: true })
    .selectOption({ label: station.name });
  await expect(form.getByLabel('Provider')).toContainText(`${PROVIDER} · claude · 2.1.999`);
  await form.getByRole('button', { name: 'Browse the station' }).click();
  // The root list shows the station's absolute allowed roots.
  await form.getByRole('button', { name: `Open ${station.home}/projects` }).click();
  await expect(form.getByRole('button', { name: 'Open demo' })).toContainText('git');
  await form.getByRole('button', { name: 'Open demo' }).click();
  await form.getByRole('button', { name: 'Use this directory' }).click();
  await expect(form).toContainText(`Chosen: ${station.workspace}`);
  await form.getByLabel('Title (optional)').fill(title);
  await form.getByLabel('First prompt (optional)').fill(prompt);
  expect(await fitsWidth()).toBe(true);
  await form.getByRole('button', { name: 'Start session' }).click();
  await expect(status('Done.')).toBeVisible();
  await fleet();
  const title_ = page.locator('.fd-card').filter({ hasText: title });
  await expect(title_).toHaveCount(1);
  const id = await title_.getAttribute('data-session');
  if (id === null) throw new Error(`no card for ${title}`);
  return id;
}

async function open(title: string): Promise<Locator> {
  await fleet();
  await page.getByRole('button', { name: `Open session ${title}` }).click();
  return page.getByRole('main', { name: 'Session' });
}

/** The deck's command rows: the expected count per method, every one done
 * after one delivery with a distinct command id and an accepted receipt. */
function expectRouted(expected: Record<string, number>) {
  const rows = deck.commands();
  const counts: Record<string, number> = {};
  for (const row of rows) counts[row.method] = (counts[row.method] ?? 0) + 1;
  expect(counts).toEqual(expected);
  expect(new Set(rows.map((row) => row.command_id)).size).toBe(rows.length);
  for (const row of rows)
    expect([row.method, row.state, row.attempts]).toEqual([row.method, 'done', 1]);
}

/** The station's own session list (the station is the source of truth). */
function stationSessions(station: Station): { session_id: string }[] {
  return JSON.parse(station.cli(['station', 'sessions', '--json']));
}

test('the board logs in and pairs a dialing and a listening station', async () => {
  await page.goto(deck.loginLink());
  await live();
  expect(new URL(page.url()).hash).toBe('');
  await page.getByRole('button', { name: 'Stations (0)' }).click();

  // Bravo has no listener: the deck makes an invitation and bravo dials.
  await page.getByRole('button', { name: 'Add station' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add station' });
  await dialog.getByRole('button', { name: 'Create invitation' }).click();
  const invitation = dialog.getByLabel('Invitation (secret, single use)');
  await expect(invitation).not.toHaveValue('');
  bravo.cli(['station', 'join', '--stdin'], await invitation.inputValue());

  // Alpha listens: it makes an invitation and the deck dials it.
  await dialog.getByLabel('This deck dials the station').check();
  await dialog
    .getByLabel('Station invitation')
    .fill(alpha.cli(['station', 'invite', '--listen', '127.0.0.1:0']));
  await dialog.getByRole('button', { name: 'Pair station' }).click();
  await expect(dialog.getByRole('status')).toHaveText('Paired station alpha.');
  await dialog.getByRole('button', { name: 'Close' }).click();

  await expect(page.getByRole('button', { name: 'Stations (2)' })).toBeVisible();
  for (const name of ['alpha', 'bravo']) {
    const st = stationCard(name);
    await expect(st).toContainText('Link up');
    await expect(st).toContainText('0 live of 4 sessions');
    await expect(st).toContainText(`${PROVIDER} · claude · 2.1.999`);
  }
  await expect(stationCard('alpha')).toContainText('Deck dials station');
  await expect(stationCard('bravo')).toContainText('Station dials deck');
  expect(await fitsWidth()).toBe(true);
});

test('a session spawned on alpha asks for an approval that the needs-you rail answers', async () => {
  const id = await spawn(alpha, 'Alpha approval', 'ask echo hi');
  await expect(card(id)).toContainText(`Branch: ${BRANCH}`);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toContainText('Alpha approval · alpha: Bash');
  await rail.getByRole('button', { name: 'Answer Bash in Alpha approval' }).click();
  const needs = page.getByRole('region', { name: 'Needs you' });
  await expect(needs).toContainText('echo hi');
  expect(await fitsWidth()).toBe(true);
  await needs.getByRole('button', { name: 'Allow once' }).click();
  await expect(status('Done.')).toBeVisible();
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  const view = page.getByRole('main', { name: 'Session' });
  await expect(view).toContainText('Bash · echo hi · ok');
  await expect(view.locator('.fd-items')).toContainText('done');
  await expect(view).toContainText('Approval: Bash · Allow once (by board)');
  await fleet();
  await expect(rail).toContainText('Nothing waits for you.');
  expect(stationSessions(alpha).map((s) => s.session_id)).toEqual([id]);
});

test('a running turn on bravo takes a steer and an interrupt, then runs the steer', async () => {
  const id = await spawn(bravo, 'Bravo long run', 'wait for the signal');
  const view = await open('Bravo long run');
  await expect(view).toContainText('working');
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer.getByLabel('Message').fill('after the stop');
  await composer.getByRole('button', { name: 'Steer now' }).click();
  await expect(status('Done.')).toBeVisible();
  await expect(composer.getByLabel('Message')).toHaveValue('');
  await page.getByRole('button', { name: 'Interrupt' }).click();
  await expect(status('Done.')).toBeVisible();
  await expect(view).toContainText('echo: after the stop');
  await expect(view).toContainText('Turn interrupted.');
  // The steered input ran once, after the interrupted turn.
  const text = (await view.locator('.fd-items').innerText()).replace(/\s+/g, ' ');
  expect(text.indexOf('Turn interrupted.')).toBeLessThan(text.indexOf('echo: after the stop'));
  expect(text.split('echo: after the stop')).toHaveLength(2);
  await expect(composer.getByRole('button', { name: 'Send' })).toBeVisible();
  expect(await fitsWidth()).toBe(true);
  expect(stationSessions(bravo).map((s) => s.session_id)).toEqual([id]);
});

test('the board survives a deck restart without a new login and shows work done meanwhile', async () => {
  await fleet();
  await deck.stop();
  await expect(status('Reconnecting · Stale fleet')).toBeVisible();
  await expect(status('Cached data is read-only until the deck reconnects.')).toBeVisible();
  // The cached session stays readable, but nothing can be sent.
  const view = await open('Alpha approval');
  await expect(view).toContainText('Controls are off until the board and the station are live.');
  const plan = view.getByRole('button', { name: 'plan', exact: true });
  await expect(plan).toBeDisabled();
  // The station works on while the deck is away.
  const away = alpha.run('while the deck is away');
  await deck.start();
  await live();
  await expect(plan).toBeEnabled();
  await expect(view).not.toContainText('Controls are off');
  await fleet();
  await expect(card(away)).toBeVisible();
  await expect(page.locator('.fd-card')).toHaveCount(3);
  await expect(page.getByLabel('Owner password')).toHaveCount(0);
  // No command ran twice: each station has only the sessions it started.
  expect(stationSessions(alpha)).toHaveLength(2);
  expect(stationSessions(bravo)).toHaveLength(1);
  // Each board command was routed once and ran once: the deck's router has
  // one row per command, and each agent read each input exactly once.
  expectRouted({
    'session.start': 2,
    'approval.answer': 1,
    'session.steer': 1,
    'session.interrupt': 1,
  });
  expect(alpha.providerInput()).toEqual({
    messages: ['ask echo hi', 'while the deck is away'],
    controls: { initialize: 2 },
    answers: 1,
  });
  expect(bravo.providerInput()).toEqual({
    messages: ['wait for the signal', 'after the stop'],
    controls: { initialize: 1, interrupt: 1 },
    answers: 0,
  });
});

test('the phone drops its network, catches up, and drives a session again', async () => {
  await context.setOffline(true);
  expect(sockets.size).toBe(1);
  for (const ws of sockets) await ws.close({ code: 4000, reason: 'network lost' });
  await expect(status('Reconnecting · Stale fleet')).toBeVisible();
  const offline = bravo.run('while the phone is offline');
  await context.setOffline(false);
  await live();
  await expect(card(offline)).toBeVisible();
  await expect(page.locator('.fd-card')).toHaveCount(4);
  const view = await open('Bravo long run');
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer.getByLabel('Message').fill('one more');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(status('Done.')).toBeVisible();
  await expect(view).toContainText('echo: one more');
  expectRouted({
    'session.start': 2,
    'approval.answer': 1,
    'session.steer': 1,
    'session.interrupt': 1,
    'session.prompt': 1,
  });
  expect(bravo.providerInput().messages).toEqual([
    'wait for the signal',
    'after the stop',
    'while the phone is offline',
    'one more',
  ]);
});

test('a station link drop shows the station unreachable until it returns', async () => {
  await fleet();
  await bravo.stop();
  await page.getByRole('button', { name: 'Stations (2)' }).click();
  await expect(stationCard('bravo')).toContainText('Unreachable');
  await expect(stationCard('alpha')).toContainText('Link up');
  await bravo.start();
  await expect(stationCard('bravo')).toContainText('Link up');
  await fleet();
  await expect(page.locator('.fd-card')).toHaveCount(4);
  // The station restart resent nothing: no new command and no new input.
  expectRouted({
    'session.start': 2,
    'approval.answer': 1,
    'session.steer': 1,
    'session.interrupt': 1,
    'session.prompt': 1,
  });
  expect(bravo.providerInput().messages).toHaveLength(4);
  expect(alpha.providerInput().messages).toHaveLength(2);
});
