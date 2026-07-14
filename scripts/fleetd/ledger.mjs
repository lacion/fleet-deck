// ledger.mjs — the file-touch ledger and conflict radar (F4). recordFile logs
// every edit and, when a live rival touched the same file inside the conflict
// window, raises a conflict + mails both sides; whisperText renders the
// in-context warning the PostToolUse hook returns. Threaded ctx state: q, card,
// mail, tick.

import path from 'node:path';
import { ledgerKey } from './repo-identity.mjs';

const CONFLICT_WINDOW_MS = 30 * 60 * 1000;

export function createLedger(ctx) {
  const { q, card, mail, tick } = ctx;

  // --------------------------------------------- file ledger + conflict radar
  // A recent touch counts even if that session already ended — its uncommitted
  // edits are exactly what you're about to clobber (spike rule, kept).
  function recordFile(sid, absFile, editorCard) {
    if (!absFile) return null;
    const now = Date.now();
    const abs = path.isAbsolute(absFile) ? absFile : path.resolve(editorCard.cwd || '/', absFile);
    const key = ledgerKey(abs, editorCard);

    const touches = q.recentTouches.all(key.repo_id ?? '', key.rel_path, now - CONFLICT_WINDOW_MS);
    // A rival is another session that is still ON THE BOARD. Archived rows are
    // retained (never deleted), and 0.7.1 archives the predecessor of every
    // /clear — so a plain "the row exists" test would let a session collide with
    // its own retired past self and raise `⚠ wren-a9e1 and wren-a9e1 both
    // touching X`. A card nobody can see is nobody to coordinate with.
    const rivalTouches = touches.filter(t => {
      if (t.session_id === sid) return false;
      const row = q.getSession.get(t.session_id);
      return !!row && row.archived_at == null;
    });
    const rivals = [...new Set(rivalTouches.map(t => t.session_id))];
    q.insertTouch.run(key.repo_id ?? '', key.rel_path, abs, sid, key.worktree ?? null, now);

    if (!rivals.length) return null;

    // Severity: warning same worktree, info across worktrees of one repo.
    const sameTree = rivalTouches.some(t => (t.worktree ?? null) === (key.worktree ?? null));
    const severity = sameTree ? 'warning' : 'info';
    const rivalNames = rivals.map(r => card(r).callsign).join(', ');
    q.insertConflict.run(now, key.repo_id ?? '', key.rel_path, severity, JSON.stringify([sid, ...rivals]));
    tick(`⚠ conflict: ${editorCard.callsign} and ${rivalNames} both touching ${path.basename(key.rel_path)}`);
    for (const r of rivals) {
      mail(r, 'fleetdeck', severity === 'warning'
        ? `Heads up: ${editorCard.callsign} is also editing ${key.rel_path}. Coordinate before you overwrite each other.`
        : `Heads up: ${editorCard.callsign} is editing ${key.rel_path} in another worktree of this repo — a future merge conflict announcing itself early.`);
    }
    return { file: key.rel_path, abs, rivals: rivalNames, severity };
  }

  function whisperText(conflict) {
    const base = `[FLEETDECK] ⚠ Session(s) ${conflict.rivals} recently edited ${conflict.file} too`;
    return conflict.severity === 'info'
      ? `${base} (in another worktree of this repo — a future merge conflict). Check their intent before building on this file, and mention the coordination in your final summary.`
      : `${base}. Re-read the file before further edits and avoid reverting their work. Mention this coordination in your final summary.`;
  }

  return { recordFile, whisperText };
}
