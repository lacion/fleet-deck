import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { DeckSession, StationView, WorktreeInfo, WorktreeVerdict } from '../gen';
import { ApiError } from '../runtime/api';
import type { Freshness } from '../runtime/state';
import { ArchiveRestore, Check, Copy, Icon, Plus, RotateCcw, Server, Trash2 } from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { ShellPanel } from '../shell/ShellPanel';
import { PairingDialog } from './PairingDialog';
import type { Outcome } from '../session/commands';
import {
  authLabel,
  confirmMatches,
  removeCommand,
  type StationActions,
  type WorktreeAction,
} from './pairing';

type Props = {
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  readonly freshness: Freshness;
  readonly actions?: StationActions | undefined;
};

const NONE = '—';

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** The shortened key fingerprint with a button that copies the whole value. */
function Fingerprint({ value }: { readonly value: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const short = value.length > 12 ? `${value.slice(0, 12)}…` : value;
  return (
    <>
      <span title={value}>{short}</span>
      <button
        type="button"
        className="fd-iconbtn fd-sm"
        aria-label="Copy the key fingerprint"
        title="Copy the key fingerprint"
        onClick={() => {
          void navigator.clipboard?.writeText(value).then(() => {
            setCopied(true);
            clearTimeout(timer.current);
            timer.current = setTimeout(() => setCopied(false), 1500);
          });
        }}
      >
        <Icon of={copied ? Check : Copy} size={14} />
      </button>
    </>
  );
}

const VERDICTS: Record<WorktreeVerdict, { label: string; tone: string }> = {
  clean: { label: 'Clean', tone: 'ok' },
  has_work: { label: 'Has work', tone: 'act' },
  live: { label: 'Live', tone: 'info' },
  missing: { label: 'Missing', tone: 'muted' },
  unknown: { label: 'Unknown', tone: 'muted' },
  trashed: { label: 'In the trash', tone: 'muted' },
};

const count = (n: number | null) => (n === null ? '?' : String(n));

/** The button that copies the command that removes a worktree. The board never runs it. */
function CopyCommand({ command }: { readonly command: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <button
      type="button"
      className="fd-ghost fd-sm"
      title={command}
      onClick={() => {
        void navigator.clipboard?.writeText(command).then(() => {
          setCopied(true);
          clearTimeout(timer.current);
          timer.current = setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      <Icon of={copied ? Check : Copy} size={14} /> Copy the remove command
    </button>
  );
}

const IN_PROGRESS: Record<string, string> = {
  trashing: 'Moving…',
  restoring: 'Restoring…',
  purging: 'Deleting…',
};

/** The longest the board polls the inventory after an accepted command. */
const POLL_MS = 2000;
const POLL_LIMIT_MS = 60_000;

type RowAction = (action: WorktreeAction, confirmName?: string) => Promise<Outcome>;

/**
 * One worktree: the verdict, the evidence, and the trash buttons (DL-150) when `onAction` is
 * given. Without it the row is read only.
 */
export function WorktreeRow({
  worktree: w,
  station,
  now,
  onAction,
}: {
  readonly worktree: WorktreeInfo;
  readonly station: string;
  readonly now: number;
  readonly onAction?: RowAction | undefined;
}) {
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [purging, setPurging] = useState(false);
  const [typed, setTyped] = useState('');
  const verdict = VERDICTS[w.verdict];
  const trash = w.trash;
  const command = trash === undefined ? removeCommand(w) : null;
  const compared = w.compared_with === 'remotes' ? 'a remote' : 'main';
  const name = basename(w.path);
  const act = async (action: WorktreeAction, confirmName?: string) => {
    if (onAction === undefined) return;
    setBusy(true);
    try {
      setOutcome(await onAction(action, confirmName));
      setPurging(false);
      setTyped('');
    } finally {
      setBusy(false);
    }
  };
  const moveToTrash = () => {
    const risky = w.verdict === 'has_work' || w.verdict === 'unknown';
    const text = `Move the worktree ${name} to the trash? It can be restored.${
      risky ? '\nIt has work that is not pushed or not saved; the trash keeps every file.' : ''
    }`;
    if (window.confirm(text)) void act('trash');
  };
  const release = () => {
    const path = trash?.trash_path ?? w.path;
    const text = `Fleet Deck stops tracking this folder. Nothing is moved or deleted. Check the folder yourself: ${path}.`;
    if (window.confirm(text)) void act('release');
  };
  const restore = (
    <button
      type="button"
      className="fd-ghost fd-sm"
      disabled={busy}
      onClick={() => void act('restore')}
    >
      <Icon of={ArchiveRestore} size={14} /> Restore
    </button>
  );
  const purge = (
    <button
      type="button"
      className="fd-danger fd-sm"
      disabled={busy}
      onClick={() => setPurging(true)}
    >
      <Icon of={Trash2} size={14} /> Delete permanently
    </button>
  );
  const actions = (() => {
    if (onAction === undefined) return null;
    if (trash === undefined) {
      return !w.live && w.verdict !== 'missing' ? (
        <button type="button" className="fd-ghost fd-sm" disabled={busy} onClick={moveToTrash}>
          <Icon of={Trash2} size={14} /> Move to trash
        </button>
      ) : null;
    }
    if (trash.state === 'trashed') {
      return (
        <>
          {restore}
          {purging ? null : purge}
        </>
      );
    }
    if (trash.state === 'failed') {
      return (
        <>
          {trash.phase === 'trash' || trash.phase === 'restore' ? restore : null}
          {trash.phase === 'purge' && !purging ? purge : null}
          <button type="button" className="fd-ghost fd-sm" disabled={busy} onClick={release}>
            Stop tracking
          </button>
        </>
      );
    }
    return null;
  })();
  const trashPath = trash?.trash_path ?? '';
  const trashName = basename(trashPath);
  return (
    <li className="fd-wt-row" data-verdict={w.verdict}>
      <div className="fd-wt-line">
        <span className="fd-badge fd-caps" data-tone={verdict.tone}>
          {verdict.label}
        </span>
        <span className="fd-wt-name" title={w.path}>
          {name}
        </span>
        {w.branch === null ? null : <span className="fd-badge">{w.branch}</span>}
        <a
          className="fd-wt-session"
          href={formatHash({
            name: 'session',
            station,
            session: w.session_id,
            tab: 'chat',
          })}
        >
          {w.title ?? w.session_id}
        </a>
      </div>
      <p className="fd-wt-evidence">
        {count(w.dirty)} changed · {count(w.ignored)} ignored · {count(w.unpushed)} commits not on{' '}
        {compared}
        {w.last_commit === null
          ? ''
          : ` · last commit “${w.last_commit.subject}” ${relativeAge(w.last_commit.at, now)}`}
      </p>
      {w.verdict === 'unknown' && w.note !== null ? <p className="fd-wt-note">{w.note}</p> : null}
      {!w.registered && w.exists ? (
        <p className="fd-wt-note" data-tone="act">
          Git does not list this worktree.
        </p>
      ) : null}
      {trash?.state === 'trashed' ? (
        <>
          <p className="fd-wt-note">
            In the trash since{' '}
            {relativeAge(trash.trashed_at, now) === 'now'
              ? 'now'
              : `${relativeAge(trash.trashed_at, now)} ago`}
          </p>
          <p className="fd-wt-path" title={trash.trash_path}>
            {trash.trash_path}
          </p>
        </>
      ) : null}
      {trash?.state === 'failed' ? (
        <p className="fd-wt-note" data-tone="act">
          The last step failed: {trash.message ?? 'no reason given'}
        </p>
      ) : null}
      {trash !== undefined && IN_PROGRESS[trash.state] !== undefined ? (
        <p className="fd-wt-note">{IN_PROGRESS[trash.state]}</p>
      ) : null}
      {actions === null ? null : <div className="fd-wt-actions">{actions}</div>}
      {purging && onAction !== undefined ? (
        <div className="fd-wt-confirm">
          <p className="fd-wt-note">
            This deletes {trashPath} and cannot be undone. The branch stays. Type {trashName} to
            confirm.
          </p>
          <div className="fd-wt-actions">
            <input
              type="text"
              aria-label="Type the folder name to confirm"
              value={typed}
              disabled={busy}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setTyped(event.target.value)}
            />
            <button
              type="button"
              className="fd-danger fd-sm"
              disabled={busy || !confirmMatches(typed, trashPath)}
              onClick={() => void act('purge', typed)}
            >
              Delete
            </button>
            <button
              type="button"
              className="fd-sm"
              disabled={busy}
              onClick={() => {
                setPurging(false);
                setTyped('');
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}
      {outcome === null ? null : outcome.ok ? (
        <p className="fd-wt-note" role="status">
          {outcome.text}
        </p>
      ) : (
        <p className="fd-wt-note" role="alert" data-tone="act">
          {outcome.text}
        </p>
      )}
      {command === null ? null : <CopyCommand command={command} />}
    </li>
  );
}

type WorktreeState =
  | { readonly kind: 'idle' }
  | { readonly kind: 'loading' }
  | { readonly kind: 'ready'; readonly worktrees: readonly WorktreeInfo[] }
  | { readonly kind: 'error'; readonly text: string; readonly status: number | null };

const failure = (error: unknown): WorktreeState => ({
  kind: 'error',
  status: error instanceof ApiError ? error.status : null,
  text: error instanceof Error ? error.message : 'The worktrees did not load.',
});

/** The worktree inventory of one station (DL-142) with the trash buttons (DL-150); loads on the first open. */
function Worktrees({
  station,
  stationId,
  load,
  act,
  now,
}: {
  readonly station: string;
  readonly stationId: string;
  readonly load: (station: string) => Promise<{ readonly worktrees: readonly WorktreeInfo[] }>;
  readonly act?: StationActions['worktreeAction'];
  readonly now: number;
}) {
  const [state, setState] = useState<WorktreeState>({ kind: 'idle' });
  const pollUntil = useRef(0);
  const run = () => {
    setState({ kind: 'loading' });
    load(station).then(
      (list) => setState({ kind: 'ready', worktrees: list.worktrees }),
      (error: unknown) => setState(failure(error)),
    );
  };
  // The list keeps its last answer while the inventory reloads after a command.
  const refresh = () => {
    load(station).then(
      (list) => setState({ kind: 'ready', worktrees: list.worktrees }),
      () => setState((current) => (current.kind === 'ready' ? { ...current } : current)),
    );
  };
  useEffect(() => {
    if (state.kind !== 'ready' || Date.now() >= pollUntil.current) return;
    const working = state.worktrees.some(
      (w) => w.trash !== undefined && IN_PROGRESS[w.trash.state] !== undefined,
    );
    if (!working) return;
    const timer = setTimeout(refresh, POLL_MS);
    return () => clearTimeout(timer);
  }, [state]);
  const rowAction = (sessionId: string): RowAction | undefined =>
    act === undefined
      ? undefined
      : async (action, confirmName) => {
          const outcome = await act(station, action, sessionId, confirmName);
          if (outcome.ok) {
            pollUntil.current = Date.now() + POLL_LIMIT_MS;
            refresh();
          }
          return outcome;
        };
  return (
    <details
      className="fd-wt"
      onToggle={(event) => {
        if (event.currentTarget.open && state.kind === 'idle') run();
      }}
    >
      <summary className="fd-label">
        Worktrees{state.kind === 'ready' ? ` (${state.worktrees.length})` : ''}
      </summary>
      <div className="fd-wt-body">
        <button
          type="button"
          className="fd-ghost fd-sm fd-wt-refresh"
          disabled={state.kind === 'loading'}
          onClick={run}
        >
          <Icon of={RotateCcw} size={14} /> Refresh
        </button>
        {state.kind === 'loading' ? <p className="fd-note">Loading…</p> : null}
        {state.kind === 'error' ? (
          state.status === 403 ? (
            <p className="fd-note">Only the owner can see worktrees.</p>
          ) : state.status === 502 ? (
            <p className="fd-note">This station does not report worktrees. Update the station.</p>
          ) : (
            <p role="alert">{state.text}</p>
          )
        ) : null}
        {state.kind === 'ready' ? (
          state.worktrees.length === 0 ? (
            <p className="fd-note">
              No worktrees. A session started with “A new worktree” gets one.
            </p>
          ) : (
            <>
              <ul className="fd-wt-list">
                {state.worktrees.map((w) => (
                  <WorktreeRow
                    key={w.session_id}
                    worktree={w}
                    station={stationId}
                    now={now}
                    onAction={rowAction(w.session_id)}
                  />
                ))}
              </ul>
              <p className="fd-note">
                Move to trash keeps every file; Restore puts the worktree back. Delete permanently
                removes a trashed folder; the branch stays.
              </p>
            </>
          )
        ) : null}
      </div>
    </details>
  );
}

/** Link health and board freshness are different facts. Never invent metadata. */
export function StationsPanel({ stations, sessions, freshness, actions }: Props) {
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const now = useNow(undefined, 30000);
  const canPair = actions?.canAdmin === true && freshness === 'live';
  return (
    <Page
      title="Stations"
      count={stations.length}
      className="fd-stations"
      actions={
        actions?.canAdmin === true ? (
          <button
            type="button"
            className="fd-primary"
            disabled={!canPair}
            onClick={() => setAdding(true)}
          >
            <Icon of={Plus} /> Add station
          </button>
        ) : undefined
      }
    >
      {actions === undefined || actions.canAdmin ? null : (
        <p className="fd-note">Pairing needs the admin scope.</p>
      )}
      {adding && actions !== undefined ? (
        <PairingDialog actions={actions} onClose={() => setAdding(false)} />
      ) : null}
      {removeError === null ? null : <p role="alert">{removeError}</p>}
      {stations.length === 0 ? (
        <EmptyState
          icon={Server}
          title="No paired stations"
          body="Pair a station to start sessions on its machine."
        />
      ) : null}
      <ul className="fd-station-list">
        {stations.map((station) => {
          const cards = sessions.filter((card) => card.station_id === station.station_id);
          const active = cards.filter((card) =>
            ['starting', 'running', 'stopping'].includes(card.summary.state),
          ).length;
          const info = station.info ?? null;
          const up = freshness !== 'stale' && station.reachable;
          return (
            <li
              className="fd-station fd-panel"
              key={station.station_id}
              data-station={station.station_id}
            >
              <div className="fd-station-head">
                <h3>{station.name}</h3>
                <span
                  className="fd-badge fd-health"
                  data-tone={up ? 'ok' : 'hazard'}
                  data-health={freshness === 'stale' ? 'stale' : station.reachable ? 'up' : 'down'}
                >
                  {freshness === 'stale'
                    ? 'Stale link status'
                    : station.reachable
                      ? 'Link up'
                      : 'Unreachable'}
                </span>
                {info?.version == null ? null : <span className="fd-badge">{info.version}</span>}
              </div>
              {freshness === 'stale' ? (
                <p className="fd-note">
                  Last reported: {station.reachable ? 'link up' : 'unreachable'}.
                </p>
              ) : null}
              <dl className="fd-facts">
                <dt>Station ID</dt>
                <dd>{station.station_id}</dd>
                <dt>Key fingerprint</dt>
                <dd>
                  <Fingerprint value={station.key} />
                </dd>
                <dt>Direction</dt>
                <dd>
                  {station.direction === 'deck_dials'
                    ? 'Deck dials station'
                    : station.direction === 'station_dials'
                      ? 'Station dials deck'
                      : NONE}
                </dd>
                <dt>Link address</dt>
                <dd>{station.dial_url ?? 'Dial-only (no stored address)'}</dd>
                <dt>Last seen</dt>
                <dd
                  title={
                    station.last_seen === null ||
                    Number.isNaN(new Date(station.last_seen).getTime())
                      ? undefined
                      : new Date(station.last_seen).toISOString()
                  }
                >
                  {station.last_seen === null ? NONE : relativeAge(station.last_seen, now)}
                </dd>
                <dt>Sessions</dt>
                <dd>{info?.max_sessions == null ? active : `${active} / ${info.max_sessions}`}</dd>
              </dl>
              <h4>Configured providers</h4>
              {info === null ? (
                <p className="fd-note">Reported when the station links to this deck.</p>
              ) : info.providers.length === 0 ? (
                <p className="fd-note">No provider configured.</p>
              ) : (
                <ul className="fd-providers">
                  {info.providers.map((p) => (
                    <li
                      key={p.instance}
                      className="fd-badge"
                      data-instance={p.instance}
                      data-tone={
                        p.auth === 'signed_in' ? 'ok' : p.auth === 'signed_out' ? 'hazard' : 'muted'
                      }
                      title={p.instance}
                    >
                      {capitalize(p.provider)}
                      {p.version === null ? '' : ` ${p.version}`} · {authLabel(p.auth)}
                    </li>
                  ))}
                </ul>
              )}
              {actions?.canAdmin === true && actions.shells !== undefined ? (
                info?.allow_shell === false ? (
                  <section className="fd-shells" aria-label={`Shells on ${station.name}`}>
                    <h4>Shells</h4>
                    <p className="fd-note">
                      Shells are off on this station. Set allow_shell = true in station.toml.
                    </p>
                  </section>
                ) : (
                  <ShellPanel
                    station={station.name}
                    actions={actions.shells}
                    disabled={!canPair || !station.reachable}
                  />
                )
              ) : null}
              {actions?.worktrees === undefined ? null : (
                <Worktrees
                  station={station.name}
                  stationId={station.station_id}
                  load={actions.worktrees}
                  act={actions.worktreeAction}
                  now={now}
                />
              )}
              {actions?.canAdmin === true ? (
                <div className="fd-station-foot">
                  {confirm === station.name ? (
                    <p>
                      Remove {station.name}? Its link closes and its cards leave the board.{' '}
                      <button
                        type="button"
                        className="fd-danger fd-sm"
                        disabled={!canPair}
                        onClick={() => {
                          setConfirm(null);
                          setRemoveError(null);
                          void actions.remove(station.name).catch((e: unknown) => {
                            setRemoveError(e instanceof Error ? e.message : 'Remove failed');
                          });
                        }}
                      >
                        Confirm remove
                      </button>{' '}
                      <button type="button" className="fd-sm" onClick={() => setConfirm(null)}>
                        Cancel
                      </button>
                    </p>
                  ) : (
                    <button
                      type="button"
                      className="fd-danger fd-sm"
                      disabled={!canPair}
                      onClick={() => setConfirm(station.name)}
                    >
                      Remove station
                    </button>
                  )}
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    </Page>
  );
}
