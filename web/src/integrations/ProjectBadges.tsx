// The badges of a project (DESIGN-SYSTEM §4.1: a chip that names a place of its own leads with its
// Lucide icon): its forge repository and its trackers. The Projects table and the project header
// show the same badges.
import type { ForgeBinding, Project, TrackerBinding } from '../gen';
import { GitPullRequest, Icon } from '../ui/icons';
import { Ticket } from './icons';
import { forgeChip, systemLabel, trackerChip } from './model';

/** The web page of a forge repository (`https://github.com/acme/web`); `null` without a host. */
export function forgeUrl(forge: ForgeBinding): string | null {
  const host = forge.host.trim();
  return host === '' || forge.repo === '' ? null : `https://${host}/${forge.repo}`;
}

/**
 * The forge of a project: the pull request icon and the repository ("acme/web"); the forge name
 * is in its title and its accessible name ("GitHub acme/web"). `link`: the badge opens the
 * repository on the forge in a new tab.
 */
export function ForgeBadge({
  forge,
  link = false,
}: {
  readonly forge: ForgeBinding;
  readonly link?: boolean;
}) {
  const body = (
    <>
      <Icon of={GitPullRequest} size={12} />
      <span className="fd-vh">{systemLabel(forge.kind)} </span>
      {forge.repo}
    </>
  );
  const url = link ? forgeUrl(forge) : null;
  return url === null ? (
    <span className="fd-badge fd-int-badge" data-chip="forge" title={forgeChip(forge)}>
      {body}
    </span>
  ) : (
    <a
      className="fd-badge fd-int-badge"
      data-chip="forge"
      title={forgeChip(forge)}
      href={url}
      target="_blank"
      rel="noopener noreferrer"
    >
      {body}
    </a>
  );
}

/** A tracker of a project: the ticket icon and its words ("Jira acme PROJ"). */
export function TrackerBadge({ tracker }: { readonly tracker: TrackerBinding }) {
  return (
    <span className="fd-badge fd-int-badge" data-chip="tracker">
      <Icon of={Ticket} size={12} />
      {trackerChip(tracker)}
    </span>
  );
}

/** The badges of a project: its forge, then its trackers. `link`: the forge badge is a link. */
export function ProjectChips({
  project,
  link = false,
}: {
  readonly project: Project;
  readonly link?: boolean;
}) {
  return (
    <span className="fd-int-chips">
      {project.forge === null ? null : <ForgeBadge forge={project.forge} link={link} />}
      {project.trackers.map((t) => (
        <TrackerBadge key={`${t.tracker}/${t.key}`} tracker={t} />
      ))}
    </span>
  );
}
