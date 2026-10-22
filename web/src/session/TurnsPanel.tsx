import { useEffect, useRef, useState } from 'react';
import { Diff } from '../chat/Diff';
import type { RevertResolution, SessionBase, SessionDiff, SessionSummary, TurnDiff } from '../gen';
import { diffReview, type ReviewProps } from '../review/diffReview';
import { ImportPrompt } from '../review/ImportPrompt';
import { spawnSnapshot, turnSnapshot } from '../review/placement';
import { ReviewBar } from '../review/ReviewBar';
import { sessionRef } from '../ui/localSet';
import { BranchesView } from './BranchesView';
import type { Outcome, SessionActions } from './commands';
import { hasChanges } from './changes';
import {
  type FinalizedTurn,
  revertBlock,
  revertHolds,
  revertText,
  setDiffTotal,
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
};

type View = 'spawn' | 'turns' | 'branches';

const VIEW_LABEL: Record<View, string> = {
  spawn: 'Since spawn',
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

/** The notes above a "Since spawn" diff: a shared folder (DL-159) and a guessed start commit. */
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
        <p className="fd-note" data-note="shared-folder">
          This session works in a shared folder. The diff shows only the files that this session
          changed.
        </p>
      ) : null}
      {base?.inferred === true ? (
        <p className="fd-note" data-note="guessed-base">
          The start commit was guessed ({base.git_ref ?? 'a commit'}), because this session started
          before Fleet Deck recorded it.
        </p>
      ) : null}
    </>
  );
}

/** The working tree of the session against its base commit, with review comments (R1.2, DL-179). */
function SinceSpawn({
  summary,
  actions,
  station,
  review,
  onOpenFile,
  onRefresh,
}: {
  readonly summary: SessionSummary;
  readonly actions: SessionActions | undefined;
  readonly station: string | undefined;
  readonly review: ReviewProps | undefined;
  readonly onOpenFile?: ((path: string) => void) | undefined;
  readonly onRefresh: () => void;
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
        setDiffTotal(ref, diff.summary);
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
  return (
    <div className="fd-since" data-view="spawn">
      <p className="fd-note">
        Since the session started
        {base ? (
          <>
            {' '}
            from <code>{base.git_ref ?? 'a commit'}</code> <code>{base.oid.slice(0, 8)}</code>
            {base.inferred ? ' (guessed)' : ''}
          </>
        ) : null}
        .{' '}
        <button type="button" className="fd-sm" onClick={onRefresh} disabled={!files}>
          Refresh
        </button>
      </p>
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
      {diff !== null && diff.patch.trim() !== '' ? (
        <>
          <p className="fd-turn-stat">
            {diff.summary.files_changed} {diff.summary.files_changed === 1 ? 'file' : 'files'}
            {diff.summary.insertions > 0 ? (
              <>
                {' '}
                <span className="fd-add">+{diff.summary.insertions}</span>
              </>
            ) : null}
            {diff.summary.deletions > 0 ? (
              <>
                {' '}
                <span className="fd-del">−{diff.summary.deletions}</span>
              </>
            ) : null}
          </p>
          <Diff
            patch={diff.patch}
            truncated={diff.truncated}
            id="since-spawn"
            onOpenFile={onOpenFile}
            review={
              review === undefined ? undefined : diffReview(review, spawnSnapshot(diff.base_oid))
            }
          />
        </>
      ) : null}
    </div>
  );
}

/** `turn:<from>..<to>` as its two commits; `null` for another text. */
function parseTurnSnapshot(snapshot: string): { from: string; to: string } | null {
  const [from, to, ...rest] = snapshot.replace(/^turn:/, '').split('..');
  return from !== undefined && to !== undefined && rest.length === 0 ? { from, to } : null;
}

/** A turn diff that the Changes tab loaded; `snapshot` is set when its comments are available. */
type LoadedDiff = {
  readonly diff: TurnDiff;
  readonly snapshot: string | null;
  /** The comments of this turn could not be loaded: the plain diff shows with a note. */
  readonly fellBack: boolean;
};

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
  const showDiff = (turnId: string) => {
    if (!files) return;
    const turn = turns.find((t) => t.turnId === turnId);
    const from = turn?.start?.state === 'available' ? turn.start.commit : null;
    const to = turn?.end?.state === 'available' ? turn.end.commit : null;
    const fail = (e: unknown) =>
      setDiffs((all) => ({
        ...all,
        [turnId]: e instanceof Error ? e.message : 'The diff request failed.',
      }));
    const plain = (fellBack: boolean) =>
      files
        .diff(turnId)
        .then(
          (diff) => setDiffs((all) => ({ ...all, [turnId]: { diff, snapshot: null, fellBack } })),
          fail,
        );
    if (review === undefined || from === null || to === null) {
      void plain(false);
      return;
    }
    // The stable checkpoint selector; any failure shows the plain diff without comment controls.
    review.actions.turnDiff(review.key, from, to).then(
      (diff) =>
        setDiffs((all) => ({
          ...all,
          [turnId]: {
            diff,
            snapshot: turnSnapshot(diff.from_commit, diff.to_commit),
            fellBack: false,
          },
        })),
      () => void plain(true),
    );
  };
  // A click on a comment chip chooses the view of its thread and loads the diff of its turn.
  const latest = useRef(showDiff);
  latest.current = showDiff;
  const turnList = useRef(turns);
  turnList.current = turns;
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
  // A guessed start commit makes the whole-session diff unreliable: open "By turn" (DL-159).
  const views: readonly View[] = [
    ...(hasBase ? (['spawn'] as const) : []),
    'turns',
    ...(hasBranches ? (['branches'] as const) : []),
  ];
  const fallback: View = hasBase && summary.base?.inferred !== true ? 'spawn' : 'turns';
  const view: View = chosen !== null && views.includes(chosen) ? chosen : fallback;
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
  const caps = summary.capabilities;
  return (
    <section className="fd-turns" aria-label="Turns">
      {toggle}
      <h3 className="fd-label">Turns</h3>
      {caps?.rollback === true ? null : (
        <p className="fd-note">
          This provider cannot roll back its conversation, so the board offers no revert. A
          file-only restore is not a revert.
        </p>
      )}
      <ol>
        {turns.map((t, i) => {
          const block = revertBlock(t, summary, caps);
          const diff = diffs[t.turnId];
          return (
            <li key={t.turnId} className="fd-panel" data-turn={t.turnId} data-reverted={t.reverted}>
              <span className="fd-turn-name">Turn {i + 1}</span>
              {t.diff && hasChanges(t.diff) ? (
                <span className="fd-turn-stat">
                  {t.diff.files_changed} {t.diff.files_changed === 1 ? 'file' : 'files'}
                  {t.diff.insertions > 0 ? (
                    <>
                      {' '}
                      <span className="fd-add">+{t.diff.insertions}</span>
                    </>
                  ) : null}
                  {t.diff.deletions > 0 ? (
                    <>
                      {' '}
                      <span className="fd-del">−{t.diff.deletions}</span>
                    </>
                  ) : null}
                </span>
              ) : t.end?.state === 'unavailable' ? (
                <span className="fd-turn-stat">no completion checkpoint: {t.end.reason}</span>
              ) : (
                <span className="fd-turn-stat">No file changes</span>
              )}
              {t.reverted ? <span className="fd-turn-stat">reverted</span> : null}
              {hasChanges(t.diff) && files ? (
                <button type="button" className="fd-sm" onClick={() => showDiff(t.turnId)}>
                  Show the diff
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
