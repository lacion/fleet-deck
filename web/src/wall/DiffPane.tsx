// R13 P1: the read-only diff of the focus and review modes: the working tree of a session against
// its base commit. The review mode adds the sent review comments; no comment control shows.
import { Diff } from '../chat/Diff';
import type { DeckSession } from '../gen';
import type { ReviewActions } from '../review/api';
import { diffReview } from '../review/diffReview';
import { spawnSnapshot } from '../review/placement';
import type { ReviewState } from '../review/useReview';
import { hasChanges } from '../session/changes';
import type { SessionActions } from '../session/commands';
import { SinceNotes } from '../session/TurnsPanel';
import { refOf } from '../watch/tiles';
import { keyOf, useSessionDiff } from './hooks';

/**
 * The review state with the threads that were sent and are open: a draft is the owner's note, and
 * a resolved thread folds behind a button that the wall does not have.
 */
function sentOnly(state: ReviewState): ReviewState {
  if (state.data === null) return state;
  return {
    ...state,
    data: {
      ...state.data,
      threads: state.data.threads.filter(
        (t) => t.comments[0]?.state !== 'draft' && t.comments[0]?.resolved !== true,
      ),
    },
  };
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
            <Diff
              patch={diff.patch}
              truncated={diff.truncated}
              id={review === undefined ? 'wall-focus' : 'wall-review'}
              review={props}
            />
          </div>
        </>
      ) : null}
    </section>
  );
}
