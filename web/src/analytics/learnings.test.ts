import { describe, expect, test } from 'bun:test';
import { monthDay, learnings } from './learnings';
import { EMPTY_TOTALS, HOUR_MS, type Merged, type SessionSums, type Totals } from './merge';

const SINCE = Date.UTC(2026, 0, 1);
const UNTIL = Date.UTC(2026, 0, 31);

const totals = (over: Partial<Totals> = {}): Totals => ({ ...EMPTY_TOTALS, ...over });

function merged(counts: Record<string, number> = {}, over: Partial<Merged> = {}): Merged {
  return {
    total: totals(),
    notInRun: null,
    days: [],
    hours: [],
    models: [],
    stations: [],
    usageSince: SINCE,
    perSession: new Map<string, SessionSums>(),
    learningSince: SINCE,
    usageComplete: true,
    counts: new Map(Object.entries(counts)),
    eventsSince: SINCE,
    keptSince: SINCE,
    priceTables: [],
    duplicateSources: [],
    window: { since: SINCE, until: UNTIL },
    ...over,
  };
}

const ids = (m: Merged): string[] => learnings(m).map((l) => l.id);

describe('learnings', () => {
  test('cache is shown at 100 requests and hidden at 99', () => {
    const t = (requests: number) => totals({ requests, input: 10, cacheRead: 80, cacheWrite: 10 });
    const shown = learnings(merged({}, { total: t(100) }));
    expect(shown[0]?.text).toBe('80 % of the input came from the prompt cache.');
    expect(ids(merged({}, { total: t(99) }))).toEqual([]);
  });

  test('tool_failures is shown at 20 calls and 5 %, hidden one under, and picks the highest share', () => {
    const at = merged({ 'tools.shell': 20, 'tools.failed.shell': 1 });
    expect(learnings(at)[0]?.text).toBe('Shell calls fail most often: 5 % of 20 failed.');
    expect(ids(merged({ 'tools.shell': 19, 'tools.failed.shell': 19 }))).toEqual([]);
    expect(ids(merged({ 'tools.shell': 100, 'tools.failed.shell': 4 }))).toEqual([]);
    const two = merged({
      'tools.shell': 100,
      'tools.failed.shell': 10,
      'tools.edit': 20,
      'tools.failed.edit': 4,
    });
    expect(learnings(two)[0]?.text).toBe('Edit calls fail most often: 20 % of 20 failed.');
  });

  test('approval_wait is shown at 5 answered and hidden at 4', () => {
    const at = merged({ 'approvals.waited': 5, 'approvals.wait_ms': 5 * 120_000 });
    expect(learnings(at)[0]?.text).toBe(
      'An approval waited 2 min on average for its answer (5 answered).',
    );
    expect(ids(merged({ 'approvals.waited': 4, 'approvals.wait_ms': 400_000 }))).toEqual([]);
  });

  test('unfinished_turns is shown at 20 turns and hidden at 19', () => {
    const at = merged({ 'turns.done': 15, 'turns.interrupted': 3, 'turns.failed': 2 });
    expect(learnings(at)[0]?.text).toBe('25 % of 20 turns ended interrupted or failed.');
    expect(ids(merged({ 'turns.done': 16, 'turns.failed': 3 }))).toEqual([]);
  });

  test('a family that starts after the window start says (since ...)', () => {
    const later = SINCE + 5 * 24 * HOUR_MS;
    const m = merged(
      { 'turns.done': 20 },
      { eventsSince: later, total: totals({ requests: 100, input: 1 }) },
    );
    const byId = Object.fromEntries(learnings(m).map((l) => [l.id, l.text]));
    expect(byId['unfinished_turns']).toBe(
      `0 % of 20 turns ended interrupted or failed. (since ${monthDay(later)})`,
    );
    expect(byId['cache']).toBe('0 % of the input came from the prompt cache.');
  });

  const session = (turns: number, over: Partial<Totals> = {}): SessionSums => ({
    turnsEnded: turns,
    totals: totals({ requests: 2, cost: 10, ...over }),
  });

  test('cost_per_turn is shown with 20 turns and complete usage', () => {
    const m = merged(
      {},
      {
        perSession: new Map([
          ['a', session(12)],
          ['b', session(8)],
        ]),
      },
    );
    expect(learnings(m)[0]?.text).toBe(
      'A turn cost about $1.00 at API prices (estimate, 20 turns).',
    );
    const few = merged({}, { perSession: new Map([['a', session(19)]]) });
    expect(ids(few)).toEqual([]);
  });

  test('cost_per_turn_is_hidden_when_a_session_with_turns_has_no_usage', () => {
    const none: SessionSums = { turnsEnded: 5, totals: totals() };
    const m = merged(
      {},
      {
        perSession: new Map([
          ['a', session(20)],
          ['b', none],
        ]),
      },
    );
    expect(ids(m)).toEqual([]);
  });

  test('cost_per_turn is hidden when attributed usage has no price', () => {
    const m = merged({}, { perSession: new Map([['a', session(20, { unpricedTokens: 5 })]]) });
    expect(ids(m)).toEqual([]);
  });

  test('cost_per_turn_is_hidden_while_a_source_is_not_complete', () => {
    const m = merged({}, { usageComplete: false, perSession: new Map([['a', session(20)]]) });
    expect(ids(m)).toEqual([]);
  });

  test('cost_per_turn is hidden when its period is empty', () => {
    const m = merged({}, { learningSince: UNTIL, perSession: new Map([['a', session(20)]]) });
    expect(ids(m)).toEqual([]);
  });
});
