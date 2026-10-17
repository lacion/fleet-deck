// R13 P2c: the change summary of a session for a wall display (design §5D items 12 to 14): the
// files and their counts, and the patches only when the owner switched `show_diffs` on. It reads
// `GET /api/displays/self/sessions/{station}/{session}/changes`; no comment and no control shows.
import { useEffect, useRef, useState } from 'react';
import { Diff } from '../chat/Diff';
import { changesPath, displayChanges } from '../display/api';
import type { DisplayChanges } from '../gen';
import { ApiError } from '../runtime/api';
import { hasChanges } from '../session/changes';
import { SinceNotes } from '../session/TurnsPanel';

/** What a wall display needs from the deck: one read call. */
export type DisplayEnv = {
  readonly get: (path: string) => Promise<unknown>;
  /** The fetch of the artifact pane (tests give their own). */
  readonly fetcher?: (path: string, init: RequestInit) => Promise<Response>;
  /** The generation of the directive that the wall shows; a new one loads an artifact again. */
  readonly generation?: number;
};

/** The wait between two reads of the changes (the deck allows one every 5 s). */
export const CHANGES_REFRESH_MS = 20_000;

export type ChangesState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'error'; readonly text: string }
  | {
      readonly kind: 'ready';
      readonly changes: DisplayChanges;
      /** The answer holds patches (`show_diffs` is on). */
      readonly patches: boolean;
    };

/**
 * One read of the changes. It asks for the patches first; a 403 `diffs_off` asks for the summary at
 * once.
 */
export async function readChanges(
  get: (path: string) => Promise<unknown>,
  station: string,
  session: string,
): Promise<{ readonly changes: DisplayChanges; readonly patches: boolean }> {
  try {
    return {
      changes: displayChanges(await get(changesPath(station, session, true))),
      patches: true,
    };
  } catch (e) {
    if (!(e instanceof ApiError) || e.code !== 'diffs_off') throw e;
    return {
      changes: displayChanges(await get(changesPath(station, session, false))),
      patches: false,
    };
  }
}

/** The changes of one session. A 429 `busy` or a failed read keeps the last answer. */
export function useDisplayChanges(
  env: DisplayEnv | undefined,
  station: string,
  session: string,
): ChangesState {
  const [state, setState] = useState<ChangesState>({ kind: 'loading' });
  const latest = useRef(env);
  latest.current = env;
  const has = env !== undefined;
  useEffect(() => {
    if (!has) return undefined;
    let live = true;
    setState({ kind: 'loading' });
    const load = async () => {
      const get = latest.current?.get;
      if (get === undefined) return;
      try {
        const read = await readChanges(get, station, session);
        if (live) setState({ kind: 'ready', ...read });
      } catch (e) {
        if (!live) return;
        const text = e instanceof Error ? e.message : 'The changes did not load.';
        setState((now) => (now.kind === 'ready' ? now : { kind: 'error', text }));
      }
    };
    void load();
    const timer = setInterval(() => void load(), CHANGES_REFRESH_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [has, station, session]);
  return state;
}

/** The files of an answer as text: "3 files +10 -2". */
function Counts({ changes }: { readonly changes: DisplayChanges }) {
  const { files_changed: files, insertions, deletions } = changes.summary;
  return (
    <p className="fd-turn-stat">
      {files} {files === 1 ? 'file' : 'files'}
      {insertions > 0 ? (
        <>
          {' '}
          <span className="fd-add">+{insertions}</span>
        </>
      ) : null}
      {deletions > 0 ? (
        <>
          {' '}
          <span className="fd-del">−{deletions}</span>
        </>
      ) : null}
    </p>
  );
}

/** The patch text of the files that have one. */
export const joinPatches = (changes: DisplayChanges): string =>
  changes.files
    .map((f) => f.patch)
    .filter((p): p is string => p !== null && p.trim() !== '')
    .join('\n');

function fileLine(f: DisplayChanges['files'][number], n: number): string {
  if (f.hidden) return 'A protected file (hidden)';
  const path = f.path ?? `File ${n + 1}`;
  if (f.binary || f.added === null || f.removed === null) return `${path} · binary`;
  return `${path} · +${f.added} −${f.removed}`;
}

/** The file list of a change answer, for the pane and for a race column. */
export function ChangeFiles({
  changes,
  max,
}: {
  readonly changes: DisplayChanges;
  readonly max?: number;
}) {
  const files = max === undefined ? changes.files : changes.files.slice(0, max);
  return (
    <ul className="fd-wall-files">
      {files.map((f, n) => (
        // The deck gives no id: the position is the key (the list does not reorder).
        <li key={n}>
          {fileLine(f, n)}
          {f.too_large ? ' · too large to show' : ''}
        </li>
      ))}
    </ul>
  );
}

export function ChangesPane({
  state,
  title,
}: {
  readonly state: ChangesState;
  readonly title: string;
}) {
  const changes = state.kind === 'ready' ? state.changes : null;
  const patch = changes === null ? '' : joinPatches(changes);
  return (
    <section className="fd-wall-diff" aria-label={title}>
      <h2 className="fd-wall-h">{title}</h2>
      <p className="fd-wall-note">Since the session started.</p>
      {changes === null ? null : <SinceNotes base={undefined} scope={changes.scope} />}
      {state.kind === 'error' ? <p className="fd-wall-note">{state.text}</p> : null}
      {state.kind === 'loading' ? <p className="fd-wall-note">Loading the changes…</p> : null}
      {changes !== null && !hasChanges(changes.summary) && changes.files.length === 0 ? (
        <p className="fd-wall-note">No file changes since the session started.</p>
      ) : null}
      {changes !== null && (hasChanges(changes.summary) || changes.files.length > 0) ? (
        <>
          <Counts changes={changes} />
          <ChangeFiles changes={changes} />
          {changes.truncated ? <p className="fd-wall-note">The list is cut.</p> : null}
          {state.kind === 'ready' && !state.patches ? (
            <p className="fd-wall-note">Patches are off for this display.</p>
          ) : null}
          {patch !== '' ? (
            <div className="fd-wall-scroll">
              <Diff patch={patch} truncated={changes.truncated} id={`wall-${title}`} />
            </div>
          ) : null}
        </>
      ) : null}
    </section>
  );
}

/** The pane of a session for a display: the hook and the view. */
export function DisplayChangesPane({
  env,
  station,
  session,
  title,
}: {
  readonly env: DisplayEnv;
  readonly station: string;
  readonly session: string;
  readonly title: string;
}) {
  return <ChangesPane state={useDisplayChanges(env, station, session)} title={title} />;
}
