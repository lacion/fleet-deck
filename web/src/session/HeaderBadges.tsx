// DL-140: the badges and the place line of a session header.
import { providerLabel } from '../board/lanes';
import { limitChip, rateMeters } from '../board/usage';
import type { DeckSession } from '../gen';
import { useProjects } from '../integrations/context';
import { IssueChip } from '../integrations/IssueChip';
import { PrChip } from '../integrations/PrChip';
import { Icon, ShieldAlert } from '../ui/icons';
import { isRiskyMode, modelFamily, shortModel } from '../ui/format';
import { isHibernated } from '../views/hibernate';

/** The word of the status fact. A hibernated session says so. */
export function statusWord(session: DeckSession): string {
  if (isHibernated(session)) return 'Hibernated';
  switch (session.status) {
    case 'needs_you':
      return 'Needs you';
    case 'working':
      return 'Working';
    case 'verifying':
      return 'Verifying';
    case 'queued':
      return 'Queued';
    case 'idle':
      return 'Idle';
    case 'ended':
      return 'Ended';
    case 'lost':
      return 'Lost';
    case 'unreachable':
      return 'Unreachable';
  }
}

/** The tone of the permission mode badge: hazard only for a mode that skips the approvals, `plan` for plan, none else. */
export function modeTone(mode: string): 'plan' | 'hazard' | undefined {
  if (isRiskyMode(mode)) return 'hazard';
  return mode === 'plan' ? 'plan' : undefined;
}

/** The status, model, mode and provider facts of a session (the phone menu puts the mode last). */
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
  const tone = s.mode ? modeTone(s.mode) : undefined;
  const mode =
    s.mode && s.mode !== 'default' ? (
      <span className="fd-sh-mode" data-tone={tone} title="Permission mode">
        {tone === 'hazard' ? <Icon of={ShieldAlert} size={13} /> : null}
        {s.mode}
      </span>
    ) : null;
  return (
    <>
      <IssueChip summary={s} projects={projects} station={session.station_id} />
      <PrChip card={session} />
      <span className="fd-sh-status" data-status={session.status}>
        <span className="fd-dot" data-status={session.status} aria-hidden="true" />
        {statusWord(session)}
      </span>
      <span
        className="fd-sh-model"
        data-family={modelFamily(s.model) ?? undefined}
        title={s.model ?? 'default model'}
      >
        {shortModel(s.model)}
      </span>
      {modeLast ? null : mode}
      <span className="fd-sh-provider">{providerLabel(s.provider)}</span>
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
          {part}
        </span>
      ))}
    </p>
  );
}
