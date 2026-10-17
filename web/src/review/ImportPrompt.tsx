// DL-179 (design 7): the prompt that imports the old local notes of a session as draft comments.
import { useState } from 'react';
import { getNotes, notesStorageKey, setNotes, useNotes } from '../chat/notes';
import type { SessionKey } from '../gen';
import type { ReviewActions } from './api';
import {
  type ImportResult,
  importFailureText,
  importNotes,
  importPrompt,
  unsentNotes,
} from './importNotes';

export function ImportPrompt({
  actions,
  session,
  snapshot,
  canWrite,
  onDone,
}: {
  readonly actions: ReviewActions;
  readonly session: SessionKey;
  readonly snapshot: string;
  readonly canWrite: boolean;
  readonly onDone: () => void;
}) {
  const storageKey = notesStorageKey(session.station_id, session.session_id);
  const notes = useNotes(storageKey);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<ImportResult | null>(null);
  const unsent = unsentNotes(notes);
  if (!canWrite || unsent.length === 0) return null;
  const run = () => {
    setBusy(true);
    setFailed(null);
    void importNotes({
      notes: getNotes(storageKey),
      station: session.station_id,
      session: session.session_id,
      key: session,
      snapshot,
      add: actions.add,
      clear: () => setNotes(storageKey, []),
    }).then((result) => {
      setBusy(false);
      if (result.error !== null) setFailed(result);
      onDone();
    });
  };
  return (
    <div className="fd-import" data-import="notes">
      <p className="fd-note">{importPrompt(unsent.length)}</p>
      <button type="button" className="fd-sm" disabled={busy} onClick={run}>
        Import
      </button>
      {failed !== null ? (
        <p role="alert" className="fd-import-alert">
          {importFailureText(failed)}
        </p>
      ) : null}
    </div>
  );
}
