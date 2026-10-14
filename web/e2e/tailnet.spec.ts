// DL-178: the Tailnet card of Settings → Deck and the "On your tailnet" tab of Add station, on a
// desktop width and on a phone. The fake deck keeps the state in memory (`FakeDeck.tailnet`).
import { expect, type Page, test } from '@playwright/test';
import { ELEVATION, FakeDeck, fitsWidth, logIn, PASSWORD, stationView } from './fake-deck';

const FINGERPRINT = '0123456789abcdef'.repeat(4);
const GROUPED = Array.from({ length: 4 }, () => '0123 4567 89ab cdef').join(' ');
const DIGEST = 'd'.repeat(64);

async function start(page: Page) {
  const deck = new FakeDeck();
  deck.stations = [stationView('st-alpha', 'Alpha')];
  const state = deck.tailnet({
    devices: [
      {
        host_name: 'lab-box',
        dns_name: 'lab-box.tail1234.ts.net',
        ips: ['100.64.0.9'],
        os: 'linux',
        tags: ['tag:lab'],
        online: true,
        state: 'none',
      },
      {
        host_name: 'old-laptop',
        dns_name: 'old-laptop.tail1234.ts.net',
        ips: ['100.64.0.10'],
        tags: [],
        online: false,
        state: 'paired',
      },
    ],
    requests: [
      {
        request_id: 'req-1',
        requested_name: 'lab-box',
        station_fingerprint: FINGERPRINT,
        offered_scopes: ['read', 'drive'],
        source_ip: '100.64.0.9',
        admitted_at: Date.now(),
        expires_at: Date.now() + 600_000,
        digest: DIGEST,
        tailscale: { host_name: 'lab-box', login: 'ann@example.com', tags: ['tag:lab'] },
      },
    ],
  });
  await deck.install(page);
  await logIn(page);
  await expect(
    page.getByRole('navigation', { name: 'Board' }).or(page.locator('nav')).first(),
  ).toBeAttached();
  return { deck, state };
}

async function go(page: Page, hash: string) {
  await page.evaluate((h) => {
    window.location.hash = h;
  }, hash);
}

async function confirm(page: Page) {
  const dialog = page.getByRole('dialog', { name: 'Confirm it is you' });
  await dialog.getByLabel('Password').fill(PASSWORD);
  await dialog.getByRole('button', { name: 'Confirm' }).click();
  await expect(dialog).toHaveCount(0);
}

function suite() {
  test('Use tailnet asks for the elevation and shows the restart text', async ({ page }) => {
    const { state } = await start(page);
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    await expect(card).toContainText('This deck on the tailnet: deck.tail1234.ts.net (100.64.0.1)');
    await expect(card).toContainText('127.0.0.1:7420');
    await expect(card).toContainText('100.64.0.1:7421');
    await card.getByRole('button', { name: 'Use tailnet' }).click();
    await confirm(page);
    await expect(card).toContainText('Saved. The new listeners start after a restart of the deck.');
    expect(state.posted).toEqual([{ path: '/api/tailnet/use', body: { elevation: ELEVATION } }]);
    // The card reloads: the managed addresses are now in use, so the button is gone.
    await expect(card).toContainText('The deck listens on its tailnet addresses.');
    await expect(card.getByRole('button')).toHaveCount(0);
    expect(await fitsWidth(page)).toBe(true);
  });

  test('Add station: the window, a device command, and approving a request', async ({ page }) => {
    const { state } = await start(page);
    await go(page, '#/stations');
    await page.getByRole('button', { name: 'Add station' }).click();
    const dialog = page.getByRole('dialog', { name: 'Add station' });
    await dialog.getByRole('tab', { name: 'On your tailnet' }).click();
    await expect(dialog.getByRole('tab', { name: 'On your tailnet' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    // The window is closed: no command yet, and no request is shown.
    await expect(dialog).toContainText('Open the window to get the station command.');
    await expect(dialog).toContainText('No waiting requests.');
    await dialog.getByRole('button', { name: 'Accept pairing requests for 10 minutes' }).click();
    await confirm(page);
    await expect(dialog).toContainText('Accepting pairing requests until');
    expect(state.posted[0]).toEqual({
      path: '/api/pairing/window',
      body: { open: true, elevation: ELEVATION },
    });
    // Now the device shows its command with a Copy button; the paired device shows "Paired".
    const device = dialog.locator('[data-device="lab-box.tail1234.ts.net"]');
    await expect(device.locator('code')).toContainText('fleetdeck station join --tailnet');
    await expect(device.getByRole('button', { name: 'Copy' })).toBeVisible();
    await expect(dialog.locator('[data-device="old-laptop.tail1234.ts.net"]')).toContainText(
      'Paired',
    );
    // The window is open: the waiting request shows the grouped fingerprint.
    const request = dialog.locator('article.fd-pair-request');
    await expect(request).toContainText('Requested name: lab-box');
    await expect(request.locator('.fd-fingerprint')).toHaveText(GROUPED);
    await expect(request).toContainText('Compare this fingerprint with the one');
    expect(await fitsWidth(page)).toBe(true);
    await request.getByRole('button', { name: 'Approve' }).click();
    await expect(dialog).toContainText('Paired lab-box. Waiting for the station to connect.');
    const approve = state.posted.find((p) => p.path === '/api/pairing/requests/req-1/approve');
    expect(approve?.body).toEqual({
      fingerprint: FINGERPRINT,
      digest: DIGEST,
      elevation: ELEVATION,
    });
    await expect(dialog).toContainText('No waiting requests.');
  });
}

test.describe('desktop width', () => {
  test.use({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });
  suite();
});

test.describe('phone width', () => {
  test.use({ viewport: { width: 390, height: 844 } });
  suite();
});
