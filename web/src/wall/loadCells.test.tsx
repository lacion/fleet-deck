// R13 P9 (design §10): the per-session CPU heatmap on the wall `side` area.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationResources, StationView } from '../gen';
import { card, station } from '../testing/fleet';
import { SideArea } from './Areas';
import { cellLabel, heatOf, loadMap, MAX_CELLS, sizeOf } from './loadCells';
import { LoadCount } from './LoadMap';

const load = (id: string, cpu: number | null, rss: number | null = 340e6) => ({
  session_id: id,
  cpu_pct: cpu,
  rss_bytes: rss,
  procs: 2,
});

const owner = (id: string, sessions: StationResources['sessions'], cores = 8): StationView =>
  station(id, {
    info: {
      name: id,
      incarnation: 'i',
      head: 1,
      live_sessions: sessions.length,
      resources: {
        at: 1,
        disks: [],
        memory: null,
        cpu: { cores, load1_x100: 0, load5_x100: 0 },
        sessions,
      },
    } as unknown as StationView['info'],
  });

const display = (
  id: string,
  sessions: { session_id: string; cpu_pct: number | null; rss_bytes: number | null }[],
) =>
  station(id, {
    display_resources: {
      at: 1,
      disks: [],
      memory: null,
      cpu: { cores: 8, load1_x100: 0, load5_x100: 0 },
      sessions,
    },
  });

describe('the cells of the load heatmap', () => {
  test('the size follows cpu_pct', () => {
    expect(sizeOf(1)).toBe(1);
    expect(sizeOf(99)).toBe(1);
    expect(sizeOf(100)).toBe(2);
    expect(sizeOf(799)).toBe(2);
    expect(sizeOf(800)).toBe(3);
  });

  test('the heat is the share of the station cores', () => {
    expect(heatOf(399, 8)).toBe('mid');
    expect(heatOf(199, 8)).toBe('low');
    expect(heatOf(200, 8)).toBe('mid');
    expect(heatOf(400, 8)).toBe('high');
    expect(heatOf(900, null)).toBe('low');
    expect(heatOf(900, 0)).toBe('low');
  });

  test('the order is cpu_pct descending, then station and session', () => {
    const cards = [card('a'), card('c'), card('d'), card('e', 'working', 'station-b')];
    const out = loadMap(cards, [
      owner('station-a', [load('a', 50), load('c', 50), load('d', 700)]),
      owner('station-b', [load('e', 50)]),
    ]);
    expect(out.cells.map((c) => c.session)).toEqual(['d', 'a', 'c', 'e']);
    expect(out.more).toBe(0);
  });

  test('an idle session (under 1% CPU) and an unknown CPU get no cell: they are counted', () => {
    const cards = [card('a'), card('idle'), card('zero'), card('dark')];
    const out = loadMap(cards, [
      owner('station-a', [load('a', 1), load('idle', 0.5), load('zero', 0), load('dark', null)]),
    ]);
    expect(out.cells.map((c) => c.session)).toEqual(['a']);
    expect(out.more).toBe(3);
  });

  test('a label leaves out a value that the station does not report', () => {
    expect(cellLabel('Fix login', 'Alpha', 12, 340e6)).toBe(
      'Fix login on Alpha: CPU 12%, memory 340 MB',
    );
    expect(cellLabel('Fix login', 'Alpha', 12, null)).toBe('Fix login on Alpha: CPU 12%');
    expect(cellLabel('Fix login', 'Alpha', null, null)).toBe('Fix login on Alpha');
  });

  test('a load with no live card gets no cell', () => {
    const cards = [card('a'), card('gone', 'ended')];
    const out = loadMap(cards, [
      owner('station-a', [load('a', 10), load('gone', 90), load('x', 5)]),
    ]);
    expect(out.cells.map((c) => c.session)).toEqual(['a']);
  });

  test('a display reads display_resources and the owner reads info', () => {
    const cards = [card('a'), card('b')];
    expect(loadMap(cards, [display('station-a', [load('a', 10)])]).cells).toHaveLength(1);
    expect(loadMap(cards, [owner('station-a', [load('b', 10)])]).cells).toHaveLength(1);
    expect(loadMap(cards, [station('station-a')]).cells).toHaveLength(0);
  });

  test('it keeps 48 cells and counts the rest (the idle s00 too)', () => {
    const ids = Array.from({ length: 50 }, (_, i) => `s${String(i).padStart(2, '0')}`);
    const out = loadMap(
      ids.map((id) => card(id)),
      [
        owner(
          'station-a',
          ids.map((id, i) => load(id, i)),
        ),
      ],
    );
    expect(out.cells).toHaveLength(MAX_CELLS);
    expect(out.more).toBe(2);
    expect(out.cells[0]?.session).toBe('s49');
  });

  test('the name is the title (as on the tile), then the callsign, then 8 characters of the id', () => {
    const cards = [
      card('a', 'working', 'station-a', { callsign: 'Falcon', title: 'Fix login' }),
      card('b', 'working', 'station-a', { callsign: 'Otter' }),
      card('0123456789abcdef'),
    ];
    const out = loadMap(cards, [
      owner('station-a', [load('a', 30), load('b', 20), load('0123456789abcdef', 10)]),
    ]);
    expect(out.cells.map((c) => c.name)).toEqual(['Fix login', 'Otter', '01234567']);
    expect(out.cells.map((c) => c.callsign)).toEqual([false, true, false]);
  });
});

describe('the load block of the side area', () => {
  const render = (
    stations: StationView[],
    sessions = [card('a', 'working', 'station-a', { callsign: 'Falcon' }), card('b')],
  ) => renderToStaticMarkup(<SideArea sessions={sessions} stations={stations} now={1} />);

  test('a cell has the tooltip, the size and the heat; an unknown load has no cell', () => {
    const html = render([owner('station-a', [load('a', 450, 2.5e9), load('b', null, null)])]);
    expect(html).toContain(
      '<h2 class="fd-wall-h">CPU by session<span class="fd-count">+1<span class="fd-vh"> more sessions</span></span></h2>',
    );
    expect(html).toContain('aria-label="CPU by session"');
    expect(html).toContain('title="Falcon on Alpha: CPU 450%, memory 2.5 GB"');
    expect(html).toContain('data-size="2" data-heat="high"');
    expect(html).toContain('>450%<');
    expect(html).not.toContain('b on Alpha');
    expect(html).not.toContain('unknown');
    expect(html).not.toContain('>—<');
  });

  test('a titled session shows its title, without the callsign type', () => {
    const html = render(
      [owner('station-a', [load('a', 10)])],
      [card('a', 'working', 'station-a', { callsign: 'Falcon', title: 'Fleet Deck UI' })],
    );
    expect(html).toContain('<span class="fd-wall-load-name">Fleet Deck UI</span>');
    expect(html).not.toContain('fd-callsign');
  });

  test('the heading counts the sessions without a cell in a +N pill, never "(+N more)"', () => {
    expect(renderToStaticMarkup(<LoadCount n={0} />)).toBe('');
    expect(renderToStaticMarkup(<LoadCount n={7} />)).toBe(
      '<span class="fd-count">+7<span class="fd-vh"> more sessions</span></span>',
    );
    expect(renderToStaticMarkup(<LoadCount n={1286} />)).toContain('>+1,286<');
  });

  test('the load block is the last block and has no separate count line', () => {
    const html = render([owner('station-a', [load('a', 10)])]);
    expect(html.lastIndexOf('data-part="load"')).toBeGreaterThan(
      html.lastIndexOf('data-part="stations"'),
    );
    expect(html.lastIndexOf('data-part=')).toBe(html.lastIndexOf('data-part="load"'));
    expect(html).toContain('<h2 class="fd-wall-h">CPU by session</h2>');
    expect(html).not.toContain('more');
  });

  test('the memory under 1 GB is whole MB', () => {
    const html = render([owner('station-a', [load('a', 10, 340e6)])]);
    expect(html).toContain('memory 340 MB');
  });

  test('a display login gets the cells from the reduced resources', () => {
    const html = render([display('station-a', [load('a', 120)])]);
    expect(html).toContain('title="Falcon on Alpha: CPU 120%, memory 340 MB"');
  });

  test('the block is left out without cells', () => {
    expect(render([owner('station-a', [])])).not.toContain('CPU by session');
    expect(render([station('station-a')])).not.toContain('CPU by session');
  });

  test('the cells are not interactive', () => {
    const html = render([owner('station-a', [load('a', 10)])]);
    const list = html.slice(html.indexOf('aria-label="CPU by session"'));
    expect(list).not.toContain('<a ');
    expect(list).not.toContain('<button');
    expect(list).not.toContain('tabindex');
  });

  test('a note counts the cells over the limit', () => {
    const ids = Array.from({ length: 50 }, (_, i) => `s${String(i).padStart(2, '0')}`);
    const html = render(
      [
        owner(
          'station-a',
          ids.map((id, i) => load(id, i)),
        ),
      ],
      ids.map((id) => card(id)),
    );
    expect(html).toContain(
      '<h2 class="fd-wall-h">CPU by session<span class="fd-count">+2<span class="fd-vh"> more sessions</span></span></h2>',
    );
    expect(html).not.toContain('</p>');
  });
});
