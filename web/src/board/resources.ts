import type { CpuLoad, ResourceWarning, StationResources } from '../gen';

/** The same limits as `crates/fleetdeck-proto/src/resources.rs` (DL-193). */
export const LOW_DISK_BYTES = 10_000_000_000;
export const LOW_MEMORY_BYTES = 1_000_000_000;
export const LOW_PERCENT = 10;
export const BUSY_LOAD_X100_PER_CORE = 150;

/** A size as decimal GB with one decimal, for example `12.3 GB`. */
export function gb(bytes: number): string {
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

/** The memory of a session: whole decimal MB under 1 GB (`340 MB`), else `gb` (R13 §10). */
export function memorySize(bytes: number): string {
  return bytes < 1e9 ? `${Math.round(bytes / 1e6)} MB` : gb(bytes);
}

/** `free` is low under `limit`, or under `LOW_PERCENT` of `total`. A `total` of 0 is never low. */
export function isLow(free: number, total: number, limit: number): boolean {
  return total > 0 && (free < limit || free * 100 < total * LOW_PERCENT);
}

/** The CPU is busy when `load5` is above the per-core limit. No cores is never busy. */
export function cpuBusy(cpu: CpuLoad): boolean {
  return cpu.cores > 0 && cpu.load5_x100 > BUSY_LOAD_X100_PER_CORE * cpu.cores;
}

/** The warnings of a report: each low disk in list order, then memory, then CPU. */
export function resourceWarnings(r: StationResources | undefined | null): ResourceWarning[] {
  if (r === undefined || r === null) return [];
  const out: ResourceWarning[] = [];
  for (const d of r.disks) {
    if (isLow(d.free_bytes, d.total_bytes, LOW_DISK_BYTES))
      out.push({ kind: 'disk', text: `Low disk: ${gb(d.free_bytes)} free on ${d.label}` });
  }
  const m = r.memory;
  if (m !== null && isLow(m.available_bytes, m.total_bytes, LOW_MEMORY_BYTES))
    out.push({
      kind: 'memory',
      text: `Low memory: ${gb(m.available_bytes)} free of ${gb(m.total_bytes)}`,
    });
  const c = r.cpu;
  if (c !== null && cpuBusy(c))
    out.push({
      kind: 'cpu',
      text: `Busy CPU: load ${(c.load5_x100 / 100).toFixed(1)} on ${c.cores} cores`,
    });
  return out;
}

/** The used share of a total, clamped to 0..1. A `total` of 0 or less gives 0. */
export function usedShare(used: number, total: number): number {
  return total <= 0 ? 0 : Math.min(1, Math.max(0, used / total));
}
