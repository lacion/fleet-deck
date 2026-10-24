// R3.9 (DL-164): the dialogs of the issue and pull request writes. Each one asks for a confirm
// step in `WriteDialog`; the write goes out once, with `confirm: true`.
import { useState } from 'react';
import type { ForgeKind, Project, ReviewEvent } from '../gen';
import type { IntegrationActions } from './actions';
import { systemLabel } from './model';
import { WriteDialog } from './WriteDialog';

type Common = {
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly onClose: () => void;
};

const Quote = ({ text }: { readonly text: string }) => <pre className="fd-int-quote">{text}</pre>;

/** "Comment…" on an issue. */
export function IssueCommentDialog({
  project,
  actions,
  issue,
  onClose,
}: Common & { readonly issue: { readonly key: string; readonly system: string } }) {
  const [body, setBody] = useState('');
  const system = systemLabel(issue.system);
  return (
    <WriteDialog
      title={`Comment on ${issue.key}`}
      question={body.trim() === '' ? null : `Post this comment to ${system} as you?`}
      preview={<Quote text={body} />}
      write={(id) =>
        actions.commentIssue(project.project_id, issue.key, {
          command_id: id,
          confirm: true,
          body,
        })
      }
      onClose={onClose}
    >
      <label>
        Comment
        <textarea rows={6} value={body} onChange={(e) => setBody(e.target.value)} />
      </label>
    </WriteDialog>
  );
}

/** "Move to…" on an issue: a status name. */
export function IssueMoveDialog({
  project,
  actions,
  issue,
  onClose,
}: Common & { readonly issue: { readonly key: string; readonly system: string } }) {
  const [to, setTo] = useState('');
  const system = systemLabel(issue.system);
  return (
    <WriteDialog
      title={`Move ${issue.key}`}
      question={
        to.trim() === '' ? null : `Move ${issue.key} to “${to.trim()}” in ${system} as you?`
      }
      preview={<Quote text={to.trim()} />}
      write={(id) =>
        actions.moveIssue(project.project_id, issue.key, {
          command_id: id,
          confirm: true,
          to: to.trim(),
        })
      }
      onClose={onClose}
    >
      <label>
        Move to (a status name)
        <input value={to} autoComplete="off" onChange={(e) => setTo(e.target.value)} />
      </label>
    </WriteDialog>
  );
}

/** "Comment…" on a pull request. */
export function PrCommentDialog({
  project,
  actions,
  number,
  forge,
  onClose,
}: Common & { readonly number: number; readonly forge: ForgeKind }) {
  const [body, setBody] = useState('');
  return (
    <WriteDialog
      title={`Comment on #${number}`}
      question={body.trim() === '' ? null : `Post this comment to ${systemLabel(forge)} as you?`}
      preview={<Quote text={body} />}
      write={(id) =>
        actions.commentPr(project.project_id, number, { command_id: id, confirm: true, body })
      }
      onClose={onClose}
    >
      <label>
        Comment
        <textarea rows={6} value={body} onChange={(e) => setBody(e.target.value)} />
      </label>
    </WriteDialog>
  );
}

export const REVIEW_EVENTS: readonly { readonly value: ReviewEvent; readonly label: string }[] = [
  { value: 'approve', label: 'Approve' },
  { value: 'request_changes', label: 'Request changes' },
  { value: 'comment', label: 'Comment' },
];

/** "Review…" of a pull request: approve, request changes, or comment, with a text. */
export function PrReviewDialog({
  project,
  actions,
  number,
  forge,
  onClose,
}: Common & { readonly number: number; readonly forge: ForgeKind }) {
  const [event, setEvent] = useState<ReviewEvent>('comment');
  const [body, setBody] = useState('');
  const verdict = REVIEW_EVENTS.find((e) => e.value === event)?.label.toLowerCase() ?? event;
  const ready = event === 'approve' || body.trim() !== '';
  return (
    <WriteDialog
      title={`Review #${number}`}
      question={ready ? `Submit this review (${verdict}) to ${systemLabel(forge)} as you?` : null}
      preview={<Quote text={body} />}
      write={(id) =>
        actions.reviewPr(project.project_id, number, { command_id: id, confirm: true, event, body })
      }
      onClose={onClose}
    >
      <label>
        Verdict
        <select value={event} onChange={(e) => setEvent(e.target.value as ReviewEvent)}>
          {REVIEW_EVENTS.map((e) => (
            <option key={e.value} value={e.value}>
              {e.label}
            </option>
          ))}
        </select>
      </label>
      <label>
        Review text
        <textarea rows={6} value={body} onChange={(e) => setBody(e.target.value)} />
      </label>
    </WriteDialog>
  );
}
