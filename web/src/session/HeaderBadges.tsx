// DL-140: the badges and the place line of a session header.
import { providerLabel } from '../board/lanes';
import { limitChip, rateMeters } from '../board/usage';
import type { DeckSession } from '../gen';
import { useProjects } from '../integrations/context';
import { IssueChip } from '../integrations/IssueChip';
import { PrChip } from '../integrations/PrChip';
import { isRiskyMode, modelFamily, shortModel } from '../ui/format';
import { isHibernated } from '../views/hibernate';

/** The tone of the status badge. No status is hazard red (DL-171): the dot and the card say it. */
function statusTone(status: string): 'act' | 'ok' | 'info' | 'muted' {
  switch (status) {
    case 'needs_you':
      return 'act';
    case 'working':
      return 'ok';
    case 'verifying':
      return 'info';
    default:
      return 'muted';
  }
}

/** The tone of the permission mode badge: hazard only for a mode that skips the approvals, `plan` for plan, none else. */
export function modeTone(mode: string): 'plan' | 'hazard' | undefined {
  if (isRiskyMode(mode)) return 'hazard';
  return mode === 'plan' ? 'plan' : undefined;
}

/** The status, model, mode and provider badges of a session (the phone menu puts the mode last). */
export function Badges({
  session,
  modeLast = false,
}: {
  readonly session: DeckSession;
  readonly modeLast?: boolean;
}) {
  const s = session.summary;
  const projects = useProjects();
  const limit = limitChip(rateMeters([session], []));
  const mode = s.mode ? (
    <span className="fd-badge" data-tone={modeTone(s.mode)} title="Permission mode">
      {s.mode}
    </span>
  ) : null;
  return (
    <>
      <IssueChip summary={s} projects={projects} station={session.station_id} />
      <PrChip card={session} />
      <span className="fd-badge" data-tone={statusTone(session.status)}>
        {isHibernated(session) ? 'hibernated' : session.status.replaceAll('_', ' ')}
      </span>
      <span
        className="fd-badge"
        data-family={modelFamily(s.model) ?? undefined}
        title={s.model ?? 'default model'}
      >
        {shortModel(s.model)}
      </span>
      {modeLast ? null : mode}
      <span className="fd-badge" data-provider={s.provider}>
        {providerLabel(s.provider)}
      </span>
      {limit === null ? null : (
        <span className="fd-badge" data-tone={limit.tone}>
          {limit.text}
        </span>
      )}
      {modeLast ? mode : null}
    </>
  );
}

/** The station, the branch and the directory of a session. */
export function Place({
  parts,
  cwd,
}: {
  readonly parts: readonly string[];
  readonly cwd: string | null;
}) {
  return (
    <p className="fd-sh-place">
      {parts.map((part, n) => (
        <span key={part} title={n === parts.length - 1 && cwd ? cwd : undefined}>
          {n > 0 ? ' · ' : ''}
          {part}
        </span>
      ))}
    </p>
  );
}
