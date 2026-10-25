// DL-179: review comments on the Changes tab against the scripted deck: add a comment on a line,
// see the thread and the review bar, send the review, and the same at a phone viewport.
import { expect, type Page, test } from '@playwright/test';
import type { DeckSession, Event } from '../src/gen';
import { codexCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { envelopes } from '../src/testing/timeline';
import { FakeDeck, fitsWidth, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
const BASE = 'b'.repeat(40);

const PATCH = [
  'diff --git a/src/a.ts b/src/a.ts',
  'index 1111111..2222222 100644',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1,3 +1,3 @@',
  ' one',
  '-two',
  '+TWO',
  ' three',
  '',
].join('\n');

function session(seq: number): DeckSession {
  return {
    station_id: ST,
    status: 'idle',
    summary: summary('s1', 'idle', 1_760_000_000_000 + seq * 1000, {
      provider: 'codex',
      title: 'Edit the docs',
      capabilities: codexCaps(),
      cwd: '/home/me/projects/app',
      session_seq: seq,
      base: { oid: BASE, inferred: false },
    }),
  };
}

const finalized = (turn: string): Event => ({
  kind: 'turn.finalized',
  turn_id: turn,
  checkpoint: {
    worktree_id: 'wt',
    scope: 'tracked',
    start: { state: 'available', commit: `${turn}-start` },
    end: { state: 'available', commit: `${turn}-end` },
  },
  diff_summary: { files_changed: 1, insertions: 1, deletions: 1 },
});

async function setUp(page: Page) {
  const deck = new FakeDeck();
  deck.sessions = [session(1)];
  deck.stations = [stationView(ST, 'Alpha')];
  deck.on(`/api/stations/${ST}/sessions/s1/diff`, () => ({
    status: 200,
    body: {
      session_id: 's1',
      base_oid: BASE,
      summary: { files_changed: 1, insertions: 1, deletions: 1 },
      patch: PATCH,
      truncated: false,
    },
  }));
  const review = deck.review(ST, 's1', PATCH);
  await deck.install(page);
  await logIn(page);
  deck.emit(ST, 's1', envelopes('s1', [finalized('t1')]));
  await openSession(page, 'Edit the docs');
  const view = page.getByRole('region', { name: 'Session' });
  await view.getByRole('tab', { name: /^Changes/ }).click();
  await expect(view.locator('[data-diff="since-spawn"]')).toBeVisible();
  return { deck, review, view };
}

async function comment(view: ReturnType<Page['getByRole']>, label: string, text: string) {
  await view.getByRole('button', { name: label, exact: true }).click();
  await view.getByLabel('Comment', { exact: true }).fill(text);
  await view.getByRole('button', { name: 'Add comment' }).click();
}

test.describe('on a desktop', () => {
  test.use({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });

  test('a comment is added on a line, shown with the review bar, and sent', async ({ page }) => {
    const { review, view } = await setUp(page);
    await comment(view, 'Comment on src/a.ts line 2', 'why uppercase?');
    const thread = view.locator('.fd-thread');
    await expect(thread).toContainText('why uppercase?');
    await expect(thread).toContainText('Draft');
    expect(review.posted[0]?.body).toEqual({
      snapshot: `spawn:${BASE}`,
      path: 'src/a.ts',
      side: 'new',
      line_start: 2,
      line_end: 2,
      body: 'why uppercase?',
    });
    const bar = view.getByRole('region', { name: 'Review' });
    await expect(bar).toContainText('Review: 1 draft');
    await bar.getByRole('button', { name: 'Send review' }).click();
    await expect(view.getByRole('region', { name: 'Review' })).toHaveCount(0);
    await expect(thread).not.toContainText('Draft');
    const sent = review.posted.find((p) => p.path.endsWith('/send'))?.body as {
      send_key: string;
      comment_ids: string[];
    };
    expect(sent.comment_ids).toHaveLength(1);
    expect(sent.send_key).toMatch(/^[0-9a-z]{25}$/);
  });

  test('a second click on the same file makes a range, and the old side takes a comment', async ({
    page,
  }) => {
    const { review, view } = await setUp(page);
    await view.getByRole('button', { name: 'Comment on src/a.ts line 1', exact: true }).click();
    await view.getByRole('button', { name: 'Comment on src/a.ts line 3', exact: true }).click();
    await expect(view.getByText('Lines 1–3')).toBeVisible();
    await view.getByLabel('Comment', { exact: true }).fill('the whole block');
    await view.getByRole('button', { name: 'Add comment' }).click();
    await expect(view.locator('.fd-thread')).toContainText('the whole block');
    expect(review.posted[0]?.body).toMatchObject({ line_start: 1, line_end: 3, side: 'new' });
    await comment(view, 'Comment on src/a.ts line 2 (old side)', 'what was there?');
    expect(review.posted[1]?.body).toMatchObject({ line_start: 2, line_end: 2, side: 'old' });
  });

  test('a failed save keeps the editor open and shows the error; Escape cancels', async ({
    page,
  }) => {
    const { deck, view } = await setUp(page);
    deck.on(`/api/sessions/${ST}/s1/review/comments`, () => ({
      status: 422,
      body: { error: 'not_in_diff', message: 'The lines are not in the diff.' },
    }));
    await comment(view, 'Comment on src/a.ts line 2', 'will fail');
    await expect(view.locator('.fd-cedit').getByRole('alert')).toHaveText(
      'The lines are not in the diff.',
    );
    await expect(view.getByLabel('Comment', { exact: true })).toHaveValue('will fail');
    await view.getByLabel('Comment', { exact: true }).press('Escape');
    await expect(view.getByLabel('Comment', { exact: true })).toHaveCount(0);
    await expect(view.getByRole('region', { name: 'Review' })).toHaveCount(0);
  });
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('a comment is added and the review bar is in view', async ({ page }) => {
    const { view } = await setUp(page);
    await comment(view, 'Comment on src/a.ts line 2', 'tap and type');
    await expect(view.locator('.fd-thread')).toContainText('tap and type');
    const bar = view.getByRole('region', { name: 'Review' });
    await expect(bar.getByRole('button', { name: 'Send review' })).toBeInViewport();
    await expect(bar.getByRole('button', { name: 'Discard all' })).toBeInViewport();
    expect(await fitsWidth(page)).toBe(true);
  });
});
