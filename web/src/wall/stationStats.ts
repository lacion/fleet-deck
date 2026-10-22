// R13 P1: the figures of a station row on the wall, read across a room: the CPU as a share of all
// cores, the memory in whole GB, and the free space of the fullest disk.
import { usedShare } from '../board/resources';
import type { StationResources } from '../gen';

/**
 * A size in a few characters: whole MB under 1 GB ("340 MB"), whole GB under 1 TB ("217 GB"),
 * then TB with one decimal ("1.2 TB").
 * The Stations page wants the same rule: `board/resources.ts` should own it (cross-area request).
 */
export function bytesShort(bytes: number): string {
  if (bytes < 1e9) return `${Math.round(bytes / 1e6)} MB`;
  const gb = Math.round(bytes / 1e9);
  return gb < 1000 ? `${gb} GB` : `${(bytes / 1e12).toFixed(1)} TB`;
}

type DiskLike = { readonly total_bytes: number; readonly free_bytes: number };

/** The disk with the largest used share, or `null` without disks. */
function fullestDisk(disks: readonly DiskLike[]): DiskLike | null {
  return disks.reduce<DiskLike | null>(
    (best, d) =>
      best === null ||
      usedShare(d.total_bytes - d.free_bytes, d.total_bytes) >
        usedShare(best.total_bytes - best.free_bytes, best.total_bytes)
        ? d
        : best,
    null,
  );
}

/** The resources that a wall row reads: `info.resources` (owner) or `display_resources`. */
type Figures = Pick<StationResources, 'cpu' | 'memory'> & { readonly disks: readonly DiskLike[] };

/**
 * The parts of the row: "CPU 9%" (100% = every core busy), "Mem 6/47 GB", "Disk 217 GB free".
 * A part that the station does not report is left out.
 */
export function stationStats(r: Figures | null | undefined): readonly string[] {
  if (r == null) return [];
  const out: string[] = [];
  if (r.cpu !== null && r.cpu.cores > 0) {
    out.push(`CPU ${Math.round(r.cpu.load5_x100 / r.cpu.cores)}%`);
  }
  if (r.memory !== null) {
    const used = Math.round((r.memory.total_bytes - r.memory.available_bytes) / 1e9);
    out.push(`Mem ${used}/${Math.round(r.memory.total_bytes / 1e9)} GB`);
  }
  const disk = fullestDisk(r.disks);
  // The free space only: "free" is the word that a reader needs, so it ends the part.
  if (disk !== null) out.push(`Disk ${bytesShort(disk.free_bytes)} free`);
  return out;
}
