// retention.mjs — the non-destructive retention sweep (silence presume-dead,
// tmux-adjudicated spawned silence, offline archival, ledger aging) and the
// manual cleanup() ("Clear means clear"). The boot invocation + the 10-minute
// interval live in the composition root. Threaded ctx state: q, updateSession,
// tick, onMutate, tombstoneCard (the shared offline-tombstone write, which
// drops the model memo + wakes watchers), forgetSpawn, the tmux adapter, port,
// the questions relay, and the PRESUME_DEAD / RETAIN_OFFLINE / ledger knobs.
// SHELL_RE is a pure helper.

import fs from 'node:fs';
import { SHELL_RE, NOT_RESUMABLE_END } from './helpers.ts';
import { CONFLICT_WINDOW_MS } from './ledger.ts';
import { pruneRunNonces } from './run-nonce.mjs';

export function createRetention(ctx) {
  const {
    q, updateSession, tick, onMutate, tombstoneCard, forgetSpawn,
    tmuxAdapter, port, home, questions, adoptSession, scopedPaneTarget,
    PRESUME_DEAD_MS, PRESUME_DEAD_WORKING_MS, RETAIN_OFFLINE_MS, RETAIN_LEDGER_MS,
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
    // Mid-turn columns ride the same machinery on a longer horizon — see
    // presumeDeadWorkingSessions for why they need one at all (without it a
    // hook session that dies mid-turn is unclearable forever). Concatenated
    // rather than branched so every candidate gets identical treatment: spawn
    // rows are adjudicated by tmux, pane-less rows by silence.
    const candidates = [
      ...q.presumeDeadSessions.all(now - PRESUME_DEAD_MS),
      ...q.presumeDeadWorkingSessions.all(now - PRESUME_DEAD_WORKING_MS),
    ];
    for (const s of candidates) {
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
          // BUG-045 (stale probe tombstones a revived spawn): every value in
          // {s, sp} was read BEFORE the listScopedWindows await — a revive that
          // landed during it (or during the paneCurrentCommand await below) can
          // have settled this row and stood a NEWER live row up on the same
          // window. Condemning on the stale row would tombstone a live, billed
          // agent. So before any write, re-read the world and require that the
          // SAME spawn row is still the session's live-eligible spawn AND the
          // window's current owner (both re-checked after every tmux await).
          const stillOurs = () => {
            const cur = q.activeSpawnBySession.get(s.session_id);
            if (!cur || cur.spawn_id !== sp.spawn_id) return false;
            const owner = q.currentWindowOwner.get(sp.tmux_window);
            return !owner || owner.spawn_id === sp.spawn_id;
          };
          if (!stillOurs()) continue;
          const win = wins.find(w => w.window === sp.tmux_window);
          // Alive: window present, pane not dead, and paneCurrentCommand confirms
          // claude (pane_cmd can read stale on remain-on-exit panes). The agent
          // is simply quiet — refresh last_seen so the clock restarts and leave
          // the card live. This is the "3.1h alive spawn got goned" fix.
          let alive = false;
          const shell = sp.kind === 'shell';
          const setupPhase = !!sp.setup_cmd && (sp.status === 'spawning' || sp.status === 'stalled');
          if (win && !win.pane_dead) {
            const pane = await tmuxAdapter.paneCurrentCommand(scopedPaneTarget(win));
            alive = !!pane && !pane.dead && (shell || setupPhase || pane.cmd === 'claude');
          }
          if (alive) {
            if (!stillOurs()) continue; // a revive interleaved the pane probe
            updateSession(s.session_id, { last_seen: now });
            changed = true;
            continue;
          }
          // tmux CONFIRMS dead — window present but pane_dead or a bare shell.
          // Condemn exactly like the liveness tick: flip the spawn 'pane-dead'
          // (still revivable) and tombstone the card. A window that is ABSENT is
          // UNKNOWN, not dead — never condemn on silence (a wrong "dead" costs a
          // duplicate billed session); leave it for a later sweep / boot reconcile.
          if (win && (win.pane_dead || (!shell && !setupPhase && SHELL_RE.test(win.pane_cmd)))) {
            if (!stillOurs()) continue; // a revive interleaved the tmux awaits
            q.setSpawnStatus.run('pane-dead', sp.spawn_id);
            forgetSpawn(sp.spawn_id);
            tombstoneCard(s.session_id, { // D8
              note: setupPhase
                ? 'pane exited during setup/bring-up — open the terminal for the error'
                : `pane confirmed dead — resume with claude --resume ${s.session_id}`,
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
    // File touches are the conflict radar's only evidence, so their pruning
    // floor is the radar's own window: envInt's below-min value falls back to
    // the 24h default, but an accepted horizon between one minute and
    // CONFLICT_WINDOW_MS would prune touches the radar still promises to
    // consider (BUG-144). Commands/conflicts/mail keep the configured horizon.
    const touchCutoff = now - Math.max(RETAIN_LEDGER_MS, CONFLICT_WINDOW_MS);
    if (q.pruneTouches.run(touchCutoff).changes) changed = true;
    // Hook run-nonce files (one per CLI process, HOME/run-<pid>) are the only
    // state here nothing ever collected: one per session for the life of the
    // HOME. Removed only when the pid is provably dead and the file has aged,
    // so a live CLI never loses the nonce its next hook will read. (Before the
    // keying fix these accumulated per HOOK — 510 in one observed session.)
    pruneRunNonces(home, { now });
    const ledgerCutoff = now - RETAIN_LEDGER_MS;
    if (q.pruneCommands.run(ledgerCutoff).changes) changed = true;
    if (q.pruneConflicts.run(ledgerCutoff).changes) changed = true;
    if (q.pruneSettledMail.run(ledgerCutoff).changes) changed = true;
    // BUG-034: an in-flight mail claim (claimed_at lease) whose deadline
    // passed never got its acknowledgement — the consumer disconnected or the
    // daemon restarted mid-delivery. Hand the row back to the claimable pool
    // so the next watcher / turn boundary / pane delivery re-delivers it.
    if (q.expireStalledClaims.run(now).changes) changed = true;
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

    // --- window-kill phase FIRST (BUG-145): the only awaits, before a single
    // byte of the DB story changes. Only windows a TERMINAL spawn row names
    // are Clear's business — the DB is the complete roster of what the fleet
    // ever owned. A null listing is UNKNOWN (tmux unreachable, generation
    // check failed, malformed rows): the old `?? []` read it as empty and
    // Clear reported success while every eligible dead window stayed up,
    // hidden with no retry path. So with named windows outstanding and the
    // listing UNKNOWN, fail loud with nothing touched — the next Clear is a
    // full retry. (With NOTHING named, an unreachable tmux cannot hide one of
    // this fleet's windows — no spawn row anywhere points at one — so the
    // Clear is honestly complete without the probe.)
    const byName = new Map(q.allSpawns.all().map(r => [r.tmux_window, r]));
    const namedDead = [...byName.values()].filter(sp =>
      sp.tmux_window && ['killed', 'pane-dead', 'gone'].includes(sp.status));
    let windows_killed = 0;
    // BUG-046: windows a revive reclaimed with a fresh live pane during the kill
    // phase below. Their new live spawn row must survive the archived-spawn
    // sweep even though its session is about to be archived — pane liveness
    // trumps archival.
    const reclaimed = new Set();
    if (namedDead.length) {
      const wins = await tmuxAdapter.listScopedWindows(port);
      if (wins === null) {
        return { ok: false, reason: 'tmux window listing unavailable — nothing was cleared; retry Clear' };
      }
      const window_errors = [];
      for (const win of wins) {
        const sp = byName.get(win.window);
        if (!win.pane_dead || !sp || !['killed', 'pane-dead', 'gone'].includes(sp.status)) continue;
        // BUG-046: the pane_dead + terminal-status checks above predate the
        // kill's own awaits — a revive can land DURING them and stand a fresh
        // live pane up on this deterministic name. Move the verdict to kill
        // time: pass the window generation and an owner re-check so a
        // generation/owner swap degrades to a stale no-op instead of destroying
        // the replacement pane. Mirrors the bulk-Clear path below.
        const out = await tmuxAdapter.killWindowVerified(win.window, {
          expectWindowId: win.window_id,
          expect: () => {
            const owner2 = q.currentWindowOwner.get(win.window);
            // currentWindowOwner excludes gone/killed; a live-eligible owner
            // (or one for a different session) means a revive reclaimed the name.
            return !(owner2 && (owner2.session_id !== sp.session_id || owner2.status !== 'pane-dead'));
          },
        });
        // {ok:false, gone:true} is fresh proof of absence — that counts as cleared.
        if (out.ok || out.gone) windows_killed++;
        // BUG-046: {ok:false, stale:true} means the kill's re-check caught a
        // revive reclaiming this window name mid-kill — the pane there now is
        // live work, not the corpse we verified. Leave it; never an error.
        else if (out.stale) { reclaimed.add(win.window); /* revive reclaimed the name — no-op */ }
        // A kill that comes back {ok:false} without proof of absence leaves the
        // window standing on the fleet's tmux session, holding its reusable
        // name — never report success.
        else window_errors.push(`${win.window}: ${out.error || 'kill failed'}`);
      }
      if (window_errors.length) {
        return {
          ok: false,
          reason: `${window_errors.length} window(s) could not be killed — ${window_errors.join('; ').slice(0, 200)} — nothing was cleared; retry Clear`,
        };
      }
    }

    const archived = Number(q.archiveAllOffline.run(now).changes);
    const mail_expired = Number(q.expireArchivedMail.run(now).changes);
    let questions_expired = 0;
    for (const sid of archiving) {
      questions_expired += Number(questions.expireAllForSession(sid, { includeFreeform: true }));
    }
    // Sweep the archived sessions' non-terminal spawns to 'gone' — but spare any
    // whose window a revive reclaimed with a live pane during the kill phase
    // (BUG-046): that is live work, not the archived corpse. With no reclaim
    // (the common case) the bulk sweep runs unchanged.
    if (reclaimed.size) {
      for (const sp of q.sweepableArchivedSpawns.all()) {
        if (sp.tmux_window && reclaimed.has(sp.tmux_window)) continue;
        q.setSpawnStatus.run('gone', sp.spawn_id);
      }
    } else {
      q.goneArchivedSpawns.run();
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
    // Refuse while the session still owns a live-eligible spawn row (R4-review):
    //   • 'stalled'          — a fail-loud human problem bulk cleanup also refuses
    //                          to sweep (archiveCandidates excludes it).
    //   • 'spawning'/'live'   — an ACTIVE row. Dismissing it would flip it 'gone',
    //                          and the very next liveness tick's resurrectSpawn
    //                          would clear archived_at and re-float the card as a
    //                          zombie the human can't re-dismiss until it dies.
    //                          Kill it first (☠), then dismiss the corpse.
    // (A genuinely-live claude sitting behind an ALREADY-'gone' row still gets
    // resurrected by design — the board must never hide a live billed agent —
    // exactly the same semantics as bulk Clear; dismiss simply refuses to CREATE
    // that situation from a still-active row.)
    const active = q.activeSpawnBySession.get(sid);
    if (active) {
      const reason = active.status === 'stalled'
        ? 'session has a stalled spawn — resolve it first'
        : `session still owns a ${active.status} spawn — kill it before dismissing`;
      return { status: 409, body: { ok: false, reason } };
    }

    // --- atomic DB block (R1-review): NO awaits, so it completes in one JS turn
    // and no hook event (applyEvent resurrection, /clear succession) can
    // interleave and leave the card half-dismissed. setArchived carries
    // `AND archived_at IS NULL`, so .changes===0 means a concurrent dismiss
    // claimed it a beat ago — report it already dismissed and touch nothing else.
    if (!q.setArchived.run(now, sid).changes) {
      return { status: 409, body: { ok: false, reason: 'already dismissed' } };
    }
    const mail_expired = Number(q.expireMailForSession.run(now, sid).changes);
    const questions_expired = Number(questions.expireAllForSession(sid, { includeFreeform: true }));
    // Any residual non-terminal spawn row (only a rare pre-pane 'provisioning'
    // survives the active-guard above) goes 'gone' so it stops counting active.
    q.goneSessionSpawns.run(sid);
    // Drop just this card's file ledger so the conflict radar can't keep arguing
    // on behalf of a corpse. In the sync block WITH the rest of the DB story, so
    // a mid-await resurrection can never observe a torn state. The worktree on
    // disk is deliberately LEFT in place (still listed in the Worktrees modal).
    q.deleteTouchesForSession.run(sid);

    // --- window-kill phase: the only awaits. A hook can resurrect the card
    // DURING an await (UserPromptSubmit → applyEvent clears archived_at); the
    // window is then a live session's again and NOT ours to kill, so re-read the
    // session after every await and bail the instant it is un-archived.
    const alive = () => q.getSession.get(sid)?.archived_at == null;
    const myWindows = new Set(q.spawnsForSession.all(sid).map(r => r.tmux_window).filter(Boolean));
    let windows_killed = 0;
    let resurrected = false;
    // BUG-145: the archive must NOT be reported as a plain success when the
    // card's dead windows could not be killed — that hid a stale window AND
    // burned the retry path (a second dismiss 409s 'already dismissed'). The
    // DB story already landed above, so the partial truth is surfaced as an
    // explicit incomplete result: ok:false, a reason naming every failed
    // window, and retry:true (the idempotent call below re-attempts the kill
    // for an already-archived card).
    const window_errors = [];
    const incomplete = (reason) => ({
      status: 409,
      body: {
        ok: false, archived: 1, mail_expired, questions_expired, windows_killed,
        retry: true, reason, ...(resurrected ? { resurrected: true } : {}),
      },
    });
    if (myWindows.size) {
      const wins = await tmuxAdapter.listScopedWindows(port);
      if (alive()) {
        resurrected = true;
      } else if (wins === null) {
        // UNKNOWN listing — none of this card's windows can even be inspected.
        return incomplete('tmux window listing unavailable — card archived, dead window(s) not killed; dismiss again to retry');
      } else {
        for (const win of wins) {
          if (!myWindows.has(win.window) || !win.pane_dead) continue;
          // R2-review (stale window-owner): a concurrent revive() can insert a
          // NEWER row owning this reused window name and stand a fresh live pane
          // up on it; killWindowVerified re-resolves BY NAME, so it would kill
          // the replacement. Kill only when the window is still owned by a
          // pane-dead row of THIS session (or by no live-eligible row at all —
          // currentWindowOwner excludes 'gone'/'killed', so null means a corpse
          // no revive has reclaimed). Anything else (an active owner, or another
          // session's row) means a live pane now lives there: skip it.
          const owner = q.currentWindowOwner.get(win.window);
          if (owner && (owner.session_id !== sid || owner.status !== 'pane-dead')) continue;
          // BUG-046: the check above still predates the kill's own awaits — a
          // revive can land DURING them, after the owner check passed. Move the
          // verdict to kill time: the kill primitive re-runs `expect` after its
          // final name re-resolve, so a window/pane generation swap (revive
          // killed the remnant and recreated the name), an owner flip to a
          // live-eligible row, or a hook resurrection mid-kill all degrade to a
          // stale no-op instead of destroying the replacement pane.
          const out = await tmuxAdapter.killWindowVerified(win.window, {
            expectWindowId: win.window_id,
            expect: () => {
              const owner2 = q.currentWindowOwner.get(win.window);
              if (owner2 && (owner2.session_id !== sid || owner2.status !== 'pane-dead')) return false;
              return !alive();
            },
          });
          // BUG-145: {ok:false, gone:true} is fresh proof of absence — that
          // counts as cleared. A kill that comes back {ok:false} without proof
          // of absence leaves the window standing, holding its reusable name —
          // never report success; surface it for the retry path below.
          if (out.ok || out.gone) windows_killed++;
          // BUG-046: {ok:false, stale:true} means the kill's own re-check caught
          // a revive reclaiming this window name mid-kill (generation/owner
          // swap). The pane standing there now is LIVE work, not the corpse we
          // set out to kill — a correct no-op, NOT an unkilled dead window. It
          // must NOT become a window_error: doing so would 409 the dismiss and
          // falsely imply a dead window still stands, and (worse) the retry would
          // then chase a name that rightly belongs to the replacement. Composes
          // with BUG-145: only a genuine {ok:false} (no gone, no stale) is a
          // failure that keeps the retry path open.
          else if (out.stale) { /* revive reclaimed the name mid-kill — leave it */ }
          else window_errors.push(`${win.window}: ${out.error || 'kill failed'}`);
          if (alive()) { resurrected = true; break; }
        }
        if (window_errors.length) {
          return incomplete(`${window_errors.length} window(s) could not be killed — ${window_errors.join('; ').slice(0, 200)} — card archived; dismiss again to retry`);
        }
      }
    }

    tick(`⌫ dismissed ${s.callsign} — card, ${mail_expired} mail, ${questions_expired} question(s)${windows_killed ? `, ${windows_killed} window(s)` : ''}`);
    onMutate();
    return {
      status: 200,
      // `resurrected` is surfaced only when it happened — a hook re-floated the
      // card mid-dismiss, so the DB story already landed but the pane was left
      // alone. The normal path omits it (the route's key set stays stable).
      body: { ok: true, archived: 1, mail_expired, questions_expired, windows_killed, ...(resurrected ? { resurrected: true } : {}) },
    };
  }

  // BUG-145 retry path: a dismiss whose window-kill phase failed reports
  // retry:true. This is that retry — idempotent: it skips every guard and DB
  // mutation (the card is already archived) and ONLY re-attempts killing the
  // card's dead remain-on-exit windows, with the same ownership re-checks.
  // 200 when every eligible pane is now killed or freshly verified absent,
  // 409 with retry:true again while any kill is still unverifiable.
  async function dismissRetry(sid) {
    const s = q.getSession.get(sid);
    if (!s) return { status: 404, body: { ok: false, reason: 'no such session' } };
    if (s.archived_at == null) return { status: 409, body: { ok: false, reason: 'session is not dismissed — nothing to retry' } };
    const myWindows = new Set(q.spawnsForSession.all(sid).map(r => r.tmux_window).filter(Boolean));
    if (!myWindows.size) {
      return { status: 200, body: { ok: true, windows_killed: 0 } };
    }
    const wins = await tmuxAdapter.listScopedWindows(port);
    if (wins === null) {
      return { status: 409, body: { ok: false, retry: true, reason: 'tmux window listing unavailable — retry again' } };
    }
    let windows_killed = 0;
    const window_errors = [];
    for (const win of wins) {
      if (!myWindows.has(win.window) || !win.pane_dead) continue;
      const owner = q.currentWindowOwner.get(win.window);
      if (owner && (owner.session_id !== sid || owner.status !== 'pane-dead')) continue;
      const out = await tmuxAdapter.killWindowVerified(win.window);
      if (out.ok || out.gone) windows_killed++;
      // BUG-046: a stale verdict (a revive reclaimed the name) is a no-op here
      // too, never a retry-worthy failure.
      else if (out.stale) { /* revive reclaimed the name — leave it */ }
      else window_errors.push(`${win.window}: ${out.error || 'kill failed'}`);
    }
    if (window_errors.length) {
      return {
        status: 409,
        body: { ok: false, retry: true, windows_killed, reason: `${window_errors.length} window(s) could not be killed — ${window_errors.join('; ').slice(0, 200)} — retry again` },
      };
    }
    return { status: 200, body: { ok: true, windows_killed } };
  }

  return { retentionSweep, cleanup, dismissSession, dismissRetry };
}
