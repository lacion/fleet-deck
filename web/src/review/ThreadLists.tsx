// DL-179: the threads that sit on no line of the rendered diff: outdated, not in this diff, or
// past the cut.
import type { ReviewThread } from '../gen';
import type { Arranged } from './diffThreads';
import { Thread } from './Thread';
import type { DiffReview } from './types';

export function ThreadLists({
  arranged,
  review,
}: {
  readonly arranged: Arranged;
  readonly review: DiffReview;
}) {
  const list = (threads: readonly ReviewThread[]) =>
    threads.map((t) => (
      <Thread
        key={t.thread_id}
        thread={t}
        env={review}
        focused={review.focus === t.thread_id}
        anchor
      />
    ));
  return (
    <>
      {arranged.outdated.size > 0 ? (
        <section className="fd-thread-list" aria-label="Outdated">
          <h4 className="fd-label">Outdated</h4>
          {[...arranged.outdated].map(([path, threads]) => (
            <div key={path} className="fd-thread-file">
              <h5>
                <code>{path}</code>
              </h5>
              {list(threads)}
            </div>
          ))}
        </section>
      ) : null}
      {arranged.gone.length > 0 ? (
        <section className="fd-thread-list" aria-label="Comments not in this diff">
          <h4 className="fd-label">Comments not in this diff</h4>
          {list(arranged.gone)}
        </section>
      ) : null}
      {arranged.cut.length > 0 ? (
        <section className="fd-thread-list" aria-label="Not shown: the diff is cut">
          <h4 className="fd-label">Not shown: the diff is cut</h4>
          {list(arranged.cut)}
        </section>
      ) : null}
    </>
  );
}
