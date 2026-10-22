// R3.6 (DL-164): the chip of a session that has a pull request (a merge request on GitLab). It
// links to the pull request on the forge.
import type { DeckSession, PrSummary } from '../gen';
import { GitPullRequest, Icon } from '../ui/icons';
import { usePrs, useProjects } from './context';
import { prOfSession, webUrl } from './model';

/** `draft`, `open`, `merged` or `closed`. */
export const prStateText = (pr: Pick<PrSummary, 'draft' | 'state'>): string =>
  pr.draft ? 'draft' : pr.state;

/** The tone of the chip: act for open with failing checks, ok for merged, muted for closed. */
export function prTone(
  pr: Pick<PrSummary, 'state' | 'checks'>,
): 'act' | 'ok' | 'muted' | undefined {
  if (pr.state === 'merged') return 'ok';
  if (pr.state === 'closed') return 'muted';
  return pr.checks === 'failing' ? 'act' : undefined;
}

export function PrChip({ card }: { readonly card: DeckSession }) {
  const prs = usePrs();
  const projects = useProjects();
  const match = prOfSession(prs, projects, card);
  if (match === undefined) return null;
  const { project, pr } = match;
  const href = webUrl(pr.url);
  if (href === null) return null;
  const mark = project.forge?.kind === 'gitlab' ? '!' : '#';
  return (
    <a
      className="fd-badge fd-int-prchip"
      data-pr={pr.number}
      data-tone={prTone(pr)}
      title={prChipTitle(pr)}
      href={href}
      target="_blank"
      rel="noopener noreferrer"
    >
      <Icon of={GitPullRequest} size={12} />
      {mark}
      {pr.number} {prStateText(pr)}
    </a>
  );
}

const REVIEW_TEXT: Readonly<Record<PrSummary['review_state'], string | null>> = {
  approved: 'approved',
  changes_requested: 'changes requested',
  review_required: 'review required',
  none: null,
};

/** The chip tooltip: the title, then the checks and the review state that are known. */
export function prChipTitle(pr: Pick<PrSummary, 'title' | 'checks' | 'review_state'>): string {
  const parts = [pr.title];
  if (pr.checks !== 'none') parts.push(`checks ${pr.checks}`);
  const review = REVIEW_TEXT[pr.review_state];
  if (review !== null) parts.push(review);
  return parts.join(' · ');
}
