// DL-179: which thread shows where in one rendered diff, and the request bodies of the board.
import type { NewComment, ReviewComment, ReviewSide, ReviewThread } from '../gen';
import { type DiffRow, diffRows, parseDiff } from './diffModel';
import { place } from './placement';
import type { CommentSpot } from './types';

/** The threads of one rendered diff, by place. */
export type Arranged = {
  readonly rows: readonly DiffRow[];
  /** The threads under the row (the index of a line of the patch) of their last line. */
  readonly under: ReadonlyMap<number, readonly ReviewThread[]>;
  /** "Outdated": by file path. */
  readonly outdated: ReadonlyMap<string, readonly ReviewThread[]>;
  /** "Comments not in this diff". */
  readonly gone: readonly ReviewThread[];
  /** "Not shown: the diff is cut". */
  readonly cut: readonly ReviewThread[];
};

const rowKey = (side: ReviewSide, path: string, line: number): string =>
  `${side}\u0000${path}\u0000${line}`;

const top = (t: ReviewThread): ReviewComment | undefined => t.comments[0];

/** The index of the patch line of `side` `path` `line`, or `undefined`. */
export function rowIndex(
  rows: readonly DiffRow[],
  side: ReviewSide,
  path: string,
  line: number,
): number | undefined {
  const at = rows.findIndex(
    (r) => r !== null && r.side === side && r.path === path && r.line === line,
  );
  return at < 0 ? undefined : at;
}

/**
 * Puts each thread in its place for the diff `patch` of `snapshot`. A thread of another view is
 * left out. A thread with a `line` place whose row the patch does not have is "outdated".
 */
export function arrange(
  patch: string,
  truncated: boolean,
  snapshot: string,
  threads: readonly ReviewThread[],
): Arranged {
  const parsed = parseDiff(patch);
  const rows = diffRows(patch);
  const index = new Map<string, number>();
  rows.forEach((r, n) => {
    if (r === null) return;
    const k = rowKey(r.side, r.path, r.line);
    if (!index.has(k)) index.set(k, n);
  });
  const under = new Map<number, ReviewThread[]>();
  const outdated = new Map<string, ReviewThread[]>();
  const gone: ReviewThread[] = [];
  const cut: ReviewThread[] = [];
  const ordered = [...threads].sort(
    (a, b) => (top(a)?.created_at ?? 0) - (top(b)?.created_at ?? 0),
  );
  for (const thread of ordered) {
    const first = top(thread);
    if (first === undefined) continue;
    const placed = place(first, { snapshot, parsed, truncated });
    if (placed === null) continue;
    if (placed.kind === 'line') {
      const at = index.get(rowKey(placed.side, placed.path, placed.line));
      if (at !== undefined) {
        under.set(at, [...(under.get(at) ?? []), thread]);
        continue;
      }
      outdated.set(placed.path, [...(outdated.get(placed.path) ?? []), thread]);
    } else if (placed.kind === 'outdated') {
      outdated.set(placed.path, [...(outdated.get(placed.path) ?? []), thread]);
    } else if (placed.kind === 'gone') {
      gone.push(thread);
    } else {
      cut.push(thread);
    }
  }
  return { rows, under, outdated, gone, cut };
}

/** The body of a new comment: these fields and no other (the deck refuses unknown fields). */
export function newCommentBody(snapshot: string, spot: CommentSpot, body: string): NewComment {
  return {
    snapshot,
    path: spot.path,
    side: spot.side,
    line_start: spot.line_start,
    line_end: spot.line_end,
    body,
  };
}

/** The body of a reply: the anchor fields of the top comment and its `thread_id`. */
export function replyBody(first: ReviewComment, threadId: string, body: string): NewComment {
  return {
    snapshot: first.snapshot,
    path: first.path,
    side: first.side,
    line_start: first.line_start,
    line_end: first.line_end,
    body,
    thread_id: threadId,
  };
}

/** Whether the comment was written by the caller (`self` is `'owner'` or the member name). */
export function isOwn(c: ReviewComment, self: string): boolean {
  return (c.author_kind === 'owner' && self === 'owner') || c.author_name === self;
}

/** "You", "Owner" or the member name. */
export function authorLabel(c: ReviewComment, self: string): string {
  if (isOwn(c, self)) return 'You';
  return c.author_kind === 'owner' ? 'Owner' : (c.author_name ?? 'Member');
}

/** "Resolved thread c-xxxxxx · 2 comments". */
export function resolvedLine(shortId: string, count: number): string {
  return `Resolved thread ${shortId} · ${count} ${count === 1 ? 'comment' : 'comments'}`;
}
