// B17 §3.3: the Remote page. The mirror draws the wall on the owner's device; it never loads a
// frame. A wall shows an artifact inert (`INERT_CSP`); an owner view link would run the scripts of
// an agent-made HTML file on this device, so the mirror shows a card and asks for no link.
import { expect, test } from '@playwright/test';
import { logIn } from './fake-deck';
import { ALPHA, NOW, threeSessionDeck } from './scenes';

test.use({ isMobile: false, hasTouch: false, viewport: { width: 1440, height: 900 } });

const ID = 'D'.repeat(26);
/** `MIRROR_ARTIFACT_TEXT` of `src/wall/ArtifactPane.tsx` (a React module stays out of the runner). */
const MIRROR_ARTIFACT_TEXT = 'The wall shows this artifact. The remote does not open it.';

test('the mirror shows an artifact as a card: no view link and no frame', async ({ page }) => {
  const deck = threeSessionDeck();
  deck.on('/api/features', () => ({ status: 200, body: { features: ['wall_control.v1'] } }));
  deck.on('/api/displays', () => ({
    status: 200,
    body: {
      displays: [
        {
          display_id: ID,
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
      pending: [],
    },
  }));
  deck.on(`/api/displays/${ID}/remote`, () => ({
    status: 200,
    body: {
      directive: {
        generation: 3,
        directive: {
          mode: 'artifact',
          artifact: { station_id: ALPHA, artifact_id: 'art', version: 2 },
          pin: false,
          filter: null,
        },
      },
      link: 'ready',
      incarnation: 'inc-1',
      seq: 0,
      state: { selected: null, frame_tenths: 0 },
      hold_left_ms: 0,
      complete: true,
      controls: [],
    },
  }));
  deck.on('/api/activity', () => ({ status: 200, body: { items: [] } }));
  await deck.install(page);
  await logIn(page);
  await expect(page.getByRole('complementary', { name: 'Sessions' }))
    .toBeVisible({ timeout: 10_000 })
    .catch(() => undefined);
  await page.evaluate((id) => {
    window.location.hash = `#/remote/${id}`;
  }, ID);
  const mirror = page.locator('.fd-remote-mirror');
  await expect(mirror).toContainText(MIRROR_ARTIFACT_TEXT, { timeout: 10_000 });
  await expect(mirror).toContainText('art · version 2');
  // Let the remote poll again: the mirror renders its view more than once.
  const polls = () => deck.calls.filter((c) => c.path === `/api/displays/${ID}/remote`).length;
  await expect.poll(polls, { timeout: 10_000 }).toBeGreaterThan(1);
  await expect(mirror).toContainText(MIRROR_ARTIFACT_TEXT);
  // No view link was asked for, and no frame is on the page.
  expect(deck.calls.filter((c) => c.path.startsWith('/api/artifacts/'))).toEqual([]);
  await expect(page.locator('iframe')).toHaveCount(0);
});
