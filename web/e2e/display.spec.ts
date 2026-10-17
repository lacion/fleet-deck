// R13 P2c: the wall display page `/display` (pair page, display link, directive) and the owner's
// Displays page. The display page has no write control, shows the code only on its own screen, and
// follows the directive that the deck gives.
import { expect, type Page, test } from '@playwright/test';
import type { DisplayEvent } from '../src/gen';
import { fitsWidth, logIn, PASSWORD } from './fake-deck';
import { ALPHA, displayDeck, NOW, threeSessionDeck } from './scenes';

const directive = (body: Record<string, unknown>, generation: number) => ({
  status: 200,
  body: { generation, directive: { pin: false, filter: null, ...body } },
});

const said = (seq: number, text: string): DisplayEvent => ({
  station_seq: seq,
  session_seq: seq,
  at: NOW,
  item: { item: 'message', turn_id: 't1', text },
});

test.describe('the wall display page', () => {
  test.use({ isMobile: false, hasTouch: false, viewport: { width: 2560, height: 1440 } });

  test('it pairs with a code, then shows the wall and follows the directive', async ({ page }) => {
    const deck = displayDeck();
    let polls = 0;
    deck.on('/api/displays/pair', () => ({
      status: 200,
      body: { pairing_id: 'p1', code: '482913', expires_at: Date.now() + 300_000 },
    }));
    deck.on('/api/displays/pair/p1', () => {
      polls += 1;
      if (polls < 2) return { status: 200, body: { state: 'pending' } };
      deck.displayIn = true;
      return { status: 200, body: { state: 'approved_claimed' } };
    });
    let current: Record<string, unknown> = { mode: 'mission' };
    let generation = 1;
    deck.on('/api/displays/self/directive', () => directive(current, generation));
    deck.on('/api/activity', () => ({ status: 200, body: { items: [] } }));
    deck.emitDisplay(ALPHA, 's2', [said(5, 'Finished the intro')]);
    await deck.install(page);
    await page.goto('/display#/display-pair');
    await expect(page.getByLabel('Pairing code')).toHaveText('482 913');
    await expect(page.getByText('On your Fleet Deck board, open Settings, Displays')).toBeVisible();
    await expect(page.locator('button, input, textarea, select, a')).toHaveCount(0);
    const wall = page.getByTestId('wall');
    await expect(wall).toBeVisible({ timeout: 15_000 });
    await expect(wall).toHaveAttribute('data-mode', 'mission');
    await expect(wall.getByText('Finished the intro')).toBeVisible();
    await expect(wall.locator('button, textarea, input, select, a')).toHaveCount(0);
    expect(await fitsWidth(page)).toBe(true);
    // A directive moves the wall (the poll is every 2 s).
    deck.on(`/api/displays/self/sessions/${ALPHA}/s2/changes`, () => ({
      status: 200,
      body: {
        session_id: 's2',
        scope: 'worktree',
        truncated: false,
        summary: { files_changed: 1, insertions: 3, deletions: 1 },
        files: [
          {
            path: 'src/a.ts',
            hidden: false,
            added: 3,
            removed: 1,
            binary: false,
            patch: null,
            too_large: false,
          },
        ],
      },
    }));
    current = { mode: 'focus', session: { station_id: ALPHA, session_id: 's2' }, pin: true };
    generation = 2;
    await expect(wall).toHaveAttribute('data-mode', 'focus', { timeout: 10_000 });
    await expect(wall.getByText('src/a.ts · +3 −1')).toBeVisible();
    current = { mode: 'ambient' };
    generation = 3;
    await expect(wall).toHaveAttribute('data-mode', 'ambient', { timeout: 10_000 });
  });

  test('a display link signs in and opens the wall', async ({ page }) => {
    const deck = displayDeck();
    deck.on('/api/auth/display-link', (body) => {
      expect((body as { token: string }).token).toBe('tok-1');
      deck.displayIn = true;
      return { status: 204, body: null };
    });
    deck.on('/api/displays/self/directive', () => directive({ mode: 'mission' }, 0));
    deck.on('/api/activity', () => ({ status: 200, body: { items: [] } }));
    await deck.install(page);
    await page.goto('/display#display=tok-1');
    await expect(page.getByTestId('wall')).toBeVisible({ timeout: 15_000 });
    await expect(page).toHaveURL(/\/display#\/wall/);
    expect(page.url()).not.toContain('tok-1');
  });

  test('an artifact that a wall cannot show is the card', async ({ page }) => {
    const deck = displayDeck();
    deck.displayIn = true;
    deck.on('/api/displays/self/directive', () =>
      directive(
        { mode: 'artifact', artifact: { station_id: ALPHA, artifact_id: 'art', version: 2 } },
        1,
      ),
    );
    deck.on(`/api/displays/self/artifact/${ALPHA}/art/2/_`, () => ({
      status: 409,
      body: { error: 'not_displayable', message: 'no' },
    }));
    deck.on('/api/activity', () => ({ status: 200, body: { items: [] } }));
    await deck.install(page);
    await page.goto('/display#/wall');
    await expect(page.getByText('Open this artifact on your own device')).toBeVisible({
      timeout: 15_000,
    });
  });
});

test.describe('the Displays page', () => {
  test.use({ viewport: { width: 375, height: 800 } });

  async function open(page: Page) {
    const deck = threeSessionDeck();
    deck.on('/api/displays', () => ({
      status: 200,
      body: {
        displays: [
          {
            display_id: 'D'.repeat(26),
            name: 'Office wall',
            ceiling: [ALPHA],
            show_diffs: false,
            wake_on_needs_you: true,
            quiet_hours: null,
            binding: null,
            created_at: NOW,
            last_seen: null,
          },
        ],
        pending: [
          {
            pairing_id: 'p1',
            code: '482913',
            source: '10.0.0.7',
            user_agent: 'Chromium/140',
            created_at: NOW,
            expires_at: NOW + 300_000,
          },
        ],
      },
    }));
    await deck.install(page);
    await logIn(page);
    await expect(page.getByLabel('Owner password')).toHaveCount(0, { timeout: 10_000 });
    await page.evaluate(() => {
      window.location.hash = '#/settings/displays';
    });
    return deck;
  }

  test('it lists the displays and the waiting pairings, never the code, and fits the phone', async ({
    page,
  }) => {
    await open(page);
    await expect(page.getByRole('heading', { name: 'Add a wall display' })).toBeVisible();
    await expect(page.getByText('A wall can be read by programs that run as you')).toBeVisible();
    const pending = page.getByRole('region', { name: 'Waiting for a code' });
    await expect(pending).toContainText('10.0.0.7');
    await expect(pending).not.toContainText('482913');
    await expect(page.getByRole('heading', { name: 'Office wall' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Show Mission' })).toBeVisible();
    expect(await fitsWidth(page)).toBe(true);
  });

  test('an approval asks for the owner password, then sends the code and the display', async ({
    page,
  }) => {
    const deck = await open(page);
    deck.on('/api/auth/elevate', (body) => {
      expect((body as { password: string }).password).toBe(PASSWORD);
      return { status: 200, body: { elevation: 'el-1', expires_at: Date.now() + 300_000 } };
    });
    let sent: unknown = null;
    deck.on('/api/displays/pair/p1/approve', (body) => {
      const b = body as { elevation?: string };
      if (b.elevation === undefined) {
        return {
          status: 400,
          body: {
            error: 'elevation_required',
            message: 'a recent sign-in is needed',
            keys: ['displays'],
          },
        };
      }
      sent = body;
      return {
        status: 200,
        body: {
          display_id: 'N'.repeat(26),
          name: 'New wall',
          ceiling: [ALPHA],
          show_diffs: false,
          wake_on_needs_you: true,
          quiet_hours: null,
          binding: null,
          created_at: NOW,
          last_seen: null,
        },
      };
    });
    const pending = page.getByRole('region', { name: 'Waiting for a code' });
    await pending.getByLabel('Name').fill('New wall');
    await pending.getByLabel('Alpha').check();
    await pending.getByLabel('Code on the wall screen').fill('482913');
    await pending.getByRole('button', { name: 'Approve' }).click();
    await page
      .getByRole('dialog', { name: 'Confirm it is you' })
      .getByLabel('Password')
      .fill(PASSWORD);
    await page.getByRole('button', { name: 'Confirm' }).click();
    await expect(pending.getByText('Approved. The wall display signs in')).toBeVisible();
    expect(sent).toEqual({
      code: '482913',
      name: 'New wall',
      ceiling: [ALPHA],
      elevation: 'el-1',
    });
  });
});
