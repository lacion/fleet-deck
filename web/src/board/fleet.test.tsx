import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { card, station } from '../testing/fleet';
import {
  cardKey,
  emptyFilter,
  filterFleet,
  lastSeen,
  observedProviders,
  statusGroups,
  usageText,
} from './fleet';
import { FleetBoard } from './FleetBoard';
import { stationActions } from './pairing';
import { StationsPanel } from './StationsPanel';

describe('fleet projections', () => {
  test('cards from two stations with the same session ID keep different keys and both appear', () => {
    const cards = [card('same'), card('same', 'working', 'station-b')] as const;
    expect(cardKey(cards[0])).not.toBe(cardKey(cards[1]));
    const html = renderToStaticMarkup(
      <FleetBoard sessions={cards} stations={[station(), station('station-b')]} freshness="live" />,
    );
    expect(html.match(/data-session="same"/g)?.length).toBe(2);
    expect(html).toContain('Alpha');
    expect(html).toContain('Beta');
  });
  test('the deck status overrides the summary activity for verifying, unreachable and lost', () => {
    const cards = [card('v', 'verifying'), card('u', 'unreachable'), card('l', 'lost')];
    const groups = statusGroups(cards);
    expect(groups.working).toHaveLength(0);
    expect(groups.verifying[0]?.summary.session_id).toBe('v');
    expect(groups.unreachable[0]?.summary.session_id).toBe('u');
    expect(groups.lost[0]?.summary.session_id).toBe('l');
    const html = renderToStaticMarkup(
      <FleetBoard sessions={cards} stations={[]} freshness="live" />,
    );
    expect(html).toContain('Last activity: working');
  });
  test('status lanes sort by update time with station-scoped deterministic ties without mutating input', () => {
    const a = card('same', 'queued', 'station-a');
    const b = card('same', 'queued', 'station-b');
    const newest = card('new', 'queued', 'station-b', { updated_at: 9 });
    const cards = [b, a, newest];
    expect(statusGroups(cards).queued).toEqual([newest, a, b]);
    expect(cards).toEqual([b, a, newest]);
  });
  test('station, provider and search filters compose and clearing restores all cards', () => {
    const stations = [station(), station('station-b')];
    const cards = [
      card('a', 'working', 'station-a', { cwd: '/repos/deck' }),
      card('b', 'idle', 'station-b', { provider: 'codex' }),
    ] as const;
    expect(
      filterFleet(cards, stations, { station: 'station-b', provider: 'codex', search: ' BETA ' }),
    ).toEqual([cards[1]]);
    expect(
      filterFleet(cards, stations, { station: 'station-a', provider: 'codex', search: '' }),
    ).toEqual([]);
    expect(filterFleet(cards, stations, { ...emptyFilter, search: 'DECK' })).toEqual([cards[0]]);
    expect(filterFleet(cards, stations, emptyFilter)).toEqual([...cards]);
  });
  test('search finds title, model, instance and IDs even when the station was removed', () => {
    const cards = [
      card('one', 'idle', 'removed', {
        title: 'fix issue',
        model: 'opus',
        provider_instance: 'personal',
      }),
    ];
    for (const search of ['issue', 'opus', 'personal', 'one', 'removed'])
      expect(filterFleet(cards, [], { ...emptyFilter, search })).toEqual(cards);
    expect(filterFleet(cards, [], { ...emptyFilter, station: 'removed' })).toEqual(cards);
  });
  test('unknown providers match only the explicit unknown filter', () => {
    const cards = [
      card('a', 'working', 'station-a', { provider: null }),
      card('b', 'working', 'station-a', { provider: 'unknown' }),
      card('c'),
    ];
    expect(filterFleet(cards, [], { ...emptyFilter, provider: 'unknown' })).toEqual(
      cards.slice(0, 2),
    );
  });
  test('stale fleet cards retain their workspace and tools with an explicit cached marker', () => {
    const html = renderToStaticMarkup(
      <FleetBoard
        sessions={[card('a', 'working', 'station-a', { cwd: '/work', background_tasks: ['bg'] })]}
        stations={[]}
        freshness="stale"
      />,
    );
    expect(html).toContain('Cached · stale');
    expect(html).toContain('/work');
    expect(html).toContain('Background tasks: 1');
    expect(html).toContain('Branch: not reported');
  });
  test('zero usage remains zero and missing usage remains unknown', () => {
    expect(usageText(card('a'))).toBe('Input unknown · Output unknown · Cost unknown');
    expect(
      usageText(
        card('a', 'idle', 'station-a', {
          usage: { input_tokens: 0, output_tokens: 0, cost_usd: 0 },
        }),
      ),
    ).toBe('Input 0 · Output 0 · Cost $0.0000');
  });
});

describe('station projections', () => {
  test('live stations show link direction, fingerprint, cursor and last seen without invented capacity', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[
          station('station-a', {
            direction: 'deck_dials',
            dial_url: '127.0.0.1:443',
            cursor: 7,
            last_seen: 0,
          }),
          station('station-b', { reachable: false }),
        ]}
        sessions={[]}
        freshness="live"
      />,
    );
    for (const text of [
      'Deck dials station',
      'Station dials deck',
      '127.0.0.1:443',
      'fingerprint',
      '1970-01-01T00:00:00.000Z',
      'Unreachable',
      'Configured capacity',
      'Not reported',
      'Never',
    ])
      expect(html).toContain(text);
  });
  test('stale station metadata never labels a cached link as currently up', () => {
    const html = renderToStaticMarkup(
      <StationsPanel stations={[station()]} sessions={[]} freshness="stale" />,
    );
    expect(html).toContain('Stale link status');
    expect(html).toContain('Last reported: link up.');
    expect(html).not.toContain('data-health="up"');
  });
  test('observed providers are scoped, deduplicated and retain different instances and versions', () => {
    const cards = [
      card('a', 'working', 'station-a', { provider_instance: 'personal', provider_version: '2.1' }),
      card('b', 'idle', 'station-a', { provider_instance: 'personal', provider_version: '2.1' }),
      card('c', 'idle', 'station-a', { provider_instance: 'work', provider_version: '2.2' }),
      card('d', 'idle', 'station-b', { provider: 'codex' }),
    ];
    expect(observedProviders(cards, 'station-a')).toEqual([
      'Claude · personal · 2.1',
      'Claude · work · 2.2',
    ]);
    const html = renderToStaticMarkup(
      <StationsPanel stations={[station()]} sessions={cards} freshness="live" />,
    );
    // Observed providers stay apart from the configured inventory.
    expect(html).toContain('<h4>Providers observed in sessions</h4>');
    expect(html).toContain('Not reported until the station links to this deck.');
    expect(html).not.toContain('Codex');
  });
  test('active counts exclude exited and lost processes but include starting and stopping processes', () => {
    const cards = [
      card('a'),
      card('b', 'ended', 'station-a', { state: 'exited' }),
      card('c', 'lost', 'station-a', { state: 'lost' }),
      card('d', 'queued', 'station-a', { state: 'starting' }),
      card('e', 'working', 'station-a', { state: 'stopping' }),
    ];
    const html = renderToStaticMarkup(
      <StationsPanel stations={[station()]} sessions={cards} freshness="live" />,
    );
    expect(html).toContain('<dt>Observed active sessions</dt><dd>3</dd>');
  });
  test('a station report shows version, capacity and configured providers with their sign-in state', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[
          station('station-a', {
            info: {
              name: 'alpha-host',
              incarnation: 'inc',
              head: 9,
              live_sessions: 1,
              version: '0.2.0',
              max_sessions: 4,
              providers: [
                { instance: 'personal', provider: 'claude', version: '2.1.5', auth: 'signed_in' },
                { instance: 'work', provider: 'codex', version: null, auth: 'unknown' },
              ],
            },
            info_at: 0,
          }),
        ]}
        sessions={[]}
        freshness="live"
      />,
    );
    for (const text of [
      '1 live of 4 sessions',
      '0.2.0',
      'alpha-host',
      'personal · claude · 2.1.5 · signed in',
      'work · codex · version unknown · sign-in unknown',
    ])
      expect(html).toContain(text);
    expect(html).not.toContain('Not reported until the station links');
  });
  test('pairing controls appear only for an admin login on a live board', () => {
    const actions = (canAdmin: boolean) => stationActions(async () => ({}), canAdmin);
    const render = (canAdmin: boolean, freshness: 'live' | 'stale') =>
      renderToStaticMarkup(
        <StationsPanel
          stations={[station()]}
          sessions={[]}
          freshness={freshness}
          actions={actions(canAdmin)}
        />,
      );
    expect(render(true, 'live')).toContain('Add station');
    expect(render(true, 'live')).toContain('Remove station');
    expect(render(true, 'stale')).toMatch(/<button[^>]*disabled=""[^>]*>Add station/);
    expect(render(false, 'live')).toContain('Pairing needs the admin scope.');
    expect(render(false, 'live')).not.toContain('Remove station');
    expect(
      renderToStaticMarkup(<StationsPanel stations={[]} sessions={[]} freshness="live" />),
    ).not.toContain('Add station');
  });
  test('cards show the reported branch or an explicit unknown', () => {
    const html = renderToStaticMarkup(
      <FleetBoard
        sessions={[card('a', 'working', 'station-a', { branch: 'fd/feature-1' }), card('b')]}
        stations={[station()]}
        freshness="live"
      />,
    );
    expect(html).toContain('Branch: fd/feature-1');
    expect(html).toContain('Branch: not reported');
  });
  test('an empty panel reports no paired stations', () => {
    expect(
      renderToStaticMarkup(<StationsPanel stations={[]} sessions={[]} freshness="live" />),
    ).toContain('No paired stations.');
  });
  test('last seen distinguishes no link, epoch zero and invalid input', () => {
    expect(lastSeen(null)).toBe('Never');
    expect(lastSeen(0)).toBe('1970-01-01T00:00:00.000Z');
    expect(lastSeen(Number.NaN)).toBe('Unknown');
  });
});
