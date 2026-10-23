// Parts of the session menu, moved out of `SessionHeader.tsx` for the module size rule: the rule
// between the item groups, the confirms that replace the items, and the shells-off note.
import type { ReactNode } from 'react';
import { formatHash } from '../app/routes';

/** The groups of a menu with a rule between two groups that have items (an empty group has none). */
export function menuGroups(groups: readonly (readonly ReactNode[])[]): ReactNode[] {
  const shown = groups.map((g) => g.filter((item) => item !== null && item !== false));
  return shown
    .filter((g) => g.length > 0)
    .flatMap((g, i) => (i === 0 ? g : [<div key={`sep-${i}`} className="fd-menu-sep" />, ...g]));
}

/** The confirm of "Open in terminal", inside a menu. */
export function OpenConfirm(props: {
  readonly ended: boolean;
  readonly busy: boolean;
  readonly onOpen: () => void;
  readonly onCancel: () => void;
}) {
  return (
    <div className="fd-sh-confirm" role="group" aria-label="Open in terminal">
      <p className="fd-menu-text">
        In the terminal, the board does not see approvals or prompts. The station holds this session
        until you return it.
        {props.ended ? '' : ' The session stops first.'}
      </p>
      <button type="button" className="fd-primary" disabled={props.busy} onClick={props.onOpen}>
        Open in terminal
      </button>
      <button type="button" onClick={props.onCancel}>
        Cancel
      </button>
    </div>
  );
}

/** The confirm of "Force stop…", inside the session menu. */
export function KillConfirm(props: {
  readonly busy: boolean;
  readonly onKill: () => void;
  readonly onCancel: () => void;
}) {
  return (
    <div className="fd-sh-confirm" role="group" aria-label="Force stop">
      <p className="fd-menu-text">Force stop kills the provider process. Continue?</p>
      <button type="button" className="fd-danger" disabled={props.busy} onClick={props.onKill}>
        Force stop
      </button>
      <button type="button" onClick={props.onCancel}>
        Cancel
      </button>
    </div>
  );
}

/**
 * The station refuses shells: a station setting is changed on the board (Settings → the station
 * tab, "Shells and setup"), never in a file of the station; the note links there.
 */
export function ShellsOffNote(props: { readonly station: string; readonly onOpen: () => void }) {
  return (
    <p className="fd-menu-text">
      Shells are off on this station.{' '}
      <a
        href={formatHash({ name: 'settings', tab: 'station', station: props.station })}
        onClick={props.onOpen}
      >
        Turn on Allow terminals in Settings
      </a>
    </p>
  );
}
