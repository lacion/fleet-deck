import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { card, station } from '../testing/fleet';
import { UsageMeters } from './UsageMeters';
import { dollars, fleetUsage, meterText, rateMeters, tokens, totalText } from './usage';

const window5h = (used: number | null, at = 1_767_240_000_000) => ({
  scope: 'five_hour',
  used_percent: used,
  resets_at: at,
});

describe('usage meters (DL-303)', () => {
  test('a total of unknown parts is unknown, never zero, and a partial total names the gap', () => {
    const u = fleetUsage([
      card('a', 'working', 'station-a', {
        usage: { input_tokens: 1200, output_tokens: null, cost_usd: 0 },
      }),
      card('b', 'working', 'station-a', {
        usage: { input_tokens: 300, output_tokens: null, cost_usd: null },
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
    expect(meters.map((m) => [m.label, m.scope, m.usedPercent])).toEqual([
      ['Alpha · personal', 'five_hour', 20],
      ['Alpha · personal', 'weekly', null],
      ['Beta · personal', 'five_hour', 70],
    ]);
    expect(meters.map(meterText).slice(0, 2)).toEqual([
      '20% used, resets 2026-01-01 04:00 UTC',
      'use unknown',
    ]);
  });

  test('the fleet screen shows the usage panel; an unknown window has no meter bar', () => {
    const sessions = [
      card('a', 'working', 'station-a', {
        provider_instance: 'personal',
        rate_limits: [window5h(42), { scope: 'weekly', used_percent: null, resets_at: null }],
      }),
    ];
    const html = renderToStaticMarkup(<UsageMeters sessions={sessions} stations={[station()]} />);
    expect(html).toContain('aria-label="Usage"');
    expect(html.match(/<meter /g)?.length).toBe(1);
    expect(html).toContain('42% used');
    expect(html).toContain('use unknown');
    expect(html).toContain('<dd>unknown</dd>');
    expect(html).not.toContain('>0<');
    const empty = renderToStaticMarkup(<App sessions={[]} freshness="live" />);
    expect(empty).toContain('No provider reported a rate limit yet.');
  });
});
