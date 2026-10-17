// DL-140: the terminal handover flows of one session. Pure: the pane gives it
// the live pieces (watch, actions, shells, terminal store) and runs each flow
// through the session controller.
import type { SessionKey } from '../gen';
import type { ShellActions } from '../shell/shell';
import { releaseHold, returnToBoard, startHandover, type Watch } from '../terminal/handover';
import { type TerminalStore, tabId } from '../terminal/store';
import type { Outcome, SessionActions } from './commands';

type Deps = {
  readonly key: SessionKey;
  readonly title: string;
  readonly watch: Watch;
  readonly actions: SessionActions;
  readonly shells: ShellActions;
  readonly terminals: TerminalStore;
};

export type HandoverFlow = {
  /** Stop the session, run its TUI and add the terminal tab. */
  open(): Promise<Outcome>;
  /** End the TUI and resume the driven session. */
  back(): Promise<Outcome>;
  /** Clear a hold whose process the station cannot see. */
  release(shellId: string): Promise<Outcome>;
};

export function handoverFlow(deps: Deps): HandoverFlow {
  const { key, title, watch, actions, shells, terminals } = deps;
  const station = key.station_id;
  return {
    async open() {
      const result = await startHandover({
        watch,
        stop: () => actions.stop('graceful'),
        handover: () => shells.handover(station, key.session_id, 120, 32),
      });
      if (!result.ok) return { ok: false, text: result.text };
      terminals.add({
        id: tabId(station, result.shellId),
        station,
        shellId: result.shellId,
        kind: 'tui',
        title,
        session: key,
      });
      return { ok: true, text: 'The terminal is open.' };
    },
    async back() {
      const hold = watch.get()?.terminal;
      if (hold == null) return actions.resume();
      const result = await returnToBoard({
        watch,
        shellLive: async () =>
          (await shells.list(station)).some(
            (s) => s.shell_id === hold.shell_id && s.state === 'running',
          ),
        close: () => shells.close(station, hold.shell_id),
        resume: () => actions.resume(),
      });
      if (watch.get()?.terminal == null) terminals.remove(tabId(station, hold.shell_id));
      return result;
    },
    release(shellId) {
      return releaseHold(() => shells.release(station, shellId));
    },
  };
}
