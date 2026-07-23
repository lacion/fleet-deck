// retention.mjs — the non-destructive retention sweep (silence presume-dead,
// tmux-adjudicated spawned silence, offline archival, ledger aging) and the
// manual cleanup() ("Clear means clear"). The boot invocation + the 10-minute
// interval live in the composition root. Threaded ctx state: q, updateSession,
// tick, onMutate, tombstoneCard (the shared offline-tombstone write, which
// drops the model memo + wakes watchers), forgetSpawn, the tmux adapter, port,
// the questions relay, and the PRESUME_DEAD / RETAIN_OFFLINE / ledger knobs.
// SHELL_RE is a pure helper.

import fs from 'node:fs';
import { SHELL_RE, NOT_RESUMABLE_END } from './helpers.mjs';

export function createRetention(ctx) {
  const {
    q, updateSession, tick, onMutate, tombstoneCard, forgetSpawn,
    tmuxAdapter, port, questions, adoptSession, scopedPaneTarget,
    PRESUME_DEAD_MS, RETAIN_OFFLINE_MS, RETAIN_LEDGER_MS,
  } = ctx;

  // Silence → presumed-ended tombstone. Pane-less hook sessions have no window
  // to consult, so their silence IS the only signal (unchanged behavior).
  function presumeDeadSilent(s, now) {
    const hours = Math.max(0, (now - s.last_seen) / 3_600_000);
    const label = Number.isInteger(hours) ? String(hours) : hours.toFixed(1).replace(/\.0$/, '');
    tombstoneCard(s.session_id, { // D8
      note: `presumed ended (silent ${label}h)`,
      at: now,
      tickMsg: `⌛ ${s.callsign} presumed ended after ${label}h silent`,
      forgetModel: true, // M-G2: terminal — clear the transcript memo
    });
    // 0.7.0 Move-to-tmux: mark this end as GUESSED, not proven. Silence is a
    // heuristic — the CLI may still be running quietly — so `claude --resume`
    // here could duplicate a live billed session. end_reason='presumed' makes
    // adopt-now refuse (409 "arm it instead"); a later real hook clears it back
    // to a live/proven state via applyEvent's resurrection block.
    updateSession(s.session_id, { end_reason: 'presumed' });
  }

  // Retention is non-destructive: sessions/mail are timestamped out of the
  // live surface, never deleted. A late hook resurrects a presumed-dead card.
  //
  // BUG 2: presume-dead is a SILENCE heuristic, valid ONLY for pane-less hook
  // sessions. A board-SPAWNED agent idling quietly at its prompt emits no hooks
  // for hours, yet its tmux pane is a live claude the whole time — silence must
  // never condemn it. This sweep therefore splits the candidates: hook-only
  // rows (no live spawn) presume dead on silence as before; spawned rows are
  // adjudicated by TMUX, never the clock — alive → refresh last_seen and keep
  // it live; tmux-confirmed dead → condemn (same verdict the liveness tick
  // reaches); window absent/unreachable → UNKNOWN, no action (firstmate rule).
  // The function is async, but stays fully SYNCHRONOUS whenever there is no
  // spawned candidate (the common case, and every boot path the tests assert):
  // the tmux probe is only awaited when `spawned.length` is non-zero.
  async function retentionSweep(now = Date.now()) {
    let changed = false;
    const spawned = [];
    // BUG 7: a FLEETDECK_SPAWN_CMD override launches a detached process, NOT a
    // tmux window, so its spawn row names a window tmux never has. BUG 2's tmux
    // adjudication would then read that window as ABSENT → UNKNOWN → never
    // presume it dead, so an override agent that crashed without a SessionEnd
    // lingered active on the board forever. An override spawn has no pane to
    // consult, so — like a pane-less hook session — its SILENCE is the only
    // signal it exposes: treat it as pane-less and let the silence heuristic
    // presume it dead. (The whole daemon is in override mode or none is, so
    // this is one check, not a per-row flag.)
    const overrideMode = !!tmuxAdapter.spawnOverrideCmd();
    for (const s of q.presumeDeadSessions.all(now - PRESUME_DEAD_MS)) {
      const sp = q.activeSpawnBySession.get(s.session_id); // live-eligible spawn row?
      if (sp && !overrideMode) { spawned.push({ s, sp }); continue; } // tmux-backed pane → ask tmux below
      // Pane-less: a hook-only session (no spawn row) OR an override process
      // (a spawn row, but no tmux window to adjudicate). Silence is the signal.
      presumeDeadSilent(s, now);
      // BUG 7: keep an override spawn row coherent with its now-offline card —
      // condemn it 'pane-dead' (never left stale 'live', so countActiveSpawns
      // stops counting it) which ALSO makes it revivable, exactly the recovery
      // path a crashed override agent needs.
      if (sp) { q.setSpawnStatus.run('pane-dead', sp.spawn_id); forgetSpawn(sp.spawn_id); }
      changed = true;
    }
    if (spawned.length) {
      const wins = await tmuxAdapter.listScopedWindows(port);
      if (wins !== null) {
        for (const { s, sp } of spawned) {
          const win = wins.find(w => w.window === sp.tmux_window);
          // Alive: window present, pane not dead, and paneCurrentCommand confirms
          // claude (pane_cmd can read stale on remain-on-exit panes). The agent
          // is simply quiet — refresh last_seen so the clock restarts and leave
          // the card live. This is the "3.1h alive spawn got goned" fix.
          let alive = false;
          if (win && !win.pane_dead) {
            const pane = await tmuxAdapter.paneCurrentCommand(scopedPaneTarget(win));
            alive = !!pane && !pane.dead && pane.cmd === 'claude';
          }
          if (alive) {
            updateSession(s.session_id, { last_seen: now });
            changed = true;
            continue;
          }
          // tmux CONFIRMS dead — window present but pane_dead or a bare shell.
          // Condemn exactly like the liveness tick: flip the spawn 'pane-dead'
          // (still revivable) and tombstone the card. A window that is ABSENT is
          // UNKNOWN, not dead — never condemn on silence (a wrong "dead" costs a
          // duplicate billed session); leave it for a later sweep / boot reconcile.
          if (win && (win.pane_dead || SHELL_RE.test(win.pane_cmd))) {
            q.setSpawnStatus.run('pane-dead', sp.spawn_id);
            forgetSpawn(sp.spawn_id);
            tombstoneCard(s.session_id, { // D8
              note: `pane confirmed dead — resume with claude --resume ${s.session_id}`,
              at: now,
              tickMsg: `💀 ${s.callsign} pane confirmed dead after long silence — window kept for scrollback`,
              forgetModel: true,
            });
            changed = true;
          }
        }
      }
    }
    // 0.7.0 Move-to-tmux: the armed-adopt safety net. The arm is durable intent
    // consumed by adoptSession itself (NOT by the SessionEnd trigger), so two
    // kinds of leftovers can sit in SQLite:
    //   • EXPIRED arms — the deadline passed with no consuming adopt: clear the
    //     columns (the snapshot already renders a past deadline as unarmed;
    //     this just keeps the rows truthful).
    //   • ORPHANED arms — the session ENDED with a hook-proven reason while the
    //     arm is still set: the deferred timer died with the daemon (a crash,
    //     or precisely a version-takeover SIGTERM inside the grace window).
    //     Complete the human's move now, with deferred semantics so every
    //     benign race stays silent and adoptSession consumes the arm one-shot —
    //     a 409 (another actor mid-flight) leaves the arm for the winner, and a
    //     consumed arm means the next sweep cannot double-fire.
    for (const s of q.allSessions.all()) {
      if (s.adopt_armed_until == null && s.adopt_armed_skip == null) continue;
      if (s.adopt_armed_until == null || s.adopt_armed_until <= now) {
        updateSession(s.session_id, { adopt_armed_until: null, adopt_armed_skip: null });
        changed = true;
        continue;
      }
      // The same allowlist the board's chip and adoptSession use — one owner for
      // "may this end be resumed?", so a third hand-rolled copy can't drift
      // (0.7.1's 'superseded' would have slipped straight through the old test).
      if (s.ended_at != null && !NOT_RESUMABLE_END.has(s.end_reason ?? null)) {
        // Fire-and-forget: a tmux stall must never wedge the sweep. Same
        // failure surface as the SessionEnd trigger — loud only for real
        // failures, never for benign 409 races.
        Promise.resolve(adoptSession(s.session_id, { dangerously_skip_permissions: !!s.adopt_armed_skip }, { deferred: true }))
          .then(out => {
            if (!out || (out.status >= 400 && out.status !== 409)) {
              tick(`✗ move-to-tmux failed for ${s.callsign}: ${(out?.body?.reason ?? 'unknown')}`.slice(0, 100));
            }
          })
          .catch(err => tick(`✗ move-to-tmux failed for ${s.callsign}: ${String(err?.message || err)}`.slice(0, 100)));
        changed = true;
      }
    }
    for (const s of q.archiveCandidates.all(now - RETAIN_OFFLINE_MS)) {
      q.setArchived.run(now, s.session_id);
      changed = true;
    }
    if (q.expireRetainedMail.run(now, now - RETAIN_OFFLINE_MS).changes) changed = true;
    if (q.goneArchivedSpawns.run().changes) changed = true;
    // M-G1: age the append-only ledgers so they cannot grow without bound.
    // file_touches is pruned to the ledger window (the conflict radar only
    // looks back CONFLICT_WINDOW_MS anyway, and the snapshot windows its query
    // to the same cutoff); commands, conflicts, and settled mail are pruned to
    // the same horizon. Pending mail is never age-pruned here.
    const ledgerCutoff = now - RETAIN_LEDGER_MS;
    if (q.pruneTouches.run(ledgerCutoff).changes) changed = true;
    if (q.pruneCommands.run(ledgerCutoff).changes) changed = true;
    if (q.pruneConflicts.run(ledgerCutoff).changes) changed = true;
    if (q.pruneSettledMail.run(ledgerCutoff).changes) changed = true;
    if (changed) onMutate();
    return { changed };
  }

  // Manual cleanup archives every offline card now, expires its pending mail
  // and questions (INCLUDING freeform — archiving is the human declaring
  // "done with these"), kills only dead panes owned by terminal spawn rows,
  // and merely LISTS orphan worktrees for explicit human cleanup.
  async function cleanup() {
    const now = Date.now();
    // Capture the about-to-be-archived sids before the UPDATE claims them.
    const archiving = q.archiveCandidates.all(now + 1).map(r => r.session_id);
    const archived = Number(q.archiveAllOffline.run(now).changes);
    const mail_expired = Number(q.expireArchivedMail.run(now).changes);
    let questions_expired = 0;
    for (const sid of archiving) {
      questions_expired += Number(questions.expireAllForSession(sid, { includeFreeform: true }));
    }
    q.goneArchivedSpawns.run();

    const wins = await tmuxAdapter.listScopedWindows(port);
    const byName = new Map(q.allSpawns.all().map(r => [r.tmux_window, r]));
    let windows_killed = 0;
    for (const win of wins ?? []) {
      const sp = byName.get(win.window);
      if (!win.pane_dead || !sp || !['killed', 'pane-dead', 'gone'].includes(sp.status)) continue;
      const out = await tmuxAdapter.killWindowVerified(win.window);
      if (out.ok) windows_killed++;
    }
    // CLEAR MEANS CLEAR. Archiving the cards was never enough: the conflict
    // banner kept shouting about files two dead sessions once touched, the rail
    // kept a wall of answered questions, and the feed kept narrating a fleet
    // that no longer exists. What survives a Clear is what is still ALIVE.
    const alive = new Set(q.aliveSessionIds.all().map(r => r.session_id));

    // A conflict is only news while every session in it can still act on it.
    let conflicts_cleared = 0;
    for (const row of q.allConflicts.all()) {
      let ids = [];
      try { ids = JSON.parse(row.sessions_json || '[]'); } catch { /* corrupt row → drop it */ }
      // R2-6: wrong-shape JSON ('null', '{}', a string) parses but would make
      // `ids.length`/`ids.every` throw (e.g. null.length) — treat it as corrupt.
      if (!Array.isArray(ids)) ids = [];
      if (ids.length && ids.every(id => alive.has(id))) continue; // still a live argument
      conflicts_cleared += Number(q.deleteConflict.run(row.id).changes);
    }
    // The ledger the radar reads: dead sessions' touches would keep raising
    // conflicts against a session that cannot answer for them.
    q.deleteDeadTouches.run();
    // Answered/expired/dismissed cards leave the rail entirely (pending ones
    // are the human's actual queue and are never touched here).
    const questions_purged = Number(questions.purgeResolved());
    q.deleteArchivedMail.run();
    // The feed is a live narration, not an archive — SQLite keeps the events.
    const feed_cleared = Number(q.clearTicker.run().changes);

    // Only worktrees still on disk are the human's chore — rows whose paths
    // were already removed by hand are silence, not a nag.
    const orphan_worktrees = q.orphanWorktrees.all()
      .map(r => r.worktree_path)
      .filter(p => { try { return fs.existsSync(p); } catch { return false; } });
    // One line of feed survives the wipe: the wipe itself.
    tick(`⌫ cleared — ${archived} card(s), ${conflicts_cleared} conflict(s), ${questions_purged} answered question(s), the feed`);
    onMutate();
    return {
      ok: true,
      archived,
      mail_expired,
      questions_expired,
      questions_purged,
      conflicts_cleared,
      feed_cleared,
      windows_killed,
      orphan_worktrees,
    };
  }

  // Per-card dismiss (Item 3): cleanup scoped to ONE offline card. Bulk "Clear"
  // archives every offline card at once; this retires exactly one, so a human
  // can dismiss a single dead session without waiting for 24h retention or
  // clearing the whole offline lane. It uses the same primitives cleanup does —
  // archive, expire the card's mail + questions, gone its non-terminal spawn
  // rows, kill a dead remain-on-exit window — just scoped by session_id, and
  // returns a control-API {status, body} so the route can speak real codes.
  async function dismissSession(sid) {
    const now = Date.now();
    const s = q.getSession.get(sid);
    if (!s) return { status: 404, body: { ok: false, reason: 'no such session' } };
    // A card is dismissible only once it is offline (a live/working card is the
    // human's to keep) and not already dismissed.
    if (s.col !== 'offline') return { status: 409, body: { ok: false, reason: `session is ${s.col}, not offline` } };
    if (s.archived_at != null) return { status: 409, body: { ok: false, reason: 'already dismissed' } };
    // A 'stalled' spawn is a fail-loud human problem that even bulk cleanup
    // refuses to sweep (archiveCandidates excludes it) — so dismiss refuses too,
    // rather than paper over a "pane up but never phoned home".
    if (q.stalledSpawnForSession.get(sid)) {
      return { status: 409, body: { ok: false, reason: 'session has a stalled spawn — resolve it first' } };
    }

    // setArchived carries AND archived_at IS NULL, so .changes===0 means a
    // concurrent dismiss claimed it a beat ago — report it already dismissed.
    if (!q.setArchived.run(now, sid).changes) {
      return { status: 409, body: { ok: false, reason: 'already dismissed' } };
    }
    const mail_expired = Number(q.expireMailForSession.run(now, sid).changes);
    const questions_expired = Number(questions.expireAllForSession(sid, { includeFreeform: true }));
    // Its non-terminal spawn rows go 'gone' so countActiveSpawns stops counting
    // a dismissed card (a 'stalled' row can't be here — refused above).
    q.goneSessionSpawns.run(sid);

    // Kill only a DEAD remain-on-exit window this card owns — the exact rule
    // cleanup uses (pane_dead + a terminal spawn status). A window whose pane is
    // still a live claude is never touched here; the liveness tick owns that.
    const myWindows = new Set(q.spawnsForSession.all(sid).map(r => r.tmux_window).filter(Boolean));
    let windows_killed = 0;
    if (myWindows.size) {
      const wins = await tmuxAdapter.listScopedWindows(port);
      const byName = new Map(q.allSpawns.all().map(r => [r.tmux_window, r]));
      for (const win of wins ?? []) {
        if (!myWindows.has(win.window)) continue;
        const sp = byName.get(win.window);
        if (!win.pane_dead || !sp || !['killed', 'pane-dead', 'gone'].includes(sp.status)) continue;
        const out = await tmuxAdapter.killWindowVerified(win.window);
        if (out.ok) windows_killed++;
      }
    }

    // Drop just this card's file ledger so the conflict radar can't keep
    // arguing on behalf of a corpse. The worktree on disk is deliberately LEFT
    // in place (it stays listed in the Worktrees modal for explicit cleanup).
    q.deleteTouchesForSession.run(sid);

    tick(`⌫ dismissed ${s.callsign} — card, ${mail_expired} mail, ${questions_expired} question(s)${windows_killed ? `, ${windows_killed} window(s)` : ''}`);
    onMutate();
    return { status: 200, body: { ok: true, archived: 1, mail_expired, questions_expired, windows_killed } };
  }

  return { retentionSweep, cleanup, dismissSession };
}
