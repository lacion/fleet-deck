// R3.7 (DL-164): the preview of one pull request: the description, the changed files, the
// comments, and the failing checks with the tail of each log.
import { useState } from 'react';
import { relativeAge } from '../app/sessions';
import type { CheckFailure, PrDetail, Project } from '../gen';
import { Icon, X } from '../ui/icons';
import { Loading } from '../ui/Loading';
import { ErrorNotice } from '../ui/page';
import type { IntegrationActions } from './actions';
import { OutsideText } from './Outside';
import { outsideNote, unfence, webUrl } from './model';
import { type Load, useLoad } from './useLoad';
import { PrCommentDialog, PrReviewDialog } from './Writes';

type Props = {
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly number: number;
  readonly now: number;
  readonly onClose: () => void;
  /** The detail and the failures at the first render (tests). */
  readonly initial?: PrDetail;
  readonly initialFailures?: readonly CheckFailure[];
};

/** The failing checks: name, link, and the log tail in a collapsed `<pre>`. */
export function FailingChecks({ failures }: { readonly failures: readonly CheckFailure[] }) {
  if (failures.length === 0) return null;
  return (
    <section aria-label="Failing checks">
      <h4>Failing checks ({failures.length})</h4>
      <ul className="fd-int-checks">
        {failures.map((f) => {
          const href = webUrl(f.url);
          const log = unfence(f.log_tail).trim();
          return (
            <li key={`${f.name}/${f.url}`}>
              {href === null ? (
                <strong>{f.name}</strong>
              ) : (
                <a href={href} target="_blank" rel="noopener noreferrer">
                  {f.name}
                </a>
              )}
              {log === '' ? (
                <p className="fd-note">No log.</p>
              ) : (
                <details>
                  <summary>Log tail</summary>
                  <pre className="fd-int-log">{log}</pre>
                </details>
              )}
            </li>
          );
        })}
      </ul>
    </section>
  );
}

export function PrPreview({
  project,
  actions,
  number,
  now,
  onClose,
  initial,
  initialFailures,
}: Props) {
  const { state, reload } = useLoad<PrDetail>(
    () => actions.pr(project.project_id, number),
    `${project.project_id}|${number}`,
    initial,
  );
  const failing = state.kind === 'ok' && state.value.summary.checks === 'failing';
  const checks = useLoad<readonly CheckFailure[]>(
    () => (failing ? actions.prChecks(project.project_id, number) : Promise.resolve([])),
    `${project.project_id}|${number}|${failing}`,
    initialFailures,
  );
  const [writing, setWriting] = useState<'comment' | 'review' | null>(null);
  const forge = project.forge?.kind;
  return (
    <aside className="fd-int-preview" aria-label={`Pull request #${number}`}>
      <div className="fd-int-preview-head">
        <button type="button" className="fd-ghost fd-sm" onClick={onClose}>
          <Icon of={X} size={14} /> Close
        </button>
      </div>
      <PrBody state={state} system={forge ?? 'github'} number={number} now={now} onRetry={reload} />
      {checks.state.kind === 'ok' ? <FailingChecks failures={checks.state.value} /> : null}
      {checks.state.kind === 'error' ? <ErrorNotice text={checks.state.text} /> : null}
      {state.kind === 'ok' && actions.canDrive && forge !== undefined ? (
        <div className="fd-plan-actions">
          <button type="button" onClick={() => setWriting('comment')}>
            Comment…
          </button>
          <button type="button" onClick={() => setWriting('review')}>
            Review…
          </button>
        </div>
      ) : null}
      {writing === 'comment' && forge !== undefined ? (
        <PrCommentDialog
          project={project}
          actions={actions}
          number={number}
          forge={forge}
          onClose={() => {
            setWriting(null);
            reload();
          }}
        />
      ) : null}
      {writing === 'review' && forge !== undefined ? (
        <PrReviewDialog
          project={project}
          actions={actions}
          number={number}
          forge={forge}
          onClose={() => {
            setWriting(null);
            reload();
          }}
        />
      ) : null}
    </aside>
  );
}

export function PrBody({
  state,
  system,
  number,
  now,
  onRetry,
}: {
  readonly state: Load<PrDetail>;
  /** The forge kind: `github` or `gitlab`. */
  readonly system: string;
  readonly number: number;
  readonly now: number;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') return <Loading text={`Loading #${number}…`} />;
  if (state.kind === 'error') return <ErrorNotice text={state.text} onRetry={onRetry} />;
  const d = state.value;
  const s = d.summary;
  const href = webUrl(s.url);
  return (
    <div className="fd-int-detail">
      <h3>
        <span className="fd-int-key">#{s.number}</span>{' '}
        {href === null ? (
          s.title
        ) : (
          <a href={href} target="_blank" rel="noopener noreferrer">
            {s.title}
          </a>
        )}
      </h3>
      <p className="fd-plan-meta">
        {s.author} · {s.head_branch} → {s.base_branch} · {relativeAge(s.updated_at, now)}
      </p>
      <OutsideText system={system} text={d.body_markdown} />
      <section aria-label="Changed files">
        <h4>Files ({d.files.length})</h4>
        {d.files.length === 0 ? <p className="fd-empty">No changed files.</p> : null}
        <ul className="fd-int-files">
          {d.files.map((f) => (
            <li key={f.path}>
              <code>{f.path}</code> <span className="fd-add">+{f.additions}</span>{' '}
              <span className="fd-del">−{f.deletions}</span>
            </li>
          ))}
        </ul>
      </section>
      <section aria-label="Comments">
        <h4>Comments ({d.comments.length})</h4>
        {d.comments.length === 0 ? (
          <p className="fd-empty">No comments.</p>
        ) : (
          <p className="fd-note fd-int-outside-note">{outsideNote(system)}</p>
        )}
        <ol className="fd-int-comments">
          {d.comments.map((c, i) => (
            <li key={i}>
              <p className="fd-plan-meta">
                {c.author} · {relativeAge(c.at, now)}
              </p>
              <OutsideText system={system} text={c.body_markdown} note={false} />
            </li>
          ))}
        </ol>
      </section>
    </div>
  );
}
