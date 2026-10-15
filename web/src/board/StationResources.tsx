import type { StationResources as Resources } from '../gen';
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

/** The disk, memory and CPU that a station reports, with a note when one is low (DL-193). */
export function StationResources({ resources }: { readonly resources: Resources | undefined }) {
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
      {warned ? (
        <p className="fd-note fd-res-warn" role="status">
          Low resources. New sessions can be slow or fail here.
        </p>
      ) : null}
    </>
  );
}
