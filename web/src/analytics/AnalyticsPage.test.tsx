import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type {
  AnalyticsAnswer,
  AnalyticsSource,
  FleetAnalytics,
  StationAnalytics,
  StationView,
  UsageHour,
} from '../gen';
import { AnalyticsPage } from './AnalyticsPage';
import { HOUR_MS } from './merge';

const NOW = Date.UTC(2026, 0, 15, 12, 30);
const H = Date.UTC(2026, 0, 15, 12);
const ROUTE = { name: 'analytics', station: null } as const;
const NO_STATIONS: readonly StationView[] = [];

const source = (over: Partial<AnalyticsSource> = {}): AnalyticsSource =>
  ({
    provider: 'claude',
    key: 'home-1',
    state: 'ok',
    verified: true,
    complete: true,
    first_at: H - 10 * HOUR_MS,
    scanned_at: NOW - 5 * 60_000,
    files: 3,
    pending_bytes: 0,
    malformed: 0,
    keeps_days: 30,
    ...over,
  }) as AnalyticsSource;

const without = (r: UsageHour, key: 'session_id' | 'cost_usd'): UsageHour => {
  const { [key]: _drop, ...rest } = r;
  return rest as UsageHour;
};

const row = (over: Partial<UsageHour> = {}): UsageHour => ({
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
  session_id: 's1',
  cost_usd: 1,
  ...over,
});

const answer = (over: Partial<AnalyticsAnswer> = {}): AnalyticsAnswer => ({
  read_at: NOW,
  price_table: '2026-10-24',
  sources: [source()],
  coverage: { events_since: H - 5 * HOUR_MS, kept_since: H - 8 * HOUR_MS },
  usage: [row()],
  activity: [],
  ...over,
});

const station = (
  name: string,
  a: AnalyticsAnswer | undefined,
  over: Partial<StationAnalytics> = {},
): StationAnalytics =>
  ({
    station_id: `id-${name}`,
    name,
    state: a === undefined ? 'offline' : 'ok',
    answer: a,
    ...over,
  }) as StationAnalytics;

const fleet = (stations: StationAnalytics[], owner = true): FleetAnalytics => ({
  read_at: NOW,
  owner,
  stations,
});

const render = (data: FleetAnalytics): string =>
  renderToStaticMarkup(
    <AnalyticsPage
      route={ROUTE}
      stations={NO_STATIONS}
      initialAnswer={data}
      now={NOW}
      zone="UTC"
    />,
  );

describe('AnalyticsPage', () => {
  test('the_page_shows_totals_and_the_price_date', () => {
    const html = render(fleet([station('A', answer())]));
    expect(html).toContain('<h1 class="fd-page-title">Analytics</h1>');
    for (const label of ['Tokens', 'New input', 'Cache read', 'Cache write', 'Output']) {
      expect(html).toContain(`<span class="fd-an-label">${label}</span>`);
    }
    expect(html).toContain('<span class="fd-an-label">Cost estimate</span>');
    expect(html).toContain('$1.00');
    expect(html).toContain('from the Fleet Deck price table of 2026-10-24.');
    expect(html).toContain('Prices of 2026-10-24');
    expect(html).toContain('Past 24 hours');
    expect(html).toContain('All stations');
    expect(html).toContain('aria-label="Refresh analytics"');
  });

  test('days_before_the_transcripts_are_not_zero', () => {
    const first = Date.UTC(2026, 0, 13, 12);
    const html = render(fleet([station('A', answer({ sources: [source({ first_at: first })] }))]));
    expect(html).toContain('class="fd-bars-start"');
    expect(html).toContain('No data before Jan 13');
    // The 30-day window has 31 local days; the days before Jan 13 have no bar.
    expect(html.match(/<rect /g)?.length).toBe(3);
    expect(html).toContain('Claude transcripts on A start on Jan 13 (the CLI keeps them 30 days).');
  });

  test('a_member_page_has_no_not_in_run_line_no_station_table_and_no_file_counts', () => {
    const memberSource = source({
      files: null,
      scanned_at: null,
      pending_bytes: null,
      malformed: null,
      keeps_days: null,
    });
    const rows = [row(), row({ hour: H - HOUR_MS })];
    const member = render(
      fleet([station('A', answer({ sources: [memberSource], usage: rows }))], false),
    );
    expect(member).toContain('Your shared sessions only.');
    expect(member).toContain('Usage of your shared sessions on A starts on Jan 15.');
    expect(member).not.toContain('Not in a Fleet Deck run');
    expect(member).not.toContain('By station');
    expect(member).not.toContain(' files');
    expect(member).not.toContain('read ');
    // The owner of the same data sees them.
    const owner = render(
      fleet([
        station('A', answer({ usage: [row(), without(row({ hour: H - HOUR_MS }), 'session_id')] })),
      ]),
    );
    expect(owner).toContain('Not in a Fleet Deck run');
    expect(owner).toContain('By station');
    expect(owner).toContain('3 files');
    expect(owner).not.toContain('Your shared sessions only.');
  });

  test('a_member_sees_a_station_that_is_still_reading', () => {
    const slow = source({ complete: false, files: null, pending_bytes: null });
    const html = render(fleet([station('A', answer({ sources: [slow] }))], false));
    expect(html).toContain('A is still reading its transcripts.');
    expect(html).not.toContain('Reading the transcripts of');
  });

  test('an_old_station_says_update', () => {
    const html = render(
      fleet([
        station('A', answer()),
        station('B', undefined, { state: 'unsupported' }),
        station('C', undefined, { state: 'offline' }),
        station('D', undefined, { state: 'busy' }),
      ]),
    );
    expect(html).toContain('Update this station to see its analytics.');
    expect(html).toContain('Offline: not in the totals.');
    expect(html).toContain('Busy: try again.');
    const none = render(fleet([station('B', undefined, { state: 'unsupported' })]));
    expect(none).toContain(
      'No station answered. Analytics needs a station that is online and up to date.',
    );
    expect(none).toContain('Update this station to see its analytics.');
  });

  test('truncated_and_partial_hours_are_said', () => {
    const cut = H - 200 * HOUR_MS;
    const html = render(
      fleet([station('A', answer(), { truncated_before: cut, partial_hours: [H - HOUR_MS] })]),
    );
    expect(html).toContain('are not shown: the answer of A was too large.');
    expect(html).toContain('Hours before ');
    expect(html).toContain('Some rows of ');
    expect(html).toContain(' are not shown.');
  });

  test('the_owner_sees_the_bytes_left_per_station', () => {
    const sources = [
      source({ pending_bytes: 1_000_000 }),
      source({ provider: 'codex', key: 'home-2', pending_bytes: 400_000 }),
    ];
    const html = render(fleet([station('Lighting', answer({ sources }))]));
    expect(html).toContain('Reading the transcripts of Lighting: 1.4 MB left.');
    expect(html.match(/Reading the transcripts of/g)?.length).toBe(1);
  });

  test('no_progress_line_when_nothing_is_pending', () => {
    const sources = [source({ pending_bytes: 0 }), source({ provider: 'codex', pending_bytes: 0 })];
    const html = render(fleet([station('Lighting', answer({ sources }))]));
    expect(html).not.toContain('Reading the transcripts of');
  });

  test('a_model_without_a_price_says_so', () => {
    const rows = [without(row({ model: 'mystery' }), 'cost_usd')];
    const html = render(fleet([station('A', answer({ usage: rows }))]));
    expect(html).toContain('no price');
    expect(html).toContain('100 tokens of mystery have no price and are not in the cost.');
  });

  test('without_a_deck_read_the_page_says_no_station_answered', () => {
    const html = renderToStaticMarkup(<AnalyticsPage route={ROUTE} stations={NO_STATIONS} />);
    expect(html).toContain(
      'No station answered. Analytics needs a station that is online and up to date.',
    );
  });

  test('the_work_section_counts_the_events', () => {
    const counts = {
      'turns.done': 8,
      'turns.interrupted': 1,
      'turns.failed': 1,
      'turns.ms': 100_000,
      'turns.timed': 10,
      'tools.shell': 4,
      'tools.failed.shell': 1,
      'lines.added': 12,
      'lines.removed': 3,
    };
    const act = [{ hour: H, session_id: 's1', counts }];
    const html = render(fleet([station('A', answer({ activity: act }))]));
    expect(html).toContain('8 done · 1 interrupted · 1 failed');
    expect(html).toContain('10 s');
    expect(html).toContain('1 failed');
    expect(html).toContain('+12 −3');
    expect(html).toContain('Since ');
  });
});
