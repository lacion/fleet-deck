// DL-140: the facts, the place chips and the details of a session header ("Flight Deck" §4.5).
import { relativeAge } from '../app/sessions';
import { providerLabel } from '../board/lanes';
import { limitChip, rateMeters } from '../board/usage';
import type { DeckSession } from '../gen';
import { useProjects } from '../integrations/context';
import { IssueChip } from '../integrations/IssueChip';
import { PrChip } from '../integrations/PrChip';
import { digestLines } from '../ui/generated';
import { Folder, GitBranch, Icon, Server, ShieldAlert } from '../ui/icons';
import { isRiskyMode, modelFamily, shortModel } from '../ui/format';
import { isHibernated } from '../views/hibernate';
import { CopyButton, MAX_PATH_CHARS, shortenMiddle } from './CopyButton';

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

/** The status glyph and word of a session. */
export function StatusFact({ session }: { readonly session: DeckSession }) {
  return (
    <span className="fd-sh-status" data-status={session.status}>
      <span className="fd-dot" data-status={session.status} aria-hidden="true" />
      {statusWord(session)}
    </span>
  );
}

/** The model and the provider of a session (the callsign cell shows them under the callsign). */
export function Who({ session }: { readonly session: DeckSession }) {
  const s = session.summary;
  return (
    <span className="fd-sh-who">
      <span
        className="fd-sh-model"
        data-family={modelFamily(s.model) ?? undefined}
        title={s.model ?? 'default model'}
      >
        {shortModel(s.model)}
      </span>
      <span className="fd-sh-provider">{providerLabel(s.provider)}</span>
    </span>
  );
}

/** The permission mode fact; absent without a mode and for `default`. */
function ModeFact({ session }: { readonly session: DeckSession }) {
  const s = session.summary;
  if (!s.mode || s.mode === 'default') return null;
  const tone = modeTone(s.mode);
  return (
    <span className="fd-sh-mode" data-tone={tone} title="Permission mode">
      {tone === 'hazard' ? <Icon of={ShieldAlert} size={13} /> : null}
      {s.mode}
    </span>
  );
}

/** The issue, the pull request, the rate limit and (with `status`) the status of a session. */
export function Facts({
  session,
  status = true,
}: {
  readonly session: DeckSession;
  readonly status?: boolean;
}) {
  const s = session.summary;
  const projects = useProjects();
  const limit = limitChip(rateMeters([session], []));
  return (
    <>
      {status ? <StatusFact session={session} /> : null}
      <IssueChip summary={s} projects={projects} station={session.station_id} />
      <PrChip card={session} />
      {limit === null ? null : (
        <span className="fd-badge" data-tone={limit.tone}>
          {limit.text}
        </span>
      )}
    </>
  );
}

/** The status, model, mode and provider facts of a session (the phone menu puts the mode last). */
export function Badges({
  session,
  modeLast = false,
}: {
  readonly session: DeckSession;
  readonly modeLast?: boolean;
}) {
  return (
    <>
      <Facts session={session} />
      {modeLast ? null : <ModeFact session={session} />}
      <Who session={session} />
      {modeLast ? <ModeFact session={session} /> : null}
    </>
  );
}

/** The station, the branch and the folder of a session as chips, then the mode. */
export function Place({
  station,
  branch,
  cwd,
  session,
}: {
  readonly station: string;
  readonly branch: string | null;
  readonly cwd: string | null;
  /** With a session, the permission mode follows the place. */
  readonly session?: DeckSession;
}) {
  return (
    <span className="fd-sh-place">
      <span className="fd-badge fd-sh-chip" title="Station">
        <Icon of={Server} size={13} />
        {station}
      </span>
      {branch ? (
        <span className="fd-badge fd-sh-chip" data-mono="" title={`Branch: ${branch}`}>
          <Icon of={GitBranch} size={13} />
          <span>{branch}</span>
        </span>
      ) : null}
      {cwd ? (
        <span className="fd-badge fd-sh-chip" data-mono="" title={cwd}>
          <Icon of={Folder} size={13} />
          <span>{shortenMiddle(cwd, MAX_PATH_CHARS)}</span>
        </span>
      ) : null}
      {session === undefined ? null : <ModeFact session={session} />}
    </span>
  );
}

/** The facts of a session: its id (with a copy button), callsign, worktree, age and event count. */
export function Details({ session, now }: { readonly session: DeckSession; readonly now: number }) {
  const s = session.summary;
  const digest = digestLines(s.digest);
  return (
    <dl className="fd-sh-details">
      <div>
        <dt>Session</dt>
        <dd>
          <span>{s.session_id}</span>
          <CopyButton text={s.session_id} label="Copy the session id" />
        </dd>
      </div>
      {s.callsign ? (
        <div>
          <dt>Callsign</dt>
          <dd>{s.callsign}</dd>
        </div>
      ) : null}
      {s.worktree ? (
        <div>
          <dt>Worktree</dt>
          <dd>{s.worktree}</dd>
        </div>
      ) : null}
      {digest.length > 0 ? (
        <div data-field="digest">
          <dt>Summary (generated)</dt>
          <dd>
            <ul className="fd-sh-digest">
              {digest.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </dd>
        </div>
      ) : null}
      <div>
        <dt>Updated</dt>
        <dd>{relativeAge(s.updated_at, now)}</dd>
      </div>
      <div>
        <dt>Events</dt>
        <dd>{s.session_seq}</dd>
      </div>
    </dl>
  );
}
