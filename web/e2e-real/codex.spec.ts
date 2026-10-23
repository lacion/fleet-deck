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
// A step shows Codex background work, which the station polls (DL-024). The
// last step answers a Codex `requestUserInput` question with the board's
// answer form (DL-068).
import { expect, test } from '@playwright/test';
import { Phone, stationSessions } from './board';
import { BRANCH, CODEX, Deck, PROVIDER, Station } from './fleet';

test.describe.configure({ mode: 'serial' });

/** The spawn form's label of the Codex instance (`fake-codex --version`). */
// The provider choice names the provider and its instance; the version is in its title.
const CODEX_LABEL = `Codex (${CODEX})`;

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
  // Dispose every node, also after a failure: a throw ends the loop, and
  // the nodes after it would keep their processes.
  const errors: unknown[] = [];
  for (const node of [bravo, alpha, deck]) {
    try {
      await node?.dispose();
    } catch (error) {
      errors.push(error);
    }
  }
  if (errors.length > 0) throw errors[0];
});

test('the board pairs both stations and lists their Claude and Codex agents', async () => {
  await phone.pair(alpha, bravo);
  for (const name of ['alpha', 'bravo']) {
    const st = phone.stationCard(name);
    await expect(st).toContainText('online');
    await expect(phone.provider(name, PROVIDER)).toContainText('Claude 2.1.999');
    await expect(phone.provider(name, CODEX)).toContainText('Codex 0.160.999');
  }
  expect(await phone.fitsWidth()).toBe(true);
});

test('the spawn form offers the Codex modes that the station reports', async () => {
  const page = phone.page;
  await (await phone.bar()).getByRole('link', { name: 'New' }).click();
  const form = page.getByRole('region', { name: 'New session' });
  await form
    .getByRole('radiogroup', { name: 'Provider' })
    .locator('label', { hasText: CODEX_LABEL })
    .click();
  // The options show the modes in words; their values are the native modes.
  const modes = await form
    .getByLabel('Permission mode')
    .locator('option')
    .evaluateAll((options) => options.map((o) => (o as HTMLOptionElement).value));
  expect(modes).toContain('untrusted/read-only');
  expect(modes).toContain('untrusted/workspace-write');
  expect(modes).not.toContain('plan');
  await phone.fleet();
});

test('a Codex session on alpha asks for an approval that the needs-you rail answers', async () => {
  const page = phone.page;
  const id = await phone.spawn(alpha, CODEX_LABEL, 'Codex approval', 'ask echo hi');
  await expect(phone.card(id)).toContainText('gpt-sim-1');
  // The place is in the tooltip of the strip's where field (DESIGN-SYSTEM §4.1).
  await expect(phone.card(id).locator('.fd-strip-where')).toHaveAttribute(
    'title',
    `Branch: ${BRANCH}`,
  );
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toContainText('Codex approval');
  await expect(rail).toContainText('alpha');
  await expect(rail).toContainText('echo hi');
  expect(await phone.fitsWidth()).toBe(true);
  await rail.getByRole('button', { name: 'Allow once: echo hi in Codex approval' }).click();
  // The rail is gone when nothing waits for the answer (the phone strip is empty).
  await expect(rail).toHaveCount(0);
  const view = await phone.open('Codex approval');
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  const log = view.getByRole('log');
  await expect(log).toContainText('Shell');
  await expect(log).toContainText('ran: echo hi');
  await expect(log).toContainText('echo hi · Allow once');
  await phone.fleet();
  await expect(rail).toHaveCount(0);
  expect(stationSessions(alpha).map((s) => [s.session_id, s.provider_instance])).toEqual([
    [id, CODEX],
  ]);
});

test('a Codex turn on bravo takes a steer into the running turn, then an interrupt', async () => {
  const page = phone.page;
  const id = await phone.spawn(bravo, CODEX_LABEL, 'Codex long run', 'wait for the signal');
  const view = await phone.open('Codex long run');
  await expect(view).toContainText('Working');
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
  await expect(view).toContainText('Working');
  await page.getByRole('button', { name: 'Interrupt' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('The turn was interrupted.');
  await expect(composer.getByRole('button', { name: 'Send' })).toBeVisible();
  expect(await phone.fitsWidth()).toBe(true);
  expect(stationSessions(bravo).map((s) => s.session_id)).toEqual([id]);
});

test('the board survives a deck restart and shows Codex work done meanwhile', async () => {
  await phone.fleet();
  await deck.stop();
  await phone.stale();
  let view = await phone.open('Codex approval');
  await expect(view).toContainText('Controls are off until the board and the station are live.');
  await view.locator('summary[aria-label="Session menu"]').click();
  // The mode rows show the mode in words; the title keeps the native id.
  const mode = view.locator('button.fd-mode-row[title="untrusted/read-only"]');
  await expect(mode).toBeDisabled();
  const away = alpha.run('while the deck is away', CODEX);
  await deck.start();
  await phone.live();
  view = await phone.open('Codex approval');
  await view.locator('summary[aria-label="Session menu"]').click();
  await expect(mode).toBeEnabled();
  await phone.fleet();
  await expect(phone.card(away)).toBeVisible();
  await expect(phone.page.locator('.fd-strip')).toHaveCount(3);
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
  await phone.stale();
  const offline = bravo.run('while the phone is offline', CODEX);
  await phone.context.setOffline(false);
  await phone.live();
  await phone.fleet();
  await expect(phone.card(offline)).toBeVisible();
  await expect(page.locator('.fd-strip')).toHaveCount(4);
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
  await phone.stations(2);
  await expect(phone.stationCard('bravo')).toContainText('offline');
  await expect(phone.stationCard('alpha')).toContainText('online');
  await bravo.start();
  await expect(phone.stationCard('bravo')).toContainText('online');
  await phone.fleet();
  await expect(page.locator('.fd-strip')).toHaveCount(4);
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
  const card = page.locator('.fd-strip').filter({ hasText: 'Codex approval' });
  const tasks = card.locator('[title="Background tasks"]');
  await expect(tasks).toHaveText('1');
  // The poll repeats every 5 s while the terminal runs; the fake ends it on
  // the second list.
  await expect(tasks).toHaveCount(0, { timeout: 30_000 });
  const a = alpha.codexInput();
  expect(a.turns).toEqual(['ask echo hi', 'while the deck is away', 'bg make build']);
  expect(a.methods['thread/backgroundTerminals/list']).toBeGreaterThanOrEqual(2);
});

test('a Codex question gets its answer from the phone answer form', async () => {
  const page = phone.page;
  await phone.fleet();
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toHaveCount(0);
  const view = await phone.open('Codex approval');
  const composer = page.getByRole('form', { name: 'Composer' });
  await composer.getByLabel('Message').fill('question Which color do you prefer?');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  const form = page.getByRole('form', { name: 'Answer: Which color do you prefer?' });
  await expect(form).toContainText('Choice: Which color do you prefer?');
  const send = form.getByRole('button', { name: 'Send answer' });
  // Nothing is chosen yet: the board does not send an empty answer.
  await expect(send).toBeDisabled();
  await form.getByRole('radio', { name: 'Blue' }).check();
  expect(await phone.fitsWidth()).toBe(true);
  await send.click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('answered: Blue');
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  await expect(view).toContainText('Question');
  await expect(view).toContainText('Which color do you prefer? · Answer');
  // A typed answer replaces the choice.
  await composer.getByLabel('Message').fill('question Which size?');
  await composer.getByRole('button', { name: 'Send' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  const size = page.getByRole('form', { name: 'Answer: Which size?' });
  await size.getByRole('radio', { name: 'Red' }).check();
  await size.getByLabel('Other answer').fill('large');
  await size.getByRole('button', { name: 'Send answer' }).click();
  await expect(phone.status('Done.')).toBeVisible();
  await expect(view).toContainText('answered: large');
  // The station sent each answer once, in the shape that Codex asks for.
  const a = alpha.codexInput();
  expect(a.questions).toEqual([
    { choice: { answers: ['Blue'] } },
    { choice: { answers: ['large'] } },
  ]);
  expect(a.turns.slice(-2)).toEqual([
    'question Which color do you prefer?',
    'question Which size?',
  ]);
  phone.expectRouted({
    'session.start': 2,
    'approval.answer': 3,
    'session.steer': 1,
    'session.prompt': 5,
    'session.interrupt': 1,
  });
});
