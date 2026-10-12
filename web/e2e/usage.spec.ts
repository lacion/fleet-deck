// M3 6-9 (DL-303): usage totals and rate-limit meters on the fleet screen
// at a phone viewport; unknown usage never shows as zero, and a delta
// moves the meter.
import { expect, test } from '@playwright/test';
import type { DeckSession } from '../src/gen';
import { summary } from '../src/testing/summary';
import { FakeDeck, fitsWidth, logIn, openFleet, openPanel, stationView } from './fake-deck';

const ST = 'st-alpha';

function session(id: string, used: number, extra: Partial<DeckSession['summary']> = {}) {
  const s: DeckSession = {
    station_id: ST,
    status: 'working',
    summary: summary(id, 'working', 1_760_000_000_000 + used, {
      title: `Session ${id}`,
      provider_instance: 'personal',
      rate_limits: [
        { scope: 'five_hour', used_percent: used, resets_at: 1_767_240_000_000 },
        { scope: 'weekly', used_percent: null, resets_at: null },
      ],
      ...extra,
    }),
  };
  return s;
}

test('the fleet shows usage totals and live rate-limit meters', async ({ page }) => {
  const deck = new FakeDeck();
  deck.stations = [stationView(ST, 'Alpha')];
  const quiet = session('s2', 10, {
    usage: { input_tokens: null, output_tokens: null, cost_usd: null },
  });
  deck.sessions = [
    session('s1', 30, { usage: { input_tokens: 1000, output_tokens: null, cost_usd: 0.5 } }),
    quiet,
  ];
  await deck.install(page);
  await logIn(page);
  await openFleet(page);
  await openPanel(page, 'Usage');
  const usage = page.getByRole('region', { name: 'Usage' });
  await expect(usage).toContainText('1,000 (1 session unknown)');
  await expect(usage.getByRole('definition').nth(1)).toHaveText('unknown');
  await expect(usage).toContainText('Alpha · personal · five_hour');
  await expect(usage).toContainText('30% used');
  await expect(usage).toContainText('Alpha · personal · weekly');
  await expect(usage).toContainText('use unknown');
  await expect(usage.getByRole('meter')).toHaveCount(1);
  expect(await fitsWidth(page)).toBe(true);

  deck.sessions = [session('s1', 95), quiet];
  deck.publish();
  await expect(usage).toContainText('95% used');
  await expect(usage.getByRole('meter')).toHaveAttribute('value', '95');
});
