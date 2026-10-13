// Menus and popovers close on an outside press; the composer `@` menu says what it does; the
// phone terminal scrolls back with a finger and has the missing keys. Runs on a phone viewport and
// on a desktop viewport (a describe sets the viewport, so the phone project runs both).
import { expect, type Locator, type Page, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import { claudeCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { FakeDeck, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 's2';
const TITLE = 'Write the docs';

const MODES = [
  { name: 'phone', viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true },
  { name: 'desktop', viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false },
] as const;

function session(): DeckSession {
  return {
    station_id: ST,
    status: 'working',
    summary: summary(ID, 'working', 1, {
      title: TITLE,
      cwd: '/work/repo',
      branch: 'main',
      capabilities: claudeCaps(),
      mode: 'default',
      current_run: 'run-1',
      slash_commands: ['compact', 'init'],
    }),
  };
}

async function setUp(page: Page, deck = new FakeDeck()) {
  deck.stations = [stationView(ST, 'Alpha')];
  deck.sessions = [session()];
  await page.routeWebSocket('**/shells/*/attach', () => {});
  await deck.install(page);
  await logIn(page);
  await openSession(page, TITLE);
  return deck;
}

/** One press (a tap on a touch screen, a click with a mouse) on an element. */
async function press(target: Locator, touch: boolean) {
  if (touch) await target.tap();
  else await target.click();
}

/** One press at a point of the page. */
async function pressAt(page: Page, x: number, y: number, touch: boolean) {
  if (touch) await page.touchscreen.tap(x, y);
  else await page.mouse.click(x, y);
}

const menuOf = (page: Page, label: string) =>
  page.locator('details.fd-menu').filter({ has: page.locator(`summary[aria-label="${label}"]`) });

for (const mode of MODES) {
  test.describe(`menus on the ${mode.name}`, () => {
    test.use({ viewport: mode.viewport, isMobile: mode.isMobile, hasTouch: mode.hasTouch });

    test('the session menu closes on a press outside it', async ({ page }) => {
      await setUp(page);
      const menu = menuOf(page, 'Session menu');
      await press(menu.locator('summary'), mode.hasTouch);
      await expect(menu).toHaveAttribute('open', '');
      await press(page.getByRole('heading', { level: 2 }).first(), mode.hasTouch);
      await expect(menu).not.toHaveAttribute('open', '');
    });

    test('the `/` menu of the composer closes on a press outside it', async ({ page }) => {
      await setUp(page);
      const composer = page.getByRole('form', { name: 'Composer' });
      await composer.getByLabel('Message').fill('/');
      const list = composer.getByRole('listbox', { name: 'Commands' });
      await expect(list).toBeVisible();
      // A press in the box keeps it open.
      await press(composer.getByLabel('Message'), mode.hasTouch);
      await expect(list).toBeVisible();
      await press(page.getByRole('heading', { level: 2 }).first(), mode.hasTouch);
      await expect(list).toHaveCount(0);
    });

    test('the palette closes on a press outside it', async ({ page }) => {
      await setUp(page);
      await page.keyboard.press('Control+k');
      const palette = page.getByRole('dialog', { name: 'Go to session' });
      await expect(palette).toBeVisible();
      await pressAt(page, 4, 4, mode.hasTouch);
      await expect(palette).toHaveCount(0);
    });

    test('the help closes on a press outside it', async ({ page }) => {
      await setUp(page);
      await page.keyboard.press('?');
      const help = page.getByRole('dialog', { name: 'Keyboard and help' });
      await expect(help).toBeVisible();
      // A press inside keeps it open.
      await press(help.getByRole('heading', { level: 2 }), mode.hasTouch);
      await expect(help).toBeVisible();
      await pressAt(page, 4, 4, mode.hasTouch);
      await expect(help).toHaveCount(0);
    });

    test('the `@` menu says what it does: letters missing, searching, hits, nothing, a failure', async ({
      page,
    }) => {
      const deck = new FakeDeck();
      let mode500 = false;
      deck.on(`/api/stations/${ST}/files/search`, (_b, q) =>
        mode500
          ? { status: 500, body: { code: 'internal', message: 'the index is offline' } }
          : {
              status: 200,
              body: {
                hits:
                  q.get('q') === 'zz'
                    ? []
                    : [
                        { path: 'src/main.rs', line: null, text: null },
                        { path: 'docs/manual.md', line: null, text: null },
                      ],
                truncated: false,
                backend: 'walk',
              },
            },
      );
      await setUp(page, deck);
      const composer = page.getByRole('form', { name: 'Composer' });
      const box = composer.getByLabel('Message');
      const list = composer.getByRole('listbox', { name: 'Files' });
      await box.fill('@');
      const status = list.getByRole('option', { name: 'Type 2 or more letters to search files' });
      await expect(status).toHaveAttribute('aria-disabled', 'true');
      // With a status row only, Tab is not captured: it moves the focus, and the text stays.
      await box.fill('@m');
      await expect(status).toBeVisible();
      await box.press('Tab');
      await expect(box).not.toBeFocused();
      await expect(box).toHaveValue('@m');
      await box.focus();

      const release = deck.hold(`/api/stations/${ST}/files/search`);
      await box.fill('@ma');
      await expect(list.getByRole('option', { name: 'Searching…' })).toBeVisible();
      release();
      await expect(list.getByRole('option', { name: 'src/main.rs' })).toBeVisible();
      await expect(list.getByRole('option', { name: 'Searching…' })).toHaveCount(0);
      await expect(list.getByRole('option', { name: 'Type 2 or more letters' })).toHaveCount(0);

      // ArrowDown moves over the hits; a pick types the path.
      await box.press('ArrowDown');
      await box.press('Enter');
      await expect(box).toHaveValue('@docs/manual.md ');

      await box.fill('@zz');
      await expect(list.getByRole('option', { name: 'No matching files' })).toBeVisible();

      mode500 = true;
      await box.fill('@fa');
      await expect(list.getByRole('option', { name: /^File search failed: / })).toBeVisible();

      // Escape closes the menu.
      await box.press('Escape');
      await expect(list).toHaveCount(0);
      await box.fill('@fb');
      await expect(list).toBeVisible();
      await press(page.getByRole('heading', { level: 2 }).first(), mode.hasTouch);
      await expect(list).toHaveCount(0);
    });
  });
}

test.describe('menus on the desktop: two menus', () => {
  test.use({ viewport: MODES[1].viewport, isMobile: false, hasTouch: false });

  test('a press on the Terminal menu closes the session menu', async ({ page }) => {
    const deck = new FakeDeck();
    deck.shells(ST);
    await setUp(page, deck);
    const sessionMenu = menuOf(page, 'Session menu');
    const terminalMenu = menuOf(page, 'Terminal');
    await sessionMenu.locator('summary').click();
    await expect(sessionMenu).toHaveAttribute('open', '');
    await terminalMenu.locator('summary').click();
    await expect(terminalMenu).toHaveAttribute('open', '');
    await expect(sessionMenu).not.toHaveAttribute('open', '');
  });
});

test.describe('the terminal on the phone', () => {
  test.use({ viewport: MODES[0].viewport, isMobile: true, hasTouch: true });

  test('the key row has PgUp and Paste, and a finger swipe scrolls the scrollback', async ({
    page,
  }) => {
    const deck = new FakeDeck();
    deck.shells(ST);
    deck.stations = [stationView(ST, 'Alpha')];
    deck.sessions = [session()];
    const output = Array.from({ length: 200 }, (_, n) => `line ${n + 1}`).join('\r\n');
    await page.routeWebSocket('**/shells/*/attach', (ws) => {
      ws.send(Buffer.from(`${output}\r\n`));
    });
    await deck.install(page);
    await logIn(page);
    await openSession(page, TITLE);
    await menuOf(page, 'Session menu').locator('summary').tap();
    await page.getByRole('button', { name: 'Open shell in workspace' }).tap();
    const keys = page.getByRole('toolbar', { name: 'Terminal keys' });
    await expect(keys.getByRole('button', { name: 'PgUp', exact: true })).toBeVisible();
    await expect(keys.getByRole('button', { name: 'Paste', exact: true })).toBeVisible();
    const terminal = page.locator('.fd-term-host');
    await expect(terminal).toContainText('line 200');
    const firstRow = () => page.locator('.xterm-rows > div').first().innerText();
    const before = await firstRow();

    const box = await terminal.boundingBox();
    if (box === null) throw new Error('the terminal has no box');
    const x = box.x + box.width / 2;
    const top = box.y + 60;
    const cdp = await page.context().newCDPSession(page);
    const swipe = async (from: number, to: number) => {
      const steps = 12;
      await cdp.send('Input.dispatchTouchEvent', {
        type: 'touchStart',
        touchPoints: [{ x, y: from }],
      });
      for (let i = 1; i <= steps; i += 1) {
        await cdp.send('Input.dispatchTouchEvent', {
          type: 'touchMove',
          touchPoints: [{ x, y: from + ((to - from) * i) / steps }],
        });
      }
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    };
    // The finger moves down: the older lines come into view.
    await swipe(top, top + 400);
    await expect.poll(firstRow).not.toBe(before);
    const scrolled = await firstRow();
    // The finger moves up: the view goes back to the end.
    await swipe(top + 400, top);
    await expect.poll(firstRow).not.toBe(scrolled);
  });
});
