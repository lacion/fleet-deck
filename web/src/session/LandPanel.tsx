// FG-8 (design §11.8, §11.11): the land of a session: the "Land" button with its confirm dialog,
// the status line of the newest land, "Stop checking", "Forget", "Cancel land" and "Send to
// session". `ProjectLands` (the project page) reuses the status line and the actions.
import { useEffect, useState } from 'react';
import { stationName } from '../board/fleet';
import type { DeckSession, LandPhase, Project, StationView } from '../gen';
import type { IntegrationActions, LandRow, ProjectLands } from '../integrations/actions';
import { useIntegrations, useProjects } from '../integrations/context';
import { Dialog } from '../integrations/Dialog';
import { message, useLoad } from '../integrations/useLoad';

/** How often the lands of a project are read again while one of them is open. */
export const LANDS_POLL_MS = 10_000;

/** `<remote>/<default>`, or "the default branch of <remote>" while the default is not set (Q8). */
export function remoteDefault(project: Project | undefined): string {
  const remote = project?.git.push_remote;
  if (remote === undefined) return 'the default branch';
  const branch = project?.git.default_branch;
  return branch === null || branch === undefined
    ? `the default branch of ${remote}`
    : `${remote}/${branch}`;
}

/** The mode of a land: the setting, else `pr` with a forge, else `merge` (the deck decides). */
export function landMode(project: Project): 'pr' | 'merge' {
  const set = project.git.land;
  if (set === 'pr' || set === 'merge') return set;
  return project.forge === null ? 'merge' : 'pr';
}

/** The text of the confirm dialog (§11.11). */
export function landConfirmText(project: Project): string {
  const where = remoteDefault(project);
  const head = `Fleet Deck rebases the branch onto ${where} in a separate folder, runs the check command, pushes the branch`;
  return landMode(project) === 'pr'
    ? `${head} and opens or updates a pull request.`
    : `${head}, then pushes the result to ${where}.`;
}

/** The phase texts of "Landing: <phase text>" (§11.11, decision Q5 for the unlisted phases). */
export function phaseText(phase: LandPhase | undefined, project: Project | undefined): string {
  switch (phase) {
    case 'admitted':
      return 'starting';
    case 'captured':
      return 'fetching';
    case 'worktree_ready':
      return 'preparing a folder';
    case 'rebased':
      return 'rebasing';
    case 'checking':
      return 'running the check';
    case 'checked':
      return 'check passed';
    case 'branch_push_intent':
      return 'pushing the branch';
    case 'branch_pushed':
      return 'branch pushed';
    case 'pr_intent':
      return 'opening the pull request';
    case 'default_push_intent':
      return `pushing ${project?.git.default_branch ?? 'the default branch'}`;
    case 'done':
      return 'finishing';
    default:
      return 'working';
  }
}

const oid7 = (oid: string): string => oid.slice(0, 7);

/** The number at the end of a pull request address, or `null`. */
export function prNumber(url: string): string | null {
  return /(\d+)\/?(?:[?#].*)?$/.exec(url)?.[1] ?? null;
}

/** The status line of a land: plain text, or a link for a pull request. */
export type LandLine = {
  readonly text: string;
  readonly link?: { readonly href: string; readonly label: string };
};

/** The status line of a row, in the order of decision Q5. */
export function landLine(
  row: LandRow,
  project: Project | undefined,
  stations: readonly StationView[],
): LandLine {
  const open = row.state === 'queued' || row.state === 'running';
  if (open && row.abandon_state === 'requested') {
    return {
      text: `Stopping… waiting for ${stationName(row.station_id, stations)} to confirm.`,
    };
  }
  if (open && row.reconciling) {
    return { text: 'Checking the result of the land after an interruption…' };
  }
  if (open && row.phase === 'lost') {
    return {
      text: 'The station has no record of this land. Check the remote, then press Stop checking.',
    };
  }
  if (row.state === 'queued') return { text: `Queued (${row.position ?? 0} ahead)` };
  if (row.state === 'running') return { text: `Landing: ${phaseText(row.phase, project)}` };
  if (row.state === 'landed') {
    const url = row.pr_url;
    if (url !== undefined && /^https?:\/\//.test(url)) {
      const n = prNumber(url);
      return {
        text: '',
        link: { href: url, label: n === null ? 'Pull request' : `Pull request #${n}` },
      };
    }
    const oid = row.default_published_oid ?? row.rebased_oid ?? row.branch_published_oid;
    return { text: oid === undefined ? 'Landed' : `Landed ${oid7(oid)}` };
  }
  if (row.state === 'cancelled') return { text: 'Cancelled.' };
  if (row.message !== undefined && row.message !== '') return { text: row.message };
  if (row.state === 'conflict') {
    const base = row.base_oid === undefined ? '' : ` (${oid7(row.base_oid)})`;
    return { text: `Conflict in ${row.files.length} files${base}.` };
  }
  return { text: row.code ?? row.state };
}

/** The status line as markup. */
export function LandStatusLine({
  row,
  project,
  stations,
}: {
  readonly row: LandRow;
  readonly project: Project | undefined;
  readonly stations: readonly StationView[];
}) {
  const line = landLine(row, project, stations);
  return (
    <span className="fd-land-status" data-state={row.state}>
      {line.link === undefined ? null : (
        <a href={line.link.href} target="_blank" rel="noreferrer noopener">
          {line.link.label}
        </a>
      )}
      {line.text}
    </span>
  );
}

/** What "Send to session" can do: the session is idle, ended, or busy. */
export type SendState = 'ready' | 'ended' | 'working';

/** The send state of a session. */
export function sendStateOf(session: DeckSession | undefined): SendState {
  const s = session?.summary;
  if (s === undefined || s.state === 'exited' || s.state === 'lost') return 'ended';
  const idle = s.state === 'running' && s.current_run === null && s.current_turn === null;
  return idle && s.open_requests.length === 0 ? 'ready' : 'working';
}

type Confirm = 'stop' | 'forget';

/** The actions of one land row: Cancel land, Stop checking, Forget, Send to session. */
export function LandActions({
  row,
  stations,
  actions,
  send,
  onChanged,
}: {
  readonly row: LandRow;
  readonly stations: readonly StationView[];
  readonly actions: IntegrationActions;
  readonly send: SendState;
  readonly onChanged: () => void;
}) {
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const open = row.state === 'queued' || row.state === 'running';
  const requested = row.abandon_state === 'requested';
  const uncertain = open && (row.reconciling || row.phase === 'lost');
  const paired = stations.some((s) => s.station_id === row.station_id);
  const canStop = uncertain && !requested;
  const canForget = open && requested && !paired;
  const canCancel = row.state === 'queued';
  const canSend = row.state === 'conflict' && row.files.length > 0;
  const run = (call: Promise<unknown>) => {
    setBusy(true);
    setError(null);
    call
      .then(() => {
        setConfirm(null);
        onChanged();
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };
  if (!canStop && !canForget && !canCancel && !canSend && error === null) return null;
  return (
    <>
      {canCancel ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={busy}
          onClick={() => run(actions.cancelLand(row.land_id))}
        >
          Cancel land
        </button>
      ) : null}
      {canStop ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={busy}
          onClick={() => setConfirm('stop')}
        >
          Stop checking
        </button>
      ) : null}
      {canForget ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={busy}
          onClick={() => setConfirm('forget')}
        >
          Forget
        </button>
      ) : null}
      {canSend ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={busy || send !== 'ready'}
          {...(send === 'working' ? { title: 'The session is working.' } : {})}
          onClick={() => run(actions.sendLand(row.land_id))}
        >
          {send === 'ended' ? 'Resume the session first' : 'Send to session'}
        </button>
      ) : null}
      {error === null || confirm !== null ? null : (
        <span role="alert" className="fd-formerror">
          {error}
        </span>
      )}
      {confirm === null ? null : (
        <Dialog
          title={confirm === 'stop' ? 'Stop checking this land?' : 'Forget this land?'}
          onClose={() => setConfirm(null)}
        >
          <p className="fd-note">
            {confirm === 'stop'
              ? `Stop checking this land? Fleet Deck frees the queue after ${stationName(row.station_id, stations)} confirms that it stopped. Fleet Deck does not know if the land changed the remote.`
              : `The station ${row.station_id} was removed. If it still runs, it can still push this land. Free the queue?`}
          </p>
          {error === null ? null : (
            <p role="alert" className="fd-formerror">
              {error}
            </p>
          )}
          <div className="fd-pane-row">
            <button type="button" className="fd-ghost" onClick={() => setConfirm(null)}>
              Cancel
            </button>
            <button
              type="button"
              className="fd-primary"
              disabled={busy}
              onClick={() =>
                run(
                  confirm === 'stop'
                    ? actions.abandonLand(row.land_id)
                    : actions.forgetLand(row.land_id),
                )
              }
            >
              {confirm === 'stop' ? 'Stop checking' : 'Forget'}
            </button>
          </div>
        </Dialog>
      )}
    </>
  );
}

/** The newest land of one session in the lists of its project. */
export function newestOf(lands: ProjectLands, station: string, session: string): LandRow | null {
  const own = [...lands.queue, ...lands.recent].filter(
    (r) => r.station_id === station && r.session_id === session,
  );
  own.sort((a, b) => b.created_at - a.created_at);
  return own[0] ?? null;
}

/** Whether the "Land" button shows (§11.1 as far as the board knows it). */
export function landOffered(
  session: DeckSession,
  project: Project,
  newest: LandRow | null,
): boolean {
  const s = session.summary;
  if (!project.git.sync || s.worktree === null || s.branch === null) return false;
  if (newest !== null && (newest.state === 'queued' || newest.state === 'running')) return false;
  return sendStateOf(session) !== 'working';
}

/** The signature of `summary.land` that makes the panel read the lands again. */
const landSignature = (s: DeckSession['summary']): string =>
  s.land === undefined
    ? ''
    : `${s.land.land_id}/${s.land.attempt}/${s.land.state}/${s.land.phase}/${s.land.reconciling}`;

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  /** The lands at the first render (tests). */
  readonly initial?: ProjectLands | null;
};

/** The land panel of a session; nothing for a login without the admin scope or an older deck. */
export function LandPanel({ session, stations, initial }: Props) {
  const env = useIntegrations();
  const projects = useProjects();
  const project = projects.find((p) => p.project_id === session.project_id);
  const projectId = project?.project_id ?? '';
  const actions = env?.actions;
  const active = actions?.canAdmin === true && project !== undefined;
  const sig = landSignature(session.summary);
  const { state, reload } = useLoad<ProjectLands | null>(
    async (key) => {
      if (actions === undefined || !active) return null;
      return actions.projectLands(key.split('|')[0] ?? '');
    },
    `${projectId}|${sig}`,
    initial,
  );
  const lands = state.kind === 'ok' ? state.value : null;
  const newest =
    lands === null ? null : newestOf(lands, session.station_id, session.summary.session_id);
  const polling = newest?.state === 'queued' || newest?.state === 'running';
  useEffect(() => {
    if (!polling) return undefined;
    const timer = setInterval(reload, LANDS_POLL_MS);
    return () => clearInterval(timer);
  }, [polling, reload]);
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!active || actions === undefined || project === undefined || lands === null) return null;
  const branch = session.summary.branch ?? '';
  const offered = landOffered(session, project, newest);
  if (!offered && newest === null) return null;
  const submit = () => {
    setBusy(true);
    setError(null);
    actions
      .land(session.station_id, session.summary.session_id)
      .then(() => {
        setAsking(false);
        reload();
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };
  return (
    <section className="fd-pane-row fd-land" aria-label="Land">
      {newest === null ? null : (
        <>
          <LandStatusLine row={newest} project={project} stations={stations} />
          <LandActions
            row={newest}
            stations={stations}
            actions={actions}
            send={sendStateOf(session)}
            onChanged={reload}
          />
          {newest.state === 'conflict' && newest.files.length > 0 ? (
            <ul className="fd-int-list" aria-label="Conflict files">
              {newest.files.slice(0, 10).map((f) => (
                <li key={f}>
                  <code>{f}</code>
                </li>
              ))}
              {newest.files.length > 10 ? <li>+{newest.files.length - 10} more</li> : null}
            </ul>
          ) : null}
        </>
      )}
      {offered ? (
        <button
          type="button"
          className="fd-ghost fd-sm"
          onClick={() => {
            setError(null);
            setAsking(true);
          }}
        >
          Land
        </button>
      ) : null}
      {asking ? (
        <Dialog title={`Land ${branch}?`} onClose={() => setAsking(false)}>
          <p className="fd-note">{landConfirmText(project)}</p>
          {error === null ? null : (
            <p role="alert" className="fd-formerror">
              {error}
            </p>
          )}
          <div className="fd-pane-row">
            <button type="button" className="fd-ghost" onClick={() => setAsking(false)}>
              Cancel
            </button>
            <button type="button" className="fd-primary" disabled={busy} onClick={submit}>
              Land
            </button>
          </div>
        </Dialog>
      ) : null}
    </section>
  );
}
