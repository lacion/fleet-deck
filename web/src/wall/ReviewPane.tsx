// R13 P1: the review mode: the whole-session diff with its sent review comments, read-only.
import type { DeckSession } from '../gen';
import { useReviewActions } from '../review/context';
import { useReview } from '../review/useReview';
import type { SessionActions } from '../session/commands';
import { DiffPane } from './DiffPane';
import { keyOf } from './hooks';

export function ReviewPane({
  card,
  actions,
  live,
}: {
  readonly card: DeckSession;
  readonly actions: SessionActions | undefined;
  readonly live: boolean;
}) {
  const reviewActions = useReviewActions();
  const state = useReview(keyOf(card), card.review_rev, live);
  return (
    <DiffPane
      card={card}
      actions={actions}
      review={reviewActions === undefined ? undefined : { state, actions: reviewActions }}
      title="Review"
    />
  );
}
