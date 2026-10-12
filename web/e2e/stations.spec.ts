// M2 P4-2: the stations screen and the pairing dialog in a phone browser.
import { expect, test } from '@playwright/test';
import { CSRF, FakeDeck, fitsWidth, logIn, openMore, stationView } from './fake-deck';

const reported = stationView('st-alpha', 'Alpha', {
  info: {
    name: 'alpha-host',
    incarnation: 'inc-1',
    head: 40,
    live_sessions: 1,
    version: '0.2.0',
    max_sessions: 4,
    providers: [
      {
        instance: 'personal',
        provider: 'claude',
        version: '2.1.5',
        auth: 'signed_in',
        capabilities: null,
      },
      { instance: 'work', provider: 'codex', version: null, auth: 'unknown', capabilities: null },
    ],
  },
  info_at: 1_760_000_000_000,
});

test('an admin sees the station report and makes an invitation for a dialing station', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [reported];
  deck.on('/api/stations/invite', () => ({
    status: 200,
    body: {
      invitation: 'fdinv1_secret_e2e',
      expires_at: Date.UTC(2026, 9, 11, 12, 15),
      join_command: 'fleetdeck station join',
    },
  }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  const card = page.locator('[data-station="st-alpha"]');
  await expect(card).toContainText('1 live of 4 sessions');
  await expect(card).toContainText('0.2.0');
  await expect(card).toContainText('personal · claude · 2.1.5 · signed in');
  await expect(card).toContainText('work · codex · version unknown · sign-in unknown');
  expect(await fitsWidth(page)).toBe(true);

  await page.getByRole('button', { name: 'Add station' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add station' });
  await dialog.getByRole('button', { name: 'Create invitation' }).click();
  await expect(dialog.getByLabel('Invitation (secret, single use)')).toHaveValue(
    'fdinv1_secret_e2e',
  );
  await expect(dialog).toContainText('fleetdeck station join');
  await expect(dialog).toContainText('Expires: 2026-10-11 12:15 UTC');
  expect(await fitsWidth(page)).toBe(true);
  const call = deck.calls.find((c) => c.path === '/api/stations/invite');
  expect(call).toEqual({ path: '/api/stations/invite', body: {}, csrf: CSRF });

  // Closing the dialog drops the secret from the page.
  await dialog.getByRole('button', { name: 'Close' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await page.content()).not.toContain('fdinv1_secret_e2e');
});

test('an admin pairs a listening station, sees a refusal, and removes a station', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [reported];
  let taken = true;
  deck.on('/api/stations', (body) => {
    if (taken) {
      taken = false;
      return {
        status: 409,
        body: { code: 'name_taken', message: 'A station named alpha-box exists.' },
      };
    }
    const added = stationView('st-box', 'alpha-box', {
      direction: 'deck_dials',
      dial_url: '192.168.1.20:7443',
    });
    deck.stations = [...deck.stations, added];
    queueMicrotask(() => deck.publish());
    expect(body).toEqual({ invitation: 'fdinv1_from_station', name: 'alpha-box' });
    return { status: 200, body: added };
  });
  deck.on('/api/stations/Alpha/remove', () => {
    deck.stations = deck.stations.filter((s) => s.name !== 'Alpha');
    queueMicrotask(() => deck.publish());
    return { status: 200, body: reported };
  });
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  await page.getByRole('button', { name: 'Add station' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add station' });
  await dialog.getByLabel('This deck dials the station').check();
  const pair = dialog.getByRole('button', { name: 'Pair station' });
  await expect(pair).toBeDisabled();
  await dialog.getByLabel('Station invitation').fill('fdinv1_from_station');
  await dialog.getByLabel('Name on this deck (optional)').fill(' alpha-box ');
  await pair.click();
  await expect(dialog.getByRole('alert')).toHaveText('A station named alpha-box exists.');
  // The input stays for a retry after a refusal.
  await expect(dialog.getByLabel('Station invitation')).toHaveValue('fdinv1_from_station');
  await pair.click();
  await expect(dialog.getByRole('status')).toHaveText('Paired station alpha-box.');
  await expect(dialog.getByLabel('Station invitation')).toHaveValue('');
  await dialog.getByRole('button', { name: 'Close' }).click();
  const box = page.locator('[data-station="st-box"]');
  await expect(box).toContainText('Deck dials station');
  await expect(box).toContainText('192.168.1.20:7443');
  await expect(box).toContainText('Not reported until the station links to this deck.');

  const alpha = page.locator('[data-station="st-alpha"]');
  await alpha.getByRole('button', { name: 'Remove station' }).click();
  await alpha.getByRole('button', { name: 'Cancel' }).click();
  expect(deck.calls.some((c) => c.path.endsWith('/remove'))).toBe(false);
  await alpha.getByRole('button', { name: 'Remove station' }).click();
  await alpha.getByRole('button', { name: 'Confirm remove' }).click();
  await expect(alpha).toHaveCount(0);
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  await expect(
    page
      .getByRole('navigation', { name: 'More pages' })
      .getByRole('link', { name: 'Stations · 1' }),
  ).toBeVisible();
  expect(await fitsWidth(page)).toBe(true);
});

test('a login without the admin scope sees the report but no pairing controls', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.stations = [reported];
  deck.scopes = ['read', 'drive', 'approve'];
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  await expect(page.getByText('Pairing needs the admin scope.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Add station' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Remove station' })).toHaveCount(0);
  await expect(page.locator('[data-station="st-alpha"]')).toContainText('1 live of 4 sessions');
});
