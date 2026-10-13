// R3.7 (DL-164): the Pull requests tab of a project: four groups, a session chip on a row that a
// fleet session works on, "Review in a session", "Fix checks", and the preview.
import { useState } from 'react';
import { formatHash } from '../app/routes';
import { relativeAge } from '../app/sessions';
import type { ChecksState, DeckSession, PrGroups, PrSummary, Project, StationView } from '../gen';
import { Icon } from '../ui/icons';
import type { IntegrationActions } from './actions';
import { CHECKS_LABEL, fixChecksPrompt, REVIEW_LABEL, sessionsOfPr } from './model';
import { PrPreview } from './PrPreview';
import { RefreshCw } from './icons';
import { StartDialog } from './StartDialog';
import { type Load, useLoad } from './useLoad';

export const GROUPS: readonly { readonly id: keyof PrGroups; readonly label: string }[] = [
  { id: 'review_requested', label: 'Review requested' },
  { id: 'mine', label: 'Yours' },
  { id: 'pending', label: 'Open' },
  { id: 'recent_closed', label: 'Recently merged or closed' },
];

const CHECKS_TONE: Record<ChecksState, string> = {
  passing: 'ok',
  failing: 'hazard',
  pending: 'act',
  none: 'muted',
};

/** One row of a group. */
export function PrRow({
  pr,
  sessions,
  now,
  canDrive,
  open,
  onOpen,
  onReview,
  onFix,
}: {
  readonly pr: PrSummary;
  /** The fleet sessions that work on this pull request. */
  readonly sessions: readonly DeckSession[];
  readonly now: number;
  readonly canDrive: boolean;
  readonly open: boolean;
  readonly onOpen: (n: number) => void;
  readonly onReview: (n: number) => void;
  readonly onFix: (pr: PrSummary) => void;
}) {
  return (
    <li data-pr={pr.number} data-open={open ? 'true' : undefined}>
      <button type="button" className="fd-int-row" onClick={() => onOpen(pr.number)}>
        <span className="fd-int-key">#{pr.number}</span>
        <span className="fd-int-title">{pr.title}</span>
        {pr.draft ? (
          <span className="fd-badge" data-tone="muted">
            Draft
          </span>
        ) : null}
        <span className="fd-badge" data-review={pr.review_state}>
          {REVIEW_LABEL[pr.review_state]}
        </span>
        <span className="fd-badge" data-tone={CHECKS_TONE[pr.checks]} data-checks={pr.checks}>
          checks {CHECKS_LABEL[pr.checks]}
        </span>
        <span className="fd-int-meta">{pr.author}</span>
        <span className="fd-int-meta">{relativeAge(pr.updated_at, now)}</span>
      </button>
      <span className="fd-int-actions">
        {sessions.map((s) => (
          <a
            key={s.summary.session_id}
            className="fd-chip fd-int-session"
            data-session={s.summary.session_id}
            href={formatHash({
              name: 'session',
              station: s.station_id,
              session: s.summary.session_id,
              tab: 'chat',
            })}
          >
            {s.summary.callsign ?? s.summary.session_id}
          </a>
        ))}
        {canDrive ? (
          <button type="button" className="fd-ghost fd-sm" onClick={() => onReview(pr.number)}>
            Review in a session
          </button>
        ) : null}
        {canDrive && pr.checks === 'failing' ? (
          <button type="button" className="fd-ghost fd-sm" onClick={() => onFix(pr)}>
            Fix checks
          </button>
        ) : null}
      </span>
    </li>
  );
}

/** The four groups of one state of the read. */
export function PrGroupsView({
  state,
  sessions,
  station,
  now,
  canDrive,
  open,
  onOpen,
  onReview,
  onFix,
  onRetry,
}: {
  readonly state: Load<PrGroups>;
  readonly sessions: readonly DeckSession[];
  readonly station: string;
  readonly now: number;
  readonly canDrive: boolean;
  readonly open: number | null;
  readonly onOpen: (n: number) => void;
  readonly onReview: (n: number) => void;
  readonly onFix: (pr: PrSummary) => void;
  readonly onRetry: () => void;
}) {
  if (state.kind === 'loading') {
    return (
      <p role="status" className="fd-note">
        Loading the pull requests…
      </p>
    );
  }
  if (state.kind === 'error') {
    return (
      <div className="fd-int-state">
        <p role="alert" className="fd-note fd-int-error">
          {state.text}
        </p>
        <button type="button" className="fd-ghost fd-sm" onClick={onRetry}>
          Try again
        </button>
      </div>
    );
  }
  const total = GROUPS.reduce((n, g) => n + state.value[g.id].length, 0);
  if (total === 0) return <p className="fd-empty">No pull requests.</p>;
  return (
    <div className="fd-int-groups">
      {GROUPS.map((g) => {
        const rows = state.value[g.id];
        return (
          <section key={g.id} aria-label={g.label} data-group={g.id}>
            <h4>
              {g.label} <span className="fd-count">{rows.length}</span>
            </h4>
            {rows.length === 0 ? (
              <p className="fd-empty">None.</p>
            ) : (
              <ul className="fd-int-list">
                {rows.map((pr) => (
                  <PrRow
                    key={pr.number}
                    pr={pr}
                    sessions={sessionsOfPr(pr, sessions, station)}
                    now={now}
                    canDrive={canDrive}
                    open={open === pr.number}
                    onOpen={onOpen}
                    onReview={onReview}
                    onFix={onFix}
                  />
                ))}
              </ul>
            )}
          </section>
        );
      })}
    </div>
  );
}

type Props = {
  readonly project: Project;
  readonly actions: IntegrationActions;
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  readonly now: number;
  /** "Fix checks": opens the spawn form with the project's station and path. */
  readonly onFixChecks: (station: string, cwd: string, prompt: string) => void;
  /** The groups at the first render (tests). */
  readonly initial?: PrGroups;
};

export function PrsTab({ project, actions, stations, sessions, now, onFixChecks, initial }: Props) {
  const [open, setOpen] = useState<number | null>(null);
  const [reviewing, setReviewing] = useState<number | null>(null);
  const { state, reload } = useLoad<PrGroups>(
    () => actions.prs(project.project_id),
    project.project_id,
    initial,
  );
  if (project.forge === null) {
    return (
      <section className="fd-int-tab" aria-label="Pull requests">
        <p className="fd-note">This project has no forge. Edit the project to add one.</p>
      </section>
    );
  }
  return (
    <section className="fd-int-tab" aria-label="Pull requests">
      <div className="fd-int-filters">
        <button type="button" className="fd-ghost fd-sm" onClick={reload}>
          <Icon of={RefreshCw} size={14} /> Refresh
        </button>
      </div>
      <div className="fd-int-split" data-open={open === null ? undefined : 'true'}>
        <div className="fd-int-main">
          <PrGroupsView
            state={state}
            sessions={sessions}
            station={project.station_id}
            now={now}
            canDrive={actions.canDrive}
            open={open}
            onOpen={setOpen}
            onReview={setReviewing}
            onFix={(pr) => onFixChecks(project.station_id, project.path, fixChecksPrompt(pr))}
            onRetry={reload}
          />
        </div>
        {open === null ? null : (
          <PrPreview
            key={open}
            project={project}
            actions={actions}
            number={open}
            now={now}
            onClose={() => setOpen(null)}
          />
        )}
      </div>
      {reviewing === null ? null : (
        <StartDialog
          project={project}
          actions={actions}
          stations={stations}
          issues={[]}
          review={reviewing}
          onClose={() => setReviewing(null)}
        />
      )}
    </section>
  );
}
