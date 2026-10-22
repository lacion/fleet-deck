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
 * The git plumbing of a file section: the view leaves these lines out, because the file header
 * row (path and +/−) says the same. A rename and a binary file stay: they tell something new.
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

/** The header row of a file section: its path and the count of its added and deleted lines. */
export type FileHead = { readonly path: string; readonly add: number; readonly del: number };

/** The header of each file section, by the index of its `diff --git` line. */
export function fileHeads(lines: readonly string[]): ReadonlyMap<number, FileHead> {
  const heads = new Map<number, FileHead>();
  let at = -1;
  let path = '';
  let add = 0;
  let del = 0;
  const flush = () => {
    if (at >= 0) heads.set(at, { path, add, del });
  };
  lines.forEach((line, n) => {
    const kind = diffLineKind(line);
    if (kind === 'file') {
      flush();
      at = n;
      path = splitDiffFileLine(line)?.path ?? line.slice('diff --git '.length);
      add = 0;
      del = 0;
    } else if (kind === 'add') add += 1;
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
