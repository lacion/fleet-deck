import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { WorktreeInfo, WorktreeTrash } from '../gen';
import type { ShellActions } from '../shell/shell';
import { card, station } from '../testing/fleet';
import { archived } from '../ui/archive';
import { priority } from '../ui/localSet';
import {
  archivedHidden,
  cardKey,
  contestedBy,
  emptyFilter,
  filterFleet,
  lastSeen,
  NO_REPO,
  observedProviders,
  repoName,
  repoNames,
  statusGroups,
  usageText,
} from './fleet';
import { COLUMNS, columnCards, FleetBoard } from './FleetBoard';
import { removeCommand, stationActions, worktreeLoader } from './pairing';
import { loadWidth, StationsPanel, WorktreeRow } from './StationsPanel';

describe('fleet projections', () => {
  test('cards from two stations with the same session ID keep different keys and both appear', () => {
    const cards = [card('same'), card('same', 'working', 'station-b')] as const;
    expect(cardKey(cards[0])).not.toBe(cardKey(cards[1]));
    const html = renderToStaticMarkup(
      <FleetBoard sessions={cards} stations={[station(), station('station-b')]} freshness="live" />,
    );
    expect(html.match(/data-session="same"/g)?.length).toBe(2);
    expect(html).toContain('aria-label="Station Alpha"');
    expect(html).toContain('aria-label="Station Beta"');
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
    // the card shows the deck status, not the activity "working" of the summary
    for (const [id, status, note] of [
      ['v', 'verifying', 'Verifying'],
      ['u', 'unreachable', 'Station unreachable'],
      ['l', 'lost', 'Lost'],
    ])
      expect(html).toMatch(
        new RegExp(
          `data-session="${id}"[^>]*data-status="${status}"[\\s\\S]*?data-note="${status}">${note}<`,
        ),
      );
    expect(html).not.toContain('data-note="working"');
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
      filterFleet(cards, stations, {
        station: 'station-b',
        provider: 'codex',
        repo: '',
        search: ' BETA ',
      }),
    ).toEqual([cards[1]]);
    expect(
      filterFleet(cards, stations, {
        station: 'station-a',
        provider: 'codex',
        repo: '',
        search: '',
      }),
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
  test('stale strips retain their workspace and tools with an explicit Stale icon', () => {
    const html = renderToStaticMarkup(
      <FleetBoard
        sessions={[card('a', 'working', 'station-a', { cwd: '/work', background_tasks: ['bg'] })]}
        stations={[]}
        freshness="stale"
      />,
    );
    expect(html).toContain('data-ico="stale" title="Stale"');
    // with no branch the place is the directory
    expect(html).toContain('title="Folder: work"');
    expect(html).toMatch(/title="Background tasks">[\s\S]*?<\/svg><span>1<\/span>/);
    const live = renderToStaticMarkup(
      <FleetBoard sessions={[card('a')]} stations={[]} freshness="live" />,
    );
    expect(live).not.toContain('Stale');
  });
  test('zero usage remains zero and missing usage remains unknown', () => {
    expect(usageText(card('a'))).toBe('Input unknown · Output unknown · Cost unknown');
    expect(
      usageText(
        card('a', 'idle', 'station-a', {
          usage: { input_tokens: 0, output_tokens: 0, cost_usd: 0, cost_inferred: false },
        }),
      ),
    ).toBe('Input 0 · Output 0 · Cost $0.0000');
  });
});

// The static render never calls a shell action.
const shellActions = {} as ShellActions;

describe('station projections', () => {
  test('live stations show link direction, fingerprint and last seen without invented capacity', () => {
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
      'offline',
      'Not stored, the station dials this deck',
      'Last seen —',
      '0 sessions',
    ])
      expect(html).toContain(text);
    expect(html).not.toContain('Dial-only');
    expect(html).not.toContain('Not reported');
  });
  test('the card head states the link health as a fact with a dot, not a badge', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a'), station('station-b', { reachable: false })]}
        sessions={[]}
        freshness="live"
      />,
    );
    expect(html).toContain(
      '<span class="fd-station-health fd-health" data-health="up"><span class="fd-dot" data-status="online" aria-hidden="true"></span>online</span>',
    );
    expect(html).toContain(
      '<span class="fd-station-health fd-health" data-health="down"><span class="fd-dot" data-status="unreachable" aria-hidden="true"></span>offline</span>',
    );
    expect(html).toContain('<h3 class="fd-station-name">Alpha</h3>');
    expect(html).not.toContain('fd-badge');
  });
  test('the Details fold holds the id, the key, the direction and the link address', () => {
    const html = renderToStaticMarkup(
      <StationsPanel stations={[station()]} sessions={[]} freshness="live" />,
    );
    const details = /<details class="fd-station-details">([\s\S]*?)<\/details>/.exec(html)?.[1];
    expect(details).toContain('<summary class="fd-label">Details</summary>');
    for (const text of ['Station ID', 'Key fingerprint', 'Direction', 'Link address'])
      expect(details).toContain(`<dt>${text}</dt>`);
    // The ids are not in the head or in the live facts.
    expect(html.replace(details ?? '', '')).not.toContain('Station ID');
  });
  test('the load shows a bar, 90 percent and more in the act color, and caps at 100 percent', () => {
    expect(loadWidth(0, 4)).toBe('0%');
    expect(loadWidth(1, 4)).toBe('25%');
    expect(loadWidth(9, 4)).toBe('100%');
    expect(loadWidth(3, 0)).toBe('0%');
    const render = (max: number, count: number) =>
      renderToStaticMarkup(
        <StationsPanel
          stations={[
            station('station-a', {
              info: {
                name: 'alpha-host',
                incarnation: 'inc',
                head: 9,
                live_sessions: 0,
                version: null,
                max_sessions: max,
                allow_shell: null,
                providers: [],
              },
              info_at: 0,
            }),
          ]}
          sessions={Array.from({ length: count }, (_, i) => card(`s${i}`))}
          freshness="live"
        />,
      );
    const low = render(10, 5);
    expect(low).toContain('<span style="width:50%"></span>');
    expect(low).toContain('5 of 10 sessions');
    expect(low).not.toContain('data-high');
    const high = render(10, 9);
    expect(high).toContain('<span style="width:90%" data-high=""></span>');
  });
  test('stale station metadata never labels a cached link as currently up', () => {
    const html = renderToStaticMarkup(
      <StationsPanel stations={[station()]} sessions={[]} freshness="stale" />,
    );
    expect(html).toContain('Stale link status');
    expect(html).toContain('Last reported: online.');
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
    expect(html).not.toContain('Providers observed in sessions');
    expect(html).toContain('Reported when the station links to this deck.');
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
    expect(html).toContain('3 sessions');
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
              allow_shell: null,
              providers: [
                {
                  instance: 'personal',
                  provider: 'claude',
                  version: '2.1.5',
                  auth: 'signed_in',
                  version_status: 'unknown',
                  capabilities: null,
                },
                {
                  instance: 'work',
                  provider: 'codex',
                  version: null,
                  auth: 'unknown',
                  version_status: 'unknown',
                  capabilities: null,
                },
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
      '0 of 4 sessions',
      '<dt>Version</dt><dd>0.2.0</dd>',
      '<li data-instance="personal" title="personal"><span>Claude 2.1.5</span><span class="fd-station-auth" data-auth="ok"><span class="fd-dot" data-status="online" aria-hidden="true"></span>signed in</span></li>',
      '<li data-instance="work" title="work"><span>Codex</span><span class="fd-station-auth" data-auth="muted"><span class="fd-dot" aria-hidden="true"></span>sign-in unknown</span></li>',
    ])
      expect(html).toContain(text);
    expect(html).not.toContain(' · ');
    expect(html).not.toContain('fd-badge');
    expect(html).not.toContain('Reported when the station links');
  });
  test('the key fingerprint is shortened with the full value in a title and a copy button', () => {
    const html = renderToStaticMarkup(
      <StationsPanel
        stations={[station('station-a', { key: 'abcdef0123456789abcdef' })]}
        sessions={[]}
        freshness="live"
      />,
    );
    expect(html).toContain('<span title="abcdef0123456789abcdef">abcdef012345…</span>');
    expect(html).toContain('aria-label="Copy the key fingerprint"');
  });
  test('a station without shells says so and offers no directory field', () => {
    const base = {
      name: 'alpha-host',
      incarnation: 'inc',
      head: 9,
      live_sessions: 0,
      version: null,
      max_sessions: null,
      providers: [],
    };
    const render = (allow: boolean | null) =>
      renderToStaticMarkup(
        <StationsPanel
          stations={[station('station-a', { info: { ...base, allow_shell: allow } })]}
          sessions={[]}
          freshness="live"
          actions={{ ...stationActions(async () => ({}), true), shells: shellActions }}
        />,
      );
    const off = render(false);
    expect(off).toContain(
      'Shells are off on this station. Set allow_shell = true in station.toml.',
    );
    expect(off).not.toContain('Open shell');
    expect(off).toContain('No provider configured.');
    expect(render(null)).toContain('Open shell');
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
    expect(render(true, 'live')).toContain('Remove station…');
    expect(render(true, 'live')).toContain('aria-label="Station actions"');
    // The remove button lives in the menu and the confirm shows only after it.
    expect(render(true, 'live')).toMatch(/fd-menu-body[^>]*><button[^>]*>[\s\S]*?Remove station…/);
    expect(render(true, 'live')).not.toContain('Confirm remove');
    expect(render(true, 'stale')).toMatch(/<button[^>]*disabled=""[^>]*>[\s\S]*?Add station/);
    expect(render(false, 'live')).toContain('Pairing needs the admin scope.');
    expect(render(false, 'live')).not.toContain('Remove station');
    expect(
      renderToStaticMarkup(<StationsPanel stations={[]} sessions={[]} freshness="live" />),
    ).not.toContain('Add station');
  });
  test('strips name the reported branch, else the directory, else no place', () => {
    const html = renderToStaticMarkup(
      <FleetBoard
        sessions={[
          card('a', 'working', 'station-a', { branch: 'fd/feature-1', cwd: '/repos/deck' }),
          card('b'),
        ]}
        stations={[station()]}
        freshness="live"
      />,
    );
    expect(html).toContain('title="Branch: fd/feature-1"');
    expect(html).not.toContain('Folder: deck');
    // a session with no title shows its place in the title cell
    expect(html).toContain('<span class="fd-strip-place">fd/feature-1</span>');
    // a session without a branch and a directory has no place title and an empty title cell
    expect(html).toMatch(
      /<span class="fd-strip-title"><span class="fd-dot" data-status="working" aria-hidden="true"><\/span><span class="fd-strip-text"><\/span><\/span>/,
    );
  });
  test('an empty panel reports no paired stations', () => {
    expect(
      renderToStaticMarkup(<StationsPanel stations={[]} sessions={[]} freshness="live" />),
    ).toContain('No paired stations');
  });
  test('last seen distinguishes no link, epoch zero and invalid input', () => {
    expect(lastSeen(null)).toBe('Never');
    expect(lastSeen(0)).toBe('1970-01-01T00:00:00.000Z');
    expect(lastSeen(Number.NaN)).toBe('Unknown');
  });
});

describe('repo names, archive and contested sessions', () => {
  const inRepo = (id: string, origin: string | null, root: string, status = 'working' as const) =>
    card(id, status, 'station-a', { repo: { root, origin } });

  test('the repo name is the last two parts of the origin, else the folder of the root', () => {
    expect(repoName(inRepo('a', 'git@github.com:org/app.git', '/w/app'))).toBe('org/app');
    expect(repoName(inRepo('a', 'https://github.com/org/app.git', '/w/app'))).toBe('org/app');
    expect(repoName(inRepo('a', 'github.com/org/app', '/w/app'))).toBe('org/app');
    expect(repoName(inRepo('a', null, '/home/me/projects/deck'))).toBe('deck');
    expect(repoName(card('a'))).toBe(NO_REPO);
  });

  test('repo names are sorted with "No repository" last', () => {
    const cards = [
      card('n'),
      inRepo('b', null, '/w/zeta'),
      inRepo('c', null, '/w/alpha'),
      inRepo('d', null, '/w/alpha'),
    ];
    expect(repoNames(cards)).toEqual(['alpha', 'zeta', NO_REPO]);
  });

  test('the repo filter matches the repo name', () => {
    const cards = [inRepo('a', null, '/w/alpha'), inRepo('b', null, '/w/beta'), card('c')] as const;
    expect(filterFleet(cards, [], { ...emptyFilter, repo: 'beta' })).toEqual([cards[1]]);
    expect(filterFleet(cards, [], { ...emptyFilter, repo: NO_REPO })).toEqual([cards[2]]);
  });

  test('an archived session is hidden only while it is ended or lost', () => {
    const refs = new Set(['station-a/e', 'station-a/w']);
    expect(archivedHidden(card('e', 'ended'), refs)).toBe(true);
    expect(archivedHidden(card('w', 'working'), refs)).toBe(false);
    expect(archivedHidden(card('x', 'ended'), refs)).toBe(false);
  });

  test('a session is contested with the other sessions that touch the same file', () => {
    const touch = (station_id: string, session_id: string, session_title: string | null) => ({
      station_id,
      session_id,
      session_title,
      path: 'lib.rs',
      touched_at: 1,
    });
    const map = contestedBy([
      {
        repo: null,
        file: 'src/lib.rs',
        touches: [touch('station-a', 's1', 'One'), touch('station-b', 's2', null)],
      },
    ]);
    expect(map.get('station-a/s1')).toEqual({ with: ['s2'], files: ['src/lib.rs'] });
    expect(map.get('station-b/s2')).toEqual({ with: ['One'], files: ['src/lib.rs'] });
    expect(map.get('station-a/s3')).toBeUndefined();
  });
});

describe('fleet cards, DL-141', () => {
  const html = (cards: ReturnType<typeof card>[], extra = {}) =>
    renderToStaticMarkup(
      <FleetBoard sessions={cards} stations={[station()]} freshness="live" {...extra} />,
    );

  test('the board has lanes of strips and no column heads', () => {
    const out = html([card('a')]);
    expect(out).toContain('class="fd-lane"');
    expect(out).toContain('<ul class="fd-strips">');
    expect(out).not.toContain('fd-colhead');
    expect(COLUMNS).toHaveLength(5);
  });

  test('a contested card is marked and names the others', () => {
    const contested = new Map([['station-a/a', { with: ['Beta fix', 'Gamma'], files: ['x.rs'] }]]);
    const out = html([card('a'), card('b')], { contested });
    expect(out).toMatch(/data-session="a"[^>]*data-contested="true"/);
    expect(out).toContain(
      'data-ico="contested" data-tone="hazard" title="Contested with Beta fix, Gamma"',
    );
    expect(out.match(/data-contested/g)).toHaveLength(1);
  });

  test('the bypass modes show a hazard shield icon, other modes none', () => {
    const out = html([
      card('a', 'working', 'station-a', { mode: 'bypassPermissions' }),
      card('b', 'working', 'station-a', { mode: 'dontAsk' }),
      card('c', 'working', 'station-a', { mode: 'default' }),
    ]);
    expect(out.match(/data-ico="risky" data-tone="hazard"/g)).toHaveLength(2);
    expect(out).toContain('title="Permission mode: bypassPermissions"');
    expect(out).toContain('title="Permission mode: dontAsk"');
    expect(out).not.toContain('Permission mode: default');
  });

  test('the last error replaces the status text of an idle, ended or lost card only', () => {
    const last_error = { code: 'rate_limit', message: 'Try later', retry_at: null };
    const out = html([
      card('i', 'idle', 'station-a', { last_error }),
      card('w', 'working', 'station-a', { last_error }),
      card('q', 'idle', 'station-a', {
        last_error,
        open_requests: [
          {
            request_id: 'r',
            node_id: null,
            request_kind: 'tool',
            title: 'Ask',
            details: '',
            input: null,
            options: [],
            response: 'rpc_reply',
            requested_at: 1,
          },
        ],
      }),
    ]);
    expect(out.match(/fd-card-error">Try later\.</g)).toHaveLength(1);
    expect(out).toMatch(/data-session="i"[\s\S]*?fd-card-error/);
    expect(out).toContain('Working</span>');
  });

  test('a starred card sorts first in its column and shows a star', () => {
    const groups = statusGroups([card('a'), card('b', 'working', 'station-a', { updated_at: 5 })]);
    const working = COLUMNS[1];
    expect(columnCards(groups, working?.statuses ?? []).map((c) => c.summary.session_id)).toEqual([
      'b',
      'a',
    ]);
    const starred = new Set(['station-a/a']);
    expect(
      columnCards(groups, working?.statuses ?? [], starred).map((c) => c.summary.session_id),
    ).toEqual(['a', 'b']);
    priority.add(['station-a/a']);
    const out = html([card('a'), card('b')]);
    priority.remove(['station-a/a']);
    expect(out.match(/class="fd-card-star"/g)).toHaveLength(1);
  });

  test('the offline tools row shows Resume all and Clear; a cleared strip shows its Archived icon', () => {
    const ended = card('e', 'ended');
    const none = html([card('a')]);
    expect(none).not.toContain('Resume all');
    expect(none).not.toContain('Offline sessions');
    const out = html([ended]);
    expect(out).toContain('<span class="fd-label">Offline sessions</span>');
    expect(out).toContain('Resume all (0)');
    expect(out).toContain('Clear (1)');
    expect(out).not.toContain('Restore all');
    archived.add(['station-a/e']);
    const shown = html([ended]);
    archived.remove(['station-a/e']);
    expect(shown).toContain('data-archived="true"');
    expect(shown).toContain('data-ico="archived" title="Archived"');
    expect(shown).toContain('Clear (0)');
    expect(shown).toContain('Restore all');
  });

  test('repo grouping makes one lane for each repo and shows the station on the cards', () => {
    const cards = [
      card('a', 'working', 'station-a', { repo: { root: '/w/alpha', origin: null } }),
      card('b', 'working', 'station-a', { repo: { root: '/w/alpha', origin: null } }),
      card('n'),
    ];
    const contested = new Map([['station-a/a', { with: ['b'], files: ['f'] }]]);
    const out = html(cards, { group: 'repo', contested });
    expect(out).toContain('aria-label="Repository alpha"');
    expect(out).toContain('aria-label="Repository No repository"');
    expect(out).not.toContain('aria-label="Station Alpha"');
    expect(out).toContain('▲ 1 contested');
    expect(out).toContain('Alpha');
  });
});

describe('worktree rows (DL-142)', () => {
  const base: WorktreeInfo = {
    session_id: 's1',
    title: 'Fix the login',
    path: '/w/deck-wt/s1',
    branch: 'fd/s1',
    main: '/w/deck',
    exists: true,
    registered: true,
    dirty: 2,
    ignored: 5,
    unpushed: 1,
    compared_with: 'remotes',
    last_commit: { subject: 'wip', at: 0 },
    live: false,
    verdict: 'has_work',
    note: null,
  };
  const row = (extra: Partial<WorktreeInfo>) =>
    renderToStaticMarkup(
      <WorktreeRow worktree={{ ...base, ...extra }} station="station-a" now={3 * 3_600_000} />,
    );

  test('a row shows the badge, the folder, the branch, the evidence and the session link', () => {
    const html = row({});
    expect(html).toContain('data-verdict="has_work"');
    expect(html).toContain('data-tone="act">Has work<');
    expect(html).toContain('title="/w/deck-wt/s1">s1<');
    expect(html).toContain('>fd/s1<');
    expect(html).toContain('href="#/s/station-a/s1"');
    expect(html).toContain('>Fix the login<');
    expect(html).toContain('2 changed · 5 ignored · 1 commits not on a remote');
    expect(html).toContain('last commit “wip” 3 h');
    expect(html).toContain('Copy the remove command');
  });

  test('each verdict has its badge and the right extras', () => {
    expect(row({ verdict: 'clean' })).toContain('data-tone="ok">Clean<');
    expect(row({ verdict: 'live', live: true })).toContain('data-tone="info">Live<');
    const missing = row({ verdict: 'missing', exists: false, registered: false });
    expect(missing).toContain('data-tone="muted">Missing<');
    expect(missing).not.toContain('Copy the remove command');
    expect(missing).not.toContain('Git does not list');
    expect(row({ verdict: 'live', live: true })).not.toContain('Copy the remove command');
    const unknown = row({ verdict: 'unknown', dirty: null, note: 'git timed out', title: null });
    expect(unknown).toContain('? changed · 5 ignored');
    expect(unknown).toContain('git timed out');
    expect(unknown).toContain('>s1</a>');
    expect(row({ registered: false })).toContain('Git does not list this worktree.');
  });

  test('a null comparison names main', () => {
    expect(row({ compared_with: 'main' })).toContain('commits not on main');
  });
});

describe('worktree rows with the trash buttons (DL-150)', () => {
  const clean: WorktreeInfo = {
    session_id: 's1',
    title: 'Fix the login',
    path: '/w/deck-wt/s1',
    branch: 'fd/s1',
    main: '/w/deck',
    exists: true,
    registered: true,
    dirty: 0,
    ignored: 0,
    unpushed: 0,
    compared_with: 'remotes',
    last_commit: null,
    live: false,
    verdict: 'clean',
    note: null,
  };
  const trash = (extra: Partial<WorktreeTrash>): WorktreeTrash => ({
    state: 'trashed',
    phase: null,
    message: null,
    trash_path: '/w/.trash/s1-abc',
    trashed_at: 0,
    ...extra,
  });
  const row = (extra: Partial<WorktreeInfo>, act = true) =>
    renderToStaticMarkup(
      <WorktreeRow
        worktree={{ ...clean, ...extra }}
        station="station-a"
        now={3 * 3_600_000}
        onAction={act ? async () => ({ ok: true, text: 'x' }) : undefined}
      />,
    );

  test('a clean ended row offers Move to trash; a live, missing or read-only row does not', () => {
    expect(row({})).toContain('Move to trash');
    expect(row({ live: true, verdict: 'live' })).not.toContain('Move to trash');
    expect(row({ exists: false, verdict: 'missing' })).not.toContain('Move to trash');
    expect(row({}, false)).not.toContain('Move to trash');
  });
  test('a trashed row shows Restore, Delete permanently, the age and the path', () => {
    const html = row({ verdict: 'trashed', trash: trash({}) });
    expect(html).toContain('data-tone="muted">In the trash<');
    expect(html).toContain('Restore');
    expect(html).toContain('Delete permanently');
    expect(html).toContain('In the trash since 3 h ago');
    expect(html).toContain('title="/w/.trash/s1-abc">/w/.trash/s1-abc<');
    expect(html).not.toContain('Move to trash');
    expect(html).not.toContain('Copy the remove command');
    expect(row({ verdict: 'trashed', trash: trash({ trashed_at: 3 * 3_600_000 }) })).toContain(
      'In the trash since now',
    );
  });
  test('a failed purge shows the message, Delete permanently and Stop tracking', () => {
    const html = row({
      verdict: 'trashed',
      trash: trash({ state: 'failed', phase: 'purge', message: 'busy' }),
    });
    expect(html).toContain('The last step failed: busy');
    expect(html).toContain('Delete permanently');
    expect(html).toContain('Stop tracking');
    expect(html).not.toContain('>Restore<');
  });
  test('a failed trash or restore offers Restore, and an unknown phase only Stop tracking', () => {
    const failed = (phase: string | null) =>
      row({ verdict: 'trashed', trash: trash({ state: 'failed', phase, message: 'no' }) });
    expect(failed('trash')).toContain('Restore');
    expect(failed('restore')).toContain('Restore');
    expect(failed('restore')).not.toContain('Delete permanently');
    const unknown = failed(null);
    expect(unknown).toContain('Stop tracking');
    expect(unknown).not.toContain('Restore');
    expect(unknown).not.toContain('Delete permanently');
  });
  test('a row in progress shows its line and no buttons', () => {
    const html = row({ verdict: 'trashed', trash: trash({ state: 'purging' }) });
    expect(html).toContain('Deleting…');
    expect(html).not.toContain('<button');
    expect(row({ trash: trash({ state: 'trashing' }) })).toContain('Moving…');
    expect(row({ trash: trash({ state: 'restoring' }) })).toContain('Restoring…');
  });
});

describe('worktree inventory calls (DL-142)', () => {
  const worktree = (extra: Partial<WorktreeInfo> = {}): WorktreeInfo => ({
    session_id: 's1',
    title: null,
    path: '/w/deck-wt/s1',
    branch: 'main-fix',
    main: '/w/deck',
    exists: true,
    registered: true,
    dirty: 0,
    ignored: 0,
    unpushed: 0,
    compared_with: 'remotes',
    last_commit: null,
    live: false,
    verdict: 'clean',
    note: null,
    ...extra,
  });

  test('the remove command quotes the values and deletes only an fd/ branch', () => {
    expect(removeCommand(worktree())).toBe("git -C '/w/deck' worktree remove '/w/deck-wt/s1'");
    expect(removeCommand(worktree({ branch: 'fd/s1' }))).toBe(
      "git -C '/w/deck' worktree remove '/w/deck-wt/s1' && git -C '/w/deck' branch -d 'fd/s1'",
    );
    expect(removeCommand(worktree({ path: "/w/it's here", main: "/w/o'k" }))).toBe(
      "git -C '/w/o'\\''k' worktree remove '/w/it'\\''s here'",
    );
  });

  test('there is no command for a missing, live or main-less worktree', () => {
    expect(removeCommand(worktree({ exists: false }))).toBeNull();
    expect(removeCommand(worktree({ live: true }))).toBeNull();
    expect(removeCommand(worktree({ main: null }))).toBeNull();
  });

  test('the loader reads the station route and checks the list', async () => {
    const paths: string[] = [];
    const load = worktreeLoader(async (path) => {
      paths.push(path);
      return path.includes('bad') ? { worktrees: 1 } : { worktrees: [worktree()] };
    });
    expect((await load('a b')).worktrees).toHaveLength(1);
    expect(paths).toEqual(['/api/stations/a%20b/worktrees']);
    await expect(load('bad')).rejects.toThrow('The deck sent an invalid worktree list.');
  });
});
