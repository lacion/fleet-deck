// R13 P9 (design §10): the cells of the per-session load heatmap on the wall `side` area.
// The wall only reads: an owner login gets the loads from `info.resources`, a display login
// from `display_resources` (the deck already reduced it to the visible sessions). A session that
// is idle (under 1% CPU) or whose CPU is not known gets no cell: the heading counts it.
import { memorySize } from '../board/resources';
import type { DeckSession, StationView } from '../gen';

/** The most cells that the heatmap shows. */
export const MAX_CELLS = 48;

/** The least CPU of a cell, in percent: an idle session is no signal on a wall. */
export const MIN_CPU = 1;

export type Heat = 'low' | 'mid' | 'high';

export type LoadCell = {
  readonly key: string;
  readonly station: string;
  readonly session: string;
  /** The title of the session, as on its tile; else its callsign; else 8 characters of its id. */
  readonly name: string;
  /** The name is the callsign (`.fd-callsign`), not a title. */
  readonly callsign: boolean;
  readonly cpu: number;
  readonly rss: number | null;
  /** Grid columns: 1 under 100%, 2 from 100% to 799%, 3 from 800%. */
  readonly size: 1 | 2 | 3;
  readonly heat: Heat;
  /** The tooltip and the accessible label. */
  readonly label: string;
  /** The text on the second line of the cell. */
  readonly value: string;
};

/** The cells, and `more`: the live sessions with a load that have no cell (idle, unknown or over
 * `MAX_CELLS`). */
export type LoadMapData = { readonly cells: readonly LoadCell[]; readonly more: number };

export function sizeOf(cpu: number): 1 | 2 | 3 {
  if (cpu < 100) return 1;
  return cpu < 800 ? 2 : 3;
}

/** The heat is the share of the station cores: 50% or more is high, 25% or more is mid. */
export function heatOf(cpu: number, cores: number | null): Heat {
  if (cores === null || cores <= 0) return 'low';
  const share = cpu / (cores * 100);
  if (share >= 0.5) return 'high';
  return share >= 0.25 ? 'mid' : 'low';
}

/** The name of the block: the cells are the CPU of each session. */
export const LOAD_TITLE = 'CPU by session';

/** The tooltip text. A value that the station does not report is left out (principle 7). */
export function cellLabel(
  name: string,
  station: string,
  cpu: number | null,
  rss: number | null,
): string {
  const parts = [
    ...(cpu === null ? [] : [`CPU ${cpu}%`]),
    ...(rss === null ? [] : [`memory ${memorySize(rss)}`]),
  ];
  return parts.length === 0
    ? `${name} on ${station}`
    : `${name} on ${station}: ${parts.join(', ')}`;
}

function byLoad(a: LoadCell, b: LoadCell): number {
  if (a.cpu !== b.cpu) return b.cpu - a.cpu;
  if (a.station !== b.station) return a.station < b.station ? -1 : 1;
  if (a.session === b.session) return 0;
  return a.session < b.session ? -1 : 1;
}

const isLive = (c: DeckSession) =>
  c.status !== 'ended' && c.status !== 'lost' && c.status !== 'unreachable';

/**
 * One cell for each load of `MIN_CPU` or more that has a live card in `sessions`, busiest first,
 * at most `MAX_CELLS`. `more` counts the other loads of live cards.
 */
export function loadMap(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
): LoadMapData {
  const cards = new Map<string, DeckSession>();
  for (const c of sessions) {
    if (isLive(c)) cards.set(`${c.station_id}\n${c.summary.session_id}`, c);
  }
  const all: LoadCell[] = [];
  let quiet = 0;
  for (const st of stations) {
    const r = st.info?.resources ?? st.display_resources;
    if (r == null) continue;
    const cores = r.cpu?.cores ?? null;
    for (const load of r.sessions) {
      const card = cards.get(`${st.station_id}\n${load.session_id}`);
      if (card === undefined) continue;
      const cpu = load.cpu_pct;
      if (cpu === null || cpu < MIN_CPU) {
        quiet += 1;
        continue;
      }
      // The tiles of the same wall name a session by its title: a cell uses the same name.
      const { title, callsign } = card.summary;
      const name = title ?? callsign ?? load.session_id.slice(0, 8);
      all.push({
        key: `${st.station_id}\n${load.session_id}`,
        station: st.station_id,
        session: load.session_id,
        name,
        callsign: title == null && callsign != null,
        cpu,
        rss: load.rss_bytes,
        size: sizeOf(cpu),
        heat: heatOf(cpu, cores),
        label: cellLabel(name, st.name, cpu, load.rss_bytes),
        value: `${cpu}%`,
      });
    }
  }
  all.sort(byLoad);
  return { cells: all.slice(0, MAX_CELLS), more: quiet + Math.max(0, all.length - MAX_CELLS) };
}
