// DL-179: the props that the review adds to a rendered diff.
import type { ReviewSide, ReviewThread } from '../gen';

/** The place of a new comment. */
export type CommentSpot = {
  readonly side: ReviewSide;
  readonly path: string;
  readonly line_start: number;
  readonly line_end: number;
};

/**
 * The comments of one rendered diff. `snapshot` is the identity of the diff (`spawn:<oid>` or
 * `turn:<from>..<to>`); the calls reject with the error of the deck, whose message the editor shows.
 */
export type DiffReview = {
  readonly snapshot: string;
  readonly threads: readonly ReviewThread[];
  readonly canWrite: boolean;
  /** `'owner'` for the owner, else the member name. */
  readonly self: 'owner' | string;
  /** The `thread_id` to scroll to and mark. */
  readonly focus: string | null;
  onAdd(spot: CommentSpot, body: string): Promise<void>;
  onReply(threadId: string, body: string): Promise<void>;
  onEdit(commentId: string, body: string): Promise<void>;
  onResolve(commentId: string, resolved: boolean): Promise<void>;
  onDelete(commentId: string): Promise<void>;
};
