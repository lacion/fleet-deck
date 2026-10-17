// DL-179: a port of the diff parser of the deck
// (`crates/fleetdeck-deck/src/store/review_anchor.rs`). The board uses it to know which line of a
// rendered diff can take a comment, and to tell whether the lines of a comment still read the
// same. The rules are the deck's:
//
// - the new side numbers the context lines and the `+` lines of a hunk, the old side the context
//   lines and the `-` lines; `\ No newline at end of file` is skipped;
// - a range must lie inside one hunk;
// - the anchor text is the lines joined with `\n` (no trailing newline);
// - the paths come from the `diff --git a/<old> b/<new>` header, replaced by the `rename from/to`
//   or `copy from/to` lines; a new file has no old path and a deleted file no new path;
// - a diff that the station cut (`\n[diff truncated]\n`) loses its last, possibly partial, line.
import type { ReviewSide } from '../gen';

/** The text that the station appends to a cut patch. */
export const CUT_MARKER = '\n[diff truncated]\n';

type Kind = 'context' | 'added' | 'removed';

type Line = {
  readonly text: string;
  readonly oldNo: number | null;
  readonly newNo: number | null;
  readonly kind: Kind;
  /** The index of the line in `patch.split('\n')`. */
  readonly index: number;
};

type Hunk = { readonly lines: Line[] };

type ParsedFile = {
  readonly old: string | null;
  readonly new: string | null;
  readonly hunks: readonly Hunk[];
};

/** A parsed unified diff. */
export type ParsedDiff = { readonly files: readonly ParsedFile[] };

/** One entry per line of a patch: where a comment on that line goes, or `null`. */
export type DiffRow = {
  readonly side: ReviewSide;
  readonly path: string;
  readonly line: number;
} | null;

/** The patch without a cut marker and without the partial line that a cut leaves before it. */
function withoutCut(patch: string): string {
  if (!patch.endsWith(CUT_MARKER)) return patch;
  const body = patch.slice(0, patch.length - CUT_MARKER.length);
  if (body === '' || body.endsWith('\n')) return body;
  const at = body.lastIndexOf('\n');
  return at < 0 ? '' : body.slice(0, at + 1);
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Reads a C-style quoted name (the `core.quotePath` form) after the opening quote. Returns the
 * name and the text after the closing quote.
 */
function unquote(body: string): { name: string; rest: string } | null {
  const raw: number[] = [];
  let i = 0;
  const push = (text: string) => {
    for (const b of encoder.encode(text)) raw.push(b);
  };
  while (i < body.length) {
    const c = String.fromCodePoint(body.codePointAt(i) ?? 0);
    if (c === '"') {
      return { name: decoder.decode(new Uint8Array(raw)), rest: body.slice(i + 1) };
    }
    if (c === '\\') {
      i += 1;
      if (i >= body.length) return null;
      const e = String.fromCodePoint(body.codePointAt(i) ?? 0);
      if (e === 'n') raw.push(0x0a);
      else if (e === 't') raw.push(0x09);
      else if (e === 'r') raw.push(0x0d);
      else if (e >= '0' && e <= '3') {
        const digits = body.slice(i, i + 3);
        if (!/^[0-7]{3}$/.test(digits)) return null;
        raw.push(Number.parseInt(digits, 8));
        i += 2;
      } else {
        push(e);
      }
      i += e.length;
    } else {
      push(c);
      i += c.length;
    }
  }
  return null;
}

/** A name of a `rename`, `copy`, `---` or `+++` line: plain or quoted. */
function name(input: string): string | null {
  const text = input.replace(/[\t\r]+$/, '');
  if (text.startsWith('"')) return unquote(text.slice(1))?.name ?? null;
  return text;
}

/** A name of a `---` or `+++` line without its `a/` or `b/`; `null` for `/dev/null`. */
function sidePath(text: string, prefix: string): string | null {
  const n = name(text);
  if (n === null || n === '/dev/null') return null;
  return n.startsWith(prefix) ? n.slice(prefix.length) : null;
}

/**
 * The `[old, new]` paths that a `diff --git` header names. A header whose paths have spaces can
 * be split in several ways: the split with the same path on both sides wins, else the header does
 * not decide (the `---`, `+++` and `rename` lines do).
 */
function headerPaths(input: string): readonly [string, string] | null {
  const rest = input.replace(/\r+$/, '');
  if (rest.startsWith('"')) {
    const first = unquote(rest.slice(1));
    if (first === null) return null;
    const after = first.rest.trimStart();
    let second: string;
    if (after.startsWith('"')) {
      const q = unquote(after.slice(1));
      if (q === null) return null;
      second = q.name;
    } else {
      second = after;
    }
    if (!first.name.startsWith('a/') || !second.startsWith('b/')) return null;
    return [first.name.slice(2), second.slice(2)];
  }
  if (!rest.startsWith('a/')) return null;
  const body = rest.slice(2);
  const quoted = body.indexOf(' "b/');
  if (quoted >= 0) {
    const q = unquote(body.slice(quoted + 2));
    if (q === null || !q.name.startsWith('b/')) return null;
    return [body.slice(0, quoted), q.name.slice(2)];
  }
  const candidates: (readonly [string, string])[] = [];
  for (let at = body.indexOf(' b/'); at >= 0; at = body.indexOf(' b/', at + 3)) {
    candidates.push([body.slice(0, at), body.slice(at + 3)]);
  }
  if (candidates.length === 1) return candidates[0] ?? null;
  return candidates.find(([o, n]) => o === n) ?? null;
}

/** `[old_start, old_count, new_start, new_count]` of a `@@ -a[,b] +c[,d] @@` line. */
function hunkCounts(line: string): readonly [number, number, number, number] | null {
  if (!line.startsWith('@@ -')) return null;
  const afterMinus = line.slice(4);
  const plus = afterMinus.indexOf(' +');
  if (plus < 0) return null;
  const old = afterMinus.slice(0, plus);
  const afterPlus = afterMinus.slice(plus + 2);
  const end = afterPlus.indexOf(' @@');
  if (end < 0) return null;
  const range = (s: string): readonly [number, number] | null => {
    const [a, b, ...more] = s.split(',');
    if (more.length > 0 || a === undefined || !/^\d+$/.test(a)) return null;
    if (b === undefined) return [Number(a), 1];
    return /^\d+$/.test(b) ? [Number(a), Number(b)] : null;
  };
  const o = range(old);
  const n = range(afterPlus.slice(0, end));
  return o === null || n === null ? null : [o[0], o[1], n[0], n[1]];
}

type Pending = {
  header: readonly [string, string] | null;
  minus: string | null | undefined;
  plus: string | null | undefined;
  renameFrom: string | null;
  renameTo: string | null;
  isNew: boolean;
  isDeleted: boolean;
  hunks: Hunk[];
};

const pending = (header: readonly [string, string] | null): Pending => ({
  header,
  minus: undefined,
  plus: undefined,
  renameFrom: null,
  renameTo: null,
  isNew: false,
  isDeleted: false,
  hunks: [],
});

function finish(p: Pending): ParsedFile {
  // An ambiguous header: the `---` and `+++` lines decide.
  let old: string | null = p.header === null ? (p.minus ?? null) : p.header[0];
  let current: string | null = p.header === null ? (p.plus ?? null) : p.header[1];
  if (p.isNew) old = null;
  if (p.isDeleted) current = null;
  if (p.renameFrom !== null && p.renameTo !== null) {
    old = p.renameFrom;
    current = p.renameTo;
  }
  return { old, new: current, hunks: p.hunks };
}

/** The hunk that the parser reads and the lines that it still owes. */
type Open = {
  hunk: Hunk;
  oldNo: number;
  newNo: number;
  oldLeft: number;
  newLeft: number;
};

const owes = (o: Open): boolean => o.oldLeft > 0 || o.newLeft > 0;

/** Takes `line` as a line of the hunk. `false`: it is not one. */
function take(o: Open, line: string, index: number): boolean {
  let kind: Kind;
  switch (line.charAt(0)) {
    case ' ':
      kind = 'context';
      break;
    case '+':
      kind = 'added';
      break;
    case '-':
      kind = 'removed';
      break;
    // `\ No newline at end of file`: no line, still in the hunk.
    case '\\':
      return true;
    default:
      return false;
  }
  let oldNo: number | null = null;
  let newNo: number | null = null;
  if (kind !== 'added') {
    oldNo = o.oldNo;
    o.oldNo += 1;
    o.oldLeft = Math.max(0, o.oldLeft - 1);
  }
  if (kind !== 'removed') {
    newNo = o.newNo;
    o.newNo += 1;
    o.newLeft = Math.max(0, o.newLeft - 1);
  }
  o.hunk.lines.push({ text: line.slice(1), oldNo, newNo, kind, index });
  return true;
}

/** Parses `patch`, a unified diff of `diff-tree -p` or `diff`. */
export function parseDiff(patch: string): ParsedDiff {
  const files: ParsedFile[] = [];
  let cur: Pending | null = null;
  let open: Open | null = null;
  const lines = withoutCut(patch).split('\n');
  for (let n = 0; n < lines.length; n += 1) {
    const line = lines[n] ?? '';
    if (open !== null && owes(open) && take(open, line, n)) continue;
    // Any other line ends the hunk: the counts are used up, or the patch is cut or damaged.
    if (open !== null) {
      cur?.hunks.push(open.hunk);
      open = null;
    }
    if (line.startsWith('diff --git ')) {
      if (cur !== null) files.push(finish(cur));
      cur = pending(headerPaths(line.slice('diff --git '.length)));
      continue;
    }
    if (cur === null) continue;
    const counts = hunkCounts(line);
    if (counts !== null) {
      open = {
        hunk: { lines: [] },
        oldNo: counts[0],
        oldLeft: counts[1],
        newNo: counts[2],
        newLeft: counts[3],
      };
    } else if (line.startsWith('--- ')) {
      cur.minus = sidePath(line.slice(4), 'a/');
    } else if (line.startsWith('+++ ')) {
      cur.plus = sidePath(line.slice(4), 'b/');
    } else if (line.startsWith('rename from ')) {
      cur.renameFrom = name(line.slice('rename from '.length));
    } else if (line.startsWith('copy from ')) {
      cur.renameFrom = name(line.slice('copy from '.length));
    } else if (line.startsWith('rename to ')) {
      cur.renameTo = name(line.slice('rename to '.length));
    } else if (line.startsWith('copy to ')) {
      cur.renameTo = name(line.slice('copy to '.length));
    } else if (line.startsWith('new file mode')) {
      cur.isNew = true;
    } else if (line.startsWith('deleted file mode')) {
      cur.isDeleted = true;
    }
  }
  if (open !== null) cur?.hunks.push(open.hunk);
  if (cur !== null) files.push(finish(cur));
  return { files };
}

/** The lines `start..=end` of `side`, when one hunk shows all of them. */
function range(file: ParsedFile, side: ReviewSide, start: number, end: number): string | null {
  const number = (l: Line): number | null => (side === 'new' ? l.newNo : l.oldNo);
  const hunk = file.hunks.find((h) => h.lines.some((l) => number(l) === start));
  if (hunk === undefined) return null;
  const picked = hunk.lines.filter((l) => {
    const n = number(l);
    return n !== null && n >= start && n <= end;
  });
  if (picked.length !== end - start + 1) return null;
  return picked.map((l) => l.text).join('\n');
}

/**
 * The anchor text of `start..=end` of `side` of the file `path`; `null` when the file, the side or
 * a line of the range is not shown, or the range crosses two hunks. No size check.
 */
export function anchorText(
  d: ParsedDiff,
  side: ReviewSide,
  path: string,
  start: number,
  end: number,
): string | null {
  if (start === 0 || end < start) return null;
  for (const file of d.files) {
    if ((side === 'new' ? file.new : file.old) !== path) continue;
    const text = range(file, side, start, end);
    if (text !== null) return text;
  }
  return null;
}

/** Every old and new path of the diff. */
export function diffPaths(d: ParsedDiff): ReadonlySet<string> {
  const paths = new Set<string>();
  for (const file of d.files) {
    if (file.old !== null) paths.add(file.old);
    if (file.new !== null) paths.add(file.new);
  }
  return paths;
}

/**
 * One entry per line of `patch.split('\n')`. A `+` line and a context line give the new side (a
 * context line is commented on the new side only); a `-` line gives the old side; every other
 * line, a line of a side that has no path, and the cut marker with the partial line before it give
 * `null`.
 */
export function diffRows(patch: string): readonly DiffRow[] {
  const rows: DiffRow[] = new Array<DiffRow>(patch.split('\n').length).fill(null);
  for (const file of parseDiff(patch).files) {
    for (const hunk of file.hunks) {
      for (const l of hunk.lines) {
        if (l.kind === 'removed') {
          if (file.old !== null && l.oldNo !== null) {
            rows[l.index] = { side: 'old', path: file.old, line: l.oldNo };
          }
        } else if (file.new !== null && l.newNo !== null) {
          rows[l.index] = { side: 'new', path: file.new, line: l.newNo };
        }
      }
    }
  }
  return rows;
}
