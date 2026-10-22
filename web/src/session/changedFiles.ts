import type { FileActions } from './workspace';

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

/** The session diff of `files` as a list of changed files; an error or no change: `null`. */
export async function loadChanged(files: FileActions): Promise<ChangedFiles | null> {
  try {
    const diff = await files.sessionDiff();
    const { paths, changes } = changedFiles(diff.patch);
    return paths.length === 0 ? null : { paths, changes, truncated: diff.truncated };
  } catch {
    return null;
  }
}
