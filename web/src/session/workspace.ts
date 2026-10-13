// M3 6-8 (DL-097): the workspace tools of a session on the board.
//
// - The file service (`files.v1`, DL-088): browse, read and search below the
//   session's directory. The station enforces the allowed roots, the
//   symlink and hidden-directory rules and the size limits; the board only
//   shows the answers.
// - The finalized turns of the timeline with their checkpoints: a diff comes
//   from the two checkpoints, a revert needs the start checkpoint and a
//   provider that can roll back its conversation (PLAN §6.6).
import { useSyncExternalStore } from 'react';
import type {
  Capabilities,
  CheckpointSide,
  DiffSummary,
  EventEnvelope,
  FileContent,
  FileListing,
  FileSearch,
  RevertState,
  SessionDiff,
  SessionKey,
  SessionSummary,
  TurnDiff,
} from '../gen';

type Get = (path: string) => Promise<unknown>;

export type SearchModeName = 'name' | 'content';

export type FileActions = {
  list(path: string): Promise<FileListing>;
  read(path: string): Promise<FileContent>;
  search(query: string, mode: SearchModeName): Promise<FileSearch>;
  diff(turnId: string): Promise<TurnDiff>;
  /** The working tree of the session against its base commit (R1.2). */
  sessionDiff(): Promise<SessionDiff>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

/** The file calls of one root: `root` is the query part that names it (`session=…` or `workspace=…`). */
function rootActions(
  get: Get,
  station: string,
  root: string,
  diff: FileActions['diff'],
  sessionDiff: FileActions['sessionDiff'],
): FileActions {
  const base = `/api/stations/${encodeURIComponent(station)}`;
  return {
    list: async (path) =>
      shaped<FileListing>(
        await get(`${base}/files/list?${root}&path=${encodeURIComponent(path)}`),
        ['path', 'entries', 'truncated'],
        'directory listing',
      ),
    read: async (path) =>
      shaped<FileContent>(
        await get(`${base}/files/read?${root}&path=${encodeURIComponent(path)}`),
        ['path', 'size', 'binary', 'text', 'truncated'],
        'file',
      ),
    search: async (query, mode) =>
      shaped<FileSearch>(
        await get(`${base}/files/search?${root}&q=${encodeURIComponent(query)}&mode=${mode}`),
        ['hits', 'truncated', 'backend'],
        'search answer',
      ),
    diff,
    sessionDiff,
  };
}

/** The file service of one session (`GET /api/stations/<id>/files/...`). */
export function fileActions(get: Get, key: SessionKey): FileActions {
  const station = `/api/stations/${encodeURIComponent(key.station_id)}`;
  return rootActions(
    get,
    key.station_id,
    `session=${encodeURIComponent(key.session_id)}`,
    async (turnId) =>
      shaped<TurnDiff>(
        await get(
          `${station}/sessions/${encodeURIComponent(key.session_id)}/turns/${encodeURIComponent(turnId)}/diff`,
        ),
        ['summary', 'patch', 'truncated'],
        'diff',
      ),
    async () =>
      shaped<SessionDiff>(
        await get(`${station}/sessions/${encodeURIComponent(key.session_id)}/diff`),
        ['summary', 'patch', 'truncated'],
        'diff',
      ),
  );
}

/** The file service of one folder of a station (`workspace=<absolute dir>`); a folder has no turns. */
export function workspaceFiles(get: Get, station: string, workspace: string): FileActions {
  return rootActions(
    get,
    station,
    `workspace=${encodeURIComponent(workspace)}`,
    async () => {
      throw new Error('A folder has no turns.');
    },
    async () => {
      throw new Error('A folder has no session diff.');
    },
  );
}

// The size of the session diff that the Changes tab loaded last, by session
// (`<station>/<session>`), for the session card. Nothing polls for it: the
// card shows what the tab already read.
const diffTotals = new Map<string, DiffSummary>();
const diffListeners = new Set<() => void>();

/** Records the size of a session diff that the Changes tab loaded. */
export function setDiffTotal(ref: string, summary: DiffSummary): void {
  diffTotals.set(ref, summary);
  for (const l of diffListeners) l();
}

/** The loaded session diff size of one session, or `undefined`; re-renders on a change. */
export function useDiffTotal(ref: string): DiffSummary | undefined {
  return useSyncExternalStore(
    (l) => {
      diffListeners.add(l);
      return () => diffListeners.delete(l);
    },
    () => diffTotals.get(ref),
    () => diffTotals.get(ref),
  );
}

/** The parent of a relative directory path (`''` is the root). */
export function parentPath(path: string): string {
  const i = path.lastIndexOf('/');
  return i < 0 ? '' : path.slice(0, i);
}

/** `dir` joined with `name`, relative to the root. */
export function joinPath(dir: string, name: string): string {
  return dir === '' ? name : `${dir}/${name}`;
}

/** One finalized turn of the timeline. */
export type FinalizedTurn = {
  readonly turnId: string;
  readonly start: CheckpointSide | null;
  readonly end: CheckpointSide | null;
  readonly diff: DiffSummary | null;
  /** A successful revert or an owner reconciliation removed the turn. */
  readonly reverted: boolean;
};

/** The finalized turns in log order, with the reverts applied. */
export function finalizedTurns(events: readonly EventEnvelope[]): FinalizedTurn[] {
  const turns: FinalizedTurn[] = [];
  const revertAt = (from: number) => {
    for (let i = Math.max(from, 0); i < turns.length; i += 1) {
      const t = turns[i];
      if (t) turns[i] = { ...t, reverted: true };
    }
  };
  for (const env of events) {
    const event = env.event as { kind: string } & Record<string, unknown>;
    if (event.kind === 'turn.finalized') {
      const checkpoint = event['checkpoint'] as {
        start: CheckpointSide;
        end: CheckpointSide;
      } | null;
      turns.push({
        turnId: String(event['turn_id']),
        start: checkpoint?.start ?? null,
        end: checkpoint?.end ?? null,
        diff: (event['diff_summary'] as DiffSummary | null) ?? null,
        reverted: false,
      });
    } else if (event.kind === 'revert.updated') {
      const state = event['state'] as RevertState;
      if (state === 'succeeded') {
        revertAt(turns.findIndex((t) => t.turnId === event['turn_id'] && !t.reverted));
      } else if (state === 'reconciled') {
        revertAt(0);
      }
    }
  }
  return turns;
}

/** A revert in one of these states holds the session: no input, no new revert. */
export function revertHolds(state: RevertState | undefined): boolean {
  return (
    state === 'admitted' ||
    state === 'conversation_rolled_back' ||
    state === 'reconciliation_required'
  );
}

const REVERT_TEXT: Record<RevertState, string> = {
  admitted: 'The revert is admitted and starts now.',
  conversation_rolled_back: 'The conversation is rolled back; the files are restored next.',
  succeeded: 'The revert succeeded: the conversation and the files are at the turn start.',
  failed: 'The revert failed. Nothing changed: not the conversation and not the files.',
  reconciliation_required:
    'The revert needs reconciliation: the conversation and the files can differ. The session takes no input.',
  reconciled: 'The owner reconciled the revert. The session takes input again.',
};

/** The board text of a revert state. */
export function revertText(state: RevertState, reason: string | null): string {
  return reason ? `${REVERT_TEXT[state]} Reason: ${reason}.` : REVERT_TEXT[state];
}

/**
 * Why the board offers no revert of `turn`, or `null` when it can. The
 * station checks all of this again in the decision transaction.
 */
export function revertBlock(
  turn: FinalizedTurn,
  summary: SessionSummary,
  caps: Capabilities | null,
): string | null {
  if (caps?.rollback !== true) return 'The provider cannot roll back its conversation.';
  if (turn.reverted) return 'The turn is reverted.';
  if (turn.start?.state !== 'available') {
    return `No start checkpoint${turn.start ? `: ${turn.start.reason}` : ''}.`;
  }
  if (revertHolds(summary.revert?.state)) return 'Another revert holds the session.';
  if (summary.current_turn !== null || summary.current_run !== null) return 'A turn runs.';
  return null;
}
