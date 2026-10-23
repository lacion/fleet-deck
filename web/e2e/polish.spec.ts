// The UI polish checks of the Flight Deck board (the test gaps of the UI refresh): the `c` key on
// the wide fleet home, the outcome notes of a coordinator grant in its Fleet access dialog, no
// sideways scroll of the ops column, the 44px touch targets of the Add station flow and the folder
// crumbs, forced colors, the free-text model of New session and the j/k order of the sidebar, the
// Focus pause of the Watch page and the one-shot Properties file focus. The phone project runs
// every test that is not desktop-only; the desktop project runs this file too
// (playwright.config.ts).
import { expect, type Locator, type Page, test } from '@playwright/test';
import type { Event } from '../src/gen';
import { codexCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { envelopes } from '../src/testing/timeline';
import { FakeDeck, fitsWidth, logIn, openSession, PASSWORD, stationView } from './fake-deck';
import { OTTER } from './gallery-data';
import { LIGHTING, polishDeck, SECRET_PROJECT } from './polish-scene';

const desktopOnly = () =>
  test.skip(test.info().project.name !== 'desktop', 'the desktop layout only');
const phoneOnly = () => test.skip(test.info().project.name !== 'phone', 'the phone layout only');

async function setUp(page: Page) {
  const scene = polishDeck();
  await page.routeWebSocket('**/shells/*/attach', () => {});
  await scene.deck.install(page);
  await logIn(page);
  await page.locator('a.fd-srow').first().waitFor();
  return scene;
}

async function goTo(page: Page, hash: string) {
  await page.evaluate((h) => {
    window.location.hash = h;
  }, hash);
}

/** No element has the focus: a global key goes to the board, not to an input. */
async function blur(page: Page) {
  await page.evaluate(() => {
    const active = document.activeElement;
    if (active instanceof HTMLElement) active.blur();
  });
}

async function confirmIdentity(page: Page) {
  const dialog = page.getByRole('dialog', { name: 'Confirm it is you' });
  await dialog.getByLabel('Password').fill(PASSWORD);
  await dialog.getByRole('button', { name: 'Confirm' }).click();
  await expect(dialog).toHaveCount(0);
}

/** Every visible element of `list` is at least 44px high; the message names the short ones. */
async function expectTall(list: Locator, what: string) {
  const count = await list.count();
  expect(count, `${what}: no element to measure`).toBeGreaterThan(0);
  const short: string[] = [];
  for (let i = 0; i < count; i += 1) {
    const el = list.nth(i);
    if (!(await el.isVisible())) continue;
    const box = await el.boundingBox();
    const height = box?.height ?? 0;
    if (height < 44) short.push(`"${(await el.innerText()).trim()}" ${height.toFixed(1)}px`);
  }
  expect(short, `${what}: targets under 44px`).toEqual([]);
}

async function screenshot(page: Page, name: string) {
  await page.evaluate(() => document.fonts.ready);
  await page.mouse.move(0, 0);
  await page.screenshot({
    path: `test-results/gallery/${test.info().project.name}-polish-${name}.png`,
    fullPage: false,
    animations: 'disabled',
  });
}

test('1: on the wide fleet home, "c" opens the Mail & tasks form with the focus in it', async ({
  page,
}) => {
  desktopOnly();
  await setUp(page);
  await goTo(page, '#/fleet');
  await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
  // From 1280px Mail & tasks is a section of the ops column, its form folded.
  const mail = page.getByRole('region', { name: 'Mail and tasks' });
  await expect(mail).toBeVisible();
  const fold = mail.locator('details.fd-fleet-compose');
  await expect(fold).not.toHaveAttribute('open');
  await blur(page);
  await page.keyboard.press('c');
  await expect(fold).toHaveAttribute('open');
  await expect(mail.getByLabel('Text')).toBeFocused();
  // The focus is in the text: a second "c" is a letter of the mail, not a key of the board.
  await page.keyboard.press('c');
  await expect(mail.getByLabel('Text')).toHaveValue('c');
});

test('2: Revoke and Renew of a coordinator grant show their outcome note', async ({ page }) => {
  desktopOnly();
  const { grants } = await setUp(page);
  await goTo(page, '#/fleet');
  // The grants are rows of their own Fleet access section (no form in the column); a row's
  // Change… opens the Fleet access dialog, which shows the outcome of Renew and Revoke.
  const access = page.getByRole('region', { name: 'Fleet access' });
  const list = access.getByRole('list', { name: 'Coordinator grants' });
  await expect(list.getByRole('listitem')).toHaveCount(2);
  const live = list.getByRole('listitem').filter({ hasText: 'active' });
  const idle = list.getByRole('listitem').filter({ hasText: 'expired' });
  const dialog = page.getByRole('dialog', { name: 'Fleet access' });

  await idle.getByRole('button', { name: 'Change…' }).click();
  await dialog.getByRole('button', { name: 'Renew' }).click();
  await expect(dialog.getByRole('status')).toHaveText(
    'Renewal sent; the grant is active when its station confirms.',
  );
  expect(grants.rows.find((g) => g.grant_id === 'g-idle')?.state).toBe('active');
  await dialog.locator('.fd-dialog-head').getByRole('button', { name: 'Close' }).click();
  await expect(dialog).toHaveCount(0);

  await live.first().getByRole('button', { name: 'Change…' }).click();
  await dialog.getByRole('button', { name: 'Revoke' }).click();
  await dialog.getByRole('button', { name: 'Revoke' }).click();
  await expect(dialog.getByRole('status')).toHaveText('Grant revoked.');
  await dialog.locator('.fd-dialog-head').getByRole('button', { name: 'Close' }).click();
  // A revoked grant leaves the list.
  await expect(list.getByRole('listitem')).toHaveCount(1);
});

test('2b: the page and the ops column never scroll sideways at 1280, 1440 and 1920', async ({
  page,
}) => {
  desktopOnly();
  await setUp(page);
  for (const width of [1280, 1440, 1920]) {
    await page.setViewportSize({ width, height: 900 });
    await goTo(page, '#/fleet');
    await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
    await expect(page.getByRole('region', { name: 'Fleet access' })).toBeVisible();
    const sizes = await page.evaluate(() => {
      const ops = document.querySelector<HTMLElement>('.fd-ops');
      const body = document.querySelector<HTMLElement>('.fd-ops-body');
      const over = (el: HTMLElement | null) => (el === null ? 0 : el.scrollWidth - el.clientWidth);
      return {
        page: document.documentElement.scrollWidth - window.innerWidth,
        ops: over(ops),
        body: over(body),
      };
    });
    expect(sizes.page, `the page at ${width}px`).toBeLessThanOrEqual(0);
    expect(sizes.ops, `the ops column at ${width}px`).toBeLessThanOrEqual(0);
    expect(sizes.body, `the ops body at ${width}px`).toBeLessThanOrEqual(0);
    // The foot of the column (Activity, Assign, All usage) is on the screen, not under the fold.
    const foot = await page.locator('.fd-ops-foot').boundingBox();
    expect(
      foot === null ? 0 : foot.y + foot.height,
      `the ops foot at ${width}px`,
    ).toBeLessThanOrEqual(900);
  }
});

test('3: the text buttons of Add station and the folder crumbs are 44px high on touch', async ({
  page,
}) => {
  phoneOnly();
  await setUp(page);
  expect(await page.evaluate(() => matchMedia('(pointer: coarse)').matches)).toBe(true);

  // Add station: the start screen ("Other ways" → listen), then the Run step (Stop, the way back).
  await goTo(page, '#/stations');
  await page.getByRole('button', { name: 'Add station' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add station' });
  await expect(dialog).toContainText('Where is the new station?');
  await dialog.locator('details.fd-add-fold > summary').click();
  await expect(
    dialog.getByRole('button', { name: 'Connect to a station that listens' }),
  ).toBeVisible();
  await expectTall(dialog.locator('.fd-add-text-btn'), 'Add station start');
  await dialog.getByRole('button', { name: /A computer on my tailnet/ }).click();
  await expect(dialog.getByRole('button', { name: 'Back', exact: true })).toBeVisible();
  await dialog
    .locator('[data-device="lab-box.tail1234.ts.net"]')
    .getByRole('button', { name: 'Add' })
    .click();
  await confirmIdentity(page);
  await expect(dialog).toContainText('Run this command on lab-box');
  await expect(dialog.getByRole('button', { name: 'Stop' })).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Choose another computer' })).toBeVisible();
  await expectTall(dialog.locator('.fd-add-text-btn'), 'Add station run step');
  await expectTall(dialog.getByRole('button', { name: 'Back', exact: true }), 'Add station back');
  await dialog.getByRole('button', { name: 'Close' }).click();
  await expect(dialog).toHaveCount(0);

  // New session → Browse…: the crumbs of the folder (Roots, projects, and app as the place).
  await goTo(page, '#/new');
  const form = page.getByRole('region', { name: 'New session' });
  await form.getByRole('button', { name: 'Browse…' }).click();
  // The station has one allowed root (projects): Browse… opens inside it.
  await form.getByRole('button', { name: 'Open app' }).click();
  const crumbs = form.getByRole('navigation', { name: 'Folder path' });
  await expect(crumbs.locator('[aria-current="location"]')).toHaveText('app');
  await expect(crumbs.getByRole('button')).toHaveText(['Roots', 'projects']);
  await expectTall(crumbs.getByRole('button'), 'folder picker crumbs');
  expect(await fitsWidth(page)).toBe(true);

  // The project page → Secret files → Browse…: the crumbs of the home folder.
  await goTo(page, `#/project/${SECRET_PROJECT}`);
  await page.getByRole('tab', { name: 'Setup' }).click();
  const secrets = page.getByRole('region', { name: 'Secret files' });
  await secrets.getByRole('button', { name: 'Browse…' }).click();
  const browser = secrets.getByRole('region', { name: 'Browse the home' });
  await browser.getByRole('button', { name: 'Open config' }).click();
  await browser.getByRole('button', { name: 'Open local' }).click();
  const secretCrumbs = browser.getByRole('navigation', { name: 'Folder path' });
  await expect(secretCrumbs.getByRole('button')).toHaveText(['fleet-deck', 'config', 'local']);
  await expectTall(secretCrumbs.getByRole('button'), 'secret files crumbs');
});

/** The edge of an element in forced colors: a border or an outline with a style and a width. */
async function edges(list: Locator): Promise<{ name: string; edge: boolean }[]> {
  return list.evaluateAll((els) =>
    els
      .filter((el) => {
        const r = el.getBoundingClientRect();
        return r.width > 0 && r.height > 0 && el.checkVisibility({ visibilityProperty: true });
      })
      .map((el) => {
        const s = getComputedStyle(el);
        const side = (k: 'Top' | 'Right' | 'Bottom' | 'Left') =>
          s.getPropertyValue(`border-${k.toLowerCase()}-style`) !== 'none' &&
          parseFloat(s.getPropertyValue(`border-${k.toLowerCase()}-width`)) > 0;
        const border = side('Top') || side('Right') || side('Bottom') || side('Left');
        const outline = s.outlineStyle !== 'none' && parseFloat(s.outlineWidth) > 0;
        const name =
          el.getAttribute('aria-label') ??
          el.getAttribute('name') ??
          (el.id === '' ? `${el.tagName.toLowerCase()}.${el.className}` : `#${el.id}`);
        return { name, edge: border || outline };
      }),
  );
}

/** The name that `edges` gives the composer text (it has no aria-label and no name). */
const COMPOSER = '#fd-composer-text';

/** The inputs, switches and segmented controls of the page that have no edge. */
async function edgeless(page: Page, seen: { inputs: number; switches: number; segs: number }) {
  const fields = await edges(
    page.locator(
      'input:not([type="checkbox"]):not([type="radio"]):not([type="hidden"]):not([type="file"]), textarea:not(.xterm-helper-textarea), select',
    ),
  );
  const switches = await edges(page.locator('input[role="switch"], .fd-switch'));
  // The design (DESIGN-SYSTEM §5): forced colors give the pressed segment a Highlight outline.
  const segs = await edges(
    page.locator(
      '.fd-seg > :is([aria-pressed="true"], [aria-selected="true"], :has(> input:checked))',
    ),
  );
  seen.inputs += fields.length;
  seen.switches += switches.length;
  seen.segs += segs.length;
  return [...fields, ...switches, ...segs].filter((e) => !e.edge).map((e) => e.name);
}

/**
 * Forced colors on, the scene in place, and the media query matches. `mixed` sets a system color
 * scheme and a stored board theme that differ (a dark system with the light theme, or the
 * reverse); the page must follow the system scheme.
 */
async function forcedColors(
  page: Page,
  mixed?: { readonly scheme: 'light' | 'dark'; readonly theme: 'light' | 'dark' },
) {
  if (mixed === undefined) {
    await page.emulateMedia({ forcedColors: 'active' });
  } else {
    await page.addInitScript((theme) => localStorage.setItem('fd-theme', theme), mixed.theme);
    await page.emulateMedia({ forcedColors: 'active', colorScheme: mixed.scheme });
  }
  await setUp(page);
  expect(await page.evaluate(() => matchMedia('(forced-colors: active)').matches)).toBe(true);
  if (mixed !== undefined) {
    // In forced colors the page takes the OS color scheme, not the stored theme (§6,
    // resolvedTheme()): the kept glyph colors are made for their own theme.
    await expect(page.locator('html')).toHaveAttribute('data-theme', mixed.scheme);
  }
}

/** The unselected tabs that draw a bottom rule: in forced colors only the selected tab has one. */
async function ruledTabs(page: Page): Promise<string[]> {
  return page.locator('[role="tab"][aria-selected="false"]').evaluateAll((els) =>
    els
      .filter((el) => el.checkVisibility({ visibilityProperty: true }))
      .filter((el) => {
        const s = getComputedStyle(el);
        return s.borderBottomStyle !== 'none' && parseFloat(s.borderBottomWidth) > 0;
      })
      .map((el) => el.getAttribute('aria-label') ?? el.textContent?.trim() ?? el.tagName),
  );
}

test('4a: in forced colors the home renders and its controls keep an edge', async ({ page }) => {
  await forcedColors(page);
  const seen = { inputs: 0, switches: 0, segs: 0 };
  // The home: the session list on the phone, the Fleet board on the desktop.
  if (test.info().project.name === 'phone') {
    const list = page.getByRole('complementary', { name: 'Sessions' });
    await expect(list.getByRole('region', { name: 'Needs you' })).toBeVisible();
  } else {
    await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
  }
  await screenshot(page, 'forced-colors-home');
  expect(await edgeless(page, seen)).toEqual([]);
  expect(seen.inputs).toBeGreaterThan(0);
});

test('4b: in forced colors the session renders and its controls keep an edge', async ({ page }) => {
  // The composer dock draws its edge with a box-shadow ring and its textarea has `border: 0`.
  // Forced colors drop box-shadow, so session.css gives the dock an outline there.
  await forcedColors(page);
  await goTo(page, `#/s/${LIGHTING}/${OTTER}`);
  const pane = page.getByRole('region', { name: 'Session' });
  await expect(pane.getByRole('heading', { level: 2 })).toHaveText('Fix flaky shim test');
  await expect(pane.getByRole('button', { name: 'Allow', exact: true }).first()).toBeVisible();
  await expect(page.locator('#fd-composer-text')).toBeVisible();
  await screenshot(page, 'forced-colors-session');
  const seen = { inputs: 0, switches: 0, segs: 0 };
  // Every control but the composer text: their own edge.
  const others = await edgeless(page, seen);
  expect(others.filter((name) => name !== COMPOSER)).toEqual([]);
  // The composer: the text box or its dock has an edge.
  const dock = await edges(page.locator('.fd-composer, .fd-composer textarea'));
  expect(
    dock.some((e) => e.edge),
    'the composer has no border or outline',
  ).toBe(true);
  // The session tabs: the selected tab has the rule, an unselected tab has none.
  await expect(page.locator('[role="tab"][aria-selected="false"]').first()).toBeVisible();
  expect(await ruledTabs(page)).toEqual([]);
  // The phone permission sheet: forced colors drop its fill and shadow, so it needs an edge.
  if (test.info().project.name === 'phone') {
    const sheet = page.locator('.fd-q-dock').first();
    await expect(sheet).toBeVisible();
    const edge = await sheet.evaluate((el) => {
      const s = getComputedStyle(el);
      return s.outlineStyle !== 'none' || parseFloat(s.borderTopWidth) >= 1;
    });
    expect(edge, 'the permission sheet has no outline and no top border').toBe(true);
  }
});

for (const mixed of [
  { scheme: 'dark', theme: 'light' },
  { scheme: 'light', theme: 'dark' },
] as const) {
  test(`4h: forced colors with a ${mixed.scheme} system and the ${mixed.theme} theme: the home and the session keep their edges`, async ({
    page,
  }) => {
    await forcedColors(page, mixed);
    const seen = { inputs: 0, switches: 0, segs: 0 };
    if (test.info().project.name === 'phone') {
      const list = page.getByRole('complementary', { name: 'Sessions' });
      await expect(list.getByRole('region', { name: 'Needs you' })).toBeVisible();
    } else {
      await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
    }
    await screenshot(page, `forced-colors-${mixed.scheme}-${mixed.theme}-home`);
    expect(await edgeless(page, seen)).toEqual([]);
    expect(await ruledTabs(page)).toEqual([]);
    expect(seen.inputs).toBeGreaterThan(0);
    await goTo(page, `#/s/${LIGHTING}/${OTTER}`);
    const pane = page.getByRole('region', { name: 'Session' });
    await expect(pane.getByRole('heading', { level: 2 })).toHaveText('Fix flaky shim test');
    await expect(page.locator('#fd-composer-text')).toBeVisible();
    await screenshot(page, `forced-colors-${mixed.scheme}-${mixed.theme}-session`);
    const others = await edgeless(page, seen);
    expect(others.filter((name) => name !== COMPOSER)).toEqual([]);
    await expect(page.locator('[role="tab"][aria-selected="false"]').first()).toBeVisible();
    expect(await ruledTabs(page)).toEqual([]);
  });
}

test('4d: in forced colors the active palette option has a Highlight outline', async ({ page }) => {
  desktopOnly();
  await forcedColors(page);
  await blur(page);
  await page.locator('body').press('Control+k');
  const palette = page.getByRole('dialog', { name: 'Go to session' });
  await expect(palette).toBeVisible();
  const active = palette.locator('[role="option"][aria-selected="true"]');
  await expect(active).toHaveCount(1);
  await expect(active).toHaveCSS('outline-style', 'solid');
});

test('4e: in forced colors the pressed Properties toggle has a Highlight outline', async ({
  page,
}) => {
  desktopOnly();
  await forcedColors(page);
  await goTo(page, `#/s/${LIGHTING}/${OTTER}`);
  const toggle = page.getByRole('button', { name: 'Properties' });
  await expect(toggle).toBeVisible();
  if ((await toggle.getAttribute('aria-pressed')) !== 'true') await toggle.click();
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(toggle).toHaveCSS('outline-style', 'solid');
});

test('4c: in forced colors the inputs, switches and segments of New session keep an edge', async ({
  page,
}) => {
  await forcedColors(page);
  await goTo(page, '#/new');
  await expect(page.getByRole('region', { name: 'New session' })).toBeVisible();
  // A phone shows the Batch switch in "More options" (its sticky footer is one row of buttons).
  if (test.info().project.name === 'phone') {
    await page.getByText('More options', { exact: true }).click();
  }
  // The checked station ring and the pressed segments are forced-colors rules: a capture shows them.
  await screenshot(page, 'forced-colors-spawn');
  const seen = { inputs: 0, switches: 0, segs: 0 };
  expect(await edgeless(page, seen)).toEqual([]);
  expect(seen.inputs).toBeGreaterThan(0);
  expect(seen.switches).toBeGreaterThan(0);
  expect(seen.segs).toBeGreaterThan(0);
});

test('4f: in forced colors the deck settings render a notice and their switches keep an edge', async ({
  page,
}) => {
  desktopOnly();
  await forcedColors(page);
  // The deck settings do not load: the tab shows its error notice (and the Tailnet card its
  // switches), the forced-colors rules of .fd-set-notice and .fd-switch.
  await page.route('**/api/settings/deck', (route) =>
    route.request().method() === 'GET'
      ? route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({ code: 'internal', message: 'The deck could not read deck.toml.' }),
        })
      : route.fallback(),
  );
  await goTo(page, '#/settings');
  await expect(page.getByRole('region', { name: 'Tailnet' })).toBeVisible();
  const notice = page.locator('.fd-set-notice[data-tone="error"]');
  await expect(notice).toBeVisible();
  await screenshot(page, 'forced-colors-settings');
  const seen = { inputs: 0, switches: 0, segs: 0 };
  expect(await edgeless(page, seen)).toEqual([]);
  expect(seen.switches).toBeGreaterThan(0);
});

test('4g: in forced colors the phone Fleet page and its needs-you rail render', async ({
  page,
}) => {
  phoneOnly();
  await forcedColors(page);
  await goTo(page, '#/fleet');
  await expect(page.getByRole('heading', { level: 1, name: 'Fleet' })).toBeVisible();
  const rail = page.getByRole('region', { name: 'Needs you rail' });
  await expect(rail).toBeVisible();
  await screenshot(page, 'forced-colors-fleet');
  // The keyboard cursor of the rail is a fill only: forced colors give it a Highlight outline.
  const selected = rail.locator('.fd-q[data-selected="true"]');
  if ((await selected.count()) > 0)
    await expect(selected.first()).toHaveCSS('outline-style', 'solid');
  const seen = { inputs: 0, switches: 0, segs: 0 };
  expect(await edgeless(page, seen)).toEqual([]);
});

test('5: without a model list the model is a text pill in the row of pills', async ({ page }) => {
  await setUp(page);
  await goTo(page, '#/new');
  const form = page.getByRole('region', { name: 'New session' });
  const model = form.getByLabel('Model (optional)');
  // No reported list: no "Other…" step, the owner types the model at once.
  await expect(form.getByLabel('Model id or alias')).toHaveCount(0);
  const pills = form.locator('.fd-spawn-pills');
  await expect(pills.getByLabel('Model (optional)')).toBeVisible();
  await expect(pills.getByLabel('Permission mode')).toBeVisible();
  await model.fill('claude-opus-4-1');
  await expect(model).toHaveValue('claude-opus-4-1');
  const box = await model.boundingBox();
  const width = page.viewportSize()?.width ?? 0;
  expect(box).not.toBeNull();
  expect((box?.x ?? 0) + (box?.width ?? 0)).toBeLessThanOrEqual(width);
  expect(await fitsWidth(page)).toBe(true);
  await screenshot(page, 'model-other');
});

test('6: j from the first session walks the sidebar rows in their visible order', async ({
  page,
}) => {
  desktopOnly();
  await setUp(page);
  const sidebar = page.getByRole('complementary', { name: 'Sessions' });
  await expect(sidebar.getByRole('region', { name: 'Needs you' })).toBeVisible();
  // The visible rows, top to bottom: "Needs you" first, then the station groups. The rows of a
  // closed "Ended" fold are not visible and j skips them.
  const hrefs = await sidebar
    .locator('a.fd-srow')
    .evaluateAll((els) =>
      els.filter((el) => el.checkVisibility()).map((el) => el.getAttribute('href') ?? ''),
    );
  const order = [...new Set(hrefs)];
  expect(order.length).toBeGreaterThan(2);
  const needs = await sidebar
    .getByRole('region', { name: 'Needs you' })
    .locator('a.fd-srow')
    .evaluateAll((els) => els.map((el) => el.getAttribute('href') ?? ''));
  expect(order.slice(0, needs.length)).toEqual(needs);

  const quote = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  await sidebar.locator(`a.fd-srow[href="${order[0]}"]`).first().click();
  await expect(page).toHaveURL(new RegExp(`${quote(order[0] ?? '')}$`));
  for (const next of order.slice(1)) {
    await page.keyboard.press('j');
    await expect(page).toHaveURL(new RegExp(`${quote(next)}$`));
  }
  // After the last row, j goes back to the first.
  await page.keyboard.press('j');
  await expect(page).toHaveURL(new RegExp(`${quote(order[0] ?? '')}$`));
});

test('7: Focus on the Watch page waits while the pointer or the keyboard is in the tile', async ({
  page,
}) => {
  desktopOnly();
  // The fake clock: Focus moves on every FOCUS_MS (20 s), and the test runs the time by hand.
  const FOCUS_MS = 20_000;
  await page.clock.install();
  await setUp(page);
  await goTo(page, '#/watch');
  await expect(page.getByRole('heading', { level: 1, name: 'Watch' })).toBeVisible();
  const focus = page.getByRole('button', { name: 'Focus', exact: true });
  await focus.click();
  await expect(focus).toHaveAttribute('aria-pressed', 'true');
  const tile = page.locator('.fd-wall[data-focus="true"] > .fd-wtile');
  const paused = page.getByRole('status').filter({ hasText: 'Paused' });
  await expect(tile).toHaveCount(1);
  await expect(paused).toHaveCount(0);
  // The two needs-you sessions take turns: the first tile, then the other one.
  const first = (await tile.getAttribute('data-session')) ?? '';
  await page.clock.runFor(FOCUS_MS);
  await expect(tile).not.toHaveAttribute('data-session', first);
  const second = (await tile.getAttribute('data-session')) ?? '';
  expect(second).not.toBe('');

  // The pointer on the tile: Focus waits.
  await tile.hover();
  await expect(paused).toBeVisible();
  await page.clock.runFor(FOCUS_MS * 2);
  await expect(tile).toHaveAttribute('data-session', second);
  // The pointer goes away: Focus moves on after one more FOCUS_MS.
  await page.mouse.move(0, 0);
  await expect(paused).toHaveCount(0);
  await page.clock.runFor(FOCUS_MS);
  await expect(tile).toHaveAttribute('data-session', first);

  // The keyboard in the tile: Focus waits; Shift+Tab out of it: Focus moves on.
  await focus.focus();
  await page.keyboard.press('Tab');
  await expect(tile.locator('a.fd-wtile-open')).toBeFocused();
  await expect(paused).toBeVisible();
  await page.clock.runFor(FOCUS_MS * 2);
  await expect(tile).toHaveAttribute('data-session', first);
  await page.keyboard.press('Shift+Tab');
  await expect(focus).toBeFocused();
  await expect(paused).toHaveCount(0);
  await page.clock.runFor(FOCUS_MS);
  await expect(tile).toHaveAttribute('data-session', second);

  // Focus off and on again with the pointer outside the tile: no old pause stays.
  await focus.click();
  await expect(focus).toHaveAttribute('aria-pressed', 'false');
  await focus.click();
  await expect(focus).toHaveAttribute('aria-pressed', 'true');
  await page.mouse.move(0, 0);
  await expect(paused).toHaveCount(0);
  const shown = (await tile.getAttribute('data-session')) ?? '';
  await page.clock.runFor(FOCUS_MS);
  await expect(tile).not.toHaveAttribute('data-session', shown);
});

test('8: a changed file of Properties focuses its diff once; a later turn end leaves the focus', async ({
  page,
}) => {
  desktopOnly();
  const st = 'st-alpha';
  const base = 'b'.repeat(40);
  const patch = ['src/a.ts', 'src/b.ts']
    .flatMap((file) => [
      `diff --git a/${file} b/${file}`,
      'index 1111111..2222222 100644',
      `--- a/${file}`,
      `+++ b/${file}`,
      '@@ -1,2 +1,2 @@',
      ' one',
      '-two',
      '+TWO',
    ])
    .join('\n');
  const finalized = (turn: string): Event => ({
    kind: 'turn.finalized',
    turn_id: turn,
    checkpoint: {
      worktree_id: 'wt',
      scope: 'tracked',
      start: { state: 'available', commit: `${turn}-start` },
      end: { state: 'available', commit: `${turn}-end` },
    },
    diff_summary: { files_changed: 2, insertions: 2, deletions: 2 },
  });
  const deck = new FakeDeck();
  deck.stations = [stationView(st, 'Alpha')];
  deck.sessions = [
    {
      station_id: st,
      status: 'idle',
      summary: summary('s1', 'idle', 1_760_000_001_000, {
        provider: 'codex',
        title: 'Edit the docs',
        capabilities: codexCaps(),
        cwd: '/home/me/projects/app',
        session_seq: 1,
        base: { oid: base, inferred: false },
      }),
    },
  ];
  const diffPath = `/api/stations/${st}/sessions/s1/diff`;
  deck.on(diffPath, () => ({
    status: 200,
    body: {
      session_id: 's1',
      base_oid: base,
      summary: { files_changed: 2, insertions: 2, deletions: 2 },
      patch,
      truncated: false,
    },
  }));
  deck.review(st, 's1', patch);
  await deck.install(page);
  await logIn(page);
  deck.emit(st, 's1', envelopes('s1', [finalized('t1')]));
  await openSession(page, 'Edit the docs');
  const view = page.getByRole('region', { name: 'Session' });
  const changes = view.getByRole('tab', { name: /^Changes/ });
  // The Changes tab loads the session diff: the Properties panel then lists its files.
  await changes.click();
  await expect(view.locator('[data-diff="since-spawn"]')).toBeVisible();
  await view.getByRole('tab', { name: 'Chat' }).click();
  const listed = page.getByRole('list', { name: 'Changed files' });
  await listed.getByRole('button', { name: 'src/b.ts' }).click();
  await expect(changes).toHaveAttribute('aria-selected', 'true');
  await expect(view.locator('.fd-dl-file[aria-label="src/b.ts"]')).toBeFocused();
  // A turn ends: the diff loads again (held until the Refresh button has the focus). The
  // request of the Properties panel is spent: the focus stays where the person put it.
  const release = deck.hold(diffPath);
  deck.emit(st, 's1', envelopes('s1', [finalized('t1'), finalized('t2')]).slice(1));
  const refresh = view.getByRole('button', { name: 'Refresh' });
  await expect(view.getByText('Loading the diff…')).toBeVisible();
  await refresh.focus();
  release();
  await expect(view.locator('[data-diff="since-spawn"]')).toBeVisible();
  await expect(view.locator('.fd-dl-file[aria-label="src/b.ts"]')).toBeVisible();
  await expect(refresh).toBeFocused();
});
