// The calm board of the live UI audit, round 3: strips that keep their place, one changes column
// for the page life, a diff that fits its column, the lane head that names a shared risky mode, the
// quiet reach, the platform words, and the provider chips of the shown sessions.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { FleetHome, providerKeys } from '../app/FleetHome';
import type { DeckSession, StationInfo } from '../gen';
import { setDiffTotal } from '../session/workspace';
import { card, station } from '../testing/fleet';
import { newArrivals, rememberArrivals } from './fleet';
import { FleetBoard, laneMode, stripCards } from './FleetBoard';
import { diffTexts, showChangesColumn } from './FleetStrip';

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

describe('a strip keeps its place in its status group', () => {
  test('two working strips whose updated_at values swap keep their order', () => {
    const arrivals = newArrivals();
    const first = [
      card('a', 'working', 'station-a', { updated_at: 10 }),
      card('b', 'working', 'station-a', { updated_at: 20 }),
    ];
    // The first look: newest first.
    expect(ids(stripCards(first, undefined, arrivals))).toEqual(['b', 'a']);
    rememberArrivals(first, arrivals);
    // A tool call of a makes it the newest: the strips stay where they were.
    const later = [
      card('a', 'working', 'station-a', { updated_at: 30 }),
      card('b', 'working', 'station-a', { updated_at: 20 }),
    ];
    expect(ids(stripCards(later, undefined, arrivals))).toEqual(['b', 'a']);
    // Without arrivals (a test, the other boards) the order is newest first.
    expect(ids(stripCards(later))).toEqual(['a', 'b']);
  });

  test('a strip that becomes idle moves to the idle group; a new session goes to the top', () => {
    const arrivals = newArrivals();
    const start = [
      card('a', 'working', 'station-a', { updated_at: 30 }),
      card('b', 'working', 'station-a', { updated_at: 20 }),
      card('c', 'idle', 'station-a', { updated_at: 50 }),
    ];
    rememberArrivals(start, arrivals);
    expect(ids(stripCards(start, undefined, arrivals))).toEqual(['a', 'b', 'c']);
    // a turns idle: it leaves the working group and arrives first in the idle group.
    const idle = [
      card('a', 'idle', 'station-a', { updated_at: 40 }),
      card('b', 'working', 'station-a', { updated_at: 60 }),
      card('c', 'idle', 'station-a', { updated_at: 50 }),
    ];
    expect(ids(stripCards(idle, undefined, arrivals))).toEqual(['b', 'a', 'c']);
    rememberArrivals(idle, arrivals);
    expect(ids(stripCards(idle, undefined, arrivals))).toEqual(['b', 'a', 'c']);
    // A new working session (older updated_at than b) comes first in its group.
    const spawned = [...idle, card('d', 'working', 'station-a', { updated_at: 5 })];
    expect(ids(stripCards(spawned, undefined, arrivals))).toEqual(['d', 'b', 'a', 'c']);
    rememberArrivals(spawned, arrivals);
    // a works again: it arrives at the top of the working group.
    const back = spawned.map((c) =>
      c.summary.session_id === 'a' ? card('a', 'working', 'station-a', { updated_at: 1 }) : c,
    );
    expect(ids(stripCards(back, undefined, arrivals))).toEqual(['a', 'd', 'b', 'c']);
  });

  test('starred strips stay first', () => {
    const arrivals = newArrivals();
    const cards = [
      card('a', 'working', 'station-a', { updated_at: 10 }),
      card('b', 'idle', 'station-a', { updated_at: 20 }),
    ];
    rememberArrivals(cards, arrivals);
    expect(ids(stripCards(cards, new Set(['station-a/b']), arrivals))).toEqual(['b', 'a']);
  });
});

describe('the changes column', () => {
  test('a long diff takes the short form; the title keeps the whole numbers', () => {
    expect(diffTexts(19_686, 83)).toEqual({
      added: '+19,686',
      removed: '−83',
      whole: '+19,686 −83',
    });
    expect(diffTexts(17_583, 6_386)).toEqual({
      added: '+18K',
      removed: '−6.4K',
      whole: '+17,583 −6,386',
    });
    setDiffTotal('station-a/long', { files_changed: 1286, insertions: 17_583, deletions: 6_386 });
    const html = board([card('long')]);
    expect(html).toContain(
      '<span class="fd-strip-diff" title="Changed since the session started: +17,583 −6,386"><span class="fd-add">+18K</span> <span class="fd-del">−6.4K</span></span>',
    );
    // A count has its digit groups (§5).
    expect(html).toContain('<span class="fd-strip-files">1,286 files</span>');
  });

  test('once shown, the board keeps its changes column, also with no strip that has changes', () => {
    showChangesColumn();
    expect(board([card('quiet')])).toContain(
      '<main class="fd-board" data-changes="true" aria-label="Fleet board">',
    );
  });
});

describe('a lane head names the risky mode that every live strip shares', () => {
  const bypass = { mode: 'bypassPermissions' };

  test('two or more live strips with one risky mode: the lane says it once, the strips do not', () => {
    const html = board([
      card('a', 'working', 'station-a', bypass),
      card('b', 'idle', 'station-a', bypass),
      card('e', 'ended', 'station-a', { mode: 'dontAsk' }),
    ]);
    expect(html).toMatch(
      /<span class="fd-lane-warn" data-warn="mode" title="Permission mode of every live session of Alpha"><svg[^>]*lucide-shield-alert[\s\S]*?<\/svg> Bypass permissions<\/span>/,
    );
    expect(html.match(/data-ico="risky"/g)).toHaveLength(1);
    // The ended strip with another mode keeps its flag; the others keep the words for a reader.
    expect(html).toContain('title="Permission mode: Don&#x27;t ask"');
    expect(
      html.match(/<span class="fd-vh">Permission mode: Bypass permissions<\/span>/g),
    ).toHaveLength(2);
  });

  test('a mixed lane, or a lane of one strip, keeps the flag on each strip', () => {
    expect(laneMode([card('a', 'working', 'station-a', bypass), card('b')])).toBeNull();
    expect(laneMode([card('a', 'working', 'station-a', bypass)])).toBeNull();
    expect(
      laneMode([card('a', 'working', 'station-a', bypass), card('b', 'idle', 'station-a', bypass)]),
    ).toBe('bypassPermissions');
    expect(
      laneMode([
        card('a', 'working', 'station-a', { mode: 'default' }),
        card('b', 'idle', 'station-a', { mode: 'default' }),
      ]),
    ).toBeNull();
    const html = board([card('a', 'working', 'station-a', bypass), card('b')]);
    expect(html).not.toContain('data-warn="mode"');
    expect(html.match(/data-ico="risky"/g)).toHaveLength(1);
  });
});

describe('the lane head facts', () => {
  const mac = station('station-a', {
    info: {
      max_sessions: 4,
      live_sessions: 1,
      providers: [],
      traits: { os: 'macos', arch: 'aarch64', labels: [] },
    } as unknown as StationInfo,
    info_at: 1,
  });

  test('the dot says online (the word is for screen readers); offline is a word', () => {
    const html = board([card('a')], { stations: [mac] });
    expect(html).toContain(
      '<span class="fd-lane-reach" data-up="true"><span class="fd-vh">online</span></span>',
    );
    expect(board([card('a')], { stations: [mac], freshness: 'stale' })).toContain(
      '<span class="fd-lane-reach" data-up="false">offline</span>',
    );
  });

  test('the platform says arm64 (archLabel of ui/format.ts); the raw arch is the title', () => {
    expect(board([card('a')], { stations: [mac] })).toContain(
      '<span class="fd-lane-platform" title="aarch64">macOS · arm64</span>',
    );
  });
});

describe('the provider chips', () => {
  const home = (cards: DeckSession[]) =>
    renderToStaticMarkup(
      <FleetHome
        sessions={cards}
        stations={[station()]}
        freshness="live"
        assign={undefined}
        fleet={undefined}
        conflicts={undefined}
        feed={[]}
        layout="desktop"
        onOpen={() => {}}
        onSpawn={() => {}}
      />,
    );

  test('a board of one provider has no Provider row: no chip filters to an empty board', () => {
    const html = home([card('a'), card('b')]);
    expect(html).not.toContain('<span class="fd-label">Provider</span>');
    expect(html).not.toContain('>Codex</button>');
  });

  test('two providers on the board: a chip for each, in the order Claude, Codex, Other', () => {
    const cards = [
      card('o', 'working', 'station-a', { provider: null }),
      card('x', 'working', 'station-a', { provider: 'codex' }),
      card('a'),
    ];
    expect(providerKeys(cards)).toEqual(['claude', 'codex', 'unknown']);
    const html = home(cards);
    expect(html).toContain('<span class="fd-label">Provider</span>');
    expect(html.indexOf('>Claude</button>')).toBeLessThan(html.indexOf('>Codex</button>'));
    expect(html.indexOf('>Codex</button>')).toBeLessThan(html.indexOf('>Other</button>'));
  });
});
