import type { FileActions } from './workspace';

/** The most paths the Files panel lists as changed. */
export const CHANGED_FILES_MAX = 50;

/** The changed files of a session: the paths in patch order and the cut flag. */
export interface ChangedFiles {
  readonly paths: readonly string[];
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

/** The changed paths of a patch: the `b/` path of each header, in patch order, at most `max`. */
export function changedPaths(patch: string, max = CHANGED_FILES_MAX): string[] {
  const paths: string[] = [];
  // A deleted file cannot be opened: its block drops the path it added.
  let current: string | null = null;
  for (const line of patch.split('\n')) {
    if (line.startsWith('deleted file mode') && current !== null) {
      paths.splice(paths.indexOf(current), 1);
      current = null;
      continue;
    }
    const path = headerPath(line);
    if (path === null) continue;
    current = null;
    if (paths.includes(path)) continue;
    if (paths.length >= max) break;
    paths.push(path);
    current = path;
  }
  return paths;
}

/** The session diff of `files` as a list of changed files; an error or no change: `null`. */
export async function loadChanged(files: FileActions): Promise<ChangedFiles | null> {
  try {
    const diff = await files.sessionDiff();
    const paths = changedPaths(diff.patch);
    return paths.length === 0 ? null : { paths, truncated: diff.truncated };
  } catch {
    return null;
  }
}
