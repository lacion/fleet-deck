import { useState } from 'react';
import { Diff } from '../chat/Diff';
import type { RevertResolution, SessionSummary, TurnDiff } from '../gen';
import type { Outcome, SessionActions } from './commands';
import { hasChanges } from './changes';
import { type FinalizedTurn, revertBlock, revertHolds, revertText } from './workspace';

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

/** The finalized turns: a diff from the checkpoints, and a revert to the turn start. */
export function TurnsPanel(props: Props) {
  const { summary, turns, actions, drive, busy, act, onOpenFile } = props;
  const [confirm, setConfirm] = useState<string | null>(null);
  const [diffs, setDiffs] = useState<Record<string, TurnDiff | string>>({});
  if (turns.length === 0) return null;
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
                />
              ) : null}
            </li>
          );
        })}
      </ol>
      {revertHolds(summary.revert?.state) ? (
        <p className="fd-note">A revert holds the session; no new revert starts.</p>
      ) : null}
    </section>
  );
}
