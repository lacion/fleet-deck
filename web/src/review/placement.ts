// DL-179 (design 5): where a thread shows in one rendered diff.
import type { ReviewComment, ReviewSide } from '../gen';
import { anchorText, diffPaths, type ParsedDiff } from './diffModel';

/** `spawn:<oid>` or `turn:<from>..<to>`: the identity of the diff that a comment was written on. */
export type Snapshot = string;

export const spawnSnapshot = (baseOid: string): Snapshot => `spawn:${baseOid}`;
export const turnSnapshot = (from: string, to: string): Snapshot => `turn:${from}..${to}`;

export type Placement =
  | {
      readonly kind: 'line';
      readonly side: ReviewSide;
      readonly path: string;
      readonly line: number;
    }
  | { readonly kind: 'outdated'; readonly path: string }
  | { readonly kind: 'gone' }
  | { readonly kind: 'cut' };

/** The diff that is on the screen. */
export type PlacementView = {
  readonly snapshot: Snapshot;
  readonly parsed: ParsedDiff;
  readonly truncated: boolean;
};

/**
 * The place of the thread of `top` in `view`; `null`: the thread does not belong to this view at
 * all (a comment of another turn, or a `spawn:` comment in a turn view).
 */
export function place(top: ReviewComment, view: PlacementView): Placement | null {
  const turnComment = top.snapshot.startsWith('turn:');
  const turnView = view.snapshot.startsWith('turn:');
  if (turnComment && top.snapshot !== view.snapshot) return null;
  if (turnView && !turnComment) return null;
  const paths = diffPaths(view.parsed);
  const found = anchorText(view.parsed, top.side, top.path, top.line_start, top.line_end);
  // An imported note has no anchor text to compare: it is never on a line, and a cut diff does
  // not make it "cut".
  if (top.anchor_kind === 'imported') {
    return paths.has(top.path) ? { kind: 'outdated', path: top.path } : { kind: 'gone' };
  }
  const line: Placement = { kind: 'line', side: top.side, path: top.path, line: top.line_end };
  // The identity of a turn never changes: the lines are where they were.
  if (turnComment && found !== null) return line;
  if (
    !turnComment &&
    top.snapshot === view.snapshot &&
    top.anchor_kind === 'diff' &&
    found === top.anchor_text
  ) {
    return line;
  }
  if (view.truncated && (!paths.has(top.path) || found === null)) return { kind: 'cut' };
  return paths.has(top.path) ? { kind: 'outdated', path: top.path } : { kind: 'gone' };
}
