// R3.3 (DL-164): the preview of one issue: title, status, priority, labels, estimate, the body,
// the comments and the linked pull requests. Text from the system is shown with a note.
import { useState } from 'react';
import type { IssueDetail, Project, StationView } from '../gen';
import { relativeAge } from '../app/sessions';
import { Icon, X } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import type { IntegrationActions } from './actions';
import { Images, OutsideText } from './Outside';
import { IssueCommentDialog, IssueMoveDialog } from './Writes';
import { lastSegment, outsideNote, systemLabel, webUrl } from './model';
import { type Load, useLoad } from './useLoad';

type Props = {
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly stations: readonly StationView[];
  readonly keyOf: string;
  readonly source: string | undefined;
  readonly now: number;
  readonly onClose: () => void;
  readonly onStart: (key: string) => void;
  /** The detail at the first render (tests). */
  readonly initial?: IssueDetail;
};

export function IssuePreview({
  project,
  actions,
  keyOf,
  source,
  now,
  onClose,
  onStart,
  initial,
}: Props) {
  const { state, reload } = useLoad<IssueDetail>(
    () => actions.issue(project.project_id, keyOf, source),
    `${project.project_id}|${keyOf}|${source ?? ''}`,
    initial,
  );
  const [writing, setWriting] = useState<'comment' | 'move' | null>(null);
  return (
    <aside className="fd-int-preview" aria-label={`Issue ${keyOf}`}>
      <div className="fd-int-preview-head">
        <button type="button" className="fd-ghost fd-sm" onClick={onClose}>
          <Icon of={X} size={14} /> Close
        </button>
      </div>
      <IssueBody
        state={state}
        project={project}
        actions={actions}
        source={source}
        now={now}
        keyOf={keyOf}
        onRetry={reload}
      />
      {state.kind === 'ok' && actions.canDrive ? (
        <div className="fd-plan-actions">
          <button type="button" className="fd-primary" onClick={() => onStart(keyOf)}>
            Start a session
          </button>
          <button type="button" onClick={() => setWriting('comment')}>
            Comment…
          </button>
          <button type="button" onClick={() => setWriting('move')}>
            Move to…
          </button>
        </div>
      ) : null}
      {state.kind === 'ok' && writing === 'comment' ? (
        <IssueCommentDialog
          project={project}
          actions={actions}
          issue={state.value.summary.ref}
          onClose={() => {
            setWriting(null);
            reload();
          }}
        />
      ) : null}
      {state.kind === 'ok' && writing === 'move' ? (
        <IssueMoveDialog
          project={project}
          actions={actions}
          issue={state.value.summary.ref}
          onClose={() => {
            setWriting(null);
            reload();
          }}
        />
      ) : null}
    </aside>
  );
}

/** The content of the preview for one state of the read. */
export function IssueBody({
  state,
  project,
  actions,
  source,
  now,
  keyOf,
  onRetry,
}: {
  readonly state: Load<IssueDetail>;
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly source: string | undefined;
  readonly now: number;
  readonly keyOf: string;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') return <Loading text={`Loading ${keyOf}…`} />;
  if (state.kind === 'error') return <ErrorNotice text={state.text} onRetry={onRetry} />;
  const d = state.value;
  const ref = d.summary.ref;
  const link = webUrl(ref.url);
  return (
    <div className="fd-int-detail">
      <h3>
        <span className="fd-int-key">{ref.key}</span>{' '}
        {link === null ? (
          ref.title
        ) : (
          <a href={link} target="_blank" rel="noopener noreferrer">
            {ref.title}
          </a>
        )}
      </h3>
      <p className="fd-plan-meta">
        <span className="fd-badge" data-tone="info">
          {d.summary.status}
        </span>
        {d.summary.priority === null ? null : <> priority {d.summary.priority}</>}
        {d.estimate === null ? null : <> · estimate {d.estimate}</>}
        {d.summary.assignee === null ? null : <> · {d.summary.assignee}</>}
      </p>
      {d.summary.labels.length === 0 ? null : (
        <p className="fd-int-chips">
          {d.summary.labels.map((l) => (
            <span key={l} className="fd-badge" data-tone="muted">
              {l}
            </span>
          ))}
        </p>
      )}
      <OutsideText system={ref.system} text={d.body_markdown} />
      <Images
        actions={actions}
        project={project.project_id}
        source={source}
        urls={d.images}
        body={d.body_markdown}
      />
      {d.linked_prs.length === 0 ? null : (
        <section aria-label="Linked pull requests">
          <h4>Linked pull requests</h4>
          <ul className="fd-int-links">
            {d.linked_prs.map((url) => {
              const href = webUrl(url);
              return (
                <li key={url}>
                  {href === null ? (
                    url
                  ) : (
                    <a href={href} target="_blank" rel="noopener noreferrer">
                      {lastSegment(url)}
                    </a>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      )}
      <section aria-label="Comments">
        <h4>Comments ({d.comments.length})</h4>
        {d.comments.length === 0 ? (
          <p className="fd-empty">No comments.</p>
        ) : (
          <p className="fd-note fd-int-outside-note">{outsideNote(ref.system)}</p>
        )}
        <ol className="fd-int-comments">
          {d.comments.map((c, i) => (
            <li key={i}>
              <p className="fd-plan-meta">
                {c.author} · {relativeAge(c.at, now)}
              </p>
              <OutsideText system={ref.system} text={c.body_markdown} note={false} />
            </li>
          ))}
        </ol>
      </section>
      <p className="fd-note">
        Open in{' '}
        {link === null ? (
          systemLabel(ref.system)
        ) : (
          <a href={link} target="_blank" rel="noopener noreferrer">
            {systemLabel(ref.system)}
          </a>
        )}
      </p>
    </div>
  );
}
