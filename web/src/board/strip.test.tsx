import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  activeFilters,
  clearFilter,
  clearText,
  FleetHome,
  scopeFleet,
  statusMatches,
} from '../app/FleetHome';
import type { FleetActions } from '../fleet/fleet';
import type { DeckSession, OpenRequest } from '../gen';
import { sessionActions } from '../session/commands';
import { claudeCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import { priority } from '../ui/localSet';
import { emptyFilter } from './fleet';
import { rememberConflicts } from './ConflictRadar';
import type { ConflictLoader } from './conflicts';
import { FleetBoard, STRIP_ORDER, stripCards } from './FleetBoard';
import { HomePanel } from './HomePanel';
import { UsageGlance } from './UsageGlance';

const NOW = 1_760_000_040_000;

const board = (cards: DeckSession[], extra = {}) =>
  renderToStaticMarkup(
    <FleetBoard
      sessions={cards}
      stations={[station(), station('station-b')]}
      freshness="live"
      now={NOW}
      {...extra}
    />,
  );

const ids = (cards: readonly DeckSession[]) => cards.map((c) => c.summary.session_id);

const ask: OpenRequest = {
  request_id: 'r1',
  node_id: null,
  request_kind: 'tool',
  title: 'Run it?',
  details: '',
  input: null,
  options: [],
  response: 'rpc_reply',
  requested_at: 1,
};

const home = (cards: DeckSession[], layout: 'desktop' | 'phone' = 'desktop') =>
  renderToStaticMarkup(
    <FleetHome
      sessions={cards}
      stations={[station()]}
      freshness="live"
      assign={undefined}
      fleet={undefined}
      conflicts={undefined}
      feed={[]}
      layout={layout}
      onOpen={() => {}}
      onSpawn={() => {}}
    />,
  );

describe('strip order', () => {
  test('the status rank is needs you, working, verifying, queued, idle, unreachable, lost, ended', () => {
    expect(STRIP_ORDER).toEqual([
      'needs_you',
      'working',
      'verifying',
      'queued',
      'idle',
      'unreachable',
      'lost',
      'ended',
    ]);
  });

  test('a lane sorts by status rank, then newest first', () => {
    const cards = [...STRIP_ORDER]
      .reverse()
      .map((status, i) => card(status, status, 'station-a', { updated_at: i }));
    cards.push(card('w-new', 'working', 'station-a', { updated_at: 99 }));
    expect(ids(stripCards(cards))).toEqual([
      'needs_you',
      'w-new',
      'working',
      'verifying',
      'queued',
      'idle',
      'unreachable',
      'lost',
      'ended',
    ]);
  });

  test('starred strips come first, and keep the rank and the age among themselves', () => {
    const cards = [
      card('idle-star', 'idle', 'station-a', { updated_at: 1 }),
      card('work', 'working', 'station-a', { updated_at: 5 }),
      card('ended-star', 'ended', 'station-a', { updated_at: 9 }),
      card('need', 'needs_you', 'station-a', { updated_at: 2 }),
    ];
    const starred = new Set(['station-a/idle-star', 'station-a/ended-star']);
    expect(ids(stripCards(cards, starred))).toEqual(['idle-star', 'ended-star', 'need', 'work']);
    priority.add(['station-a/idle-star']);
    const html = board(cards);
    priority.remove(['station-a/idle-star']);
    expect(html.indexOf('data-session="idle-star"')).toBeLessThan(
      html.indexOf('data-session="need"'),
    );
  });

  test('a lane with only ended or lost sessions goes after the lanes with a live one', () => {
    const html = board([
      card('a', 'ended', 'station-a'),
      card('b', 'lost', 'station-a'),
      card('c', 'idle', 'station-b'),
    ]);
    expect(html.indexOf('aria-label="Station Beta"')).toBeLessThan(
      html.indexOf('aria-label="Station Alpha"'),
    );
  });
});

describe('strip markup', () => {
  test('the state edge is the data-status of the strip', () => {
    for (const status of STRIP_ORDER) {
      const html = board([card('x', status)]);
      expect(html).toMatch(new RegExp(`<a class="fd-strip"[^>]*data-status="${status}"`));
    }
  });

  test('a strip has the callsign, the title, the model and the age cells', () => {
    const html = board([
      card('abcdefghij', 'working', 'station-a', {
        callsign: 'falcon-899q',
        title: 'Fix the test',
        model: 'claude-opus-4-1',
        updated_at: NOW - 4 * 60_000,
      }),
      card('nocall'),
    ]);
    expect(html).toMatch(/class="fd-strip-call fd-callsign"[^>]*>falcon-899q</);
    expect(html).toContain('>Fix the test<');
    expect(html).toContain('class="fd-strip-model" data-family="opus">Opus 4.1<');
    expect(html).toContain('>4 min<');
    // no callsign: the first 8 characters of the id
    expect(html).toMatch(/class="fd-strip-call fd-callsign"[^>]*>nocall</);
    // the status glyph leads the title (state not by color alone, DESIGN-SYSTEM §6)
    expect(html).toContain(
      '<span class="fd-strip-title"><span class="fd-dot" data-status="working" aria-hidden="true"></span>',
    );
  });

  test('the meta cell shows an icon only for a fact that is present', () => {
    const bare = board([card('a')]);
    expect(bare).not.toContain('fd-strip-ico');
    expect(bare).not.toContain('fd-strip-diff');
    const full = board(
      [
        card('b', 'working', 'station-a', {
          mode: 'bypassPermissions',
          artifacts_count: 3,
          background_tasks: ['t1', 't2'],
          terminal: { token: 't', cols: 80, rows: 24 } as never,
        }),
      ],
      {
        extras: new Map([
          ['station-a/b', { station_id: 'station-a', session_id: 'b', mail_pending: 2, files: [] }],
        ]),
      },
    );
    for (const kind of ['risky', 'mail', 'artifacts', 'background', 'terminal'])
      expect(full).toContain(`data-ico="${kind}"`);
    expect(full).not.toContain('data-ico="stale"');
    expect(full).not.toContain('data-ico="archived"');
    expect(full).not.toContain('data-ico="contested"');
    expect(full).toContain(
      '<span role="img" aria-label="In terminal" class="fd-strip-ico" data-ico="terminal" title="In terminal">',
    );
    // each fact is named for the strip link, with its count
    expect(full).toContain('<span role="img" aria-label="Artifacts: 3" class="fd-strip-ico"');
    expect(full).toContain(
      '<span role="img" aria-label="Background tasks: 2" class="fd-strip-ico"',
    );
    expect(full).toContain('data-artifacts="3"');
  });

  test('the now cell shows the focus line before the status, and an ask before the focus', () => {
    const focus = { focus: 'writing the changelog' };
    const withFocus = board([card('f', 'working', 'station-a', focus)]);
    expect(withFocus).toContain('class="fd-focus" title="writing the changelog"');
    expect(withFocus).not.toContain('data-note="working"');
    const asking = board([card('g', 'working', 'station-a', { ...focus, open_requests: [ask] })]);
    expect(asking).toContain('Run it?');
    expect(asking).not.toContain('fd-focus');
    // An ended session shows its state, not the last focus of the agent.
    const ended = board([card('h', 'ended', 'station-a', focus)]);
    expect(ended).toContain('data-note="ended"');
    expect(ended).not.toContain('fd-focus');
  });

  test('a repo grouping puts the station under the branch in the where cell, not in the title', () => {
    const cards = [card('a', 'working', 'station-a', { title: 'T', branch: 'fix/x' })];
    expect(board(cards)).not.toContain('fd-strip-station');
    expect(board(cards)).not.toContain('data-showstation');
    const repo = board(cards, { group: 'repo' });
    expect(repo).toContain('<span class="fd-strip-text">T</span>');
    expect(repo).not.toMatch(/fd-strip-title[\s\S]*fd-strip-station[\s\S]*fd-strip-text/);
    expect(repo).toMatch(
      /<span class="fd-strip-where" title="Branch: fix\/x" data-station="true"><span class="fd-strip-wline"><svg[^>]*lucide-git-branch[\s\S]*?<\/svg><span class="fd-strip-path">fix\/x<\/span><\/span><span class="fd-strip-station">Alpha<\/span><\/span>/,
    );
    // The chips line starts after an invisible copy of the name (the visible one is in the link).
    expect(repo).toContain('<span class="fd-strip-spacer" aria-hidden="true">Alpha</span>');
    expect(repo).toContain('data-showstation="true"');
  });
});

describe('toolbar filters', () => {
  test('only station, provider, repo and archived count as active filters', () => {
    const stations = [station(), station('station-b')];
    expect(activeFilters(emptyFilter, false, stations)).toEqual([]);
    expect(activeFilters({ ...emptyFilter, search: 'x' }, false, stations)).toEqual([]);
    const all = activeFilters(
      { station: 'station-b', provider: 'codex', repo: 'org/x', search: '' },
      true,
      stations,
    );
    expect(all.map((f) => f.text)).toEqual([
      'Station: Beta',
      'Provider: Codex',
      'Repo: org/x',
      'Archived shown',
    ]);
  });

  test('clearing one active filter keeps the others and the search', () => {
    const filter = { station: 'station-b', provider: 'codex', repo: 'org/x', search: 'find' };
    expect(clearFilter(filter, 'station')).toEqual({ ...filter, station: '' });
    expect(clearFilter(filter, 'provider')).toEqual({ ...filter, provider: '' });
    expect(clearFilter(filter, 'repo')).toEqual({ ...filter, repo: '' });
  });

  test('the toolbar has a Filters menu with the filter group inside, and the group select', () => {
    const html = home([card('a')]);
    expect(html).toContain('<details class="fd-menu fd-filters-menu">');
    expect(html).toMatch(
      /<summary class="fd-ghost fd-sm"><svg[^>]*lucide-list-filter[\s\S]*?<\/svg>Filters<\/summary>/,
    );
    expect(html).toContain('<option value="station" selected="">Group: Station</option>');
    expect(html).toContain('role="group" aria-label="Fleet filters"');
    expect(html).toContain('aria-label="Group by"');
    expect(html).not.toContain('fd-active-filters');
    expect(html).not.toContain('fd-filters-count');
  });

  test('the header shows facts as separate spans, and All clear when nothing waits', () => {
    const html = home([card('a')]);
    expect(html).toContain('<span>1 session</span>');
    expect(html).toContain('<span class="fd-home-needs" data-clear="true">All clear</span>');
    expect(html).not.toContain(' · ');
    const needs = home([card('n', 'needs_you', 'station-a', { open_requests: [ask] })]);
    expect(needs).toContain('<span class="fd-home-needs">1 needs you</span>');
    // A request of an unreachable station still waits: no All clear.
    const away = home([card('u', 'unreachable', 'station-a', { open_requests: [ask] })]);
    expect(away).toContain('<span class="fd-home-needs">1 needs you</span>');
  });
});

describe('rail and panel', () => {
  test('on a wide desktop the side rail is absent with no request, and present with one', () => {
    const none = home([card('a')]);
    expect(none).not.toContain('Needs you rail');
    expect(none).toContain('data-rail="false"');
    const some = home([card('n', 'needs_you', 'station-a', { open_requests: [ask] })]);
    expect(some).toContain('aria-label="Needs you rail"');
    expect(some).toContain('data-rail="true"');
  });

  test('the activity panel is closed when nothing is stored', () => {
    const html = home([card('a')]);
    expect(html).toContain('aria-label="Fleet panel"');
    expect(html).toContain('data-open="false"');
    expect(html).toMatch(/aria-expanded="false"[^>]*aria-label="Expand panel"/);
  });
});

describe('phone board', () => {
  test('the phone renders the lanes of strips, without the group select and the old columns', () => {
    const html = home([card('a', 'working'), card('b', 'ended')], 'phone');
    expect(html).toContain('data-layout="phone"');
    expect(html).toContain('class="fd-lane"');
    expect(html).toContain('class="fd-strip"');
    expect(html).not.toContain('fd-phone-col');
    expect(html).not.toContain('aria-label="Group by"');
    expect(html).not.toContain('fd-strip-station');
    // The phone folds the ended strips of a lane as the desktop does (§4.2).
    expect(html).toContain(
      '<details class="fd-lane-ended"><summary>Ended <span class="fd-count">1</span></summary>',
    );
    expect(html).not.toContain('Offline sessions');
  });

  test('the phone shows the conflict radar under its header, above the lanes', () => {
    const load: ConflictLoader = async () => [];
    rememberConflicts(load, [
      {
        repo: null,
        file: 'src/lib.rs',
        touches: [
          {
            station_id: 'station-a',
            session_id: 'a',
            session_title: 'A',
            path: 'src/lib.rs',
            touched_at: 1,
          },
          {
            station_id: 'station-a',
            session_id: 'b',
            session_title: 'B',
            path: 'src/lib.rs',
            touched_at: 2,
          },
        ],
      },
    ]);
    const html = renderToStaticMarkup(
      <FleetHome
        sessions={[card('a'), card('b')]}
        stations={[station()]}
        freshness="live"
        assign={undefined}
        fleet={undefined}
        conflicts={load}
        feed={[]}
        layout="phone"
        onOpen={() => {}}
        onSpawn={() => {}}
      />,
    );
    expect(html).toContain('<span class="fd-home-conf">1 conflict</span>');
    const radar = html.indexOf('aria-label="Conflict radar"');
    expect(radar).toBeGreaterThan(html.indexOf('</header>'));
    expect(radar).toBeLessThan(html.indexOf('aria-label="Fleet board"'));
    expect(html.match(/aria-label="Conflict radar"/g)).toHaveLength(1);
  });
});

describe('flight deck fixes', () => {
  test('a strip without known changes has no changes cell: its where cell spans the column', () => {
    const html = board([card('a')]);
    expect(html).toContain('data-nochanges="true"');
    expect(html).not.toContain('fd-strip-changes');
  });

  test('every lane head shows how many strips it holds, a station lane too', () => {
    const html = board([card('a'), card('b'), card('c', 'working', 'station-b')]);
    expect(html).toContain(
      '<h2 class="fd-lane-name fd-wide">Alpha</h2><span class="fd-count">2</span>',
    );
    expect(html).toContain(
      '<h2 class="fd-lane-name fd-wide">Beta</h2><span class="fd-count">1</span>',
    );
  });

  test('a folded board puts the ended strips of a lane in a collapsed Ended group with its tools', () => {
    const html = board([card('a'), card('e', 'ended'), card('l', 'lost')], { foldEnded: true });
    expect(html).toContain(
      '<details class="fd-lane-ended"><summary>Ended <span class="fd-count">2</span></summary>',
    );
    expect(html).toMatch(
      /<details class="fd-lane-ended">[\s\S]*data-session="e"[\s\S]*Resume all \(0\)[\s\S]*Clear \(2\)[\s\S]*<\/details>/,
    );
    expect(html).not.toContain('Offline sessions');
    expect(board([card('a')], { foldEnded: true })).not.toContain('fd-lane-ended');
    // not folded (the phone): the ended strip stays in its lane and the offline row closes the board
    const flat = board([card('a'), card('e', 'ended')]);
    expect(flat).not.toContain('fd-lane-ended');
    expect(flat).toContain('Offline sessions');
  });

  test('a folded lane counts its shown strips; the Ended row carries the ended count', () => {
    const cards = [card('a'), card('e', 'ended'), card('b', 'working', 'station-b')];
    const html = board(cards, { foldEnded: true });
    expect(html).toContain(
      '<h2 class="fd-lane-name fd-wide">Alpha</h2><span class="fd-count" title="2 sessions on Alpha">1</span>',
    );
    expect(html).toContain('<summary>Ended <span class="fd-count">1</span></summary>');
    expect(html).toContain(
      '<h2 class="fd-lane-name fd-wide">Beta</h2><span class="fd-count">1</span>',
    );
  });

  test('a folded board has one Resume all for the ended sessions of every lane', () => {
    const resumable = (id: string, stationId: string) =>
      card(id, 'ended', stationId, { state: 'exited', capabilities: claudeCaps() });
    const actions = (key: { station_id: string; session_id: string }) =>
      sessionActions(async () => ({}), key, ['read', 'drive']);
    const html = board([resumable('e1', 'station-a'), resumable('e2', 'station-b')], {
      foldEnded: true,
      sessionActions: actions,
    });
    expect(html.match(/<details class="fd-lane-ended">/g)).toHaveLength(2);
    const outside = html.replace(/<details class="fd-lane-ended">[\s\S]*?<\/details>/g, '');
    expect(outside).toContain('<span class="fd-label">Offline sessions</span>');
    expect(outside).toContain('Resume all (2)');
    expect(outside).toContain('Clear (2)');
    // One lane to resume: its Ended row has the tools, so no second Resume all.
    const one = board([resumable('e1', 'station-a'), card('b', 'working', 'station-b')], {
      foldEnded: true,
      sessionActions: actions,
    });
    expect(one).not.toContain('Offline sessions');
    expect(one.match(/Resume all \(1\)/g)).toHaveLength(1);
    // An unreachable station cannot resume a session: no dead row.
    const away = board([card('u', 'unreachable'), card('v', 'unreachable', 'station-b')], {
      foldEnded: true,
      sessionActions: actions,
    });
    expect(away).not.toContain('Offline sessions');
    expect(away).not.toContain('Resume all');
  });

  test('the status filter: Working holds queued and verifying, Needs you an open request', () => {
    expect(statusMatches(card('q', 'queued'), 'working')).toBe(true);
    expect(statusMatches(card('v', 'verifying'), 'working')).toBe(true);
    expect(statusMatches(card('i', 'idle'), 'working')).toBe(false);
    expect(statusMatches(card('i', 'idle'), 'idle')).toBe(true);
    expect(statusMatches(card('e', 'ended'), 'idle')).toBe(false);
    expect(
      statusMatches(card('w', 'working', 'station-a', { open_requests: [ask] }), 'needs_you'),
    ).toBe(true);
    expect(statusMatches(card('e', 'ended'), 'all')).toBe(true);
  });

  test('the status filter hides strips, not the usage: Needs you keeps the meter of a working session', () => {
    const working = card('w', 'working', 'station-a', {
      rate_limits: [{ scope: 'five_hour', used_percent: 42, resets_at: null }],
    });
    const waiting = card('n', 'needs_you', 'station-a', { open_requests: [ask] });
    const { scoped, visible } = scopeFleet(
      [working, waiting],
      [station()],
      emptyFilter,
      'needs_you',
    );
    expect(ids(visible)).toEqual(['n']);
    expect(ids(scoped)).toEqual(['w', 'n']);
    const glance = renderToStaticMarkup(
      <UsageGlance sessions={scoped} stations={[station()]} now={NOW} />,
    );
    expect(glance).toContain('<span class="fd-glance-pct">42%</span>');
  });

  test('the header says Nothing needs you, not All clear, next to a conflict', () => {
    expect(clearText(0)).toBe('All clear');
    expect(clearText(1)).toBe('Nothing needs you');
    expect(clearText(3)).toBe('Nothing needs you');
  });

  test('the desktop header has the status segments and, with nothing waiting, the bell slot', () => {
    const html = home([card('a')]);
    expect(html).toContain('<div class="fd-seg fd-home-status" role="group" aria-label="Status">');
    expect(html).toContain('<button type="button" aria-pressed="true">All</button>');
    expect(html).toContain('<span class="fd-home-bell"></span>');
    const phone = home([card('a')], 'phone');
    expect(phone).not.toContain('fd-home-status');
    // The phone header has the slot too: with nothing waiting, the switch goes there.
    expect(phone).toContain('<span class="fd-home-bell"></span></header>');
  });

  test('wide, the ops panel has a Mail & tasks section and Activity, Assign and All usage foot buttons', () => {
    const actions: FleetActions = {
      mail: async () => ({ ok: true, text: '' }),
      task: async () => ({ ok: true, text: '' }),
      tasks: async () => [],
      grants: async () => [],
      revoke: async () => ({ ok: true, text: '' }),
      renew: async () => ({ ok: true, text: '' }),
      caps: async () => ({ ok: true, text: '' }),
      desktopOps: async () => [],
      give: async () => ({ ok: true, text: '' }),
      changeAccess: async () => ({ ok: true, text: '' }),
    };
    const html = renderToStaticMarkup(
      <HomePanel
        hasAssign
        guest={false}
        stations={[station()]}
        sessions={[card('a')]}
        visible={[card('a')]}
        feed={[]}
        fleet={actions}
        assign={undefined}
        onOpen={() => {}}
        onSpawn={() => {}}
        request={undefined}
        wide
        waiting={[{ key: 'station-a/a', name: 'heron-mc2q', count: 1 }]}
      />,
    );
    expect(html).toContain('data-layout="section" aria-label="Mail and tasks"');
    expect(html).toContain('<span class="fd-count">1</span>');
    expect(html).toContain('1 mail waiting');
    expect(html).toContain('For delivery to heron-mc2q');
    expect(html).toContain('<details class="fd-fleet-compose"><summary>New mail or task</summary>');
    expect(html).not.toContain('role="tablist"');
    expect(html).toMatch(
      /<div class="fd-ops-foot"><button type="button" class="fd-ghost" aria-expanded="false">/,
    );
    expect(html).toContain('</svg> Activity</button>');
    expect(html).toContain('</svg> Assign</button>');
    // The glance shows only the tightest meters: the foot keeps the full Usage panel.
    // The Usage section above names the glance: the foot button says All usage.
    expect(html).toContain('</svg> All usage</button>');
  });
});
