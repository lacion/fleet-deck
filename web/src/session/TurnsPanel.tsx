import { useEffect, useRef, useState } from 'react';
import { Diff } from '../chat/Diff';
import type {
  DiffSummary,
  RevertResolution,
  SessionBase,
  SessionDiff,
  SessionSummary,
  TurnDiff,
} from '../gen';
import { providerLabel } from '../board/lanes';
import { diffReview, type ReviewProps } from '../review/diffReview';
import { ImportPrompt } from '../review/ImportPrompt';
import { spawnSnapshot, turnSnapshot } from '../review/placement';
import { ReviewBar } from '../review/ReviewBar';
import { addedText, removedText } from '../ui/format';
import { ChevronRight, Icon, RefreshCw } from '../ui/icons';
import { sessionRef } from '../ui/localSet';
import { EmptyState } from '../ui/page';
import { BranchesView } from './BranchesView';
import { allChangedPaths, turnForPath } from './changedFiles';
import {
  checkpointText,
  hasSnapshot,
  NOT_A_GIT_WORKTREE,
  outsideGit,
  sharedNote,
  sharedReason,
} from './checkpoint';
import type { Outcome, SessionActions } from './commands';
import { hasChanges } from './changes';
import { FileDiff } from './icons';
import {
  type FinalizedTurn,
  mergeTurnPaths,
  revertBlock,
  revertHolds,
  revertText,
  setDiffTotal,
  setTurnPaths,
  turnPathsOf,
} from './workspace';

type Props = {
  readonly summary: SessionSummary;
  readonly turns: readonly FinalizedTurn[];
  readonly actions: SessionActions | undefined;
  /** The board and the station are live and the session has a process. */
  readonly drive: boolean;
  /** The board and the station are live (a resolution also works on an ended session). */
  readonly live: boolean;
  readonly busy: boolean;
  readonly act: (call: () => Promise<Outcome>) => void;
  /** A file name of a diff opens that file in the Files panel. */
  readonly onOpenFile?: ((path: string) => void) | undefined;
  /** The station of the session: the key of its card total. */
  readonly station?: string | undefined;
  /** The review comments of the session (DL-179); absent: the diffs have no comment controls. */
  readonly review?: ReviewProps | undefined;
  /** DL-191: the login is the owner and the deck can list branches: the "Branches" view is offered. */
  readonly branchesOffered?: boolean | undefined;
  /** FG-10: the project of the session; the Branches view then shows "Other stations". */
  readonly projectId?: string | undefined;
  /** §4.5: a changed file of the Properties panel; "Whole session" shows and scrolls to it. */
  readonly fileRequest?: FileRequest | undefined;
};

/** A request to show one file; `seq` makes a second request of the same path new. */
export type FileRequest = { readonly path: string; readonly seq: number };

/**
 * The turn diffs in flight (by turn id). A second Show press while a load runs starts no second
 * request; a Hide press during a load drops its late answer, so the diff stays closed.
 */
export class DiffFlight {
  private readonly pending = new Set<string>();
  private readonly hidden = new Set<string>();

  /** `true`: start a load of `turnId`; `false`: one runs (it shows its answer again). */
  start(turnId: string): boolean {
    this.hidden.delete(turnId);
    if (this.pending.has(turnId)) return false;
    this.pending.add(turnId);
    return true;
  }

  /** The load of `turnId` runs and no Hide came after its Show. */
  loading(turnId: string): boolean {
    return this.pending.has(turnId) && !this.hidden.has(turnId);
  }

  /** A Hide press: a running load of `turnId` drops its answer. */
  hide(turnId: string): void {
    if (this.pending.has(turnId)) this.hidden.add(turnId);
  }

  /** The load of `turnId` ended; `true` when its answer may show (no Hide came). */
  finish(turnId: string): boolean {
    this.pending.delete(turnId);
    return !this.hidden.delete(turnId);
  }
}

type View = 'spawn' | 'turns' | 'branches';

const VIEW_LABEL: Record<View, string> = {
  spawn: 'Whole session',
  turns: 'By turn',
  branches: 'Branches',
};

/** The latest revert, and the owner's resolution of one that needs reconciliation (DL-097). */
export function RevertStatus({ summary, actions, live, busy, act }: Props) {
  const revert = summary.revert;
  if (!revert) return null;
  const canResolve =
    live && actions?.canDrive === true && !busy && revert.state === 'reconciliation_required';
  return (
    <section className="fd-revert" aria-label="Revert" data-revert-state={revert.state}>
      <h3 className="fd-label">Revert</h3>
      <p role={revert.state === 'reconciliation_required' ? 'alert' : 'status'}>
        {revertText(revert.state, revert.reason)}
      </p>
      {revert.state === 'reconciliation_required' ? (
        <>
          <p className="fd-note">
            Check the conversation and the files. "Retry the file restore" restores the files again;
            it works only after a confirmed conversation rollback and while the files still equal a
            checkpoint. "Mark as reconciled" accepts the current state; no earlier turn can be
            reverted after it.
          </p>
          <div className="fd-revert-actions">
            {(['retry_restore', 'reconciled'] as const).map((r: RevertResolution) => (
              <button
                key={r}
                type="button"
                className="fd-sm"
                disabled={!canResolve}
                onClick={() => actions && act(() => actions.resolveRevert(revert.revert_id, r))}
              >
                {r === 'retry_restore' ? 'Retry the file restore' : 'Mark as reconciled'}
              </button>
            ))}
          </div>
        </>
      ) : null}
    </section>
  );
}

/** The notes above a "Whole session" diff: a shared folder (DL-159) and a guessed start commit. */
export function SinceNotes({
  base,
  scope,
}: {
  readonly base: SessionBase | undefined;
  readonly scope: SessionDiff['scope'];
}) {
  return (
    <>
      {scope === 'session_files' ? (
        <p className="fd-note fd-since-note" data-note="shared-folder">
          This session works in a shared folder. The diff shows only the files that this session
          changed.
        </p>
      ) : null}
      {base?.inferred === true ? (
        <p className="fd-note fd-since-note" data-note="guessed-base">
          The start commit was guessed ({base.git_ref ?? 'a commit'}), because this session started
          before Fleet Deck recorded it.
        </p>
      ) : null}
    </>
  );
}

/** The file header of `path` inside `root` gets the scroll and the focus; `true` when it has them. */
function focusFileHead(root: HTMLElement | null | undefined, path: string): boolean {
  const heads = root?.querySelectorAll<HTMLElement>('.fd-dl-file') ?? [];
  const head = [...heads].find((h) => h.getAttribute('aria-label') === path);
  if (head === undefined) return false;
  head.scrollIntoView({ block: 'start' });
  head.focus({ preventScroll: true });
  return document.activeElement === head;
}

/** The working tree of the session against its base commit, with review comments (R1.2, DL-179). */
function SinceSpawn({
  summary,
  actions,
  station,
  review,
  onOpenFile,
  onRefresh,
  target,
  onTargetShown,
}: {
  readonly summary: SessionSummary;
  readonly actions: SessionActions | undefined;
  readonly station: string | undefined;
  readonly review: ReviewProps | undefined;
  readonly onOpenFile?: ((path: string) => void) | undefined;
  readonly onRefresh: () => void;
  /** The file to scroll to and focus when the diff shows (a Properties panel request). */
  readonly target?: FileRequest | undefined;
  /** The target file got the focus: the request is spent (a later load does not move again). */
  readonly onTargetShown?: (() => void) | undefined;
}) {
  const [state, setState] = useState<{
    readonly diff: SessionDiff | null;
    readonly error: string | null;
  }>({ diff: null, error: null });
  const files = actions?.files ?? null;
  const filesRef = useRef(files);
  filesRef.current = files;
  const ref = sessionRef(station ?? '', summary.session_id);
  // The parent remounts this component to load again (a turn ended, or "Refresh").
  useEffect(() => {
    const read = filesRef.current;
    if (!read) return undefined;
    let live = true;
    read.sessionDiff().then(
      (diff) => {
        if (!live) return;
        setState({ diff, error: null });
        setDiffTotal(ref, diff.summary, allChangedPaths(diff.patch));
      },
      (e: unknown) => {
        if (live) {
          setState({
            diff: null,
            error: e instanceof Error ? e.message : 'The diff request failed.',
          });
        }
      },
    );
    return () => {
      live = false;
    };
  }, [ref]);
  const base = summary.base;
  const { diff, error } = state;
  const box = useRef<HTMLDivElement>(null);
  const shown = target?.seq;
  // The file header of the requested file (its button names the path) gets the focus.
  useEffect(() => {
    if (diff === null || target === undefined) return;
    if (focusFileHead(box.current, target.path)) onTargetShown?.();
  }, [diff, shown]);
  const hasPatch = diff !== null && diff.patch.trim() !== '';
  return (
    <div className="fd-since" data-view="spawn" ref={box}>
      {/* One head row: the totals first (the key fact), then where they count from in --dim,
          then Refresh at the end of the row; the notes follow as --faint lines. */}
      <div className="fd-since-head">
        {diff !== null && hasPatch ? <SinceTotal stat={diff.summary} /> : null}
        <span className="fd-since-base">
          {hasPatch ? 'since' : 'Changes since'}{' '}
          {base ? (
            <>
              {base.git_ref ? (
                <>
                  <code>{base.git_ref}</code> at{' '}
                </>
              ) : null}
              <code>{base.oid.slice(0, 8)}</code>
            </>
          ) : (
            'the session started'
          )}
        </span>
        <button
          type="button"
          className="fd-iconbtn fd-sm"
          aria-label="Refresh"
          title="Load the diff again"
          onClick={onRefresh}
          disabled={!files}
        >
          <Icon of={RefreshCw} size={14} />
        </button>
      </div>
      <SinceNotes base={base} scope={diff?.scope} />
      {review !== undefined && diff !== null ? (
        <ImportPrompt
          actions={review.actions}
          session={review.key}
          snapshot={spawnSnapshot(diff.base_oid)}
          canWrite={review.canWrite}
          onDone={review.state.reload}
        />
      ) : null}
      {error !== null ? (
        <p role="alert" className="fd-since-alert">
          {error}
        </p>
      ) : null}
      {error === null && diff === null ? <p className="fd-note">Loading the diff…</p> : null}
      {diff !== null && !hasChanges(diff.summary) && diff.patch.trim() === '' ? (
        <p className="fd-note">No file changes since the session started.</p>
      ) : null}
      {diff !== null && hasPatch ? (
        <Diff
          patch={diff.patch}
          truncated={diff.truncated}
          id="since-spawn"
          onOpenFile={onOpenFile}
          review={
            review === undefined ? undefined : diffReview(review, spawnSnapshot(diff.base_oid))
          }
        />
      ) : null}
    </div>
  );
}

/**
 * The counts of a diff, the same order as the properties panel: `+9 −3 1 file`. The parent
 * (`.fd-turn-stat`) is an inline flex row, so its gap spaces the parts.
 */
function TurnStat({ stat }: { readonly stat: DiffSummary }) {
  return (
    <>
      {stat.insertions > 0 ? <span className="fd-add">{addedText(stat.insertions)}</span> : null}
      {stat.deletions > 0 ? <span className="fd-del">{removedText(stat.deletions)}</span> : null}
      <span className="fd-turn-files">
        {stat.files_changed} {stat.files_changed === 1 ? 'file' : 'files'}
      </span>
    </>
  );
}

/** The totals of the session diff, the key fact of its head row: `+1,286 −400 · 3 files`. */
export function SinceTotal({ stat }: { readonly stat: DiffSummary }) {
  const lines = stat.insertions > 0 || stat.deletions > 0;
  return (
    <span className="fd-since-total">
      {stat.insertions > 0 ? <span className="fd-add">{addedText(stat.insertions)}</span> : null}
      {stat.deletions > 0 ? <span className="fd-del">{removedText(stat.deletions)}</span> : null}
      <span className="fd-since-files">
        {lines ? '· ' : ''}
        {stat.files_changed} {stat.files_changed === 1 ? 'file' : 'files'}
      </span>
    </span>
  );
}

/** `turn:<from>..<to>` as its two commits; `null` for another text. */
function parseTurnSnapshot(snapshot: string): { from: string; to: string } | null {
  const [from, to, ...rest] = snapshot.replace(/^turn:/, '').split('..');
  return from !== undefined && to !== undefined && rest.length === 0 ? { from, to } : null;
}

/** A turn diff that the Changes tab loaded; `snapshot` is set when its comments are available. */
export type LoadedDiff = {
  readonly diff: TurnDiff;
  readonly snapshot: string | null;
  /** The comments of this turn could not be loaded: the plain diff shows with a note. */
  readonly fellBack: boolean;
};

/**
 * The turn diff shows (the button hides it). A failed load (an error text) is not open: the
 * button says "Show the diff" and a press loads again.
 */
export function diffOpen(diff: LoadedDiff | string | undefined): boolean {
  return diff !== undefined && typeof diff !== 'string';
}

/** Why the turn list offers no revert, with the name of the provider. */
export function rollbackNote(provider: SessionSummary['provider']): string {
  return provider === null || provider === 'unknown'
    ? 'This session cannot be rolled back, so the board offers no revert.'
    : `${providerLabel(provider)} sessions cannot be rolled back, so the board offers no revert.`;
}

/** The review bar of the Changes tab. */
function Bar({ review }: { readonly review: ReviewProps | undefined }) {
  return review === undefined ? null : (
    <ReviewBar
      state={review.state}
      actions={review.actions}
      session={review.key}
      canWrite={review.canWrite}
      self={review.self}
    />
  );
}

/** The Changes tab: the diff since the session started, or the finalized turns (a diff and a revert each). */
export function TurnsPanel(props: Props) {
  const { summary, turns, actions, drive, busy, act, onOpenFile, station, review } = props;
  const branchesRead = actions?.branches ?? null;
  const branchDiffRead = actions?.branchDiff ?? null;
  const hasBranches =
    props.branchesOffered === true && branchesRead !== null && branchDiffRead !== null;
  const [confirm, setConfirm] = useState<string | null>(null);
  const [diffs, setDiffs] = useState<Record<string, LoadedDiff | string>>({});
  const [chosen, setChosen] = useState<View | null>(null);
  const [loads, setLoads] = useState(0);
  const hasBase = Boolean(summary.base);
  const files = actions?.files ?? null;
  const flight = useRef(new DiffFlight());
  const [, setFlights] = useState(0);
  // A file request of the Properties panel opens "Whole session" on that file; without a spawn base
  // it opens the newest turn diff that lists the file (`targetTurn`).
  const [target, setTarget] = useState<FileRequest>();
  const [targetTurn, setTargetTurn] = useState<string>();
  const request = props.fileRequest;
  const turnsBox = useRef<HTMLOListElement>(null);
  const stationRef = station === undefined ? undefined : sessionRef(station, summary.session_id);
  const showDiff = (turnId: string) => {
    if (!files) return;
    const started = flight.current.start(turnId);
    setFlights((n) => n + 1);
    if (!started) return;
    const turn = turns.find((t) => t.turnId === turnId);
    const from = turn?.start?.state === 'available' ? turn.start.commit : null;
    const to = turn?.end?.state === 'available' ? turn.end.commit : null;
    const plain = (fellBack: boolean): Promise<LoadedDiff> =>
      files.diff(turnId).then((diff) => ({ diff, snapshot: null, fellBack }));
    // The stable checkpoint selector; any failure shows the plain diff without comment controls.
    const load: Promise<LoadedDiff> =
      review === undefined || from === null || to === null
        ? plain(false)
        : review.actions.turnDiff(review.key, from, to).then(
            (diff) => ({
              diff,
              snapshot: turnSnapshot(diff.from_commit, diff.to_commit),
              fellBack: false,
            }),
            () => plain(true),
          );
    // A Hide press during the load drops the answer (DiffFlight).
    const settle = (shown: LoadedDiff | string) => {
      if (flight.current.finish(turnId)) setDiffs((all) => ({ ...all, [turnId]: shown }));
      setFlights((n) => n + 1);
      // §4.5: the files of a kept turn also list in the Changes row of the Properties panel.
      if (typeof shown !== 'string' && station !== undefined) {
        const paths = allChangedPaths(shown.diff.patch);
        const key = sessionRef(station, summary.session_id);
        mergeTurnPaths(key, turn, paths);
        if (turn?.reverted !== true) setTurnPaths(key, turnId, paths);
      }
    };
    load.then(settle, (e: unknown) =>
      settle(e instanceof Error ? e.message : 'The diff request failed.'),
    );
  };
  const hideDiff = (turnId: string) => {
    flight.current.hide(turnId);
    setFlights((n) => n + 1);
    setDiffs((all) => {
      const { [turnId]: _hidden, ...rest } = all;
      return rest;
    });
  };
  // A click on a comment chip chooses the view of its thread and loads the diff of its turn.
  const latest = useRef(showDiff);
  latest.current = showDiff;
  const turnList = useRef(turns);
  turnList.current = turns;
  useEffect(() => {
    if (request === undefined) return;
    if (hasBase) {
      setTarget(request);
      setTargetTurn(undefined);
      setChosen('spawn');
      return;
    }
    const turn =
      stationRef === undefined
        ? undefined
        : turnForPath(turnList.current, turnPathsOf(stationRef), request.path);
    if (turn === undefined) return;
    setTarget(request);
    setTargetTurn(turn.turnId);
    setChosen('turns');
    if (!diffOpen(diffs[turn.turnId])) latest.current(turn.turnId);
  }, [request?.seq]);
  const focusNonce = review?.focus?.nonce;
  const focusThread = review?.focus?.threadId;
  const threadList = review?.state.data?.threads;
  useEffect(() => {
    if (focusNonce === undefined || focusThread === undefined) return;
    const first = threadList?.find((t) => t.thread_id === focusThread)?.comments[0];
    if (first === undefined) return;
    if (first.snapshot.startsWith('spawn:')) {
      setChosen('spawn');
      return;
    }
    setChosen('turns');
    const pair = parseTurnSnapshot(first.snapshot);
    const turn =
      pair === null
        ? undefined
        : turnList.current.find(
            (t) =>
              t.start?.state === 'available' &&
              t.start.commit === pair.from &&
              t.end?.state === 'available' &&
              t.end.commit === pair.to,
          );
    if (turn !== undefined) latest.current(turn.turnId);
    // Only a new click (the nonce) moves the view; a reload of the threads does not.
  }, [focusNonce]);
  // A folder outside Git has no branches: no Branches view.
  const noGit = outsideGit(turns);
  // A guessed start commit makes the whole-session diff unreliable: open "By turn" (DL-159).
  const views: readonly View[] = [
    ...(hasBase ? (['spawn'] as const) : []),
    'turns',
    ...(hasBranches && !noGit ? (['branches'] as const) : []),
  ];
  const fallback: View = hasBase && summary.base?.inferred !== true ? 'spawn' : 'turns';
  const view: View = chosen !== null && views.includes(chosen) ? chosen : fallback;
  // The requested turn diff shows: its file header gets the scroll and the focus (a failed load
  // spends the request).
  const targetDiff = targetTurn === undefined ? undefined : diffs[targetTurn];
  useEffect(() => {
    if (target === undefined || targetTurn === undefined || view !== 'turns') return;
    if (typeof targetDiff === 'string') {
      setTarget(undefined);
      return;
    }
    if (targetDiff === undefined) return;
    const item = turnsBox.current?.querySelector<HTMLElement>(`li[data-turn="${targetTurn}"]`);
    if (focusFileHead(item, target.path)) setTarget(undefined);
  }, [targetDiff, target?.seq, view]);
  if (turns.length === 0 && !hasBase && !hasBranches) return null;
  const toggle =
    views.length >= 2 ? (
      <div className="fd-viewtoggle fd-seg" role="group" aria-label="Changes view">
        {views.map((v) => (
          <button key={v} type="button" aria-pressed={view === v} onClick={() => setChosen(v)}>
            {VIEW_LABEL[v]}
          </button>
        ))}
      </div>
    ) : null;
  if (view === 'branches' && branchesRead !== null && branchDiffRead !== null) {
    return (
      <section className="fd-turns" aria-label="Turns">
        {toggle}
        <BranchesView
          load={branchesRead}
          loadDiff={branchDiffRead}
          onOpenFile={onOpenFile}
          fleet={
            props.projectId === undefined || station === undefined
              ? undefined
              : { projectId: props.projectId, station, session: summary.session_id }
          }
        />
      </section>
    );
  }
  if (view === 'spawn') {
    return (
      <section className="fd-turns" aria-label="Turns">
        {toggle}
        <SinceSpawn
          key={`${turns.length}:${loads}`}
          summary={summary}
          actions={actions}
          station={station}
          review={review}
          onOpenFile={onOpenFile}
          onRefresh={() => setLoads((n) => n + 1)}
          target={target}
          onTargetShown={() => setTarget(undefined)}
        />
        <Bar review={review} />
      </section>
    );
  }
  if (turns.length === 0) {
    return (
      <section className="fd-turns" aria-label="Turns">
        {toggle}
        <p className="fd-note">No finished turns yet. Changes appear here when a turn ends.</p>
        <Bar review={review} />
      </section>
    );
  }
  // A folder outside Git: no turn can have a file snapshot. One empty state (§4.7), not a list
  // of rows that all say "No snapshot".
  if (noGit) {
    return (
      <section className="fd-turns" aria-label="Turns">
        {toggle}
        <EmptyState
          icon={FileDiff}
          title="No changes to show"
          body="This folder is not a Git repository, so the board keeps no file snapshots for this session."
        />
        <Bar review={review} />
      </section>
    );
  }
  const caps = summary.capabilities;
  // No turn has a snapshot: the reason shows once above the list, and the rows that share it
  // only say "No snapshot".
  const shared = sharedReason(turns);
  const sharedBy = (reason: string) =>
    shared !== null && (shared === NOT_A_GIT_WORKTREE || reason === shared);
  return (
    <section className="fd-turns" aria-label="Turns">
      {toggle}
      <h3 className="fd-vh">Turns</h3>
      {/* Without a snapshot no revert is possible anyway: the note would explain nothing. */}
      {caps?.rollback === true || !hasSnapshot(turns) ? null : (
        <p className="fd-note">{rollbackNote(summary.provider)}</p>
      )}
      {shared === null ? null : (
        <p className="fd-note" data-note="no-snapshots">
          {sharedNote(shared)}
        </p>
      )}
      <ol ref={turnsBox}>
        {turns.map((t, i) => {
          const block = revertBlock(t, summary, caps);
          const diff = diffs[t.turnId];
          const loaded = diffOpen(diff);
          const loading = !loaded && flight.current.loading(t.turnId);
          return (
            <li key={t.turnId} data-turn={t.turnId} data-reverted={t.reverted}>
              <span className="fd-turn-name">Turn {i + 1}</span>
              {t.diff && hasChanges(t.diff) ? (
                <span className="fd-turn-stat">
                  <TurnStat stat={t.diff} />
                </span>
              ) : t.end?.state === 'unavailable' ? (
                <span className="fd-turn-stat" title={t.end.reason}>
                  {sharedBy(t.end.reason) ? 'No snapshot' : checkpointText(t.end.reason)}
                </span>
              ) : (
                <span className="fd-turn-stat">No file changes</span>
              )}
              {t.reverted ? <span className="fd-turn-stat">reverted</span> : null}
              {hasChanges(t.diff) && files ? (
                <button
                  type="button"
                  className="fd-sm fd-turn-diff"
                  aria-expanded={loaded}
                  aria-busy={loading || undefined}
                  onClick={() => (loaded || loading ? hideDiff(t.turnId) : showDiff(t.turnId))}
                >
                  <Icon of={ChevronRight} size={14} />
                  {loaded ? 'Hide the diff' : loading ? 'Loading…' : 'Show the diff'}
                </button>
              ) : null}
              {caps?.rollback === true && !t.reverted ? (
                confirm === t.turnId ? (
                  <span className="fd-confirm" role="group" aria-label="Confirm the revert">
                    Revert the conversation and the files to the start of this turn? This turn and
                    every later turn go away.
                    <button
                      type="button"
                      className="fd-sm fd-danger"
                      disabled={!drive || busy || block !== null}
                      onClick={() => {
                        setConfirm(null);
                        if (actions) act(() => actions.revert(t.turnId));
                      }}
                    >
                      Revert
                    </button>
                    <button type="button" className="fd-sm" onClick={() => setConfirm(null)}>
                      Cancel
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="fd-sm"
                    disabled={!drive || busy || block !== null}
                    title={block ?? undefined}
                    onClick={() => setConfirm(t.turnId)}
                  >
                    Revert to here
                  </button>
                )
              ) : null}
              {typeof diff === 'string' ? <p role="alert">{diff}</p> : null}
              {diff && typeof diff !== 'string' ? (
                <>
                  {diff.fellBack ? (
                    <p className="fd-note">
                      Comments are not available for this turn on this station.
                    </p>
                  ) : null}
                  <Diff
                    patch={diff.diff.patch}
                    truncated={diff.diff.truncated}
                    id={t.turnId}
                    onOpenFile={onOpenFile}
                    review={
                      review === undefined || diff.snapshot === null
                        ? undefined
                        : diffReview(review, diff.snapshot)
                    }
                  />
                </>
              ) : null}
            </li>
          );
        })}
      </ol>
      {revertHolds(summary.revert?.state) ? (
        <p className="fd-note">A revert holds the session; no new revert starts.</p>
      ) : null}
      <Bar review={review} />
    </section>
  );
}
