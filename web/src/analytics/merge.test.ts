import { describe, expect, test } from 'bun:test';
import type {
  ActivityHour,
  AnalyticsAnswer,
  AnalyticsSource,
  FleetAnalytics,
  StationAnalytics,
  UsageHour,
} from '../gen';
import { HOUR_MS, merge, tokensOf, windowOf } from './merge';

const H = Date.UTC(2026, 0, 15, 12); // an hour start
const ZONE = 'UTC';

const source = (over: Partial<AnalyticsSource> = {}): AnalyticsSource =>
  ({
    provider: 'claude',
    key: 'home-1',
    state: 'ok',
    verified: true,
    complete: true,
    first_at: H - 10 * HOUR_MS,
    scanned_at: H,
    files: 3,
    pending_bytes: 0,
    malformed: 0,
    keeps_days: 30,
    ...over,
  }) as AnalyticsSource;

type UsageOver = Omit<Partial<UsageHour>, 'session_id' | 'cost_usd'> & {
  session_id?: string | undefined;
  cost_usd?: number | undefined;
};

const usage = (over: UsageOver = {}): UsageHour => {
  const row: UsageHour = {
    hour: H,
    provider: 'claude',
    model: 'claude-opus-5-5',
    conversation: 'c1',
    source: 0,
    requests: 1,
    input: 10,
    cache_read: 20,
    cache_write: 30,
    output: 40,
    reasoning: 0,
    ...over,
    session_id: 's1',
    cost_usd: 1,
  };
  // A key given as undefined means "none": the generated type leaves it out.
  if ('session_id' in over) {
    if (over.session_id === undefined) delete row.session_id;
    else row.session_id = over.session_id;
  }
  if ('cost_usd' in over) {
    if (over.cost_usd === undefined) delete row.cost_usd;
    else row.cost_usd = over.cost_usd;
  }
  return row;
};

const answer = (over: Partial<AnalyticsAnswer> = {}): AnalyticsAnswer => ({
  read_at: H,
  price_table: '2026-10-24',
  sources: [source()],
  coverage: { events_since: H - 5 * HOUR_MS, kept_since: H - 8 * HOUR_MS },
  usage: [],
  activity: [],
  ...over,
});

const station = (
  name: string,
  a: AnalyticsAnswer | undefined,
  over: Partial<StationAnalytics> = {},
) =>
  ({
    station_id: `id-${name}`,
    name,
    state: a === undefined ? 'offline' : 'ok',
    answer: a,
    ...over,
  }) as StationAnalytics;

const fleet = (stations: StationAnalytics[], owner = true): FleetAnalytics => ({
  read_at: H,
  owner,
  stations,
});

const WIDE = [H - 100 * HOUR_MS, H + 100 * HOUR_MS] as const;

describe('merge', () => {
  test('merge_sums_the_four_parts_and_never_adds_reasoning', () => {
    const row = usage({ reasoning: 25 });
    const m = merge(fleet([station('A', answer({ usage: [row] }))]), ...WIDE, ZONE);
    expect(m.total.input).toBe(10);
    expect(m.total.reasoning).toBe(25);
    expect(tokensOf(m.total)).toBe(100);
    expect(m.models[0]?.totals.output).toBe(40);
  });

  test('merge_groups_hours_into_local_days', () => {
    const hour = Date.UTC(2026, 0, 15, 3); // 22:00 on Jan 14 in New York (UTC-5)
    const a = answer({ usage: [usage({ hour })] });
    const m = merge(
      fleet([station('A', a)]),
      Date.UTC(2026, 0, 10),
      Date.UTC(2026, 0, 20),
      'America/New_York',
    );
    const day = m.days.find((d) => d.key === '2026-01-14');
    expect(day?.totals.requests).toBe(1);
    expect(m.days.find((d) => d.key === '2026-01-15')?.totals.requests).toBe(0);
    expect(m.days[0]?.key).toBe('2026-01-09');
  });

  test('merge_keeps_the_largest_row_of_a_shared_home', () => {
    const hour2 = H + HOUR_MS;
    const a = answer({
      usage: [usage({ requests: 3, session_id: undefined }), usage({ hour: hour2, requests: 2 })],
    });
    const b = answer({ usage: [usage({ requests: 5, session_id: undefined })] });
    const m = merge(fleet([station('A', a), station('B', b)]), ...WIDE, ZONE);
    // The bucket of H: 5 once (B). The bucket that only A holds counts.
    expect(m.total.requests).toBe(7);
    expect(m.duplicateSources).toEqual(['Claude on B (same home as A)']);
  });

  test('a tie keeps the row with a session', () => {
    const a = answer({ usage: [usage({ requests: 4, session_id: undefined, cost_usd: 1 })] });
    const b = answer({ usage: [usage({ requests: 4, session_id: 's9', cost_usd: 2 })] });
    const m = merge(fleet([station('A', a), station('B', b)]), ...WIDE, ZONE);
    expect(m.total.requests).toBe(4);
    expect(m.total.cost).toBe(2);
    expect(m.notInRun?.requests).toBe(0);
  });

  test('two rows of one station in one bucket both count', () => {
    const a = answer({ usage: [usage({ session_id: 's1' }), usage({ session_id: undefined })] });
    const m = merge(fleet([station('A', a)]), ...WIDE, ZONE);
    expect(m.total.requests).toBe(2);
    expect(m.notInRun?.requests).toBe(1);
  });

  test('merge_keeps_unpriced_tokens_out_of_the_cost', () => {
    const a = answer({
      usage: [
        usage({ cost_usd: undefined, model: 'mystery' }),
        usage({ conversation: 'c2', cost_usd: 2 }),
      ],
    });
    const m = merge(fleet([station('A', a)]), ...WIDE, ZONE);
    expect(m.total.cost).toBe(2);
    expect(m.total.unpricedTokens).toBe(100);
    expect(m.total.unpricedModels).toEqual(['mystery']);
  });

  test('merge_skips_stations_that_did_not_answer', () => {
    const a = answer({ usage: [usage()] });
    const m = merge(
      fleet([
        station('A', a),
        station('B', undefined),
        station('C', undefined, { state: 'timeout' }),
      ]),
      ...WIDE,
      ZONE,
    );
    expect(m.total.requests).toBe(1);
    expect(m.stations.map((s) => [s.name, s.state, s.totals === null])).toEqual([
      ['A', 'ok', false],
      ['B', 'offline', true],
      ['C', 'timeout', true],
    ]);
  });

  test('merge_has_no_not_in_run_for_a_member', () => {
    const a = answer({ usage: [usage()] });
    expect(merge(fleet([station('A', a)], false), ...WIDE, ZONE).notInRun).toBeNull();
    expect(merge(fleet([station('A', a)], true), ...WIDE, ZONE).notInRun).not.toBeNull();
  });

  test('the_window_starts_at_an_hour', () => {
    const now = Date.UTC(2026, 0, 15, 10, 30);
    expect(windowOf(1, now)).toEqual({ since: Date.UTC(2026, 0, 14, 10), until: now });
    expect(windowOf('all', now)).toEqual({ since: 0, until: now });
    expect(windowOf(30, now).since % HOUR_MS).toBe(0);
  });

  test('the past 24 hours has one entry per hour', () => {
    const now = Date.UTC(2026, 0, 15, 10, 30);
    const { since, until } = windowOf(1, now);
    const m = merge(
      fleet([station('A', answer({ usage: [usage({ hour: until - 1800000 - 1800000 })] }))]),
      since,
      until,
      ZONE,
    );
    expect(m.hours).toHaveLength(25);
    expect(merge(fleet([]), windowOf(7, now).since, until, ZONE).hours).toHaveLength(0);
  });

  test('all time starts at the first data', () => {
    const a = answer({ usage: [usage()] });
    const m = merge(fleet([station('A', a)]), 0, H + HOUR_MS, ZONE);
    expect(m.days.length).toBeGreaterThan(0);
    expect(m.days[0]?.start).toBeLessThanOrEqual(H - 10 * HOUR_MS);
  });

  test('perSession counts ended turns and attributed usage from the period start', () => {
    const act: ActivityHour = {
      hour: H,
      session_id: 's1',
      counts: { 'turns.done': 3, 'turns.failed': 1, 'tools.shell': 4 },
    };
    const m = merge(
      fleet([station('A', answer({ usage: [usage()], activity: [act] }))]),
      ...WIDE,
      ZONE,
    );
    expect(m.perSession.get('s1')?.turnsEnded).toBe(4);
    expect(m.perSession.get('s1')?.totals.requests).toBe(1);
    expect(m.counts.get('tools.shell')).toBe(4);
    expect(m.usageComplete).toBe(true);
  });

  test('usage is not complete while a source reads or a page is cut', () => {
    const reading = answer({ sources: [source({ complete: false })] });
    expect(merge(fleet([station('A', reading)]), ...WIDE, ZONE).usageComplete).toBe(false);
    const cut = station('A', answer(), { truncated_before: H });
    expect(merge(fleet([cut]), ...WIDE, ZONE).usageComplete).toBe(false);
  });
});
