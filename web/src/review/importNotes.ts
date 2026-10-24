// DL-179 (design 7): the old local notes (`fd-notes:<station>/<session>`) become draft comments.
// The import is an explicit button press; a repeat is safe because each note has an import key.
import type { NewComment, ReviewComment, SessionKey } from '../gen';
import type { Note } from '../chat/notes';
import { failureText } from './CommentEditor';

/** The notes that an import takes: the ones that were not sent. */
export const unsentNotes = (notes: readonly Note[]): Note[] => notes.filter((n) => !n.sent);

/** The text of the prompt. */
export const importPrompt = (n: number): string =>
  `You have ${n} local ${n === 1 ? 'note' : 'notes'} from an older version. Import ${n === 1 ? 'it' : 'them'} as draft comments?`;

/** Lower-case hex SHA-256 of `text`. */
export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** The import key of a note: SHA-256 of `<station>/<session>/<file>/<line>/<text>`. */
export const importKey = (
  station: string,
  session: string,
  note: Note,
  hash: (text: string) => Promise<string> = sha256Hex,
): Promise<string> => hash(`${station}/${session}/${note.file}/${note.line}/${note.text}`);

export type ImportResult = {
  /** How many notes got a success (also "already imported"). */
  readonly imported: number;
  readonly total: number;
  /** The message of the first failure. */
  readonly error: string | null;
};

/** "Imported 1 of 3 notes. <first error>". */
export const importFailureText = (r: ImportResult): string =>
  `Imported ${r.imported} of ${r.total} ${r.total === 1 ? 'note' : 'notes'}. ${r.error ?? ''}`.trimEnd();

/**
 * Imports the unsent notes, in order, as draft comments on the new side of the Since-spawn diff.
 * It tries every note. When every note is done it calls `clear` (the local key goes away; sent
 * notes are dropped with it). With a failure the local notes stay.
 */
export async function importNotes(args: {
  readonly notes: readonly Note[];
  readonly station: string;
  readonly session: string;
  readonly key: SessionKey;
  readonly snapshot: string;
  readonly add: (key: SessionKey, body: NewComment) => Promise<ReviewComment>;
  readonly clear: () => void;
  readonly hash?: (text: string) => Promise<string>;
}): Promise<ImportResult> {
  const todo = unsentNotes(args.notes);
  let imported = 0;
  let error: string | null = null;
  for (const note of todo) {
    try {
      await args.add(args.key, {
        snapshot: args.snapshot,
        path: note.file,
        side: 'new',
        line_start: note.line,
        line_end: note.line,
        body: note.text,
        import_key: await importKey(args.station, args.session, note, args.hash),
      });
      imported += 1;
    } catch (e) {
      error ??= failureText(e);
    }
  }
  if (imported === todo.length) args.clear();
  return { imported, total: todo.length, error };
}
