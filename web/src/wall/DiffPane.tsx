// R13 P1: the read-only diff of the focus and review modes: the working tree of a session against
// its base commit. The review mode adds the sent review comments; no comment control shows.
import { useMemo } from 'react';
import { Diff } from '../chat/Diff';
import { diffLineKind, fileHeads, isPlumbing } from '../chat/diffLines';
import type { DeckSession } from '../gen';
import type { ReviewActions } from '../review/api';
import { diffReview } from '../review/diffReview';
import { spawnSnapshot } from '../review/placement';
import type { ReviewState } from '../review/useReview';
import { hasChanges } from '../session/changes';
import type { SessionActions } from '../session/commands';
import { SinceNotes } from '../session/TurnsPanel';
import { refOf } from '../watch/tiles';
import { FileText, Icon } from '../ui/icons';
import { keyOf, useSessionDiff } from './hooks';
import { sentOnly } from './model';

/** A path as its folders (they give way to an ellipsis first) and its file name, as in `Diff`. */
function PathText({ path }: { readonly path: string }) {
  const cut = path.lastIndexOf('/') + 1;
  return (
    <>
      {cut > 0 ? <span className="fd-dl-dir">{path.slice(0, cut)}</span> : null}
      <span className="fd-dl-base">{path.slice(cut)}</span>
    </>
  );
}

/**
 * The read-only diff of the focus mode: the lines of `Diff` with the same classes. A file starts
 * with the file header row of `Diff` (icon, path, +/− counts), and the git plumbing lines
 * (`--- a/…`, `+++ b/…`, `index …`) are left out: the header row says the same (DESIGN-SYSTEM §3).
 */
function WallDiff({ patch, truncated }: { readonly patch: string; readonly truncated: boolean }) {
  const lines = useMemo(() => patch.split('\n'), [patch]);
  const heads = useMemo(() => fileHeads(lines), [lines]);
  return (
    <pre className="fd-diff" data-diff="wall-focus">
      {lines.map((line, n) => {
        const kind = diffLineKind(line);
        const head = kind === 'file' ? heads.get(n) : undefined;
        if (head !== undefined) {
          return (
            <span key={n} className="fd-dl" data-kind="file">
              <Icon of={FileText} size={14} />
              <span className="fd-dl-path">
                <PathText path={head.path} />
              </span>
              {head.add > 0 || head.del > 0 ? (
                <span className="fd-dl-stat">
                  {head.add > 0 ? <span data-sign="add">{`+${head.add}`}</span> : null}
                  {head.del > 0 ? <span data-sign="del">{`\u2212${head.del}`}</span> : null}
                </span>
              ) : null}
            </span>
          );
        }
        if (kind === 'meta' && isPlumbing(line)) return null;
        return (
          <span key={n} className="fd-dl" data-kind={kind}>
            {line === '' ? ' ' : line}
          </span>
        );
      })}
      {truncated ? (
        <span className="fd-dl" data-kind="meta">
          … (the diff is cut at the size limit)
        </span>
      ) : null}
    </pre>
  );
}

export function DiffPane({
  card,
  actions,
  review,
  title,
}: {
  readonly card: DeckSession;
  readonly actions: SessionActions | undefined;
  /** The review state of the session; set only in the review mode. */
  readonly review?: { readonly state: ReviewState; readonly actions: ReviewActions } | undefined;
  readonly title: string;
}) {
  const state = useSessionDiff(actions?.files ?? null, refOf(card));
  const { diff, error } = {
    diff: state.kind === 'ready' ? state.diff : null,
    error: state.kind === 'error' ? state.text : null,
  };
  const base = card.summary.base;
  const props =
    review === undefined
      ? undefined
      : diffReview(
          {
            state: sentOnly(review.state),
            actions: review.actions,
            key: keyOf(card),
            canWrite: false,
            self: 'owner',
            focus: null,
          },
          spawnSnapshot(diff?.base_oid ?? ''),
        );
  return (
    <section className="fd-wall-diff" aria-label={title}>
      <h2 className="fd-wall-h">{title}</h2>
      <p className="fd-wall-note">
        Since the session started
        {base ? ` from ${base.git_ref ?? 'a commit'} ${base.oid.slice(0, 8)}` : ''}.
      </p>
      <SinceNotes base={base} scope={diff?.scope} />
      {error !== null ? <p className="fd-wall-note">{error}</p> : null}
      {error === null && diff === null ? <p className="fd-wall-note">Loading the diff…</p> : null}
      {diff !== null && !hasChanges(diff.summary) && diff.patch.trim() === '' ? (
        <p className="fd-wall-note">No file changes since the session started.</p>
      ) : null}
      {diff !== null && diff.patch.trim() !== '' ? (
        <>
          <p className="fd-turn-stat">
            {diff.summary.files_changed} {diff.summary.files_changed === 1 ? 'file' : 'files'}
            {diff.summary.insertions > 0 ? (
              <>
                {' '}
                <span className="fd-add">+{diff.summary.insertions}</span>
              </>
            ) : null}
            {diff.summary.deletions > 0 ? (
              <>
                {' '}
                <span className="fd-del">−{diff.summary.deletions}</span>
              </>
            ) : null}
          </p>
          <div className="fd-wall-scroll">
            {review === undefined ? (
              <WallDiff patch={diff.patch} truncated={diff.truncated} />
            ) : (
              <Diff patch={diff.patch} truncated={diff.truncated} id="wall-review" review={props} />
            )}
          </div>
        </>
      ) : null}
    </section>
  );
}
