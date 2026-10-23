// The phone artifact version (B23, DL-504): the panel that a chat chip opens reads as the page: the
// actions of its bar have the field size of the version select, and the select label sits on the
// gutter of the title. The page of the artifact keeps its summary and select.
import { expect, test, type Page } from '@playwright/test';
import type { DeckSession, Event } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities, envelopes } from '../src/testing/timeline';
import { FakeDeck, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 's1';
const MIN = 60_000;
const now = Date.now();

const session = (): DeckSession => ({
  station_id: ST,
  status: 'idle',
  summary: summary(ID, 'idle', 1, {
    title: 'Write the report',
    capabilities: claudeCapabilities(),
    cwd: '/home/me/app',
    artifacts_count: 1,
  }),
});
const published = (version: number): Event => ({
  kind: 'artifact.published',
  artifact_id: 'art-1',
  version,
  title: 'Shim test run report',
  artifact_kind: 'html',
});
const events: Event[] = [
  { kind: 'run.started', run_id: 'run-1', input_summary: 'go', origin: 'board' },
  { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: {} },
  { kind: 'message.completed', turn_id: 'turn-1', parent_node_id: null, text: 'First draft.' },
  published(1),
  { kind: 'message.completed', turn_id: 'turn-1', parent_node_id: null, text: 'Second draft.' },
  published(2),
  { kind: 'message.completed', turn_id: 'turn-1', parent_node_id: null, text: 'Final.' },
  published(3),
];
const row = {
  station_id: ST,
  station_name: 'Alpha',
  artifact_id: 'art-1',
  session_id: ID,
  title: 'Shim test run report',
  kind: 'html',
  latest_version: 3,
  updated_at: now - 5 * MIN,
};
const detail = {
  artifact: row,
  versions: [1, 2, 3].map((version) => ({
    artifact_id: 'art-1',
    version,
    size: 1000 * version,
    sha256: 'ab'.repeat(32),
    files: ['index.html'],
    created_at: now - (4 - version) * 50 * MIN,
  })),
  shares: [],
};

async function setUp(page: Page) {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  deck.stations = [stationView(ST, 'Alpha')];
  deck.on('/api/artifacts', () => ({ status: 200, body: [row] }));
  deck.on(`/api/artifacts/${ST}/art-1`, () => ({ status: 200, body: detail }));
  deck.on(`/api/artifacts/${ST}/art-1/view`, (body) => ({
    status: 200,
    body: {
      url: `/a/v${(body as { version?: number })?.version ?? 3}/`,
      expires_at: now + 3600_000,
    },
  }));
  await page.route('**/a/v*/**', (route) =>
    route.fulfill({
      contentType: 'text/html',
      body: `<!doctype html><h1>Report</h1><p>${route.request().url()}</p>`,
    }),
  );
  await deck.install(page);
  await logIn(page);
  deck.emit(ST, ID, envelopes(ID, events));
}

test('the phone panel bar has one type size and its version label on the gutter', async ({
  page,
}) => {
  await setUp(page);
  await openSession(page, 'Write the report');
  const chips = page.locator('.fd-art-chip');
  await expect(chips).toHaveCount(3);
  await chips.nth(1).click();
  const viewer = page.locator('.fd-art-viewer');
  await expect(viewer).toBeVisible();
  const select = viewer.locator('.fd-art-bar select');
  await expect(select).toHaveValue('2');
  const action = viewer.locator('.fd-art-bar > .fd-art-action').first();
  await expect(action).toBeVisible();
  const [actionSize, selectSize] = await Promise.all([
    action.evaluate((el) => getComputedStyle(el).fontSize),
    select.evaluate((el) => getComputedStyle(el).fontSize),
  ]);
  expect(selectSize).toBe('16px');
  expect(actionSize).toBe(selectSize);
  const selectBox = await select.boundingBox();
  const nameBox = await viewer.locator('.fd-art-name').boundingBox();
  if (selectBox === null || nameBox === null) throw new Error('the bar has no boxes');
  expect(Math.abs(selectBox.x + 8 - nameBox.x)).toBeLessThanOrEqual(1);
});

test('the page of the artifact keeps its summary and select', async ({ page }) => {
  await setUp(page);
  await page.evaluate(() => {
    window.location.hash = '#/artifact/st-alpha/art-1/2';
  });
  const pageView = page.locator('.fd-art-pageview');
  await expect(pageView).toBeVisible();
  await expect(pageView.locator('select')).toHaveValue('2');
});
