import { Fragment, useMemo, useState } from 'react';
import { diffLineKind } from './diffLines';
import { MAX_NOTE_CHARS, type Note, linePositions } from './notes';

const FILE_LINE = /^(diff --git a\/.* b\/)(.+)$/;

/** The parts of a `diff --git a/X b/Y` line: the text before the path Y, and Y; `null` for another shape. */
export function splitDiffFileLine(
  line: string,
): { readonly head: string; readonly path: string } | null {
  const match = FILE_LINE.exec(line);
  return match?.[1] !== undefined && match[2] !== undefined
    ? { head: match[1], path: match[2] }
    : null;
}

/** Line notes (R1.3): a click on a line number opens a one-line editor under the line. */
export type DiffNotes = {
  readonly notes: readonly Note[];
  /** Sets the note of one line; an empty text removes it. */
  readonly onSet: (file: string, line: number, text: string) => void;
};

type Spot = { readonly file: string; readonly line: number };

function NoteEditor({
  initial,
  onSave,
  onCancel,
}: {
  readonly initial: string;
  readonly onSave: (text: string) => void;
  readonly onCancel: () => void;
}) {
  const [text, setText] = useState(initial);
  return (
    <span className="fd-dl-note" data-editing="true">
      <input
        type="text"
        className="fd-dl-input"
        value={text}
        maxLength={MAX_NOTE_CHARS}
        aria-label="Note"
        placeholder="One line"
        ref={(el) => el?.focus()}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onSave(text);
          else if (e.key === 'Escape') onCancel();
        }}
      />
      <button type="button" className="fd-sm" onClick={() => onSave(text)}>
        Save
      </button>
      <button type="button" className="fd-sm" onClick={onCancel}>
        Cancel
      </button>
    </span>
  );
}

/**
 * A unified diff with colored lines (UI-DESIGN §2.5). A file name opens the file when `onOpenFile` is given.
 * With `notes`, a line that exists in the new file has its number as a button that opens a note editor.
 */
export function Diff({
  patch,
  truncated,
  id,
  onOpenFile,
  notes,
}: {
  readonly patch: string;
  readonly truncated: boolean;
  readonly id: string;
  readonly onOpenFile?: ((path: string) => void) | undefined;
  readonly notes?: DiffNotes | undefined;
}) {
  const lines = patch.split('\n');
  const noted = notes !== undefined;
  const spots = useMemo(() => (noted ? linePositions(patch) : []), [patch, noted]);
  const [editing, setEditing] = useState<Spot | null>(null);
  return (
    <pre className="fd-diff" data-diff={id}>
      {lines.map((line, n) => {
        const kind = diffLineKind(line);
        const file = kind === 'file' && onOpenFile ? splitDiffFileLine(line) : null;
        const spot = spots[n] ?? null;
        const note =
          notes && spot
            ? notes.notes.find((x) => x.file === spot.file && x.line === spot.line)
            : undefined;
        return (
          <Fragment key={n}>
            <span className="fd-dl" data-kind={kind}>
              {notes && spot ? (
                <button
                  type="button"
                  className="fd-dl-n"
                  aria-label={`Note on ${spot.file} line ${spot.line}`}
                  onClick={() => setEditing(spot)}
                >
                  {spot.line}
                </button>
              ) : null}
              {file ? (
                <>
                  {file.head}
                  <button
                    type="button"
                    className="fd-dl-file"
                    onClick={() => onOpenFile?.(file.path)}
                  >
                    {file.path}
                  </button>
                </>
              ) : line === '' ? (
                ' '
              ) : (
                line
              )}
            </span>
            {notes && spot && editing?.file === spot.file && editing.line === spot.line ? (
              <NoteEditor
                initial={note?.text ?? ''}
                onSave={(text) => {
                  notes.onSet(spot.file, spot.line, text);
                  setEditing(null);
                }}
                onCancel={() => setEditing(null)}
              />
            ) : note ? (
              <span className="fd-dl-note" data-sent={note.sent ? 'true' : undefined}>
                {note.text}
                {note.sent ? <span className="fd-dl-sent"> (sent)</span> : null}
              </span>
            ) : null}
          </Fragment>
        );
      })}
      {truncated ? (
        <span className="fd-dl" data-kind="meta">
          … (the diff is cut at the size limit)
        </span>
      ) : null}
    </pre>
  );
}
