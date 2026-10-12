// M3 6-7 (DL-306): the conflict radar across stations. The deck reports
// each file that two or more sessions changed in the last 30 minutes; the
// board lists them on the fleet screen.
import type { FileConflict } from '../gen';

type Get = (path: string) => Promise<unknown>;

export type ConflictLoader = () => Promise<FileConflict[]>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const touchOk = (t: unknown) =>
  record(t) &&
  typeof t['station_id'] === 'string' &&
  typeof t['session_id'] === 'string' &&
  typeof t['path'] === 'string' &&
  typeof t['touched_at'] === 'number';

/** Validates one conflict from the deck. */
export function conflictView(v: unknown): FileConflict {
  if (
    !record(v) ||
    typeof v['file'] !== 'string' ||
    !(v['repo'] === null || typeof v['repo'] === 'string') ||
    !Array.isArray(v['touches']) ||
    !v['touches'].every(touchOk)
  ) {
    throw new Error('The deck sent an invalid conflict.');
  }
  return v as FileConflict;
}

/** `GET /api/conflicts`. */
export function conflictLoader(get: Get): ConflictLoader {
  return async () => {
    const v = await get('/api/conflicts');
    if (!Array.isArray(v)) throw new Error('The deck sent an invalid conflict list.');
    return v.map(conflictView);
  };
}

/** "3 min ago" relative to `now`. */
export function agoText(at: number, now: number): string {
  const min = Math.max(0, Math.round((now - at) / 60_000));
  return min === 0 ? 'just now' : `${min} min ago`;
}
