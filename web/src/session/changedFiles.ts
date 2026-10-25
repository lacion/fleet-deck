import type { DiffSummary } from '../gen';
import type { FileActions, FinalizedTurn } from './workspace';

/** The most paths the Files panel lists as changed. */
export const CHANGED_FILES_MAX = 50;

/** How a file changed: `A` added, `M` modified, `R` renamed (a deleted file is not listed). */
export type ChangeKind = 'A' | 'M' | 'R';

/** The change of one file: its kind and its added and removed lines. */
export interface FileChange {
  readonly kind: ChangeKind;
  readonly insertions: number;
  readonly deletions: number;
}

/** The changed files of a session: the paths in patch order, their changes and the cut flag. */
export interface ChangedFiles {
  readonly paths: readonly string[];
  /** The change of each path in `paths`. */
  readonly changes: Readonly<Record<string, FileChange>>;
  readonly truncated: boolean;
}

/**
 * The `b/` path of one `diff --git a/<old> b/<new>` header line, or `null`.
 * When both sides name the same path, the line splits in the middle (so a
 * path with spaces stays whole); else the last ` b/` starts the new path.
 */
function headerPath(line: string): string | null {
  const head = 'diff --git a/';
  if (!line.startsWith(head)) return null;
  const rest = line.slice(head.length);
  const half = (rest.length - 3) / 2;
  if (
    Number.isInteger(half) &&
    half > 0 &&
    rest.slice(half, half + 3) === ' b/' &&
    rest.slice(0, half) === rest.slice(half + 3)
  ) {
    return rest.slice(0, half);
  }
  const at = rest.lastIndexOf(' b/');
  if (at < 0) return null;
  const path = rest.slice(at + 3);
  return path === '' ? null : path;
}

/** The changed files of a patch with their changes, in patch order, at most `max`. */
export function changedFiles(
  patch: string,
  max = CHANGED_FILES_MAX,
): { readonly paths: string[]; readonly changes: Record<string, FileChange> } {
  const found = new Map<string, { kind: ChangeKind; insertions: number; deletions: number }>();
  // The block of the current header; a deleted file cannot be opened: its block drops its path.
  let current: string | null = null;
  // The lines of a hunk count; the `---`/`+++` lines before the first hunk do not.
  let inHunk = false;
  for (const line of patch.split('\n')) {
    const path = headerPath(line);
    if (path !== null) {
      current = null;
      inHunk = false;
      if (found.has(path)) continue;
      if (found.size >= max) break;
      found.set(path, { kind: 'M', insertions: 0, deletions: 0 });
      current = path;
      continue;
    }
    const change = current === null ? undefined : found.get(current);
    if (current === null || change === undefined) continue;
    if (inHunk) {
      if (line.startsWith('+')) change.insertions += 1;
      else if (line.startsWith('-')) change.deletions += 1;
    } else if (line.startsWith('@@')) {
      inHunk = true;
    } else if (line.startsWith('deleted file mode')) {
      found.delete(current);
      current = null;
    } else if (line.startsWith('new file mode')) {
      change.kind = 'A';
    } else if (line.startsWith('rename to ')) {
      change.kind = 'R';
    }
  }
  return { paths: [...found.keys()], changes: Object.fromEntries(found) };
}

/** The changed paths of a patch: the `b/` path of each header, in patch order, at most `max`. */
export function changedPaths(patch: string, max = CHANGED_FILES_MAX): string[] {
  return changedFiles(patch, max).paths;
}

/**
 * All changed paths of a patch for diff navigation: the `b/` path of each header in patch order,
 * at most `max`. A deleted file is included (`changedPaths` leaves it out: it cannot be opened).
 */
export function allChangedPaths(patch: string, max = CHANGED_FILES_MAX): string[] {
  const found = new Set<string>();
  for (const line of patch.split('\n')) {
    const path = headerPath(line);
    if (path === null || found.has(path)) continue;
    if (found.size >= max) break;
    found.add(path);
  }
  return [...found];
}

/** What one read of the session diff says: its size, the openable files, and every changed path. */
export interface SessionChanges {
  /** The size of the whole diff (also of the files that a cut patch does not list). */
  readonly summary: DiffSummary;
  /** The files for the Files browser (readable files only); `null` when the diff has none. */
  readonly changed: ChangedFiles | null;
  /** Every changed path for diff navigation (deleted files too); empty when the diff is clean. */
  readonly all: readonly string[];
}

/** Reads the session diff of `files` once; an error: `null`. */
export async function loadSessionChanges(files: FileActions): Promise<SessionChanges | null> {
  try {
    const diff = await files.sessionDiff();
    const { paths, changes } = changedFiles(diff.patch);
    return {
      summary: diff.summary,
      changed: paths.length === 0 ? null : { paths, changes, truncated: diff.truncated },
      all: allChangedPaths(diff.patch),
    };
  } catch {
    return null;
  }
}

/** The session diff of `files` as a list of changed files; an error or no change: `null`. */
export async function loadChanged(files: FileActions): Promise<ChangedFiles | null> {
  return (await loadSessionChanges(files))?.changed ?? null;
}

/**
 * The turn that a changed file of the Properties panel opens when the session has no spawn base:
 * the newest kept turn whose loaded patch lists `path`. `byTurn` holds the paths of each loaded
 * turn diff (by turn id). No such turn: `undefined` (the file is not a link).
 */
export function turnForPath(
  turns: readonly FinalizedTurn[],
  byTurn: ReadonlyMap<string, readonly string[]> | undefined,
  path: string,
): FinalizedTurn | undefined {
  for (let i = turns.length - 1; i >= 0; i -= 1) {
    const turn = turns[i];
    if (turn === undefined || turn.reverted) continue;
    if (byTurn?.get(turn.turnId)?.includes(path) === true) return turn;
  }
  return undefined;
}
