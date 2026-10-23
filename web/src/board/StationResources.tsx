import { formatHash } from '../app/routes';
import type { DeckSession, StationResources as Resources } from '../gen';
import {
  bytesText,
  cpuBusy,
  isLow,
  loadText,
  LOW_DISK_BYTES,
  LOW_MEMORY_BYTES,
  resourceWarnings,
  usedShare,
} from './resources';

/**
 * One resource row: the label, a bar of the used share, the used amount of the total in the text
 * color, then the quiet aside (what is free, the cores). The aside never breaks inside; a narrow
 * card puts it on a line of its own without the separator (stations.css).
 */
function Row({
  label,
  share,
  low,
  title,
  value,
  aside,
}: {
  readonly label: string;
  readonly share: number;
  readonly low: boolean;
  readonly title?: string | undefined;
  readonly value: string;
  readonly aside: string;
}) {
  return (
    <li data-low={low ? '' : undefined} title={title}>
      <span className="fd-station-res-label">{label}</span>
      <span
        className="fd-meter"
        data-tone="neutral"
        data-level={low ? 'high' : undefined}
        aria-hidden="true"
      >
        <span style={{ width: `${Math.round(share * 100)}%` }} />
      </span>
      <span className="fd-station-res-value">
        {/* The space is outside the aside: the line may break there, never inside the aside. */}
        {`${value} `}
        <span className="fd-station-res-aside">
          <span className="fd-station-res-sep">· </span>
          {aside}
        </span>
      </span>
    </li>
  );
}

/** The used part of a total, then the total: `864 GB of 1.08 TB`. */
const usedOf = (free: number, total: number): string =>
  `${bytesText(Math.max(0, total - free))} of ${bytesText(total)}`;

/** The most sessions in the busiest list (R13 §10). */
const BUSIEST = 5;

/**
 * The busiest sessions of a station, as the station sorted them (R13 §10): a property of its own
 * on the card. A row is one link to its session: the name, then the CPU and the memory with their
 * units in two right-aligned columns. The name is the title, as the sidebar and the strips name a
 * session, with the callsign after it (`.fd-callsign`, §2.3); a session without a title is its
 * callsign, else 8 characters of its id.
 */
export function StationBusiest({
  resources,
  station,
  sessions,
}: {
  readonly resources: Resources | undefined;
  /** The station id, for the links. */
  readonly station: string;
  /** The station's own session cards, for the names. */
  readonly sessions: readonly DeckSession[];
}) {
  if (resources === undefined || resources.sessions.length === 0) return null;
  return (
    <div className="fd-station-prop" data-prop="busiest">
      <h4 className="fd-station-prop-label">Busiest sessions</h4>
      <ul className="fd-station-busy" aria-label="Busiest sessions">
        {resources.sessions.slice(0, BUSIEST).map((load) => {
          const summary = sessions.find((c) => c.summary.session_id === load.session_id)?.summary;
          const title = summary?.title || null;
          const callsign = summary?.callsign || null;
          // The callsign follows the title; without a title it is the name (condensed).
          const nameClass =
            title === null && callsign !== null
              ? 'fd-station-busy-name fd-callsign'
              : 'fd-station-busy-name';
          return (
            <li key={load.session_id} data-session={load.session_id}>
              {/* The spaces between the cells are for the link name only: a grid and a flex
                  container do not draw them. */}
              <a
                href={formatHash({
                  name: 'session',
                  station,
                  session: load.session_id,
                  tab: 'chat',
                })}
                title={title ?? undefined}
              >
                <span className="fd-station-busy-who">
                  <span className={nameClass}>
                    {title ?? callsign ?? load.session_id.slice(0, 8)}
                  </span>
                  {title === null || callsign === null ? null : (
                    <>
                      {' '}
                      <span className="fd-callsign fd-station-busy-cs">{callsign}</span>
                    </>
                  )}
                </span>{' '}
                <span className="fd-station-busy-num">
                  {load.cpu_pct === null ? '—' : `${load.cpu_pct}% CPU`}
                </span>{' '}
                <span className="fd-station-busy-num">
                  {load.rss_bytes === null ? '—' : bytesText(load.rss_bytes)}
                </span>
              </a>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** The disk, memory and CPU that a station reports, with a note when one is low (DL-193). */
export function StationResources({ resources }: { readonly resources: Resources | undefined }) {
  if (resources === undefined) return null;
  const { disks, memory, cpu } = resources;
  const warned = resourceWarnings(resources).length > 0;
  return (
    <div className="fd-station-prop" data-prop="resources">
      <h4 className="fd-station-prop-label">Resources</h4>
      <div className="fd-station-prop-body">
        <ul className="fd-station-res" aria-label="Resources">
          {disks.map((d) => (
            <Row
              key={`${d.label}\n${d.path}`}
              label={d.label}
              share={usedShare(d.total_bytes - d.free_bytes, d.total_bytes)}
              low={isLow(d.free_bytes, d.total_bytes, LOW_DISK_BYTES)}
              title={d.path}
              value={usedOf(d.free_bytes, d.total_bytes)}
              aside={`${bytesText(d.free_bytes)} free`}
            />
          ))}
          {memory === null ? null : (
            <Row
              label="Memory"
              share={usedShare(memory.total_bytes - memory.available_bytes, memory.total_bytes)}
              low={isLow(memory.available_bytes, memory.total_bytes, LOW_MEMORY_BYTES)}
              value={usedOf(memory.available_bytes, memory.total_bytes)}
              aside={`${bytesText(memory.available_bytes)} free`}
            />
          )}
          {cpu === null ? null : (
            <Row
              label="CPU"
              share={usedShare(cpu.load5_x100, cpu.cores * 100)}
              low={cpuBusy(cpu)}
              title="Average load of the last 5 minutes"
              value={`Load ${loadText(cpu.load5_x100)}`}
              aside={cpu.cores === 1 ? '1 core' : `${cpu.cores} cores`}
            />
          )}
        </ul>
        {warned ? (
          <p className="fd-note fd-res-warn" role="status">
            Low resources. New sessions can be slow or fail here.
          </p>
        ) : null}
      </div>
    </div>
  );
}
