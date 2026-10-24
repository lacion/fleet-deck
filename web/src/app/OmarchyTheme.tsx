// R13 P4 task F2 (design `omarchy-p4.md` §3.7): "Use the Omarchy theme of <station>". The choice (a
// station id, or off) lives in localStorage. While a station is chosen, the board shows its palette at
// load and every 60 s. The select lists the stations that answer 200 for their palette.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { StationView } from '../gen';
import { readPalette, settlePalette, showPalette, type Palette } from '../ui/palette';

/** The call of the owner board for the palette of one station; a 404 throws an `ApiError`. */
export type GetPalette = (stationId: string) => Promise<unknown>;

export const OMARCHY_KEY = 'fd-omarchy-theme';

/** The wait between two reads of a palette. */
export const OMARCHY_REFRESH_MS = 60_000;

export const OMARCHY_LABEL = 'Use the Omarchy theme of';

type StoreLike = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

const browserStorage = (): StoreLike | null => {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
};

/** The chosen station id, or `null` (off). A throwing storage is off. */
export function readOmarchyChoice(storage: StoreLike | null = browserStorage()): string | null {
  try {
    const value = storage?.getItem(OMARCHY_KEY);
    return typeof value === 'string' && value !== '' ? value : null;
  } catch {
    return null;
  }
}

/** Stores the choice (`null`: off). A throwing storage keeps the choice in memory only. */
export function writeOmarchyChoice(storage: StoreLike | null, id: string | null): void {
  try {
    if (id === null) storage?.removeItem(OMARCHY_KEY);
    else storage?.setItem(OMARCHY_KEY, id);
  } catch {
    // The choice stays in memory.
  }
}

let current: string | null = readOmarchyChoice();
const listeners = new Set<() => void>();

export function setOmarchyChoice(id: string | null): void {
  current = id;
  writeOmarchyChoice(browserStorage(), id);
  for (const l of listeners) l();
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

/** The chosen station id (`null`: off) and its setter. */
export function useOmarchyChoice(): [string | null, (id: string | null) => void] {
  const choice = useSyncExternalStore(
    subscribe,
    () => current,
    () => current,
  );
  return [choice, setOmarchyChoice];
}

/**
 * The stations of `ids` that have a palette: a 200 lists it, a 404 (or a body that is not a palette)
 * drops it, an error keeps it only when it was in `last`.
 */
export async function probeStations(
  get: GetPalette,
  ids: readonly string[],
  last: readonly string[],
): Promise<string[]> {
  const reads = await Promise.all(ids.map((id) => readPalette(() => get(id))));
  return ids.filter((id, i) => {
    const read = reads[i];
    if (read?.kind === 'palette') return true;
    return read?.kind !== 'none' && last.includes(id);
  });
}

/**
 * Shows the palette that `load` reads, at once and every `pollMs`; one request at a time. An
 * undefined `load` shows the normal theme. A 401 calls `onEnded`. The normal theme returns when the
 * palette goes away (a 404, a new `load` that is undefined, or an unmount).
 */
export function useShownPalette(
  load: (() => Promise<unknown>) | undefined,
  onEnded?: () => void,
  pollMs: number = OMARCHY_REFRESH_MS,
  show: (palette: Palette | null) => void = showPalette,
): void {
  const latest = useRef({ onEnded, show });
  latest.current = { onEnded, show };
  const shownRef = useRef(false);
  useEffect(() => {
    if (load === undefined) {
      if (shownRef.current) latest.current.show(null);
      shownRef.current = false;
      return undefined;
    }
    let live = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const tick = async () => {
      const read = await readPalette(load);
      if (!live) return;
      shownRef.current = settlePalette(
        read,
        shownRef.current,
        (p) => latest.current.show(p),
        () => latest.current.onEnded?.(),
      );
      timer = setTimeout(() => void tick(), pollMs);
    };
    void tick();
    return () => {
      live = false;
      if (timer !== null) clearTimeout(timer);
    };
  }, [load, pollMs]);
  useEffect(
    () => () => {
      if (shownRef.current) latest.current.show(null);
      shownRef.current = false;
    },
    [],
  );
}

/** Mounted once in the board: applies the palette of the chosen station on every page. */
export function useOmarchyTheme(getPalette: GetPalette | undefined): void {
  const [choice] = useOmarchyChoice();
  const load = useMemo(
    () => (getPalette === undefined || choice === null ? undefined : () => getPalette(choice)),
    [getPalette, choice],
  );
  useShownPalette(load);
}

/** The stations that have a palette, read at mount, when the station ids change and every 60 s. */
export function useStationsWithPalette(
  stations: readonly StationView[],
  getPalette: GetPalette | undefined,
  pollMs: number = OMARCHY_REFRESH_MS,
): readonly string[] {
  const [list, setList] = useState<readonly string[]>([]);
  const lastList = useRef(list);
  lastList.current = list;
  const ids = stations.map((s) => s.station_id).join('\n');
  useEffect(() => {
    if (getPalette === undefined) return undefined;
    let live = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const tick = async () => {
      const next = await probeStations(
        getPalette,
        ids === '' ? [] : ids.split('\n'),
        lastList.current,
      );
      if (!live) return;
      setList((now) =>
        now.length === next.length && now.every((id, i) => id === next[i]) ? now : next,
      );
      timer = setTimeout(() => void tick(), pollMs);
    };
    void tick();
    return () => {
      live = false;
      if (timer !== null) clearTimeout(timer);
    };
  }, [getPalette, ids, pollMs]);
  return list;
}

type SelectProps = {
  readonly stations: readonly StationView[];
  readonly getPalette: GetPalette;
};

/**
 * The select of the Prefs row: "Off" and the stations that have a palette. A chosen station stays
 * in the list while it has no palette. Nothing shows while no station has a palette and the choice
 * is off.
 */
export function OmarchyThemeSelect({ stations, getPalette }: SelectProps) {
  const [choice, setChoice] = useOmarchyChoice();
  const withPalette = useStationsWithPalette(stations, getPalette);
  const ids =
    choice !== null && !withPalette.includes(choice) ? [...withPalette, choice] : withPalette;
  if (ids.length === 0) return null;
  const nameOf = (id: string) => stations.find((s) => s.station_id === id)?.name ?? id;
  return (
    <select
      className="fd-sm"
      aria-label={OMARCHY_LABEL}
      title={OMARCHY_LABEL}
      value={choice ?? ''}
      onChange={(e) => setChoice(e.target.value === '' ? null : e.target.value)}
    >
      <option value="">Off</option>
      {ids.map((id) => (
        <option key={id} value={id}>
          {nameOf(id)}
        </option>
      ))}
    </select>
  );
}
