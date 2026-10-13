import type { DiffSummary } from '../gen';

/** A turn has changes when its diff summary exists and names at least one file. */
export function hasChanges(diff: DiffSummary | null): boolean {
  return diff !== null && diff.files_changed > 0;
}
