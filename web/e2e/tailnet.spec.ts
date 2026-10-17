// DL-178: the Tailnet card of Settings → Deck and the guided Add station dialog, on a
// desktop width and on a phone. The fake deck keeps the state in memory (`FakeDeck.tailnet`).
import { expect, type Page, test } from '@playwright/test';
import {
  ELEVATION,
  FakeDeck,
  fitsWidth,
  logIn,
  openAddDevice,
  PASSWORD,
  stationView,
} from './fake-deck';

const FINGERPRINT = '0123456789abcdef'.repeat(4);
const GROUPED = Array.from({ length: 4 }, () => '0123 4567 89ab cdef').join(' ');
const DIGEST = 'd'.repeat(64);

const REQUEST = {
  request_id: 'req-1',
  requested_name: 'lab-box',
  station_fingerprint: FINGERPRINT,
  offered_scopes: ['read', 'drive'],
  source_ip: '100.64.0.9',
  admitted_at: Date.now(),
  expires_at: Date.now() + 600_000,
  digest: DIGEST,
  tailscale: { host_name: 'lab-box', login: 'ann@example.com', tags: ['tag:lab'] },
};

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
    requests: [],
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

  test('Tailscale for Windows: the text, no "Use this program" button', async ({ page }) => {
    const { state } = await start(page);
    state.view.program = 'windows';
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    await expect(card).toContainText(
      'Using Tailscale for Windows. It runs as your Windows user, with your Windows environment, the same as in a Windows terminal.',
    );
    await expect(card).not.toContainText('Tailscale is not available');
    await expect(card).toContainText('Board address');
    await expect(card.getByRole('button', { name: 'Use this program' })).toHaveCount(0);
    expect(state.posted).toEqual([]);
    expect(await fitsWidth(page)).toBe(true);
  });

  test('the toggle is on when the key is absent', async ({ page }) => {
    await start(page);
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    await expect(card.getByRole('switch', { name: 'Use Tailscale automatically' })).toBeChecked();
    await expect(card).not.toContainText('Using Tailscale for Windows');
  });

  test('the toggle "Use Tailscale automatically" and its notes', async ({ page }) => {
    const { state } = await start(page);
    state.view.auto = {
      enabled: false,
      start_reason: 'Tailscale did not answer within 3 seconds.',
      skipped: [{ address: '100.64.0.9:7420', reason: 'the address is not on this machine' }],
      changed: true,
    };
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    const toggle = card.getByRole('switch', { name: 'Use Tailscale automatically' });
    await expect(toggle).not.toBeChecked();
    await expect(card).toContainText(
      'Keeps the tailnet addresses current at each start. Applies after a restart.',
    );
    await expect(card).toContainText(
      'Not applied at start: Tailscale did not answer within 3 seconds.',
    );
    await expect(card).toContainText(
      '100.64.0.9:7420 not used: the address is not on this machine',
    );
    await expect(card).toContainText('Your tailnet address changed. Restart the deck to use it.');
    // On: the confirmation, then the save.
    await toggle.click();
    await confirm(page);
    await expect(toggle).toBeChecked();
    await expect(card).toContainText('Saved. The tailnet addresses are set at the next start');
    expect(state.posted).toEqual([
      {
        path: '/api/settings/deck',
        body: { changes: { tailnet_auto: true }, revision: 'rev-e2e', elevation: ELEVATION },
      },
    ]);
    // Off: no confirmation dialog.
    await toggle.click();
    await expect(toggle).not.toBeChecked();
    await expect(page.getByRole('dialog', { name: 'Confirm it is you' })).toHaveCount(0);
    expect(state.posted[1]).toEqual({
      path: '/api/settings/deck',
      body: { changes: { tailnet_auto: false }, revision: 'rev-e2e', elevation: ELEVATION },
    });
    expect(await fitsWidth(page)).toBe(true);
  });

  test('the Trusted certificate section shows the state and saves the toggle', async ({ page }) => {
    const { state } = await start(page);
    state.view.certificate = { enabled: false, state: 'off' };
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    const toggle = card.getByRole('switch', { name: 'Trusted certificate from Tailscale' });
    await expect(toggle).not.toBeChecked();
    await expect(card).toContainText(
      'Browsers trust the board on its tailnet name. The name becomes public in Certificate Transparency logs.',
    );
    await expect(card).not.toContainText('Trusted certificate for');
    // On: the confirmation, then the save. The deck starts getting the certificate.
    await toggle.click();
    await confirm(page);
    await expect(toggle).toBeChecked();
    await expect(card).toContainText('Getting the certificate…');
    expect(state.posted).toEqual([
      {
        path: '/api/settings/deck',
        body: { changes: { tailscale_cert: true }, revision: 'rev-e2e', elevation: ELEVATION },
      },
    ]);
    expect(await fitsWidth(page)).toBe(true);
  });

  test('the card says when the certificate is valid and why it failed', async ({ page }) => {
    const { state } = await start(page);
    const end = Date.UTC(2027, 0, 15, 12);
    state.view.certificate = {
      enabled: true,
      state: 'valid',
      name: 'deck.tail1234.ts.net',
      not_after: end,
    };
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    const date = await page.evaluate((ms) => new Date(ms).toLocaleDateString(), end);
    await expect(card).toContainText(
      `Trusted certificate for deck.tail1234.ts.net, valid until ${date}`,
    );
    await expect(
      card.getByRole('switch', { name: 'Trusted certificate from Tailscale' }),
    ).toBeChecked();
    expect(await fitsWidth(page)).toBe(true);
  });

  test('the card shows the failure text of a reason code', async ({ page }) => {
    const { state } = await start(page);
    state.view.certificate = { enabled: true, state: 'failed', reason_code: 'https_disabled' };
    await go(page, '#/settings');
    await expect(page.getByRole('region', { name: 'Tailnet' })).toContainText(
      'HTTPS certificates are off for your tailnet. Turn them on in the Tailscale admin console → DNS → HTTPS Certificates.',
    );
  });

  test('the owner certificate files note shows above the switch', async ({ page }) => {
    const { state } = await start(page);
    state.view.certificate = { enabled: true, state: 'valid', owner_files: true };
    await go(page, '#/settings');
    const card = page.getByRole('region', { name: 'Tailnet' });
    const note = card.locator('[data-note="owner-files"]');
    await expect(note).toHaveText(
      'Your own certificate files (board.crt) are used for every name. Remove them to use the Tailscale certificate.',
    );
    expect(await fitsWidth(page)).toBe(true);
  });

  test('the Add a device dialog selects the trusted entry first', async ({ page }) => {
    const { deck } = await start(page);
    const sha = 'ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12';
    deck.on('/api/auth/device-link', () => ({
      status: 200,
      body: {
        links: [
          {
            label: 'Network 192.168.1.5',
            url: 'https://192.168.1.5:7420/#login=tok',
            certificate_sha256: sha,
            trust: 'self_signed',
          },
          {
            label: 'Tailnet deck.tail1234.ts.net',
            url: 'https://deck.tail1234.ts.net:7420/#login=tok',
            certificate_sha256: 'cd'.repeat(32),
            trust: 'trusted',
          },
        ],
        expires_at: Date.now() + 5 * 60_000,
        certificate_sha256: sha,
        self_signed: true,
      },
    }));
    if ((page.viewportSize()?.width ?? 0) < 600) {
      await page
        .getByRole('navigation', { name: 'Board' })
        .getByRole('link', { name: 'More' })
        .click();
    }
    await openAddDevice(page);
    const dialog = page.getByRole('dialog', { name: 'Add a device' });
    await expect(dialog.getByRole('textbox', { name: 'Login link' })).toHaveValue(
      'https://deck.tail1234.ts.net:7420/#login=tok',
    );
    await expect(dialog.getByRole('combobox', { name: 'Board address' })).toHaveValue('1');
    await expect(dialog).toContainText('Trusted');
    await expect(dialog).not.toContainText('check that the fingerprint starts with');
    expect(await fitsWidth(page)).toBe(true);
  });

  test('Add station: start screen, choose a computer, run the command, confirm the code', async ({
    page,
  }) => {
    const { state } = await start(page);
    await go(page, '#/stations');
    await page.getByRole('button', { name: 'Add station' }).click();
    const dialog = page.getByRole('dialog', { name: 'Add station' });
    await expect(dialog).toContainText('Where is the new station?');
    const choice = dialog.getByRole('button', { name: /A computer on my tailnet/ });
    await expect(choice).toContainText('1 computer can be added');
    await expect(dialog.getByRole('button', { name: /one-time invitation/ })).toBeVisible();
    await choice.click();

    // Step 1: the list. The paired device is folded away; the window is still closed.
    await expect(dialog).toContainText('Choose the computer that will run the station.');
    const row = dialog.locator('[data-device="lab-box.tail1234.ts.net"]');
    await expect(row).toContainText('lab-box');
    await expect(row).toContainText('Linux');
    await expect(row).toHaveAttribute('title', '100.64.0.9');
    await expect(dialog.getByText('Already stations (1)')).toBeVisible();
    await expect(dialog).not.toContainText('Accepting new stations');
    expect(await fitsWidth(page)).toBe(true);

    // Add opens the window (with the elevation) and shows the command.
    await row.getByRole('button', { name: 'Add' }).click();
    await confirm(page);
    expect(state.posted[0]).toEqual({
      path: '/api/pairing/window',
      body: { open: true, elevation: ELEVATION },
    });
    await expect(dialog).toContainText('Accepting new stations');
    await expect(dialog).toContainText('Run this command on lab-box');
    await expect(dialog.locator('.fd-add-command code')).toContainText(
      'fleetdeck station join --tailnet',
    );
    await expect(dialog.getByRole('button', { name: 'Copy', exact: true })).toBeVisible();
    await expect(dialog.getByRole('status')).toContainText('Waiting for lab-box to ask to join…');
    expect(await fitsWidth(page)).toBe(true);

    // The station asks to join: step 3 shows the code in groups and the plain scopes.
    state.requests.push(REQUEST);
    const request = dialog.locator('article.fd-pair-request');
    await expect(request).toContainText('lab-box asks to join this deck', { timeout: 8_000 });
    await expect(request.locator('.fd-fingerprint')).toHaveText(GROUPED);
    await expect(request).toContainText('See its sessions and files');
    await expect(request).toContainText('Start sessions and send prompts');
    expect(await fitsWidth(page)).toBe(true);
    await request.getByRole('button', { name: 'Yes, the codes match. Add lab-box' }).click();
    await expect(dialog).toContainText(
      '✓ lab-box is added. It connects to this deck in a few seconds.',
    );
    const approve = state.posted.find((p) => p.path === '/api/pairing/requests/req-1/approve');
    expect(approve?.body).toEqual({
      fingerprint: FINGERPRINT,
      digest: DIGEST,
      elevation: ELEVATION,
    });
    await dialog.getByRole('button', { name: 'Add another station' }).click();
    await expect(dialog).toContainText('Choose the computer that will run the station.');

    // Stop closes the window.
    await dialog.getByRole('button', { name: 'Stop' }).click();
    await expect(dialog).not.toContainText('Accepting new stations');
    expect(state.posted.at(-1)).toEqual({ path: '/api/pairing/window', body: { open: false } });
  });

  test('Add station: refuse a request, and Back to the start screen', async ({ page }) => {
    const { state } = await start(page);
    await go(page, '#/stations');
    await page.getByRole('button', { name: 'Add station' }).click();
    const dialog = page.getByRole('dialog', { name: 'Add station' });
    await dialog.getByRole('button', { name: /A computer on my tailnet/ }).click();
    await dialog
      .locator('[data-device="lab-box.tail1234.ts.net"]')
      .getByRole('button', { name: 'Add' })
      .click();
    await confirm(page);
    state.requests.push(REQUEST);
    await dialog.getByRole('button', { name: 'No, refuse' }).click({ timeout: 8_000 });
    await expect(dialog).toContainText('lab-box was not added.');
    expect(state.posted.some((p) => p.path === '/api/pairing/requests/req-1/deny')).toBe(true);
    await dialog.getByRole('button', { name: 'Back to the list' }).click();
    await dialog.getByRole('button', { name: '← Back' }).click();
    await expect(dialog).toContainText('Where is the new station?');
  });

  test('Add station: Tailscale is not available', async ({ page }) => {
    const { state } = await start(page);
    state.view.available = false;
    state.view.reason = 'tailscale is not installed';
    await go(page, '#/stations');
    await page.getByRole('button', { name: 'Add station' }).click();
    const dialog = page.getByRole('dialog', { name: 'Add station' });
    const choice = dialog.getByRole('button', { name: /A computer on my tailnet/ });
    await expect(choice).toContainText('Tailscale is not available on this deck');
    await choice.click();
    await expect(dialog).toContainText(
      'Tailscale is not available on this deck: tailscale is not installed.',
    );
    await dialog.getByRole('button', { name: 'Use an invitation instead' }).click();
    await expect(dialog).toContainText('Create a one-time invitation.');
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
