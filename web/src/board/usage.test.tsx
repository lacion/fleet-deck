import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type { DeckSession } from '../gen';
import { card, station } from '../testing/fleet';
import { newUsage, summary } from '../testing/summary';
import { sessionsText, UsageGlance } from './UsageGlance';
import { UsageMeters } from './UsageMeters';
import {
  apiPricesTitle,
  costText,
  costTitle,
  dollars,
  fleetUsage,
  inputLabel,
  instanceLimit,
  limitChip,
  limitSuffix,
  meterText,
  mostRoom,
  rateMeters,
  resetsText,
  splitUsage,
  tokens,
  totalText,
} from './usage';

const NOW = 1_767_240_000_000 - 2 * 3_600_000 - 10 * 60_000;

const window5h = (used: number | null, at = 1_767_240_000_000) => ({
  scope: 'five_hour',
  used_percent: used,
  resets_at: at,
});

describe('usage meters (DL-303)', () => {
  test('a total of unknown parts is unknown, never zero, and a partial total names the gap', () => {
    const u = fleetUsage([
      card('a', 'working', 'station-a', {
        usage: newUsage({ input_tokens: 1200, cost_usd: 0 }),
      }),
      card('b', 'working', 'station-a', {
        usage: newUsage({ input_tokens: 300 }),
      }),
    ]);
    expect(totalText(u.input, tokens)).toBe('1,500');
    expect(totalText(u.output, tokens)).toBe('unknown');
    expect(totalText(u.cost, dollars)).toBe('at least $0.00 (1 session unknown)');
    expect(totalText(fleetUsage([]).input, tokens)).toBe('unknown');
  });

  test('a meter keeps the latest window per station, provider account and scope', () => {
    const old = card('old', 'idle', 'station-a', {
      provider_instance: 'personal',
      updated_at: 1,
      rate_limits: [window5h(90)],
    });
    const recent = card('new', 'working', 'station-a', {
      provider_instance: 'personal',
      updated_at: 5,
      rate_limits: [window5h(20), { scope: 'weekly', used_percent: null, resets_at: null }],
    });
    const other = card('b', 'working', 'station-b', {
      provider_instance: 'personal',
      updated_at: 2,
      rate_limits: [window5h(70)],
    });
    const meters = rateMeters([recent, old, other], [station(), station('station-b')]);
    // The tightest window first; an unknown one last (R4.1).
    expect(meters.map((m) => [m.label, m.scope, m.usedPercent])).toEqual([
      ['Beta · personal', 'five_hour', 70],
      ['Alpha · personal', 'five_hour', 20],
      ['Alpha · personal', 'weekly', null],
    ]);
    expect(meters.map((m) => meterText(m, NOW))).toEqual([
      '70% used, resets in 2 h 10 min',
      '20% used, resets in 2 h 10 min',
      'use unknown',
    ]);
  });

  test('the usage panel shows the meters; an unknown window has no meter bar', () => {
    const sessions = [
      card('a', 'working', 'station-a', {
        provider_instance: 'personal',
        rate_limits: [window5h(42), { scope: 'weekly', used_percent: null, resets_at: null }],
      }),
    ];
    const html = renderToStaticMarkup(
      <UsageMeters sessions={sessions} stations={[station()]} now={NOW} />,
    );
    expect(html).toContain('aria-label="Usage"');
    expect(html.match(/<meter /g)?.length).toBe(1);
    expect(html).toContain('42% used, resets in 2 h 10 min');
    expect(html).toContain('use unknown');
    expect(html).toContain('<dd>unknown</dd>');
    expect(html).not.toContain('>0<');
    const empty = renderToStaticMarkup(<UsageMeters sessions={[]} stations={[]} />);
    expect(empty).toContain('No provider reported a rate limit yet.');
  });

  test('the fleet home has a Usage tab in its bottom panel', () => {
    const html = renderToStaticMarkup(<App sessions={[]} freshness="live" />);
    expect(html).toContain('role="tablist" aria-label="Fleet panels"');
    expect(html).toMatch(/role="tab"[^>]*>Usage<\/button>/);
    // only the active tab (Activity) has its content
    expect(html).not.toContain('aria-label="Usage"');
  });

  test('a reset time reads as a countdown, or as a weekday and a clock time after a day (R4.1)', () => {
    const min = 60_000;
    expect(resetsText(NOW + 5 * min, NOW)).toBe('resets in 5 min');
    expect(resetsText(NOW + 60 * min, NOW)).toBe('resets in 1 h');
    expect(resetsText(NOW + 130 * min, NOW)).toBe('resets in 2 h 10 min');
    expect(resetsText(NOW - 1, NOW)).toBe('reset due');
    const later = new Date(NOW + 30 * 60 * min);
    const hhmm = `${String(later.getHours()).padStart(2, '0')}:${String(later.getMinutes()).padStart(2, '0')}`;
    const day = later.toLocaleDateString(undefined, { weekday: 'short' });
    expect(resetsText(later.getTime(), NOW)).toBe(`resets ${day} ${hhmm}`);
  });

  test('the warning chip starts at 80% and turns to a hazard at 100% (R4.2)', () => {
    const meters = (used: number | null) =>
      rateMeters(
        [
          card('a', 'working', 'station-a', {
            provider_instance: 'p',
            rate_limits: [window5h(used)],
          }),
        ],
        [station()],
      );
    expect(limitChip(meters(79))).toBeNull();
    expect(limitChip(meters(null))).toBeNull();
    expect(limitChip(meters(85))).toEqual({ tone: 'act', text: 'Limit 85% · 5-hour window' });
    expect(limitChip(meters(100))).toEqual({
      tone: 'hazard',
      text: 'Limit reached · 5-hour window',
    });
  });

  test('the fleet home and the session header show the chip (R4.2)', () => {
    const hot = card('a', 'working', 'station-a', {
      provider_instance: 'p',
      rate_limits: [window5h(85)],
    });
    const html = renderToStaticMarkup(
      <App sessions={[hot]} stations={[station()]} freshness="live" />,
    );
    expect(html).toContain('data-tone="act">Limit 85% · 5-hour window</span>');
    const cold = card('b', 'working', 'station-a', { rate_limits: [window5h(10)] });
    const calm = renderToStaticMarkup(
      <App sessions={[cold]} stations={[station()]} freshness="live" />,
    );
    expect(calm).not.toContain('Limit ');
  });

  test('the spawn picker names the limit of each instance and the one with the most room (R4.3)', () => {
    const at = NOW + 60 * 60_000;
    const sessions = [
      card('a', 'working', 'station-a', {
        provider_instance: 'work',
        rate_limits: [window5h(42, at)],
      }),
      card('b', 'working', 'station-a', {
        provider_instance: 'home',
        rate_limits: [window5h(90, at)],
      }),
    ];
    const meters = rateMeters(sessions, [station()]);
    const work = instanceLimit(meters, 'station-a', 'work');
    expect(limitSuffix(work, NOW)).toBe('42% of 5-hour window, resets in 1 h');
    expect(limitSuffix(null, NOW)).toBe('');
    const limits = new Map([
      ['work', work],
      ['home', instanceLimit(meters, 'station-a', 'home')],
      ['none', instanceLimit(meters, 'station-a', 'none')],
    ]);
    expect(mostRoom(limits)).toBe('work');
    // One reporting instance has nothing to compare with.
    expect(mostRoom(new Map([['work', work]]))).toBeNull();
  });

  test('an inferred cost is labeled and makes the fleet total approximate (R4.6)', () => {
    const inferred = newUsage({
      input_tokens: 1,
      output_tokens: 1,
      cost_usd: 1.2,
      cost_inferred: true,
    });
    expect(costText(inferred)).toBe('≈ $1.20 inferred');
    expect(costText({ ...inferred, cost_inferred: false })).toBe('$1.20');
    expect(costText({ ...inferred, cost_usd: null })).toBeNull();
    const sessions = [
      card('a', 'working', 'station-a', { usage: inferred }),
      card('b', 'working', 'station-a', { usage: { ...inferred, cost_inferred: false } }),
    ];
    const html = renderToStaticMarkup(<UsageMeters sessions={sessions} stations={[]} now={NOW} />);
    expect(html).toContain('<dd>≈ $2.40</dd>');
    const reported = renderToStaticMarkup(
      <UsageMeters sessions={[sessions[1] as DeckSession]} stations={[]} now={NOW} />,
    );
    expect(reported).toContain('<dd>$1.20</dd>');
  });
});

describe('the usage glance totals (live UI audit)', () => {
  const big = (id: string, provider: 'claude' | 'codex' = 'claude') =>
    card(id, 'working', 'station-a', {
      provider,
      usage: newUsage({ input_tokens: 1550, output_tokens: 750_000, cost_usd: 25_867.93 }),
    });

  test('a cost has digit groups and cents (one money format, DESIGN-SYSTEM §5)', () => {
    expect(dollars(1.2)).toBe('$1.20');
    expect(dollars(812.4)).toBe('$812.40');
    expect(dollars(51_735.857)).toBe('$51,735.86');
    expect(dollars(1_234_567.8)).toBe('$1,234,567.80');
    expect(dollars(0)).toBe('$0.00');
  });

  test('the_input_label_is_new_input_for_every_provider', () => {
    // Both providers count the input that the prompt cache did not hold (DL-441).
    expect(inputLabel()).toBe('new input');
    const html = renderToStaticMarkup(
      <UsageGlance sessions={[big('a'), big('c', 'codex')]} stations={[station()]} now={NOW} />,
    );
    expect(html).toContain('<dt>new input</dt>');
    expect(html).not.toContain('<dt>input</dt>');
  });

  test('the glance says its scope and keeps the exact values as titles', () => {
    const html = renderToStaticMarkup(
      <UsageGlance sessions={[big('a'), big('b')]} stations={[station()]} now={NOW} />,
    );
    expect(html).toContain('<p class="fd-glance-span">2 live sessions</p>');
    expect(html).toContain('<dd title="1,500,000">1.5M</dd>');
    // One-line labels, the words of the Properties panel ("682 input · 309K output").
    expect(html).toContain('<dt>new input</dt>');
    expect(html).toContain('<dt>output</dt>');
    // The cost is the price at the API rates: named so, and explained in the title.
    const about =
      'What the tokens of these sessions cost at the provider&#x27;s API prices, cached input included. A subscription does not bill it.';
    expect(html).toContain(`<dt title="${about}">at API prices</dt>`);
    expect(html).toContain(`<dd title="$51,735.86\n${about}">$51,735.86</dd>`);
    expect(html).not.toContain('>cost<');
  });

  test('the scope names the live and the ended sessions, as the header counts them', () => {
    const live = card('a');
    const ended = card('e', 'ended');
    expect(sessionsText([live])).toBe('1 live session');
    expect(sessionsText([live, card('b', 'idle')])).toBe('2 live sessions');
    expect(sessionsText([live, card('b'), ended, card('l', 'lost')])).toBe(
      '2 live and 2 ended sessions',
    );
    expect(sessionsText([live, card('b'), ended])).toBe('2 live and 1 ended session');
    expect(sessionsText([live, card('u', 'unreachable'), ended])).toBe(
      '1 live, 1 unreachable and 1 ended session',
    );
    expect(sessionsText([ended])).toBe('1 ended session');
  });

  test('the All usage tab keeps the unit in its labels: four token stats, then the cost', () => {
    const html = renderToStaticMarkup(
      <UsageMeters sessions={[big('a')]} stations={[station()]} now={NOW} />,
    );
    const order = [
      '<dt class="fd-label">New input tokens</dt>',
      '<dt class="fd-label">Cache read tokens</dt>',
      '<dt class="fd-label">Cache write tokens</dt>',
      '<dt class="fd-label">Output tokens</dt>',
      '>At API prices</dt>',
    ].map((text) => html.indexOf(text));
    expect(order.every((at) => at >= 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    // The cache parts that no session reported are unknown, not zero.
    expect(html).toContain('<dd>unknown</dd>');
    expect(html).toMatch(
      /<dt class="fd-label" title="What the tokens of this session[^"]*">At API prices<\/dt>/,
    );
  });
});

describe('usage of the new rules and the old count (usage-cost §3.6)', () => {
  const old = { input_tokens: 99_999, output_tokens: 99_999, cost_usd: 99, cost_inferred: false };
  const split = (usage: Parameters<typeof summary>[3]) =>
    splitUsage(summary('s', 'idle', 1, usage));

  test('split_usage_of_a_new_an_old_and_a_mixed_session', () => {
    const fresh = newUsage({ input_tokens: 5 });
    expect(split({ usage: fresh })).toEqual({ current: fresh, old: null, oldUntil: null });
    expect(split({ usage: old })).toEqual({ current: null, old, oldUntil: null });
    expect(split({ usage: fresh, usage_legacy: { usage: old, until: 123 } })).toEqual({
      current: fresh,
      old,
      oldUntil: 123,
    });
    // A session with no usage at all is a new session, not an old count.
    const empty = summary('e', 'idle', 1).usage;
    expect(splitUsage(summary('e', 'idle', 1))).toEqual({
      current: empty,
      old: null,
      oldUntil: null,
    });
  });

  const a = newUsage({
    input_tokens: 100,
    output_tokens: 10,
    cache_read_tokens: 1000,
    cache_write_tokens: 50,
    cost_usd: 1.5,
  });
  const b = newUsage({
    input_tokens: 200,
    output_tokens: 20,
    cache_read_tokens: 2000,
    cost_usd: 2.5,
    cost_inferred: true,
    prices: '2026-10',
  });
  const mixed = newUsage({
    input_tokens: 7,
    output_tokens: 1,
    cache_read_tokens: 3,
    cache_write_tokens: 1,
    cost_usd: 0.5,
  });
  const fleet = [
    card('a', 'working', 'station-a', { usage: a }),
    card('b', 'working', 'station-a', { usage: b }),
    card('old', 'working', 'station-a', { usage: old }),
    card('mixed', 'working', 'station-a', { usage: mixed, usage_legacy: { usage: old, until: 5 } }),
  ];

  test('fleet_usage_sums_only_new_rule_usage_and_cache_parts', () => {
    const u = fleetUsage(fleet);
    expect(u.input.known).toBe(307);
    expect(u.output.known).toBe(31);
    expect(u.cacheRead.known).toBe(3003);
    // Only the first and the mixed session reported a cache write.
    expect(u.cacheWrite).toEqual({ known: 51, unknown: 2, partial: 1 });
    expect(u.cost.known).toBe(4.5);
    expect(u.costInferred).toBe(true);
    // The old counts are not in the sums: the old session and the old part of the mixed one.
    expect(u.oldCounts).toBe(2);
    expect(totalText(u.input, tokens)).toBe('at least 307 (1 unknown, 1 partial)');
  });

  test('an_old_count_session_is_unknown_and_a_mixed_one_partial', () => {
    const only = fleetUsage([fleet[2] as DeckSession]);
    for (const part of [only.input, only.output, only.cacheRead, only.cacheWrite, only.cost]) {
      expect(part).toEqual({ known: null, unknown: 1, partial: 0 });
    }
    expect(only.oldCounts).toBe(1);
    const one = fleetUsage([fleet[3] as DeckSession]);
    expect(one.input).toEqual({ known: 7, unknown: 0, partial: 1 });
    expect(one.cost).toEqual({ known: 0.5, unknown: 0, partial: 1 });
    // `tokens_partial` marks the token parts, `cost_partial` the cost.
    const t = fleetUsage([
      card('t', 'working', 'station-a', {
        usage: newUsage({ input_tokens: 5, cost_usd: 1, tokens_partial: true }),
      }),
    ]);
    expect([t.input.partial, t.cost.partial]).toEqual([1, 0]);
    const c = fleetUsage([
      card('c', 'working', 'station-a', {
        usage: newUsage({ input_tokens: 5, cost_usd: 1, cost_partial: true }),
      }),
    ]);
    expect([c.input.partial, c.cost.partial]).toEqual([0, 1]);
  });

  test('total_text_says_at_least_for_partial_parts', () => {
    const t = (unknown: number, partial: number) =>
      totalText({ known: 1500, unknown, partial }, tokens);
    expect(t(0, 0)).toBe('1,500');
    expect(t(2, 0)).toBe('at least 1,500 (2 sessions unknown)');
    expect(t(1, 0)).toBe('at least 1,500 (1 session unknown)');
    expect(t(0, 1)).toBe('at least 1,500 (1 partial)');
    expect(t(2, 3)).toBe('at least 1,500 (2 unknown, 3 partial)');
    expect(totalText({ known: null, unknown: 2, partial: 0 }, tokens)).toBe('unknown');
  });

  test('cost_text_says_at_least_for_a_partial_cost', () => {
    const cost = newUsage({ cost_usd: 1.2, cost_partial: true });
    expect(costText(cost)).toBe('at least $1.20');
    expect(costText({ ...cost, cost_inferred: true })).toBe('at least ≈ $1.20 inferred');
    expect(costText({ ...cost, cost_partial: false })).toBe('$1.20');
    expect(costText({ ...cost, cost_usd: null })).toBeNull();
  });

  test('an_inferred_cost_title_names_the_price_table', () => {
    const inferred = newUsage({ cost_usd: 1, cost_inferred: true, prices: '2026-10' });
    expect(costTitle(inferred)).toBe(
      `${apiPricesTitle(1)} Fleet Deck inferred it from its price table of 2026-10.`,
    );
    expect(costTitle({ ...inferred, cost_inferred: false })).toBe(apiPricesTitle(1));
    expect(costTitle(newUsage({ cost_usd: 1, cost_inferred: true }))).toBe(apiPricesTitle(1));
  });

  test('the usage tab says how many sessions hold an old count', () => {
    const html = (sessions: DeckSession[]) =>
      renderToStaticMarkup(<UsageMeters sessions={sessions} stations={[]} now={NOW} />);
    expect(html(fleet)).toContain(
      '<p class="fd-note">2 sessions have old counts that are not in these totals.</p>',
    );
    expect(html([fleet[2] as DeckSession])).toContain(
      '1 session has an old count that is not in these totals.',
    );
    expect(html([fleet[0] as DeckSession])).not.toContain('old count');
  });
});
