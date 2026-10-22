// The facts of the stations page header and of a station card header: the summary sentence
// (DESIGN-SYSTEM §4.3) and when a card shows "Last seen" (principle 7).
import type { ReactNode } from 'react';
import type { StationView } from '../gen';
import type { Freshness } from '../runtime/state';

/** A link seen within this time is "now": an online station shows no "Last seen". */
const SEEN_RECENTLY_MS = 2 * 60_000;

/**
 * The "Last seen" fact is noise for a station that is up and was seen within 2 min, and a station
 * that was never seen has no fact to show (DESIGN-SYSTEM principle 7).
 */
export const showLastSeen = (lastSeen: number | null, up: boolean, now: number): boolean =>
  lastSeen !== null && (!up || now - lastSeen > SEEN_RECENTLY_MS);

/** The summary sentence of the page header (§4.3): how many stations are online. */
export function stationsSummary(stations: readonly StationView[], freshness: Freshness): ReactNode {
  const n = stations.length;
  if (n === 0) return null;
  const total = n === 1 ? '1 station' : `${n} stations`;
  if (freshness === 'stale') return `${total}. The link status is stale.`;
  const down = stations.filter((s) => !s.reachable).length;
  if (down === 0) return n === 1 ? '1 station, online.' : `${n} stations, all online.`;
  if (n === 1)
    return (
      <>
        1 station, <span className="fd-stations-off">offline</span>.
      </>
    );
  return (
    <>
      {n - down} of {total} online. <span className="fd-stations-off">{down} offline</span>.
    </>
  );
}
