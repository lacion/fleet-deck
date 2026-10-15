// M2 P4-2: the stations screen and the pairing dialog in a phone browser.
import { expect, test } from '@playwright/test';
import type { WorktreeInfo } from '../src/gen';
import { CSRF, FakeDeck, fitsWidth, logIn, openMore, stationView } from './fake-deck';

const reported = stationView('st-alpha', 'Alpha', {
  info: {
    name: 'alpha-host',
    incarnation: 'inc-1',
    head: 40,
    live_sessions: 1,
    version: '0.2.0',
    max_sessions: 4,
    allow_shell: null,
    providers: [
      {
        instance: 'personal',
        provider: 'claude',
        version: '2.1.5',
        auth: 'signed_in',
        version_status: 'unknown',
        capabilities: null,
      },
      {
        instance: 'work',
        provider: 'codex',
        version: null,
        auth: 'unknown',
        capabilities: null,
        version_status: 'unknown',
      },
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
      expires_at: Date.now() + 15 * 60_000,
      join_command: 'fleetdeck station join',
    },
  }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  const card = page.locator('[data-station="st-alpha"]');
  await expect(card).toContainText('Link up');
  await expect(card).toContainText('0 of 4 sessions');
  await expect(card.locator('.fd-load-bar')).toBeVisible();
  // The version, the ids and the key sit in the Details fold.
  await expect(card.getByText('Station ID')).toBeHidden();
  await card.getByText('Details', { exact: true }).click();
  await expect(card.locator('.fd-facts')).toContainText('0.2.0');
  await expect(card.locator('.fd-facts')).toContainText('st-alpha');
  await expect(card.locator('[data-instance="personal"]')).toHaveText('Claude 2.1.5signed in');
  await expect(card.locator('[data-instance="work"]')).toHaveText('Codexsign-in unknown');
  expect(await fitsWidth(page)).toBe(true);

  await page.getByRole('button', { name: 'Add station' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add station' });
  await expect(dialog).toContainText('Where is the new station?');
  await dialog.getByRole('button', { name: /Any computer, with a one-time invitation/ }).click();
  await dialog.getByRole('button', { name: 'Create invitation' }).click();
  await expect(dialog).toContainText('1. On the new computer, run:');
  await expect(dialog.locator('.fd-add-command code')).toHaveText('fleetdeck station join');
  // The invitation is masked until the owner shows it.
  await expect(dialog).toContainText('••••••••••••');
  await expect(dialog.getByLabel('Invitation (secret, single use)')).toHaveCount(0);
  expect(await page.content()).not.toContain('fdinv1_secret_e2e');
  await expect(dialog).toContainText('Keep it secret. Anyone with it can add a station until');
  await expect(dialog.getByRole('status')).toContainText('Waiting for the station to join…');
  await dialog.getByRole('button', { name: 'Show' }).click();
  await expect(dialog.getByLabel('Invitation (secret, single use)')).toHaveValue(
    'fdinv1_secret_e2e',
  );
  expect(await fitsWidth(page)).toBe(true);
  const call = deck.calls.find((c) => c.path === '/api/stations/invite');
  expect(call).toEqual({ path: '/api/stations/invite', body: {}, csrf: CSRF });

  // A new station on the board means the invitation was used: the secret is dropped.
  deck.stations = [...deck.stations, stationView('st-new', 'new-box')];
  deck.publish();
  await expect(dialog).toContainText('✓ new-box joined this deck.');
  await expect(dialog.getByLabel('Invitation (secret, single use)')).toHaveCount(0);
  expect(await page.content()).not.toContain('fdinv1_secret_e2e');
  await dialog.getByRole('button', { name: 'Add another station' }).click();
  await expect(dialog.getByRole('button', { name: 'Create invitation' })).toBeVisible();

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
    expect(body).toEqual({ invitation: 'fdinv1_from_station', name: null });
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
  await dialog.getByText('Other ways').click();
  await dialog.getByRole('button', { name: 'Connect to a station that listens' }).click();
  const pair = dialog.getByRole('button', { name: 'Add station' });
  await expect(pair).toBeDisabled();
  await dialog.getByLabel('Station invitation').fill('fdinv1_from_station');
  await pair.click();
  await expect(dialog.getByRole('alert')).toHaveText('A station named alpha-box exists.');
  // The input stays for a retry after a refusal.
  await expect(dialog.getByLabel('Station invitation')).toHaveValue('fdinv1_from_station');
  await pair.click();
  await expect(dialog.getByRole('status')).toHaveText('✓ alpha-box is added.');
  await expect(dialog.getByLabel('Station invitation')).toHaveValue('');
  await dialog.getByRole('button', { name: 'Close' }).click();
  const box = page.locator('[data-station="st-box"]');
  await expect(box).toContainText('Deck dials station');
  await expect(box).toContainText('192.168.1.20:7443');
  await expect(box).toContainText('Reported when the station links to this deck.');

  const alpha = page.locator('[data-station="st-alpha"]');
  // Remove sits in the overflow menu; the confirm shows only after it.
  await expect(alpha.getByRole('button', { name: 'Confirm remove' })).toHaveCount(0);
  await alpha.getByLabel('Station actions').click();
  await alpha.getByRole('button', { name: 'Remove station…' }).click();
  await alpha.getByRole('button', { name: 'Cancel' }).click();
  expect(deck.calls.some((c) => c.path.endsWith('/remove'))).toBe(false);
  await alpha.getByLabel('Station actions').click();
  await alpha.getByRole('button', { name: 'Remove station…' }).click();
  await alpha.getByRole('button', { name: 'Confirm remove' }).click();
  await expect(alpha).toHaveCount(0);
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  await expect(
    page
      .getByRole('navigation', { name: 'More pages' })
      .getByRole('link', { name: /^Stations\s*1$/ }),
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
  await expect(page.getByLabel('Station actions')).toHaveCount(0);
  await expect(page.locator('[data-station="st-alpha"]')).toContainText('0 of 4 sessions');
});

const worktree = (extra: Partial<WorktreeInfo>): WorktreeInfo => ({
  session_id: 's1',
  title: 'Fix the login',
  path: '/w/deck-wt/s1',
  branch: 'fd/s1',
  main: '/w/deck',
  exists: true,
  registered: true,
  dirty: 0,
  ignored: 3,
  unpushed: 0,
  compared_with: 'remotes',
  last_commit: { subject: 'Fix it', at: Date.now() - 2.5 * 3_600_000 },
  live: false,
  verdict: 'clean',
  note: null,
  ...extra,
});

test('the worktree list of a station shows the evidence and copies the remove command', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  const deck = new FakeDeck();
  deck.stations = [reported];
  deck.on('/api/stations/Alpha/worktrees', () => ({
    status: 200,
    body: {
      worktrees: [
        worktree({}),
        worktree({
          session_id: 's2',
          title: null,
          path: "/w/deck-wt/it's",
          branch: 'topic',
          dirty: 4,
          unpushed: 2,
          verdict: 'has_work',
        }),
      ],
    },
  }));
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  const card = page.locator('[data-station="st-alpha"]');
  const section = card.locator('details.fd-wt');
  await expect(section).not.toHaveAttribute('open', '');
  expect(deck.calls.some((c) => c.path.endsWith('/worktrees'))).toBe(false);
  await section.locator('summary').click();
  await expect(section.locator('summary')).toHaveText('Worktrees (2)');
  const rows = section.locator('li.fd-wt-row');
  await expect(rows).toHaveCount(2);
  const clean = rows.nth(0);
  await expect(clean).toHaveAttribute('data-verdict', 'clean');
  await expect(clean.locator('.fd-badge').first()).toHaveText('Clean');
  await expect(clean).toContainText('0 changed · 3 ignored · 0 commits not on a remote');
  await expect(clean).toContainText('last commit “Fix it” 2h');
  await expect(clean.getByRole('link', { name: 'Fix the login' })).toHaveAttribute(
    'href',
    '#/s/st-alpha/s1',
  );
  const work = rows.nth(1);
  await expect(work).toHaveAttribute('data-verdict', 'has_work');
  await expect(work.locator('.fd-badge').first()).toHaveText('Has work');
  await expect(work.getByRole('link', { name: 's2' })).toBeVisible();
  await expect(section).toContainText(
    'Move to trash keeps every file; Restore puts the worktree back.',
  );
  const copy = section.getByRole('button', { name: 'Copy the remove command' });
  await expect(copy).toHaveCount(2);
  await copy.first().click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    "git -C '/w/deck' worktree remove '/w/deck-wt/s1' && git -C '/w/deck' branch -d 'fd/s1'",
  );
  await copy.nth(1).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    "git -C '/w/deck' worktree remove '/w/deck-wt/it'\\''s'",
  );
  expect(await fitsWidth(page)).toBe(true);
  // Refresh loads again.
  await section.getByRole('button', { name: 'Refresh' }).click();
  await expect(rows).toHaveCount(2);
  expect(deck.calls.filter((c) => c.path.endsWith('/worktrees'))).toHaveLength(2);
});

test('the worktree list says why it is empty or refused', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [reported];
  let reply: { status: number; body: unknown } = {
    status: 403,
    body: { code: 'owner_only', message: 'Only the owner lists worktrees.' },
  };
  deck.on('/api/stations/Alpha/worktrees', () => reply);
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  const section = page.locator('[data-station="st-alpha"] details.fd-wt');
  await section.locator('summary').click();
  await expect(section).toContainText('Only the owner can see worktrees.');
  reply = { status: 502, body: { code: 'station_error', message: 'no such method' } };
  await section.getByRole('button', { name: 'Refresh' }).click();
  await expect(section).toContainText(
    'This station does not report worktrees. Update the station.',
  );
  reply = { status: 504, body: { code: 'timeout', message: 'The station did not answer.' } };
  await section.getByRole('button', { name: 'Refresh' }).click();
  await expect(section.getByRole('alert')).toHaveText('The station did not answer.');
  reply = { status: 200, body: { worktrees: [] } };
  await section.getByRole('button', { name: 'Refresh' }).click();
  await expect(section).toContainText(
    'No worktrees. A session started with “A new worktree” gets one.',
  );
});

test('a login without the admin scope has no worktree section', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [reported];
  deck.scopes = ['read', 'drive', 'approve'];
  await deck.install(page);
  await logIn(page);
  await openMore(page, 'Stations');
  await expect(page.locator('[data-station="st-alpha"]')).toContainText('0 of 4 sessions');
  await expect(page.locator('details.fd-wt')).toHaveCount(0);
});

test('More shows the deck version when the login answer has one', async ({ page }) => {
  const deck = new FakeDeck();
  deck.deckVersion = '0.7.3';
  await deck.install(page);
  await logIn(page);
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  await expect(page.getByText('deck 0.7.3')).toHaveAttribute('title', 'Deck version');
});

test('More shows no version for an older deck', async ({ page }) => {
  const deck = new FakeDeck();
  await deck.install(page);
  await logIn(page);
  await page.getByRole('navigation', { name: 'Board' }).getByRole('link', { name: 'More' }).click();
  await expect(page.getByRole('button', { name: 'Log out' })).toBeVisible();
  await expect(page.getByText(/^deck \d/)).toHaveCount(0);
});

test.describe('on a wide screen', () => {
  test.use({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });

  test('the sidebar footer shows no deck version, but Log out', async ({ page }) => {
    const deck = new FakeDeck();
    deck.stations = [reported];
    deck.deckVersion = '0.7.3';
    await deck.install(page);
    await logIn(page);
    const sidebar = page.getByRole('complementary', { name: 'Sessions' });
    await expect(sidebar.getByText('deck 0.7.3')).toHaveCount(0);
    await expect(sidebar.getByRole('button', { name: 'Log out' })).toBeVisible();
  });
});
