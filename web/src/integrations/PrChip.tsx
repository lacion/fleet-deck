// R3.6 (DL-164): the chip of a session that has a pull request (a merge request on GitLab). It
// links to the pull request on the forge.
import type { DeckSession, PrSummary } from '../gen';
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
      title={`${pr.title} · checks ${pr.checks} · review ${pr.review_state.replaceAll('_', ' ')}`}
      href={href}
      target="_blank"
      rel="noopener noreferrer"
    >
      {mark}
      {pr.number} {prStateText(pr)}
    </a>
  );
}
