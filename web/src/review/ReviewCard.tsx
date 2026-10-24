// DL-179: the "Review" card that the chat shows for the follow-up of a sent review.
import type { ReviewComment, ReviewSummary } from '../gen';
import { cutText, firstLine, lineRange } from './text';

/** The words of the heading: "3 comments", "1 comment". */
export const commentCount = (n: number): string => `${n} ${n === 1 ? 'comment' : 'comments'}`;

export function ReviewCard({
  review,
  text,
  byShort,
  onOpen,
}: {
  readonly review: ReviewSummary;
  /** The follow-up text that the agent received. */
  readonly text: string;
  readonly byShort: ReadonlyMap<string, ReviewComment>;
  readonly onOpen: (shortId: string) => void;
}) {
  const rows = review.comment_ids
    .map((id) => byShort.get(id))
    .filter((c): c is ReviewComment => c !== undefined);
  return (
    <div className="fd-review-card" data-review={review.short_id}>
      <h3 className="fd-review-card-head">
        Review {review.short_id}{' '}
        <span className="fd-review-card-count">{commentCount(review.comment_ids.length)}</span>
      </h3>
      {rows.length > 0 ? (
        <ul>
          {rows.map((c) => (
            <li key={c.short_id}>
              <button type="button" className="fd-review-crow" onClick={() => onOpen(c.short_id)}>
                <code>{c.short_id}</code>{' '}
                <span>
                  {c.path}:{lineRange(c)}
                </span>{' '}
                <span className="fd-review-first">{cutText(firstLine(c.body), 80)}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      <details>
        <summary>Show the text that the agent received</summary>
        <pre className="fd-review-text">{text}</pre>
      </details>
    </div>
  );
}
