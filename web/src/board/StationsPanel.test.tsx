import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationView } from '../gen';
import { SettingsProvider, type SettingsEnv } from '../settings/context';
import { station } from '../testing/fleet';
import { StationsPanel } from './StationsPanel';
import { showLastSeen, stationsSummary } from './StationSummary';

const NOW = 1_000_000_000;
const text = (node: ReturnType<typeof stationsSummary>) =>
  renderToStaticMarkup(<p>{node}</p>).replace(/<[^>]+>/g, '');

describe('the station header meta', () => {
  test('an online station seen in the last 2 min shows no "Last seen"', () => {
    expect(showLastSeen(NOW - 60_000, true, NOW)).toBe(false);
    expect(showLastSeen(NOW - 120_000, true, NOW)).toBe(false);
    expect(showLastSeen(NOW - 121_000, true, NOW)).toBe(true);
  });
  test('a station that is not up shows when it was last seen', () => {
    expect(showLastSeen(NOW - 1_000, false, NOW)).toBe(true);
  });
  test('a station that was never seen shows no "Last seen"', () => {
    expect(showLastSeen(null, false, NOW)).toBe(false);
    expect(showLastSeen(null, true, NOW)).toBe(false);
  });
  test('an online station seen now renders no "Last seen" and no empty meta line', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { last_seen: Date.now() })]}
        sessions={[]}
        freshness="live"
      />,
    );
    expect(html).not.toContain('Last seen');
    expect(html).toContain('<div class="fd-station-live"></div>');
  });
  test('an offline station renders "Last seen"', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { reachable: false, last_seen: Date.now() })]}
        sessions={[]}
        freshness="live"
      />,
    );
    expect(html).toContain('Last seen now');
  });
});

describe('the page summary sentence (§4.3)', () => {
  const up = (id: string): StationView => station(id);
  const down = (id: string): StationView => station(id, { reachable: false });
  test('no station has no sentence', () => {
    expect(stationsSummary([], 'live')).toBeNull();
  });
  test('all online', () => {
    expect(text(stationsSummary([up('a'), up('b'), up('c')], 'live'))).toBe(
      '3 stations, all online.',
    );
    expect(text(stationsSummary([up('a')], 'live'))).toBe('1 station, online.');
  });
  test('some offline: the offline count is a hazard', () => {
    const html = renderToStaticMarkup(
      <p>{stationsSummary([up('a'), down('b'), down('c')], 'live')}</p>,
    );
    expect(html).toContain('<span class="fd-stations-off">2 offline</span>');
    expect(text(stationsSummary([up('a'), down('b'), down('c')], 'live'))).toBe(
      '1 of 3 stations online. 2 offline.',
    );
    expect(text(stationsSummary([down('a')], 'live'))).toBe('1 station, offline.');
  });
  test('a stale board does not claim a link status', () => {
    expect(text(stationsSummary([up('a'), down('b')], 'stale'))).toBe(
      '2 stations. The link status is stale.',
    );
  });
  test('the page header shows the sentence', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[up('station-a'), down('station-b')]}
        sessions={[]}
        freshness="live"
      />,
    );
    expect(html).toContain('<p class="fd-page-desc">1 of 2 stations online. ');
  });
});

describe('the desktop of a station card', () => {
  test('a station with info and no desktop says so', () => {
    const view = station('station-a', {
      info: {
        name: 'alpha-host',
        incarnation: 'inc',
        head: 0,
        live_sessions: 0,
        version: null,
        max_sessions: null,
        allow_shell: null,
        providers: [],
      },
      info_at: 0,
    });
    const html = renderToStaticMarkup(
      <SettingsProvider env={{} as SettingsEnv}>
        <StationsPanel stations={[view]} sessions={[]} freshness="live" />
      </SettingsProvider>,
    );
    expect(html).toContain('<p class="fd-note">This station reports no desktop.</p>');
  });
});
