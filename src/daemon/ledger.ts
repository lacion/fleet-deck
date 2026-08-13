// ledger.ts — the file-touch ledger and conflict radar (F4). recordFile logs
// every edit and, when a live rival touched the same file inside the conflict
// window, raises a conflict + mails both sides; whisperText renders the
// in-context warning the PostToolUse hook returns. Threaded ctx state: q, card,
// mail, tick.

import path from 'node:path';
import { ledgerKey, type SessionRef } from './repo-identity.ts';

export const CONFLICT_WINDOW_MS = 30 * 60 * 1000;

// Row shapes the ledger reads out of the (still-JS) statements layer.
// Provisional structural types — they document exactly what this module depends
// on and get replaced by the real exports when statements + db convert.
interface TouchRow {
  session_id: string;
  worktree: string | null;
}
interface SessionStateRow {
  archived_at: number | null;
}
type Severity = 'warning' | 'info';
interface Conflict {
  file: string;
  abs: string;
  rivals: string;
  severity: Severity;
}
// A card row as threaded through recordFile: the identity fields ledgerKey
// consumes (SessionRef) plus the display callsign.
interface CardRow extends SessionRef {
  callsign: string;
}
// The daemon-core state createLedger closes over. Each `q.*` is a prepared
// statement from statements.mjs; only the methods used here are typed.
interface LedgerCtx {
  q: {
    recentTouches: { all(repoId: string, relPath: string, since: number): TouchRow[] };
    getSession: { get(sessionId: string): SessionStateRow | undefined };
    insertTouch: {
      run(
        repoId: string,
        relPath: string,
        abs: string,
        sid: string,
        worktree: string | null,
        now: number,
      ): unknown;
    };
    insertConflict: {
      run(
        ts: number,
        repoId: string,
        relPath: string,
        severity: string,
        participants: string,
      ): unknown;
    };
  };
  card: (id: string) => CardRow;
  mail: (to: string, from: string, body: string) => void;
  tick: (message: string) => void;
}

export function createLedger(ctx: LedgerCtx) {
  const { q, card, mail, tick } = ctx;

  // --------------------------------------------- file ledger + conflict radar
  // A recent touch counts even if that session already ended — its uncommitted
  // edits are exactly what you're about to clobber (spike rule, kept).
  function recordFile(
    sid: string,
    absFile: string | null | undefined,
    // Widened from CardRow: events threads the raw session row here, whose
    // callsign is nullable — CardRow assumed a non-null display name (logged in
    // ts-migration-bugs.md). ledgerKey consumes only the SessionRef identity;
    // callsign is display-only and coalesced to `sid` below.
    editorCard: SessionRef & { callsign: string | null },
  ): Conflict | null {
    if (!absFile) return null;
    const now = Date.now();
    const abs = path.isAbsolute(absFile) ? absFile : path.resolve(editorCard.cwd ?? '/', absFile);
    const key = ledgerKey(abs, editorCard);

    const touches = q.recentTouches.all(key.repo_id, key.rel_path, now - CONFLICT_WINDOW_MS);
    // A rival is another session that is still ON THE BOARD. Archived rows are
    // retained (never deleted), and 0.7.1 archives the predecessor of every
    // /clear — so a plain "the row exists" test would let a session collide with
    // its own retired past self and raise `⚠ wren-a9e1 and wren-a9e1 both
    // touching X`. A card nobody can see is nobody to coordinate with.
    const rivalTouches = touches.filter((t) => {
      if (t.session_id === sid) return false;
      const row = q.getSession.get(t.session_id);
      return !!row && row.archived_at == null;
    });
    const rivals = [...new Set(rivalTouches.map((t) => t.session_id))];
    q.insertTouch.run(key.repo_id, key.rel_path, abs, sid, key.worktree, now);

    if (!rivals.length) return null;

    // Severity: warning same worktree, info across worktrees of one repo.
    const sameTree = rivalTouches.some((t) => t.worktree === key.worktree);
    const severity: Severity = sameTree ? 'warning' : 'info';
    const rivalNames = rivals.map((r) => card(r).callsign).join(', ');
    q.insertConflict.run(
      now,
      key.repo_id,
      key.rel_path,
      severity,
      JSON.stringify([sid, ...rivals]),
    );
    tick(
      `⚠ conflict: ${editorCard.callsign ?? sid} and ${rivalNames} both touching ${path.basename(key.rel_path)}`,
    );
    for (const r of rivals) {
      mail(
        r,
        'fleetdeck',
        severity === 'warning'
          ? `Heads up: ${editorCard.callsign ?? sid} is also editing ${key.rel_path}. Coordinate before you overwrite each other.`
          : `Heads up: ${editorCard.callsign ?? sid} is editing ${key.rel_path} in another worktree of this repo — a future merge conflict announcing itself early.`,
      );
    }
    return { file: key.rel_path, abs, rivals: rivalNames, severity };
  }

  function whisperText(conflict: Pick<Conflict, 'rivals' | 'file' | 'severity'>): string {
    const base = `[FLEETDECK] ⚠ Session(s) ${conflict.rivals} recently edited ${conflict.file} too`;
    return conflict.severity === 'info'
      ? `${base} (in another worktree of this repo — a future merge conflict). Check their intent before building on this file, and mention the coordination in your final summary.`
      : `${base}. Re-read the file before further edits and avoid reverting their work. Mention this coordination in your final summary.`;
  }

  return { recordFile, whisperText };
}
