// R13 P9 (design §10): the per-session load heatmap on the wall `side` area.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { StationResources, StationView } from '../gen';
import { card, station } from '../testing/fleet';
import { SideArea } from './Areas';
import { heatOf, loadMap, MAX_CELLS, sizeOf } from './loadCells';

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
  test('the size follows cpu_pct and None is the smallest', () => {
    expect(sizeOf(null)).toBe(1);
    expect(sizeOf(99)).toBe(1);
    expect(sizeOf(100)).toBe(2);
    expect(sizeOf(799)).toBe(2);
    expect(sizeOf(800)).toBe(3);
  });

  test('the heat is the share of the station cores', () => {
    expect(heatOf(null, 8)).toBe('unknown');
    expect(heatOf(399, 8)).toBe('mid');
    expect(heatOf(199, 8)).toBe('low');
    expect(heatOf(200, 8)).toBe('mid');
    expect(heatOf(400, 8)).toBe('high');
    expect(heatOf(900, null)).toBe('low');
    expect(heatOf(900, 0)).toBe('low');
  });

  test('the order is cpu_pct descending, None last, then station and session', () => {
    const cards = [card('a'), card('b'), card('c'), card('d'), card('e', 'working', 'station-b')];
    const out = loadMap(cards, [
      owner('station-a', [load('a', 50), load('b', null), load('c', 50), load('d', 700)]),
      owner('station-b', [load('e', 50)]),
    ]);
    expect(out.cells.map((c) => c.session)).toEqual(['d', 'a', 'c', 'e', 'b']);
    expect(out.more).toBe(0);
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

  test('it keeps 48 cells and counts the rest', () => {
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

  test('the name is the callsign, then the title, then 8 characters of the id', () => {
    const cards = [
      card('a', 'working', 'station-a', { callsign: 'Falcon' }),
      card('b', 'working', 'station-a', { title: 'Fix login' }),
      card('0123456789abcdef'),
    ];
    const out = loadMap(cards, [
      owner('station-a', [load('a', 30), load('b', 20), load('0123456789abcdef', 10)]),
    ]);
    expect(out.cells.map((c) => c.name)).toEqual(['Falcon', 'Fix login', '01234567']);
  });
});

describe('the load block of the side area', () => {
  const render = (
    stations: StationView[],
    sessions = [card('a', 'working', 'station-a', { callsign: 'Falcon' }), card('b')],
  ) => renderToStaticMarkup(<SideArea sessions={sessions} stations={stations} now={1} />);

  test('a cell has the tooltip, the size and the heat', () => {
    const html = render([owner('station-a', [load('a', 450, 2.5e9), load('b', null, null)])]);
    expect(html).toContain('<h2 class="fd-wall-h">Session load</h2>');
    expect(html).toContain('aria-label="Session load"');
    expect(html).toContain('title="Falcon on Alpha: CPU 450%, memory 2.5 GB"');
    expect(html).toContain('data-size="2" data-heat="high"');
    expect(html).toContain('title="b on Alpha: CPU unknown, memory unknown"');
    expect(html).toContain('data-size="1" data-heat="unknown"');
    expect(html).toContain('>450%<');
    expect(html).toContain('>—<');
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
    expect(render([owner('station-a', [])])).not.toContain('Session load');
    expect(render([station('station-a')])).not.toContain('Session load');
  });

  test('the cells are not interactive', () => {
    const html = render([owner('station-a', [load('a', 10)])]);
    const list = html.slice(html.indexOf('aria-label="Session load"'));
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
    expect(html).toContain('+2 more');
  });
});
