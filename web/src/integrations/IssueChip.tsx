// R3.4 (DL-164): the chip of a session that works on an issue. It links to the project page when
// a project of the session's station has a matching tracker or forge, else to the issue itself.
import type { Project, SessionSummary } from '../gen';
import { projectOfIssue, webUrl } from './model';

export function IssueChip({
  summary,
  projects,
  station,
}: {
  readonly summary: Pick<SessionSummary, 'issue'>;
  readonly projects: readonly Project[];
  /** The station of the session: only a project of that station matches. */
  readonly station?: string | undefined;
}) {
  const issue = summary.issue;
  if (issue === undefined) return null;
  const project = projectOfIssue(issue, projects, station);
  if (project !== undefined) {
    return (
      <a
        className="fd-badge fd-int-issuechip"
        data-issue={issue.key}
        title={issue.title}
        href={`#/project/${encodeURIComponent(project.project_id)}`}
      >
        {issue.key}
      </a>
    );
  }
  const href = webUrl(issue.url);
  return href === null ? (
    <span className="fd-badge fd-int-issuechip" data-issue={issue.key} title={issue.title}>
      {issue.key}
    </span>
  ) : (
    <a
      className="fd-badge fd-int-issuechip"
      data-issue={issue.key}
      title={issue.title}
      href={href}
      target="_blank"
      rel="noopener noreferrer"
    >
      {issue.key}
    </a>
  );
}
