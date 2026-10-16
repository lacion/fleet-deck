import { formatHash } from '../app/routes';
import type { DeckSession, StationResources as Resources } from '../gen';
import {
  cpuBusy,
  gb,
  isLow,
  LOW_DISK_BYTES,
  LOW_MEMORY_BYTES,
  resourceWarnings,
  usedShare,
} from './resources';

function Row({
  share,
  low,
  title,
  children,
}: {
  readonly share: number;
  readonly low: boolean;
  readonly title?: string | undefined;
  readonly children: string;
}) {
  return (
    <li data-low={low ? '' : undefined} title={title}>
      <span className="fd-load-bar" aria-hidden="true">
        <span style={{ width: `${Math.round(share * 100)}%` }} data-high={low ? '' : undefined} />
      </span>
      <span>{children}</span>
    </li>
  );
}

/** The most sessions in the busiest list (R13 §10). */
const BUSIEST = 5;

/** The busiest sessions of a station, as the station sorted them (R13 §10). */
function Busiest({
  resources,
  station,
  sessions,
}: {
  readonly resources: Resources;
  readonly station: string;
  readonly sessions: readonly DeckSession[];
}) {
  if (resources.sessions.length === 0) return null;
  return (
    <>
      <p className="fd-note">Busiest sessions</p>
      <ul className="fd-station-res" aria-label="Busiest sessions">
        {resources.sessions.slice(0, BUSIEST).map((load) => {
          const summary = sessions.find((c) => c.summary.session_id === load.session_id)?.summary;
          const name = summary?.callsign ?? summary?.title ?? load.session_id.slice(0, 8);
          return (
            <li key={load.session_id} data-session={load.session_id}>
              <a
                href={formatHash({
                  name: 'session',
                  station,
                  session: load.session_id,
                  tab: 'chat',
                })}
              >
                {name}
              </a>
              <span>{load.cpu_pct === null ? '—' : `${load.cpu_pct}%`}</span>
              <span>{load.rss_bytes === null ? '—' : gb(load.rss_bytes)}</span>
            </li>
          );
        })}
      </ul>
    </>
  );
}

/** The disk, memory and CPU that a station reports, with a note when one is low (DL-193). */
export function StationResources({
  resources,
  station = '',
  sessions = [],
}: {
  readonly resources: Resources | undefined;
  /** The station id, for the links of the busiest sessions. */
  readonly station?: string | undefined;
  /** The station's own session cards, for the names of the busiest sessions. */
  readonly sessions?: readonly DeckSession[] | undefined;
}) {
  if (resources === undefined) return null;
  const { disks, memory, cpu } = resources;
  const warned = resourceWarnings(resources).length > 0;
  return (
    <>
      <ul className="fd-station-res" aria-label="Resources">
        {disks.map((d) => (
          <Row
            key={`${d.label}\n${d.path}`}
            share={usedShare(d.total_bytes - d.free_bytes, d.total_bytes)}
            low={isLow(d.free_bytes, d.total_bytes, LOW_DISK_BYTES)}
            title={d.path}
          >
            {`${d.label}: ${gb(d.free_bytes)} free of ${gb(d.total_bytes)}`}
          </Row>
        ))}
        {memory === null ? null : (
          <Row
            share={usedShare(memory.total_bytes - memory.available_bytes, memory.total_bytes)}
            low={isLow(memory.available_bytes, memory.total_bytes, LOW_MEMORY_BYTES)}
          >
            {`Memory: ${gb(memory.available_bytes)} free of ${gb(memory.total_bytes)}`}
          </Row>
        )}
        {cpu === null ? null : (
          <Row share={usedShare(cpu.load5_x100, cpu.cores * 100)} low={cpuBusy(cpu)}>
            {`CPU: load ${(cpu.load5_x100 / 100).toFixed(1)} on ${cpu.cores} cores`}
          </Row>
        )}
      </ul>
      <Busiest resources={resources} station={station} sessions={sessions} />
      {warned ? (
        <p className="fd-note fd-res-warn" role="status">
          Low resources. New sessions can be slow or fail here.
        </p>
      ) : null}
    </>
  );
}
