// M2 P5-4: the MVP board flows of `mvp.spec.ts` with Codex sessions, on a
// phone viewport against real processes. One deck and two stations run on
// temporary nodes; `alpha` listens and the deck dials it, `bravo` dials the
// deck. Both stations offer the offline `fake-claude sim` and `fake-codex`
// (`app-server`, scenario `sim`) agents; every session here uses Codex.
//
// The steps are the same as for Claude: pairing, spawn on each station, an
// approval from the needs-you rail, steer and interrupt, a deck restart, a
// phone network drop and a station link drop. The expected texts and the
// provider input differ where the capabilities differ (DL-066): a Codex steer
// joins the running turn, and the station holds a follow-up (no native queue).
// A last step shows Codex background work, which the station polls (DL-024).
import { expect, test } from '@playwright/test';
import { Phone, stationSessions } from './board';
import { BRANCH, CODEX, Deck, PROVIDER, Station } from './fleet';

test.describe.configure({ mode: 'serial' });

/** The spawn form's label of the Codex instance (`fake-codex --version`). */
const CODEX_LABEL = `${CODEX} · codex · 0.160.999`;

let deck: Deck;
let alpha: Station;
let bravo: Station;
let phone: Phone;

test.beforeAll(async ({ browser }) => {
  deck = await Deck.create();
  alpha = new Station('alpha');
  bravo = new Station('bravo');
  await deck.start();
  await alpha.start();
  await bravo.start();
  phone = await Phone.open(browser, deck);
});

test.afterAll(async () => {
  await phone?.context.close();
  for (const node of [bravo, alpha, deck]) await node?.dispose();
});

test('the board pairs both stations and lists their Claude and Codex agents', async () => {
  await phone.pair(alpha, bravo);
  for (const name of ['alpha', 'bravo']) {
    const st = phone.stationCard(name);
    await expect(st).toContainText('Link up');
    await expect(st).toContainText(`${PROVIDER} · claude · 2.1.999`);
    await expect(st).toContainText(CODEX_LABEL);
  }
  expect(await phone.fitsWidth()).toBe(true);
});

test('the spawn form offers the Codex modes that the station reports', async () => {
  const page = phone.page;
  await page.getByRole('button', { name: 'New session' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('combobox', { name: 'Provider' }).selectOption({ label: CODEX_LABEL });
  const modes = await form.getByLabel('Permission mode').locator('option').allTextContents();
  expect(modes).toContain('untrusted/read-only');
  expect(modes).toContain('untrusted/workspace-write');
  expect(modes).not.toContain('plan');
  await phone.fleet();
});

test('a Codex session on alpha asks for an approval that the needs-you rail answers', async () => {
  const page = phone.page;
  const id = await phone.spawn(alpha, CODEX_LABEL, 'Codex approval', 'ask echo hi');
  await expect(phone.card(id)).toContainText(`Branch: ${BRANCH}`);
  await expect(phone.card(id)).toContainText(`Instance: ${CODEX}`);
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toContainText('Codex approval · alpha: echo hi');
  await rail.getByRole('button', { name: 'Answer echo hi in Codex approval' }).click();
  const needs = page.getByRole('region', { name: 'Needs you' });
  await expect(needs).toContainText('echo hi');
  expect(await phone.fitsWidth()).toBe(true);
  await needs.getByRole('button', { name: 'Allow once' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  const view = page.getByRole('main', { name: 'Session' });
  await expect(view).toContainText('Shell · echo hi');
  await expect(view).toContainText('ran: echo hi');
  await expect(view).toContainText('Approval: echo hi · Allow once (by board)');
  await phone.fleet();
  await expect(rail).toContainText('Nothing waits for you.');
  expect(stationSessions(alpha).map((s) => [s.session_id, s.provider_instance])).toEqual([
    [id, CODEX],
  ]);
});

test('a Codex turn on bravo takes a steer into the running turn, then an interrupt', async () => {
  const page = phone.page;
  const id = await phone.spawn(bravo, CODEX_LABEL, 'Codex long run', 'wait for the signal');
  const view = await phone.open('Codex long run');
  await expect(view).toContainText('working');
  const composer = page.getByRole('form', { name: 'Composer' });
  // Codex has no native follow-up queue: the board says that the station
  // holds a follow-up (capability-driven, DL-065).
  await expect(composer).toContainText('The station holds a follow-up until the turn ends.');
  await composer.getByLabel('Message').fill('after the stop');
  await composer.getByRole('button', { name: 'Steer now' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  // The steer joins the running turn, which then ends.
  await expect(view).toContainText('steered: after the stop');
  await expect(composer.getByRole('button', { name: 'Send' })).toBeVisible();
  // A second waiting turn ends with the Interrupt control.
  await composer.getByLabel('Message').fill('wait once more');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('working');
  await page.getByRole('button', { name: 'Interrupt' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('Turn interrupted.');
  await expect(composer.getByRole('button', { name: 'Send' })).toBeVisible();
  expect(await phone.fitsWidth()).toBe(true);
  expect(stationSessions(bravo).map((s) => s.session_id)).toEqual([id]);
});

test('the board survives a deck restart and shows Codex work done meanwhile', async () => {
  await phone.fleet();
  await deck.stop();
  await expect(phone.status('Reconnecting · Stale fleet')).toBeVisible();
  const view = await phone.open('Codex approval');
  await expect(view).toContainText('Controls are off until the board and the station are live.');
  const mode = view.getByRole('button', { name: 'untrusted/read-only', exact: true });
  await expect(mode).toBeDisabled();
  const away = alpha.run('while the deck is away', CODEX);
  await deck.start();
  await phone.live();
  await expect(mode).toBeEnabled();
  await phone.fleet();
  await expect(phone.card(away)).toBeVisible();
  await expect(phone.page.locator('.fd-card')).toHaveCount(3);
  await expect(phone.page.getByLabel('Owner password')).toHaveCount(0);
  expect(stationSessions(alpha)).toHaveLength(2);
  expect(stationSessions(bravo)).toHaveLength(1);
  phone.expectRouted({
    'session.start': 2,
    'approval.answer': 1,
    'session.steer': 1,
    'session.prompt': 1,
    'session.interrupt': 1,
  });
  // Each Codex agent read each input exactly once.
  const a = alpha.codexInput();
  expect([a.turns, a.steers, a.answers]).toEqual([
    ['ask echo hi', 'while the deck is away'],
    [],
    1,
  ]);
  const b = bravo.codexInput();
  expect([b.turns, b.steers, b.answers]).toEqual([
    ['wait for the signal', 'wait once more'],
    ['after the stop'],
    0,
  ]);
  expect(b.methods['turn/interrupt']).toBe(1);
  // No Claude agent ran a turn.
  expect(alpha.providerInput().messages).toEqual([]);
  expect(bravo.providerInput().messages).toEqual([]);
});

test('the phone drops its network, catches up, and drives a Codex session again', async () => {
  const page = phone.page;
  await phone.context.setOffline(true);
  expect(phone.sockets.size).toBe(1);
  for (const ws of phone.sockets) await ws.close({ code: 4000, reason: 'network lost' });
  await expect(phone.status('Reconnecting · Stale fleet')).toBeVisible();
  const offline = bravo.run('while the phone is offline', CODEX);
  await phone.context.setOffline(false);
  await phone.live();
  await expect(phone.card(offline)).toBeVisible();
  await expect(page.locator('.fd-card')).toHaveCount(4);
  const view = await phone.open('Codex long run');
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer.getByLabel('Message').fill('one more');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('echo: one more');
  phone.expectRouted({
    'session.start': 2,
    'approval.answer': 1,
    'session.steer': 1,
    'session.prompt': 2,
    'session.interrupt': 1,
  });
  expect(bravo.codexInput().turns).toEqual([
    'wait for the signal',
    'wait once more',
    'while the phone is offline',
    'one more',
  ]);
});

test('a station link drop shows the station unreachable until it returns', async () => {
  const page = phone.page;
  await phone.fleet();
  await bravo.stop();
  await page.getByRole('button', { name: 'Stations (2)' }).click();
  await expect(phone.stationCard('bravo')).toContainText('Unreachable');
  await expect(phone.stationCard('alpha')).toContainText('Link up');
  await bravo.start();
  await expect(phone.stationCard('bravo')).toContainText('Link up');
  await phone.fleet();
  await expect(page.locator('.fd-card')).toHaveCount(4);
  phone.expectRouted({
    'session.start': 2,
    'approval.answer': 1,
    'session.steer': 1,
    'session.prompt': 2,
    'session.interrupt': 1,
  });
  expect(bravo.codexInput().turns).toHaveLength(4);
  expect(alpha.codexInput().turns).toHaveLength(2);
});

test('a Codex background command shows on the card until the station poll sees it end', async () => {
  const page = phone.page;
  const view = await phone.open('Codex approval');
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer.getByLabel('Message').fill('bg make build');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('started in background');
  await phone.fleet();
  const card = page.locator('.fd-card').filter({ hasText: 'Codex approval' });
  await expect(card).toContainText('Background tasks: 1');
  // The poll repeats every 5 s while the terminal runs; the fake ends it on
  // the second list.
  await expect(card).not.toContainText('Background tasks', { timeout: 30_000 });
  const a = alpha.codexInput();
  expect(a.turns).toEqual(['ask echo hi', 'while the deck is away', 'bg make build']);
  expect(a.methods['thread/backgroundTerminals/list']).toBeGreaterThanOrEqual(2);
});
