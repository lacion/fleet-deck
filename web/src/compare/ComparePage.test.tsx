import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetBoard } from '../board/FleetBoard';
import type { DeckSession, Event } from '../gen';
import { sessionActions } from '../session/commands';
import { card, station } from '../testing/fleet';
import { newUsage } from '../testing/summary';
import { envelopes } from '../testing/timeline';
import type { Timeline } from '../runtime/state';
import { batchRef } from './compare';
import { ComparePage } from './ComparePage';

const member = (id: string, index: number, extra: Partial<DeckSession['summary']> = {}) =>
  card(id, 'idle', 'station-a', {
    title: `Try ${id}`,
    batch: batchRef('B1', index, 3),
    model: 'claude-sonnet-4-5',
    usage: newUsage({
      input_tokens: 1200,
      output_tokens: 300,
      cost_usd: 0.42,
      cache_read_tokens: 9000,
    }),
    ...extra,
  });

const fleet = [member('a', 1), member('b', 2), member('c', 3), card('other')];

const message = (text: string): Event => ({
  kind: 'message.completed',
  turn_id: 'turn-1',
  parent_node_id: null,
  text,
});
const timeline = (text: string): Timeline => ({
  incarnation: 'i',
  cursor: 1,
  events: envelopes('a', [message(text)]),
  freshness: 'live',
  error: null,
  ready: true,
});

const drive = (key: { station_id: string; session_id: string }) =>
  sessionActions(async () => ({}), key, ['read', 'drive']);

function page(over: Partial<Parameters<typeof ComparePage>[0]> = {}) {
  return renderToStaticMarkup(
    <ComparePage
      batchId="B1"
      sessions={fleet}
      stations={[station()]}
      timelines={{ [JSON.stringify(['station-a', 'a'])]: timeline('Added the retry loop.') }}
      freshness="live"
      sessionActions={drive}
      trash={async () => ({ ok: true, text: 'Moving…' })}
      onOpen={() => {}}
      {...over}
    />,
  );
}

describe('compare page (R1.4)', () => {
  test('one column per sibling, in batch order, with status, usage and cost', () => {
    const html = page();
    expect(html).toContain('Compare 3 sessions');
    expect(html.match(/class="fd-compare-col"/g)).toHaveLength(3);
    expect(html.indexOf('Try a')).toBeLessThan(html.indexOf('Try b'));
    expect(html.indexOf('Try b')).toBeLessThan(html.indexOf('Try c'));
    expect(html).not.toContain('data-session="other"');
    expect(html).toContain('Waiting for a prompt');
    expect(html).toContain('1 of 3');
    expect(html).toContain('1,200 in');
    expect(html).toContain('9,000 cache read');
    expect(html).toContain('300 out');
    expect(html).toContain('$0.42');
    expect(html).toContain('Sonnet 4.5');
  });
  test('a session with only an old count shows unknown, never the old numbers', () => {
    const old = member('a', 1, {
      usage: { input_tokens: 5000, output_tokens: 70, cost_usd: 9.99, cost_inferred: false },
    });
    const html = page({ sessions: [old, member('b', 2), member('c', 3)] });
    expect(html).not.toContain('5,000 in');
    expect(html).not.toContain('$9.99');
    expect(html).toContain('<p class="fd-compare-usage">unknown</p>');
  });
  test('the last agent message shows when the timeline is loaded', () => {
    const html = page();
    expect(html).toContain('Added the retry loop.');
    expect(html).toContain('Loading the timeline…');
  });
  test('the digest line shows only when the summary has one', () => {
    const none = page();
    expect(none).not.toContain('Digest');
    const withDigest = page({
      sessions: [member('a', 1, { digest: 'Wrote the parser.' } as never), member('b', 2)],
    });
    expect(withDigest).toContain('Wrote the parser.');
    expect(withDigest.match(/aria-label="Digest"/g)).toHaveLength(1);
  });
  test('each column has Keep this one, enabled for the owner on a live board', () => {
    const html = page();
    expect(html.match(/Keep this one<\/button>/g)).toHaveLength(3);
    expect(html).not.toContain('disabled=""');
  });
  test('Keep this one is off without the trash action, without the drive scope and when stale', () => {
    const noTrash = page({ trash: undefined });
    expect(noTrash).toContain('Only the owner can move worktrees to the trash.');
    expect(noTrash).toContain('disabled=""');
    const readOnly = page({
      sessionActions: (key) => sessionActions(async () => ({}), key, ['read']),
    });
    expect(readOnly).toContain('disabled=""');
    const stale = page({ freshness: 'stale' });
    expect(stale).toContain('The board is not live.');
  });
  test('the summary slot shows only when the page has a race action', () => {
    expect(page()).not.toContain('Summarize the race');
    const html = page({ race: async () => ({ kind: 'absent' }) });
    expect(html).toContain('aria-label="Summary"');
    expect(html).toContain('Summarize the race');
  });
  test('an unknown batch says so and links back', () => {
    const html = page({ batchId: 'nope' });
    expect(html).toContain('This batch has no session on the board.');
    expect(html).toContain('href="#/fleet"');
  });
});

describe('the Compare chip on a batch card', () => {
  const board = (sessions: DeckSession[]) =>
    renderToStaticMarkup(
      <FleetBoard sessions={sessions} stations={[station()]} freshness="live" />,
    );
  test('a batch of two or more has the chip with the count, beside the card link', () => {
    const html = board(fleet);
    expect(html.match(/class="fd-card-compare"/g)).toHaveLength(3);
    expect(html).toContain('href="#/compare/B1"');
    expect(html).toContain('>Compare 3</a>');
    // The chip is not inside the card link (no nested anchors).
    expect(html).not.toMatch(/<a class="fd-card"[^>]*>(?:(?!<\/a>)[\s\S])*fd-card-compare/);
  });
  test('a single session of a batch and a plain session have no chip', () => {
    expect(board([member('a', 1), card('other')])).not.toContain('fd-card-compare');
  });
});
