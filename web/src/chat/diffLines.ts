// DL-141: the line kinds of a unified diff, for the colors of the diff view.
import { parseDiff } from '../review/diffModel';

export type DiffLineKind = 'file' | 'meta' | 'hunk' | 'add' | 'del' | 'ctx';

const META = [
  '--- ',
  '+++ ',
  'index ',
  'new file mode',
  'deleted file mode',
  'similarity index',
  'rename from',
  'rename to',
  'Binary files',
  'old mode',
  'new mode',
];

export function diffLineKind(line: string): DiffLineKind {
  if (line.startsWith('diff --git ')) return 'file';
  if (META.some((m) => line.startsWith(m))) return 'meta';
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return 'ctx';
}

/**
 * The kind of every line of a patch. In a hunk (from its first `@@` line to the next `diff --git`
 * line), a line that starts with `-` or `+` is a deletion or an addition, also when it looks like a
 * file header: a deleted `-- comment` line is `--- comment` in the patch.
 */
export function diffLineKinds(lines: readonly string[]): DiffLineKind[] {
  let inHunk = false;
  return lines.map((line) => {
    const kind = diffLineKind(line);
    if (kind === 'file') inHunk = false;
    else if (line.startsWith('@@')) inHunk = true;
    else if (inHunk && kind === 'meta') {
      if (line.startsWith('-')) return 'del';
      if (line.startsWith('+')) return 'add';
    }
    return kind;
  });
}

/**
 * The git plumbing of a file section: the view leaves these lines out, because the file header
 * row (path, +/− and the status badge of a new, deleted or mode-changed file) says the same.
 * A rename and a binary file stay: they tell something new.
 */
const PLUMBING = [
  '--- ',
  '+++ ',
  'index ',
  'new file mode',
  'deleted file mode',
  'similarity index',
  'old mode',
  'new mode',
];

export const isPlumbing = (line: string): boolean => PLUMBING.some((p) => line.startsWith(p));

const FILE_LINE = /^(diff --git a\/.* b\/)(.+)$/;

/** The parts of a `diff --git a/X b/Y` line: the text before the path Y, and Y; `null` for another shape. */
export function splitDiffFileLine(
  line: string,
): { readonly head: string; readonly path: string } | null {
  const match = FILE_LINE.exec(line);
  return match?.[1] !== undefined && match[2] !== undefined
    ? { head: match[1], path: match[2] }
    : null;
}

/** What a file section does to the file, besides its lines: from its `new file mode`, `deleted file mode` or `old mode`/`new mode` lines. */
export type FileStatus =
  | { readonly kind: 'added' }
  | { readonly kind: 'deleted' }
  | { readonly kind: 'mode'; readonly old: string; readonly new: string };

/**
 * The header row of a file section: its path, the count of its added and deleted lines, and its
 * status (`null` for a file that git only changes in its lines, or renames).
 */
export type FileHead = {
  readonly path: string;
  readonly add: number;
  readonly del: number;
  readonly status: FileStatus | null;
};

/** The status that a header line of a file section gives, merged into the status so far. */
function statusOf(line: string, now: FileStatus | null): FileStatus | null {
  if (line.startsWith('new file mode')) return { kind: 'added' };
  if (line.startsWith('deleted file mode')) return { kind: 'deleted' };
  const mode = (prefix: string) => line.slice(prefix.length).trim();
  if (line.startsWith('old mode ')) return { kind: 'mode', old: mode('old mode '), new: '' };
  if (line.startsWith('new mode ') && now?.kind === 'mode')
    return { ...now, new: mode('new mode ') };
  return now;
}

/** The header of each file section, by the index of its `diff --git` line. */
export function fileHeads(lines: readonly string[]): ReadonlyMap<number, FileHead> {
  const heads = new Map<number, FileHead>();
  let at = -1;
  let path = '';
  let add = 0;
  let del = 0;
  let status: FileStatus | null = null;
  const flush = () => {
    if (at >= 0) heads.set(at, { path, add, del, status });
  };
  const kinds = diffLineKinds(lines);
  lines.forEach((line, n) => {
    const kind = kinds[n];
    if (kind === 'file') {
      flush();
      at = n;
      path = splitDiffFileLine(line)?.path ?? line.slice('diff --git '.length);
      add = 0;
      del = 0;
      status = null;
    } else if (kind === 'meta') status = statusOf(line, status);
    else if (kind === 'add') add += 1;
    else if (kind === 'del') del += 1;
  });
  flush();
  return heads;
}

/** The old and the new line number of a hunk line; `null` on the side that does not have it. */
export type LineNo = { readonly old: number | null; readonly new: number | null };

/** The line numbers of every hunk line of `patch`, by the index of the line in `patch.split('\n')`. */
export function lineNumbers(patch: string): ReadonlyMap<number, LineNo> {
  const out = new Map<number, LineNo>();
  for (const file of parseDiff(patch).files) {
    for (const hunk of file.hunks) {
      for (const l of hunk.lines) out.set(l.index, { old: l.oldNo, new: l.newNo });
    }
  }
  return out;
}
