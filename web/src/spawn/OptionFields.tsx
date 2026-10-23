// The groups of "More options" on the New session sheet (DESIGN-SYSTEM.md §4.6): the Coordinator
// choice and the import of a native session. Moved out of `SpawnForm.tsx` for the module size
// rule; the form keeps every state and every handler.
import type { ReactNode } from 'react';
import type { TranscriptPin } from '../gen';

/** The id of the note that says what a coordinator session can do. */
const COORD_NOTE = 'fd-spawn-coord-note';

/**
 * ADR-006 §4, DL-217: the Coordinator checkbox, the note of what it gives (its description), and
 * the grant fields under it while it is on (`children`).
 */
export function CoordinatorOption({
  on,
  onChange,
  children = null,
}: {
  readonly on: boolean;
  readonly onChange: (on: boolean) => void;
  readonly children?: ReactNode;
}) {
  return (
    <div className="fd-spawn-coord">
      <label className="fd-spawn-opt">
        <input
          type="checkbox"
          checked={on}
          aria-describedby={COORD_NOTE}
          onChange={(event) => onChange(event.target.checked)}
        />
        Coordinator
      </label>
      <p className="fd-note" id={COORD_NOTE}>
        This session can read the fleet, send mail and drive the sessions that it starts.
      </p>
      {children}
    </div>
  );
}

/**
 * B7, DL-308: the native session to continue: its id, "Browse sessions on this station", what an
 * import does, and the problem of the typed id.
 */
export function ImportField({
  value,
  pin,
  onChange,
  onBrowse,
  problem,
}: {
  readonly value: string;
  /** The pin of the verified entry that "Continue it" chose. */
  readonly pin: TranscriptPin | null;
  readonly onChange: (id: string) => void;
  /** Opens the native sessions of the station; `null`: this login cannot browse them. */
  readonly onBrowse: (() => void) | null;
  readonly problem: string | null;
}) {
  return (
    <div className="fd-spawn-import">
      <label>
        Session to import (optional)
        <input
          value={value}
          placeholder="Session id"
          spellCheck={false}
          autoCapitalize="off"
          data-native-pin={pin === null ? undefined : `${pin.dev}/${pin.ino}`}
          onChange={(event) => onChange(event.target.value)}
        />
      </label>
      {onBrowse === null ? null : (
        <button type="button" className="fd-spawn-browse" onClick={onBrowse}>
          Browse sessions on this station
        </button>
      )}
      <p className="fd-note">
        Continue a Claude or Codex session that you started in a terminal. Close it there first. An
        import runs in the chosen folder and ignores the workspace and batch choices.
      </p>
      {problem === null ? null : <p role="alert">{problem}</p>}
    </div>
  );
}
