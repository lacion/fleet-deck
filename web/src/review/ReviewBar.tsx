// DL-179: the review bar at the bottom of the Changes tab: "Review: n drafts", Send review and
// Discard all, and the reviews whose delivery failed or is unknown.
import { useRef, useState } from 'react';
import type { SessionKey } from '../gen';
import type { ReviewActions } from './api';
import { failureText } from './CommentEditor';
import {
  canSendAgain,
  confirmText,
  discardDrafts,
  draftCount,
  draftsText,
  type KeyHolder,
  type KeyMap,
  keyHolder,
  keyMap,
  sendDrafts,
  sendLabel,
  troubled,
  troubleText,
} from './send';
import type { ReviewState } from './useReview';

export function ReviewBar({
  state,
  actions,
  session,
  canWrite,
  self,
}: {
  readonly state: ReviewState;
  readonly actions: ReviewActions;
  readonly session: SessionKey;
  readonly canWrite: boolean;
  /** `'owner'` or the member name. */
  readonly self: string;
}) {
  const holder = useRef<KeyHolder | null>(null);
  const attempts = useRef<KeyMap | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  if (!canWrite) return null;
  const threads = state.data?.threads ?? [];
  const count = draftCount(threads);
  const trouble = troubled(state.data?.reviews ?? [], self);
  if (count === 0 && trouble.length === 0) return null;
  holder.current ??= keyHolder();
  attempts.current ??= keyMap();
  const keys = holder.current;
  const again = attempts.current;
  const run = (call: () => Promise<unknown>) => {
    setBusy(true);
    setProblem(null);
    call().then(
      () => {
        setBusy(false);
        setConfirm(false);
        state.reload();
      },
      (e: unknown) => {
        setBusy(false);
        setProblem(failureText(e));
        state.reload();
      },
    );
  };
  return (
    <div className="fd-review-bar" role="region" aria-label="Review">
      {count > 0 ? (
        <div className="fd-review-row">
          {confirm ? (
            <>
              <span>{confirmText(count)}</span>
              <button
                type="button"
                className="fd-sm fd-danger"
                disabled={busy}
                onClick={() => run(() => discardDrafts(actions, session, threads))}
              >
                Delete
              </button>
              <button
                type="button"
                className="fd-sm"
                disabled={busy}
                onClick={() => setConfirm(false)}
              >
                Cancel
              </button>
            </>
          ) : (
            <>
              <span>{draftsText(count)}</span>
              <button
                type="button"
                className="fd-primary fd-sm"
                disabled={busy}
                onClick={() => run(() => sendDrafts(actions, session, keys, threads))}
              >
                {sendLabel(count)}
              </button>
              <button
                type="button"
                className="fd-sm"
                disabled={busy}
                onClick={() => setConfirm(true)}
              >
                Discard all
              </button>
            </>
          )}
        </div>
      ) : null}
      {problem !== null ? (
        <p role="alert" className="fd-review-alert">
          {problem}
        </p>
      ) : null}
      {trouble.length > 0 ? (
        <ul className="fd-review-trouble">
          {trouble.map((r) => (
            <li key={r.review_id}>
              <span>{troubleText(r)}</span>
              {canSendAgain(r) ? (
                <button
                  type="button"
                  className="fd-sm"
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      await actions.sendAgain(session, r.review_id, {
                        attempt_key: again.get(r.review_id),
                      });
                      again.done(r.review_id);
                    })
                  }
                >
                  Send again
                </button>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
