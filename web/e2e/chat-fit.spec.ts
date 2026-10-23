// The chat log fits a 390px phone (DESIGN-SYSTEM §4.5): a long URL, a long inline code, a long
// notice, a tool row with long file names and a long artifact title never make the log wider
// than its box, and a wide table scrolls in its own box.
import { expect, test } from '@playwright/test';
import type { DeckSession, Event } from '../src/gen';
import { summary } from '../src/testing/summary';
import { claudeCapabilities, envelopes } from '../src/testing/timeline';
import { FakeDeck, fitsWidth, logIn, openSession, stationView } from './fake-deck';

const ST = 'st-alpha';
const ID = 's1';
const URL = `https://example.com/${'a1b2c3d4e5'.repeat(18)}`; // 200 characters
const CODE = `fleetdeck_station::shim::${'install_reaper_with_a_long_name_'.repeat(4)}`;
const TABLE = [
  '| Station | Branch | Folder | Model | Tokens | Cost |',
  '| --- | --- | --- | --- | --- | --- |',
  '| lighting | fd/otter-gd7r | ~/projects/obsurvive/fleet-deck-worktrees/v2 | Opus 5.5 | 522 k in | $14,876.41 |',
].join('\n');
const DIR = '/home/me/projects/fleet-deck/crates/fleetdeck-station/src';
const FILES = [
  'session_view_properties_panel.test.tsx',
  'chat_overflow_guard_regression.spec.ts',
  'artifact_chip_ellipsis_fixture.json',
].map((name) => `${DIR}/${name}`);

const session = (): DeckSession => ({
  station_id: ST,
  status: 'idle',
  summary: summary(ID, 'idle', 1, {
    title: 'Fit the phone',
    capabilities: claudeCapabilities(),
    cwd: '/home/me/app',
  }),
});

const events: Event[] = [
  { kind: 'run.started', run_id: 'run-1', input_summary: 'go', origin: 'board' },
  { kind: 'turn.started', run_id: 'run-1', turn_id: 'turn-1', provider_refs: {} },
  {
    kind: 'message.completed',
    turn_id: 'turn-1',
    parent_node_id: null,
    text: `See ${URL} and \`${CODE}\`.\n\n${TABLE}`,
  },
  {
    kind: 'tool.started',
    node_id: 'edit-1',
    parent_node_id: null,
    tool_kind: 'edit',
    title: 'MultiEdit',
    input_summary: 'three files',
    files: FILES,
    provider_refs: {},
  },
  { kind: 'tool.completed', node_id: 'edit-1', status: 'ok', files: [], exit_code: null },
  {
    kind: 'file.changed',
    path: `${DIR}/${'a_generated_file_name_without_a_break_'.repeat(3)}.rs`,
    change: 'modified',
    node_id: null,
  },
  {
    kind: 'artifact.published',
    artifact_id: 'art-1',
    version: 2,
    title: 'Delphi RFC revision 2 against the production research, with every figure',
    artifact_kind: 'html',
  },
];

test('the chat log fits the phone: long words wrap, a wide table scrolls in its own box', async ({
  page,
}) => {
  const deck = new FakeDeck();
  deck.sessions = [session()];
  deck.stations = [stationView(ST, 'Alpha')];
  await deck.install(page);
  await logIn(page);
  deck.emit(ST, ID, envelopes(ID, events));
  await openSession(page, 'Fit the phone');
  const log = page.locator('.fd-chat-scroll');
  await expect(log.locator('table')).toBeVisible();
  await expect(log.getByText('MultiEdit')).toBeVisible();
  await expect(log.locator('.fd-art-chip')).toBeVisible();
  const table = log.locator('.fd-md-table');
  expect(await table.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
  const [scrollWidth, clientWidth] = await log.evaluate(
    (el) => [el.scrollWidth, el.clientWidth] as const,
  );
  expect(scrollWidth).toBeLessThanOrEqual(clientWidth);
  expect(await fitsWidth(page)).toBe(true);
});
