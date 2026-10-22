import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type { DeckSession } from '../gen';
import { card, station } from '../testing/fleet';
import { UsageMeters } from './UsageMeters';
import {
  costText,
  dollars,
  fleetUsage,
  instanceLimit,
  limitChip,
  limitSuffix,
  meterText,
  mostRoom,
  rateMeters,
  resetsText,
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
        usage: { input_tokens: 1200, output_tokens: null, cost_usd: 0, cost_inferred: false },
      }),
      card('b', 'working', 'station-a', {
        usage: { input_tokens: 300, output_tokens: null, cost_usd: null, cost_inferred: false },
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
    const inferred = {
      input_tokens: 1,
      output_tokens: 1,
      cost_usd: 1.2,
      cost_inferred: true,
    };
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
