// B29: the read of `GET /api/analytics` (design analytics.md §3.9, §3.10). The shape is the
// deck's `FleetAnalytics` (`crates/fleetdeck-proto/src/analytics.rs`).
import type { FleetAnalytics } from '../gen';

type Get = (path: string) => Promise<unknown>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** True when `v` has the shape of a `FleetAnalytics` (the deck's answer, checked at its top). */
export function isFleetAnalytics(v: unknown): v is FleetAnalytics {
  return (
    record(v) &&
    typeof v['read_at'] === 'number' &&
    typeof v['owner'] === 'boolean' &&
    Array.isArray(v['stations'])
  );
}

/** The path of the read: `since` is an hour start (ms), `station` a station name or null. */
export function analyticsPath(since: number, until: number, station: string | null): string {
  const query = `since=${since}&until=${until}`;
  return station === null
    ? `/api/analytics?${query}`
    : `/api/analytics?${query}&station=${encodeURIComponent(station)}`;
}

/** Reads the analytics of the fleet, or of one station. Errors of the deck pass on unchanged. */
export async function loadAnalytics(
  get: Get,
  since: number,
  until: number,
  station: string | null,
): Promise<FleetAnalytics> {
  const v = await get(analyticsPath(since, until, station));
  if (!isFleetAnalytics(v)) throw new Error('The deck sent analytics that the board cannot read.');
  return v;
}
