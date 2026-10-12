import { useState } from 'react';
import { MAX_TITLE_CHARS } from './commands';

type Props = {
  readonly title: string | null;
  readonly enabled: boolean;
  readonly onRename: (title: string | null) => void;
  /** Open the form at mount (the header owns the button that opens it). */
  readonly startEditing?: boolean;
  /** Called after Save, Clear the title, or Cancel. */
  readonly onClose?: () => void;
};

/**
 * The board title of a session (DL-095). An empty title clears it; the
 * station trims it and refuses a control character or more than 80
 * characters, and the board shows that refusal as the command outcome.
 */
export function RenameForm({ title, enabled, onRename, startEditing = false, onClose }: Props) {
  const [editing, setEditing] = useState(startEditing);
  const [draft, setDraft] = useState(title ?? '');
  if (!editing) {
    return (
      <button
        type="button"
        className="fd-sm"
        disabled={!enabled}
        onClick={() => {
          setDraft(title ?? '');
          setEditing(true);
        }}
      >
        Rename
      </button>
    );
  }
  const trimmed = draft.trim();
  const tooLong = [...trimmed].length > MAX_TITLE_CHARS;
  return (
    <form
      className="fd-rename"
      aria-label="Rename the session"
      onSubmit={(e) => {
        e.preventDefault();
        if (tooLong) return;
        onRename(trimmed === '' ? null : trimmed);
        setEditing(false);
        onClose?.();
      }}
    >
      <label>
        Title
        <input
          value={draft}
          maxLength={MAX_TITLE_CHARS * 2}
          onChange={(e) => setDraft(e.target.value)}
          aria-invalid={tooLong}
        />
      </label>
      {tooLong ? (
        <span role="alert">The title is longer than {MAX_TITLE_CHARS} characters.</span>
      ) : null}
      <button type="submit" className="fd-sm" disabled={!enabled || tooLong}>
        {trimmed === '' ? 'Clear the title' : 'Save'}
      </button>
      <button
        type="button"
        className="fd-sm"
        onClick={() => {
          setEditing(false);
          onClose?.();
        }}
      >
        Cancel
      </button>
    </form>
  );
}
