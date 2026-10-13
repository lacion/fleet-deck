import { useEffect, useRef, useState } from 'react';
import { Diff } from '../chat/Diff';
import {
  type Note,
  notesMessage,
  notesStorageKey,
  setNotes,
  useNotes,
  withNote,
} from '../chat/notes';
import type { RevertResolution, SessionDiff, SessionSummary, TurnDiff } from '../gen';
import { sessionRef } from '../ui/localSet';
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
  /** The station of the session: the key of its line notes and of its card total. */
  readonly station?: string | undefined;
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

/** The notes bar of the Changes tab: the notes of the session, sent as one follow-up (R1.3). */
function NotesBar({
  notes,
  canSend,
  onSend,
  onClear,
}: {
  readonly notes: readonly Note[];
  readonly canSend: boolean;
  readonly onSend: () => void;
  readonly onClear: () => void;
}) {
  if (notes.length === 0) return null;
  const unsent = notes.filter((n) => !n.sent).length;
  return (
    <section className="fd-notes" aria-label="Notes">
      <h3 className="fd-label">Notes ({notes.length})</h3>
      <ul>
        {notes.map((n) => (
          <li key={`${n.file}:${n.line}`} data-sent={n.sent ? 'true' : undefined}>
            {n.file} L{n.line}: {n.text}
            {n.sent ? <span className="fd-note-sent"> (sent)</span> : null}
          </li>
        ))}
      </ul>
      <div className="fd-notes-actions">
        <button
          type="button"
          className="fd-sm"
          disabled={!canSend || unsent === 0}
          title={canSend ? undefined : 'Sending notes needs a live board and the drive scope.'}
          onClick={onSend}
        >
          Send notes
        </button>
        <button type="button" className="fd-sm" onClick={onClear}>
          Clear
        </button>
      </div>
    </section>
  );
}

/** The working tree of the session against its base commit, with line notes (R1.2, R1.3). */
function SinceSpawn({
  summary,
  actions,
  station,
  notes,
  onOpenFile,
  onRefresh,
}: {
  readonly summary: SessionSummary;
  readonly actions: SessionActions | undefined;
  readonly station: string | undefined;
  readonly notes: {
    readonly list: readonly Note[];
    readonly onSet: (file: string, line: number, text: string) => void;
  };
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
            notes={{ notes: notes.list, onSet: notes.onSet }}
          />
        </>
      ) : null}
    </div>
  );
}

/** The Changes tab: the diff since the session started, or the finalized turns (a diff and a revert each). */
export function TurnsPanel(props: Props) {
  const { summary, turns, actions, drive, busy, act, onOpenFile, station } = props;
  const [confirm, setConfirm] = useState<string | null>(null);
  const [diffs, setDiffs] = useState<Record<string, TurnDiff | string>>({});
  const [chosen, setChosen] = useState<'spawn' | 'turns' | null>(null);
  const [loads, setLoads] = useState(0);
  const notesKey = notesStorageKey(station ?? '', summary.session_id);
  const noteList = useNotes(notesKey);
  const hasBase = Boolean(summary.base);
  const view = hasBase ? (chosen ?? 'spawn') : 'turns';
  if (turns.length === 0 && !hasBase) return null;
  const onSet = (file: string, line: number, text: string) =>
    setNotes(notesKey, withNote(noteList, file, line, text));
  const sendNotes = () => {
    const unsent = noteList.filter((n) => !n.sent);
    if (!actions || unsent.length === 0) return;
    act(async () => {
      const outcome = await actions.send('follow_up', notesMessage(unsent));
      if (outcome.ok) {
        setNotes(
          notesKey,
          noteList.map((n) => (n.sent ? n : { ...n, sent: true })),
        );
      }
      return outcome;
    });
  };
  const notesBar = (
    <NotesBar
      notes={noteList}
      canSend={drive && !busy && actions?.canDrive === true}
      onSend={sendNotes}
      onClear={() => setNotes(notesKey, [])}
    />
  );
  const toggle = hasBase ? (
    <div className="fd-viewtoggle" role="group" aria-label="Changes view">
      {(['spawn', 'turns'] as const).map((v) => (
        <button
          key={v}
          type="button"
          className="fd-chip"
          aria-pressed={view === v}
          onClick={() => setChosen(v)}
        >
          {v === 'spawn' ? 'Since spawn' : 'By turn'}
        </button>
      ))}
    </div>
  ) : null;
  if (view === 'spawn') {
    return (
      <section className="fd-turns" aria-label="Turns">
        {toggle}
        <SinceSpawn
          key={`${turns.length}:${loads}`}
          summary={summary}
          actions={actions}
          station={station}
          notes={{ list: noteList, onSet }}
          onOpenFile={onOpenFile}
          onRefresh={() => setLoads((n) => n + 1)}
        />
        {notesBar}
      </section>
    );
  }
  if (turns.length === 0) {
    return (
      <section className="fd-turns" aria-label="Turns">
        {toggle}
        <p className="fd-note">No finished turns yet. Changes appear here when a turn ends.</p>
        {notesBar}
      </section>
    );
  }
  const caps = summary.capabilities;
  const files = actions?.files ?? null;
  const showDiff = (turnId: string) => {
    if (!files) return;
    void files.diff(turnId).then(
      (d) => setDiffs((all) => ({ ...all, [turnId]: d })),
      (e: unknown) =>
        setDiffs((all) => ({
          ...all,
          [turnId]: e instanceof Error ? e.message : 'The diff request failed.',
        })),
    );
  };
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
                <Diff
                  patch={diff.patch}
                  truncated={diff.truncated}
                  id={t.turnId}
                  onOpenFile={onOpenFile}
                  notes={{ notes: noteList, onSet }}
                />
              ) : null}
            </li>
          );
        })}
      </ol>
      {revertHolds(summary.revert?.state) ? (
        <p className="fd-note">A revert holds the session; no new revert starts.</p>
      ) : null}
      {notesBar}
    </section>
  );
}
