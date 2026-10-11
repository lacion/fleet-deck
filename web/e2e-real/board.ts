// The phone board steps that the real-process flows share (P5-4): one phone
// page, one deck and its stations.
import {
  type BrowserContext,
  type Browser,
  expect,
  type Locator,
  type Page,
  type WebSocketRoute,
} from '@playwright/test';
import { type Deck, type Station } from './fleet';

/** A phone page with its own browser context. The board sockets are routed
 * through the test: Chromium's offline mode stops new requests but keeps open
 * sockets, so a network-drop step closes them itself. */
export class Phone {
  readonly sockets = new Set<WebSocketRoute>();

  private constructor(
    readonly context: BrowserContext,
    readonly page: Page,
    private readonly deck: Deck,
  ) {}

  static async open(browser: Browser, deck: Deck): Promise<Phone> {
    const context = await browser.newContext({
      viewport: { width: 390, height: 844 },
      isMobile: true,
      hasTouch: true,
      ignoreHTTPSErrors: true,
    });
    const phone = new Phone(context, await context.newPage(), deck);
    await context.routeWebSocket('**/api/ws', (ws) => {
      const server = ws.connectToServer();
      phone.sockets.add(ws);
      ws.onClose(() => {
        phone.sockets.delete(ws);
        void server.close();
      });
      server.onClose(() => {
        phone.sockets.delete(ws);
        void ws.close();
      });
    });
    return phone;
  }

  status(text: string): Locator {
    return this.page.getByRole('status').filter({ hasText: text });
  }

  async live(): Promise<void> {
    await expect(this.status('Connected · Live fleet')).toBeVisible();
  }

  card(sessionId: string): Locator {
    return this.page.locator(`.fd-card[data-session="${sessionId}"]`);
  }

  stationCard(name: string): Locator {
    return this.page
      .locator('.fd-station[data-station]')
      .filter({ has: this.page.getByRole('heading', { name }) });
  }

  /** The page fits the phone width: no horizontal scroll. */
  fitsWidth(): Promise<boolean> {
    return this.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
  }

  async fleet(): Promise<void> {
    await this.page.getByRole('button', { name: 'Fleet', exact: true }).click();
  }

  /** Logs in with the deck's login link and pairs `listening` (the deck dials
   * it) and `dialing` (it dials the deck). */
  async pair(listening: Station, dialing: Station): Promise<void> {
    const page = this.page;
    await page.goto(this.deck.loginLink());
    await this.live();
    await page.getByRole('button', { name: 'Stations (0)' }).click();
    await page.getByRole('button', { name: 'Add station' }).click();
    const dialog = page.getByRole('dialog', { name: 'Add station' });
    await dialog.getByRole('button', { name: 'Create invitation' }).click();
    const invitation = dialog.getByLabel('Invitation (secret, single use)');
    await expect(invitation).not.toHaveValue('');
    dialing.cli(['station', 'join', '--stdin'], await invitation.inputValue());
    await dialog.getByLabel('This deck dials the station').check();
    await dialog
      .getByLabel('Station invitation')
      .fill(listening.cli(['station', 'invite', '--listen', '127.0.0.1:0']));
    await dialog.getByRole('button', { name: 'Pair station' }).click();
    await expect(dialog.getByRole('status')).toHaveText(`Paired station ${listening.name}.`);
    await dialog.getByRole('button', { name: 'Close' }).click();
    await expect(page.getByRole('button', { name: 'Stations (2)' })).toBeVisible();
  }

  /** Starts a session from the spawn form with the provider whose label is
   * `provider` and returns its id. */
  async spawn(station: Station, provider: string, title: string, prompt: string): Promise<string> {
    const page = this.page;
    await page.getByRole('button', { name: 'New session' }).click();
    const form = page.getByRole('region', { name: 'New session' });
    await form
      .getByRole('combobox', { name: 'Station', exact: true })
      .selectOption({ label: station.name });
    await form.getByRole('combobox', { name: 'Provider' }).selectOption({ label: provider });
    await form.getByRole('button', { name: 'Browse the station' }).click();
    await form.getByRole('button', { name: `Open ${station.home}/projects` }).click();
    await form.getByRole('button', { name: 'Open demo' }).click();
    // Wait for the demo listing: "Use this directory" takes the path on
    // screen, and the projects listing has the same button (DL-069).
    await expect(form.locator('.fd-path')).toHaveText(station.workspace);
    await form.getByRole('button', { name: 'Use this directory' }).click();
    await expect(form).toContainText(`Chosen: ${station.workspace}`);
    await form.getByLabel('Title (optional)').fill(title);
    await form.getByLabel('First prompt (optional)').fill(prompt);
    expect(await this.fitsWidth()).toBe(true);
    await form.getByRole('button', { name: 'Start session' }).click();
    await expect(this.status('Done.')).toBeVisible();
    await this.fleet();
    const card = page.locator('.fd-card').filter({ hasText: title });
    await expect(card).toHaveCount(1);
    const id = await card.getAttribute('data-session');
    if (id === null) throw new Error(`no card for ${title}`);
    return id;
  }

  async open(title: string): Promise<Locator> {
    await this.fleet();
    await this.page.getByRole('button', { name: `Open session ${title}` }).click();
    return this.page.getByRole('main', { name: 'Session' });
  }

  /** The deck's command rows: the expected count per method, every one done
   * after one delivery with a distinct command id. */
  expectRouted(expected: Record<string, number>): void {
    const rows = this.deck.commands();
    const counts: Record<string, number> = {};
    for (const row of rows) counts[row.method] = (counts[row.method] ?? 0) + 1;
    expect(counts).toEqual(expected);
    expect(new Set(rows.map((row) => row.command_id)).size).toBe(rows.length);
    for (const row of rows)
      expect([row.method, row.state, row.attempts]).toEqual([row.method, 'done', 1]);
  }
}

/** The station's own session list (the station is the source of truth). */
export function stationSessions(
  station: Station,
): { session_id: string; provider: string | null; provider_instance: string | null }[] {
  return JSON.parse(station.cli(['station', 'sessions', '--json']));
}
