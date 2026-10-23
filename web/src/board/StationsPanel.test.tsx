import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationInfo, StationView } from '../gen';
import { SettingsProvider, type SettingsEnv } from '../settings/context';
import { card, station } from '../testing/fleet';
import { stationActions } from './pairing';
import { StationsPanel } from './StationsPanel';
import { stationsSummary } from './StationSummary';

const HOURS_8 = 8 * 3600_000;
const text = (node: ReturnType<typeof stationsSummary>) =>
  renderToStaticMarkup(<p>{node}</p>).replace(/<[^>]+>/g, '');
const panel = (stations: readonly StationView[], freshness: 'live' | 'stale' = 'live') =>
  renderToStaticMarkup(<StationsPanel stations={stations} sessions={[]} freshness={freshness} />);

describe('the station header meta', () => {
  // `last_seen` is the time the last link came up: on an online station it is the link age.
  test('an online station never shows "Last seen": the link age is the Linked fact', () => {
    const html = panel([station('station-a', { last_seen: Date.now() - HOURS_8 })]);
    expect(html).not.toContain('Last seen');
    expect(html).toContain('<dt>Linked</dt>');
    expect(html).toMatch(/<dd class="fd-facts-text" title="[^"]+">8 h ago<\/dd>/);
  });
  test('an offline station shows when it was last seen, and no Linked fact', () => {
    const html = panel([
      station('station-a', { reachable: false, last_seen: Date.now() - HOURS_8 }),
    ]);
    expect(html).toContain('Last seen 8 h ago');
    expect(html).not.toContain('<dt>Linked</dt>');
  });
  test('a station that was never seen shows no "Last seen" and no Linked fact', () => {
    const html = panel([station('station-a'), station('station-b', { reachable: false })]);
    expect(html).not.toContain('Last seen');
    expect(html).not.toContain('Linked');
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
  test('the page title has no count: the summary sentence carries it (§4.3)', () => {
    const html = renderToStaticMarkup(
      <StationsPanel stations={[station('station-a')]} sessions={[]} freshness="live" />,
    );
    expect(html).toContain('<h1 class="fd-page-title">Stations</h1></div>');
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

describe('the station card', () => {
  const info = (extra: Partial<StationInfo> = {}): StationInfo => ({
    name: 'alpha-host',
    incarnation: 'inc',
    head: 0,
    live_sessions: 0,
    version: null,
    max_sessions: 12,
    allow_shell: null,
    providers: [],
    ...extra,
  });
  test('a signed-out provider links the owner to the accounts of its station', () => {
    const signedOut = info({
      providers: [
        {
          instance: 'work',
          provider: 'codex',
          version: '0.163.0',
          auth: 'signed_out',
          version_status: 'unknown',
          capabilities: null,
        },
      ],
    });
    const render = (canAdmin: boolean) =>
      renderToStaticMarkup(
        <StationsPanel
          stations={[station('station-a', { info: signedOut })]}
          sessions={[]}
          freshness="live"
          actions={stationActions(async () => ({}), canAdmin)}
        />,
      );
    // The version is quiet; the next step follows the state.
    expect(render(true)).toContain(
      '<span>Codex <span class="fd-station-ver">0.163.0</span></span><span class="fd-station-auth" data-auth="hazard">',
    );
    expect(render(true)).toContain(
      'signed out</span><a class="fd-station-signin fd-tap" href="#/settings/accounts?station=station-a">Sign in…</a></li>',
    );
    expect(render(false)).not.toContain('Sign in…');
  });
  test('Stations is a wide page, the same width as the other list pages', () => {
    expect(panel([station('station-a')])).toContain(
      '<section class="fd-page fd-stations" data-wide="true" aria-label="Stations">',
    );
  });
  test('the slot count reads "2 of 12"; "sessions" is for a screen reader only', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { info: info() })]}
        sessions={[card('s1'), card('s2')]}
        freshness="live"
      />,
    );
    expect(html).toContain(
      '<span class="fd-station-load-count">2 of 12<span class="fd-vh"> sessions</span></span>',
    );
    expect(html).toContain('<span data-slot="used"></span>');
  });
  test('a station that runs no session shows its free slots as words, as its fleet lane', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { info: info() })]}
        sessions={[card('s1', 'ended', 'station-a', { state: 'exited' })]}
        freshness="live"
      />,
    );
    expect(html).toContain(
      '<span class="fd-station-load"><span class="fd-station-load-count" data-free="true">12 free<span class="fd-vh"> session slots</span></span></span>',
    );
    expect(html).not.toContain('fd-slots');
    expect(html).not.toContain('0 of 12');
  });
  test('more than 24 slots draw a neutral bar, not ticks', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { info: info({ max_sessions: 40 }) })]}
        sessions={[card('s1'), card('s2')]}
        freshness="live"
      />,
    );
    expect(html).toContain('<span class="fd-meter" data-tone="neutral" aria-hidden="true">');
    expect(html).toContain('2 of 40<span class="fd-vh"> sessions</span>');
    expect(html).not.toContain('fd-slots');
  });
  test('a login without the admin scope reads why in the page sentence, not a loose note', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a')]}
        sessions={[]}
        freshness="live"
        actions={stationActions(async () => ({}), false)}
      />,
    );
    expect(html).toContain(
      '<p class="fd-page-desc">1 station, online. Pairing needs the admin scope.</p>',
    );
    expect(html).not.toContain('<p class="fd-note">Pairing needs the admin scope.</p>');
    // No station: the scope sentence alone.
    expect(
      renderToStaticMarkup(
        <StationsPanel
          stations={[]}
          sessions={[]}
          freshness="live"
          actions={stationActions(async () => ({}), false)}
        />,
      ),
    ).toContain('<p class="fd-page-desc">Pairing needs the admin scope.</p>');
    expect(panel([station('station-a')])).not.toContain('admin scope');
  });
  test('the station id is shortened like the key fingerprint, with its own copy button', () => {
    const id = 'f'.repeat(64);
    const html = panel([station(id)]);
    expect(html).toContain(`<span title="${id}">ffffffff…ffff</span>`);
    expect(html).toContain('aria-label="Copy the station ID"');
    expect(html).toContain('aria-label="Copy the key fingerprint"');
    expect(html).not.toContain(`>${id}<`);
  });
  test('a station without Taildrive has a quiet Details fact, not a body line', () => {
    const taildrive = {
      state: 'unavailable' as const,
      reason: 'tailscale drive list failed (exit 1)',
      shares: [],
      checked_at: 1,
      inventory_revision: 1,
    };
    const html = panel([station('station-a', { info: info({ taildrive }) })]);
    expect(html).toContain(
      '<dt>Taildrive</dt><dd class="fd-facts-text" title="tailscale drive list failed (exit 1)">Not available</dd>',
    );
    // The tool error is only the tooltip.
    expect(html.match(/exit 1/g)?.length).toBe(1);
    expect(html).not.toContain('fd-station-taildrive');
  });
  test('the menu is a column of its own in the header, after the line', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a')]}
        sessions={[]}
        freshness="live"
        actions={{ canAdmin: true } as never}
        drift={{ offered: false, station: null, error: null, set: async () => {} }}
      />,
    );
    expect(html).toMatch(
      /<div class="fd-station-head"><div class="fd-station-line">.*<\/div><div class="fd-station-menu">/,
    );
  });
});

describe('the desktop of a station card', () => {
  test('a station with info and no desktop has no Desktop fold and no note', () => {
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
    expect(html).not.toContain('This station reports no desktop.');
    expect(html).not.toContain('fd-desk');
    // The folds are toggles (base.css summary[data-variant=toggle]).
    expect(html).toContain('<summary class="fd-label" data-variant="toggle">Details</summary>');
  });
});

describe('the config reference', () => {
  test('a refused reference shows the deck error as a page error notice and keeps the old reference', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('st-alpha', { name: 'Alpha' }), station('st-bravo', { name: 'Bravo' })]}
        sessions={[]}
        freshness="live"
        drift={{
          offered: true,
          station: 'st-alpha',
          error: 'Unknown station.',
          set: async () => {},
        }}
      />,
    );
    // §4.7: an error of a page is ErrorNotice, never a bare alert paragraph.
    expect(html).toContain(
      '<div class="fd-notice fd-notice-row" data-tone="error" data-place="page">',
    );
    expect(html).toContain('<span class="fd-notice-text" role="alert">Unknown station.</span>');
    expect(html).not.toContain('<p role="alert">');
    expect(html).toContain('Configs vs Alpha');
  });
  test('no alert while the deck has not answered', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('st-alpha')]}
        sessions={[]}
        freshness="live"
        drift={{ offered: false, station: null, error: 'Unknown station.', set: async () => {} }}
      />,
    );
    expect(html).not.toContain('role="alert"');
  });
});
