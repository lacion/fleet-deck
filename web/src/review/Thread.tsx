// DL-179: one thread of review comments: the top comment and its replies, with the buttons that
// the caller may use.
import { useEffect, useRef, useState } from 'react';
import { Markdown } from '../chat/Markdown';
import type { ReviewComment, ReviewThread } from '../gen';
import { CommentEditor, failureText } from './CommentEditor';
import { authorLabel, isOwn, resolvedLine } from './diffThreads';
import type { DiffReview } from './types';

type Env = Pick<DiffReview, 'canWrite' | 'self' | 'onReply' | 'onEdit' | 'onResolve' | 'onDelete'>;

function CommentView({ comment, env }: { readonly comment: ReviewComment; readonly env: Env }) {
  const [editing, setEditing] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const own = isOwn(comment, env.self);
  const ownDraft = own && comment.state === 'draft';
  return (
    <div className="fd-comment" data-comment-id={comment.short_id} data-state={comment.state}>
      <p className="fd-comment-head">
        <strong>{authorLabel(comment, env.self)}</strong>
        <time dateTime={new Date(comment.updated_at).toISOString()}>
          {new Date(comment.updated_at).toLocaleString()}
        </time>
        <code>{comment.short_id}</code>
        {comment.state === 'draft' ? (
          <span className="fd-badge" data-tone="muted">
            Draft
          </span>
        ) : null}
      </p>
      {editing ? (
        <CommentEditor
          initial={comment.body}
          submitLabel="Save"
          onSubmit={(body) => env.onEdit(comment.comment_id, body)}
          onClose={() => setEditing(false)}
        />
      ) : (
        <div className="fd-comment-body">
          <Markdown text={comment.body} />
        </div>
      )}
      {env.canWrite && ownDraft && !editing ? (
        <div className="fd-comment-actions">
          <button type="button" className="fd-sm" onClick={() => setEditing(true)}>
            Edit
          </button>
          <button
            type="button"
            className="fd-sm"
            disabled={removing}
            onClick={() => {
              setRemoving(true);
              setProblem(null);
              env.onDelete(comment.comment_id).then(
                () => setRemoving(false),
                (e: unknown) => {
                  setProblem(failureText(e));
                  setRemoving(false);
                },
              );
            }}
          >
            Delete
          </button>
        </div>
      ) : null}
      {problem !== null ? (
        <p role="alert" className="fd-cedit-alert">
          {problem}
        </p>
      ) : null}
    </div>
  );
}

/**
 * A thread. A resolved thread is collapsed to one line with a "Show"/"Hide" toggle. With `anchor`
 * the lines that the comment sat on show above it (the lists of outdated, gone and cut threads).
 */
export function Thread({
  thread,
  env,
  focused,
  anchor,
}: {
  readonly thread: ReviewThread;
  readonly env: Env;
  readonly focused: boolean;
  readonly anchor: boolean;
}) {
  const first = thread.comments[0];
  const [replying, setReplying] = useState(false);
  const [open, setOpen] = useState(focused);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (focused) root.current?.scrollIntoView?.({ block: 'center' });
  }, [focused]);
  if (first === undefined) return null;
  const resolved = first.resolved;
  const canResolve =
    env.canWrite && (first.state === 'sent' || (first.state === 'draft' && isOwn(first, env.self)));
  const collapsed = resolved && !open;
  const mark = focused ? { 'data-focus': 'true' } : {};
  if (collapsed) {
    return (
      <div
        className="fd-thread"
        data-thread={thread.thread_id}
        data-resolved="true"
        ref={root}
        {...mark}
      >
        <p className="fd-thread-resolved">
          {resolvedLine(first.short_id, thread.comments.length)}{' '}
          <button
            type="button"
            className="fd-sm"
            aria-expanded={false}
            onClick={() => setOpen(true)}
          >
            Show
          </button>
        </p>
      </div>
    );
  }
  return (
    <div
      className="fd-thread"
      data-thread={thread.thread_id}
      data-resolved={resolved ? 'true' : undefined}
      ref={root}
      {...mark}
    >
      {resolved ? (
        <p className="fd-thread-resolved">
          {resolvedLine(first.short_id, thread.comments.length)}{' '}
          <button
            type="button"
            className="fd-sm"
            aria-expanded={true}
            onClick={() => setOpen(false)}
          >
            Hide
          </button>
        </p>
      ) : null}
      {anchor ? (
        <>
          {first.anchor_kind === 'imported' ? (
            <p className="fd-thread-imported">Imported note</p>
          ) : null}
          <p className="fd-thread-where">
            <code>
              {first.path}:{first.line_start}
              {first.line_end === first.line_start ? '' : `-${first.line_end}`}
            </code>
            {first.side === 'old' ? ' (old side)' : ''}
          </p>
          {first.anchor_text !== '' ? (
            <pre className="fd-thread-anchor">{first.anchor_text}</pre>
          ) : null}
        </>
      ) : null}
      {thread.comments.map((c) => (
        <CommentView key={c.comment_id} comment={c} env={env} />
      ))}
      {problem !== null ? (
        <p role="alert" className="fd-cedit-alert">
          {problem}
        </p>
      ) : null}
      {replying ? (
        <CommentEditor
          initial=""
          submitLabel="Add comment"
          onSubmit={(body) => env.onReply(thread.thread_id, body)}
          onClose={() => setReplying(false)}
        />
      ) : (
        <div className="fd-comment-actions">
          {env.canWrite ? (
            <button type="button" className="fd-sm" onClick={() => setReplying(true)}>
              Reply
            </button>
          ) : null}
          {canResolve ? (
            <button
              type="button"
              className="fd-sm"
              disabled={busy}
              onClick={() => {
                setBusy(true);
                setProblem(null);
                env.onResolve(first.comment_id, !resolved).then(
                  () => setBusy(false),
                  (e: unknown) => {
                    setProblem(failureText(e));
                    setBusy(false);
                  },
                );
              }}
            >
              {resolved ? 'Unresolve' : 'Resolve'}
            </button>
          ) : null}
        </div>
      )}
    </div>
  );
}
