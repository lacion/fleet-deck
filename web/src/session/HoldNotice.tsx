// DL-140: the notice of a terminal hold under the session header, with the release of the hold.
// Moved out of `SessionView.tsx` for the module size rule.
import { useState } from 'react';
import type { TerminalHold } from '../gen';
import type { ShellActions } from '../shell/shell';
import { releaseHold } from '../terminal/handover';
import type { Outcome } from './commands';
import type { Controller } from './controller';

type Props = {
  readonly hold: TerminalHold;
  readonly station: string;
  readonly shells: ShellActions | undefined;
  readonly controller: Controller;
  /** The handover flow releases the hold when it exists. */
  readonly release: ((shellId: string) => Promise<Outcome>) | null;
};

/** "A terminal holds this session", and "Release the hold" with its confirm. */
export function HoldNotice({ hold, station, shells, controller, release }: Props) {
  const [confirmRelease, setConfirmRelease] = useState(false);
  const busy = controller.busy;
  return (
    <div className="fd-pane-notice" role="note">
      <p>
        A terminal holds this session (shell {hold.shell_id}).
        {hold.reason ? ` ${hold.reason}` : ''}
      </p>
      {shells ? (
        confirmRelease ? (
          <div className="fd-pane-confirm" role="group" aria-label="Release the hold">
            <p>Only do this when no terminal process uses this session.</p>
            <div className="fd-pane-row">
              <button
                type="button"
                className="fd-sm"
                disabled={busy !== null}
                onClick={() => {
                  setConfirmRelease(false);
                  controller.act('release', () =>
                    release
                      ? release(hold.shell_id)
                      : releaseHold(() => shells.release(station, hold.shell_id)),
                  );
                }}
              >
                Release the hold
              </button>
              <button type="button" className="fd-sm" onClick={() => setConfirmRelease(false)}>
                Cancel
              </button>
            </div>
          </div>
        ) : (
          <button
            type="button"
            className="fd-sm"
            disabled={busy !== null}
            onClick={() => setConfirmRelease(true)}
          >
            Release the hold
          </button>
        )
      ) : null}
    </div>
  );
}
