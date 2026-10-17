// DL-179: the comment props of one rendered diff, built from the review state of the session.
// Every mutation calls the deck and then reloads the review state; an error goes to the editor.
import type { SessionKey } from '../gen';
import type { ReviewActions } from './api';
import { newCommentBody, replyBody } from './diffThreads';
import type { DiffReview } from './types';
import type { ReviewState } from './useReview';

/** What the Changes tab needs of the review state of its session. */
export type ReviewProps = {
  readonly state: ReviewState;
  readonly actions: ReviewActions;
  readonly key: SessionKey;
  readonly canWrite: boolean;
  /** `'owner'` or the member name. */
  readonly self: string;
  readonly focus: { readonly threadId: string; readonly nonce: number } | null;
};

/** The comment props of the diff of `snapshot`. */
export function diffReview(review: ReviewProps, snapshot: string): DiffReview {
  const { state, actions, key } = review;
  const threads = state.data?.threads ?? [];
  const done = async (call: Promise<unknown>): Promise<void> => {
    await call;
    state.reload();
  };
  return {
    snapshot,
    threads,
    canWrite: review.canWrite,
    self: review.self,
    focus: review.focus?.threadId ?? null,
    onAdd: (spot, body) => done(actions.add(key, newCommentBody(snapshot, spot, body))),
    onReply: (threadId, body) => {
      const first = threads.find((t) => t.thread_id === threadId)?.comments[0];
      if (first === undefined) return Promise.reject(new Error('The thread is not on the board.'));
      return done(actions.add(key, replyBody(first, threadId, body)));
    },
    onEdit: (commentId, body) => done(actions.edit(key, commentId, { body })),
    onResolve: (commentId, resolved) => done(actions.edit(key, commentId, { resolved })),
    onDelete: (commentId) => done(actions.remove(key, commentId)),
  };
}
