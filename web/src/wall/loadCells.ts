// R13 P9 (design §10): the cells of the per-session load heatmap on the wall `side` area.
// The wall only reads: an owner login gets the loads from `info.resources`, a display login
// from `display_resources` (the deck already reduced it to the visible sessions).
import { memorySize } from '../board/resources';
import type { DeckSession, StationView } from '../gen';

/** The most cells that the heatmap shows. */
export const MAX_CELLS = 48;

export type Heat = 'low' | 'mid' | 'high' | 'unknown';

export type LoadCell = {
  readonly key: string;
  readonly station: string;
  readonly session: string;
  readonly name: string;
  readonly cpu: number | null;
  readonly rss: number | null;
  /** Grid columns: 1 under 100%, 2 from 100% to 799%, 3 from 800%. `None` is 1. */
  readonly size: 1 | 2 | 3;
  readonly heat: Heat;
  /** The tooltip and the accessible label. */
  readonly label: string;
  /** The text on the second line of the cell. */
  readonly value: string;
};

export type LoadMapData = { readonly cells: readonly LoadCell[]; readonly more: number };

export function sizeOf(cpu: number | null): 1 | 2 | 3 {
  if (cpu === null || cpu < 100) return 1;
  return cpu < 800 ? 2 : 3;
}

/** The heat is the share of the station cores: 50% or more is high, 25% or more is mid. */
export function heatOf(cpu: number | null, cores: number | null): Heat {
  if (cpu === null) return 'unknown';
  if (cores === null || cores <= 0) return 'low';
  const share = cpu / (cores * 100);
  if (share >= 0.5) return 'high';
  return share >= 0.25 ? 'mid' : 'low';
}

/** The tooltip text. A value that is `None` is "unknown". */
export function cellLabel(
  name: string,
  station: string,
  cpu: number | null,
  rss: number | null,
): string {
  const c = cpu === null ? 'unknown' : `${cpu}%`;
  const m = rss === null ? 'unknown' : memorySize(rss);
  return `${name} on ${station}: CPU ${c}, memory ${m}`;
}

function byLoad(a: LoadCell, b: LoadCell): number {
  if (a.cpu !== b.cpu) {
    if (a.cpu === null) return 1;
    if (b.cpu === null) return -1;
    return b.cpu - a.cpu;
  }
  if (a.station !== b.station) return a.station < b.station ? -1 : 1;
  if (a.session === b.session) return 0;
  return a.session < b.session ? -1 : 1;
}

const isLive = (c: DeckSession) =>
  c.status !== 'ended' && c.status !== 'lost' && c.status !== 'unreachable';

/** One cell for each load that has a live card in `sessions`, busiest first, at most `MAX_CELLS`. */
export function loadMap(
  sessions: readonly DeckSession[],
  stations: readonly StationView[],
): LoadMapData {
  const cards = new Map<string, DeckSession>();
  for (const c of sessions) {
    if (isLive(c)) cards.set(`${c.station_id}\n${c.summary.session_id}`, c);
  }
  const all: LoadCell[] = [];
  for (const st of stations) {
    const r = st.info?.resources ?? st.display_resources;
    if (r == null) continue;
    const cores = r.cpu?.cores ?? null;
    for (const load of r.sessions) {
      const card = cards.get(`${st.station_id}\n${load.session_id}`);
      if (card === undefined) continue;
      const name = card.summary.callsign ?? card.summary.title ?? load.session_id.slice(0, 8);
      all.push({
        key: `${st.station_id}\n${load.session_id}`,
        station: st.station_id,
        session: load.session_id,
        name,
        cpu: load.cpu_pct,
        rss: load.rss_bytes,
        size: sizeOf(load.cpu_pct),
        heat: heatOf(load.cpu_pct, cores),
        label: cellLabel(name, st.name, load.cpu_pct, load.rss_bytes),
        value: load.cpu_pct === null ? '—' : `${load.cpu_pct}%`,
      });
    }
  }
  all.sort(byLoad);
  return { cells: all.slice(0, MAX_CELLS), more: Math.max(0, all.length - MAX_CELLS) };
}
