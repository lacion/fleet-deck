// FG-9 (design §12.4A, revision 4): "Undo move" and its confirm dialog. A move whose new session
// could not resume leaves the source read only; this makes the source resumable again.
import { useState } from 'react';
import { useIntegrations } from '../integrations/context';
import { Dialog } from '../integrations/Dialog';
import { message } from '../integrations/useLoad';

type Props = {
  readonly move: string;
  /** The station of the source session, where the conversation continues. */
  readonly source: string;
  /** The station of the new session, which stays stopped and read only. */
  readonly target: string;
  /** The undo was accepted: the session view reads the move again. */
  readonly onUndone: () => void;
};

export function MoveUndo({ move, source, target, onUndone }: Props) {
  const actions = useIntegrations()?.actions;
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (actions === undefined) return null;
  const undo = () => {
    setBusy(true);
    setError(null);
    actions
      .moveUndo(move)
      .then(() => {
        setAsking(false);
        onUndone();
      })
      .catch((e: unknown) => setError(message(e)))
      .finally(() => setBusy(false));
  };
  return (
    <>
      <button
        type="button"
        className="fd-ghost fd-sm"
        onClick={() => {
          setError(null);
          setAsking(true);
        }}
      >
        Undo move
      </button>
      {asking ? (
        <Dialog title="Undo the move?" onClose={() => setAsking(false)}>
          <p className="fd-note">
            The session on {source} becomes resumable again. The new session on {target} stays
            stopped and read only.
          </p>
          {error === null ? null : (
            <p role="alert" className="fd-formerror">
              {error}
            </p>
          )}
          <div className="fd-pane-row">
            <button type="button" className="fd-ghost" onClick={() => setAsking(false)}>
              Cancel
            </button>
            <button type="button" className="fd-primary" disabled={busy} onClick={undo}>
              Undo move
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  );
}
