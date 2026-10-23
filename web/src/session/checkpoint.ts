// DL-084: why a turn has no file snapshot (a checkpoint side is `unavailable` with a reason code of
// the station, `workspace/boundary.rs`). The board shows plain words, never the code itself; the
// code stays in the `title` of the row.
import { countText } from '../ui/format';
import { hasChanges } from './changes';
import type { FinalizedTurn } from './workspace';

/** The reason code of a session directory outside Git: no turn of the session can have a snapshot. */
export const NOT_A_GIT_WORKTREE = 'not_a_git_worktree';

/** The cause after "No snapshot: " for each reason code that the station sends. */
const CAUSE: Readonly<Record<string, string>> = {
  turn_running: 'another turn was running',
  queued_input: 'a queued message started next',
  background_writers: 'background work was running',
  provider_initiated: 'the agent started the turn on its own',
  session_ended: 'the session ended',
  station_restart: 'the station restarted',
  [NOT_A_GIT_WORKTREE]: 'the folder is not a Git repository',
  capture_failed: 'the capture failed',
  checkpoint_missing: 'the snapshot is gone',
  checkpoint_changed: 'the snapshot was changed',
};

/** The cause of a reason code in plain words (`background work was running`), or `null`. */
export function checkpointCause(reason: string): string | null {
  return CAUSE[reason] ?? null;
}

/** The text of a turn without a snapshot: `No snapshot: background work was running`. */
export function checkpointText(reason: string): string {
  const cause = checkpointCause(reason);
  return cause === null ? 'No snapshot of the files for this turn' : `No snapshot: ${cause}`;
}

/**
 * The reason that the Changes tab says once above the turn list, or `null`: only when two or more
 * turns are listed and none of them has a snapshot or a change. A folder outside Git explains
 * every turn, so it wins; else the most frequent reason (the first one on a tie).
 */
export function sharedReason(turns: readonly FinalizedTurn[]): string | null {
  if (turns.length < 2) return null;
  const counts = new Map<string, number>();
  for (const t of turns) {
    if (t.end?.state !== 'unavailable' || hasChanges(t.diff)) return null;
    counts.set(t.end.reason, (counts.get(t.end.reason) ?? 0) + 1);
  }
  if (counts.has(NOT_A_GIT_WORKTREE)) return NOT_A_GIT_WORKTREE;
  let best: string | null = null;
  let most = 0;
  for (const [reason, n] of counts) {
    if (n > most) {
      best = reason;
      most = n;
    }
  }
  return best;
}

/**
 * The session folder is outside Git: a turn says so and no turn has a snapshot or a change. The
 * Changes tab then shows one empty state (§4.7) in place of a list of turns that only say "No
 * snapshot", and offers no Branches view.
 */
export function outsideGit(turns: readonly FinalizedTurn[]): boolean {
  return (
    turns.some((t) => t.end?.state === 'unavailable' && t.end.reason === NOT_A_GIT_WORKTREE) &&
    turns.every(
      (t) => t.start?.state !== 'available' && t.end?.state !== 'available' && !hasChanges(t.diff),
    )
  );
}

/** A turn of the list has a snapshot at its start or its end. */
export function hasSnapshot(turns: readonly FinalizedTurn[]): boolean {
  return turns.some((t) => t.start?.state === 'available' || t.end?.state === 'available');
}

/**
 * The note of a turn list that has no snapshot at all (`sharedReason`). With `count` (the empty
 * state that replaces the list) it names the turns, as no list shows them.
 */
export function sharedNote(reason: string, count?: number): string {
  if (reason === NOT_A_GIT_WORKTREE) {
    return 'The board keeps no file snapshots for this session: the folder is not a Git repository.';
  }
  const turns = count === undefined ? 'these turns' : `the ${countText(count)} finished turns`;
  const cause = checkpointCause(reason);
  return cause === null
    ? `The board kept no file snapshots for ${turns}.`
    : `The board kept no file snapshots for ${turns}: ${cause}.`;
}
