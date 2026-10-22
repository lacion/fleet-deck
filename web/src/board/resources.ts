import type { CpuLoad, ResourceWarning, StationResources } from '../gen';

/** The same limits as `crates/fleetdeck-proto/src/resources.rs` (DL-193). */
export const LOW_DISK_BYTES = 10_000_000_000;
export const LOW_MEMORY_BYTES = 1_000_000_000;
export const LOW_PERCENT = 10;
export const BUSY_LOAD_X100_PER_CORE = 150;

/** `46.80` → `46.8`, `2.00` → `2`: a number with at most `digits` decimals, no trailing zeros. */
const trimmed = (n: number, digits: number): string => String(Number(n.toFixed(digits)));

/**
 * A size in decimal units for a station card: whole MB under 1 GB (`340 MB`), one decimal GB under
 * 100 GB (`46.8 GB`, `6 GB`), whole GB under 1 TB (`864 GB`), then TB with up to two decimals
 * (`1.08 TB`, `2 TB`). The limits sit where the rounding would reach the next step.
 */
export function bytesText(bytes: number): string {
  if (bytes < 999.5e6) return `${Math.round(bytes / 1e6)} MB`;
  if (bytes < 99.95e9) return `${trimmed(bytes / 1e9, 1)} GB`;
  if (bytes < 999.5e9) return `${Math.round(bytes / 1e9)} GB`;
  return `${trimmed(bytes / 1e12, 2)} TB`;
}

/** The memory of a session (R13 §10): the one size format of the station (`bytesText`). */
export const memorySize = (bytes: number): string => bytesText(bytes);

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
      out.push({ kind: 'disk', text: `Low disk: ${bytesText(d.free_bytes)} free on ${d.label}` });
  }
  const m = r.memory;
  if (m !== null && isLow(m.available_bytes, m.total_bytes, LOW_MEMORY_BYTES))
    out.push({
      kind: 'memory',
      text: `Low memory: ${bytesText(m.available_bytes)} free of ${bytesText(m.total_bytes)}`,
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
