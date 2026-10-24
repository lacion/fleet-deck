// The facts of the stations page header: the summary sentence (DESIGN-SYSTEM §4.3). A station
// card shows "Last seen" (the end of its last link) only for a station that the deck reports
// offline (StationsPanel.tsx).
import type { ReactNode } from 'react';
import type { StationView } from '../gen';
import type { Freshness } from '../runtime/state';

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
