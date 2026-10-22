// DL-142, DL-150: the worktree inventory of a station card (the Worktrees fold) with the trash
// buttons. Moved out of StationsPanel.tsx (the 800-line module rule); the look is in stations.css.
import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { WorktreeInfo, WorktreeVerdict } from '../gen';
import { ApiError } from '../runtime/api';
import { ArchiveRestore, Check, Copy, GitBranch, Icon, RotateCcw, Trash2 } from '../ui/icons';
import type { Outcome } from '../session/commands';
import { confirmMatches, removeCommand, type StationActions, type WorktreeAction } from './pairing';

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
        {w.branch === null ? null : (
          <span className="fd-wt-branch">
            <Icon of={GitBranch} size={12} />
            {w.branch}
          </span>
        )}
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
      {actions === null && command === null ? null : (
        <div className="fd-wt-actions">
          {actions}
          {command === null ? null : <CopyCommand command={command} />}
        </div>
      )}
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
export function Worktrees({
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
      <summary className="fd-label" data-variant="toggle">
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
