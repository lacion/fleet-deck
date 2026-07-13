// snapshot.mjs — the /state snapshot (spike field names preserved; adds repo
// fields, sparklines, per-card spawn ownership + mail routing), plus fleetSize
// and the live-terminal spawn resolver. Threaded ctx state: q, t0, the STALE /
// ledger-retention / per-session-file-cap knobs, the questions relay, the mail
// facts (hasWatchWaiter/ownedPaneRow), spawnCapability + spawnState from the
// spawn module. spawnRowRevivable is a pure helper.

import { spawnRowRevivable } from './helpers.mjs';

export function createSnapshot(ctx) {
  const {
    q, t0, STALE_MS, RETAIN_LEDGER_MS, SNAPSHOT_FILES_PER_SESSION,
    questions, hasWatchWaiter, ownedPaneRow, spawnCapability, spawnState,
  } = ctx;

  // -------------------------------------------------------------- snapshot
  // Spike field names preserved; adds repo fields + sparkline + uptime.
  function snapshot() {
    const now = Date.now();
    // M-G1: window the touch aggregation by time (retentionSweep deletes older
    // rows) and cap the per-session list, so a snapshot can never GROUP-BY an
    // unbounded table nor ship an unbounded file list per card on every frame.
    // R2-7: filesBySession is ordered newest-touch-first, so taking the first
    // SNAPSHOT_FILES_PER_SESSION keeps each card's MOST RECENT files.
    const filesBySid = new Map();
    for (const row of q.filesBySession.all(now - RETAIN_LEDGER_MS)) {
      let list = filesBySid.get(row.session_id);
      if (!list) { list = []; filesBySid.set(row.session_id, list); }
      if (list.length < SNAPSHOT_FILES_PER_SESSION) list.push(row.abs_path);
    }
    const sparkBySid = new Map();
    const nowMin = Math.floor(now / 60000);
    for (const row of q.sparkline.all(now - 30 * 60000)) {
      if (!sparkBySid.has(row.session_id)) sparkBySid.set(row.session_id, new Array(30).fill(0));
      const idx = 29 - (nowMin - row.minute);
      if (idx >= 0 && idx < 30) sparkBySid.get(row.session_id)[idx] = row.n;
    }
    // v1.2: per-card spawn ownership (spawn object when a spawns row owns the
    // session) and the stale badge (working/verifying with no events for
    // FLEETDECK_STALE_MS — derived from lastSeen, zero new machinery).
    const spawnBySid = new Map();
    for (const r of q.allSpawns.all()) spawnBySid.set(r.session_id, r);
    const pendingBySid = new Map(q.pendingCounts.all().map(r => [r.to_session, r]));
    const callsignById = new Map(q.allSessions.all().map(s => [s.session_id, s.callsign]));
    const visible = q.visibleSessions.all();
    // M-P2: the owned-pane and watcher facts were each recomputed TWICE per
    // session (mail_pending.deliverable and again in mail_meta.route, each
    // doing its own getSession + spawnBySession query). Compute each once.
    const waiterBySid = new Map();
    const ownedPaneBySid = new Map();
    for (const s of visible) {
      waiterBySid.set(s.session_id, hasWatchWaiter(s.session_id));
      ownedPaneBySid.set(s.session_id, !!ownedPaneRow(s.session_id));
    }
    const sessions = visible.map(s => {
      const sp = spawnBySid.get(s.session_id);
      const pending = pendingBySid.get(s.session_id);
      return {
        session_id: s.session_id,
        callsign: s.callsign,
        model: s.model,
        cwd: s.cwd,
        branch: s.branch,
        col: s.col,
        note: s.note,
        task: s.task,
        files: filesBySid.get(s.session_id) || [],
        lastTool: s.last_tool,
        events: s.events,
        startedAt: s.started_at,
        lastSeen: s.last_seen,
        endedAt: s.ended_at,
        repo_id: s.repo_id,
        repo_name: s.repo_name,
        worktree: s.worktree,
        source: s.source,
        notification_type: s.notification_type ?? null, // F3e: WHY it needs you
        mail_pending: {
          count: pending?.n ?? 0,
          oldest_at: pending?.oldest_at ?? null,
          // Approximation by design: no tmux subprocess in a snapshot. A
          // qualifying active spawn row is treated as pane-capable here.
          deliverable: waiterBySid.get(s.session_id) || ownedPaneBySid.get(s.session_id),
        },
        sparkline: sparkBySid.get(s.session_id) || new Array(30).fill(0),
        stale: (s.col === 'working' || s.col === 'verifying')
          && s.last_seen != null && (now - s.last_seen > STALE_MS),
        ...(sp ? {
          spawn: {
            spawn_id: sp.spawn_id,
            tmux_window: sp.tmux_window,
            status: sp.status,
            stalled: sp.status === 'stalled', // watchdog chip ("never registered")
            skip_permissions: !!sp.skip_permissions, // v1.3 unsupervised chip
            remote: { enabled: !!sp.remote_control, url: sp.remote_url ?? null },
            // Snapshot cost is intentionally uncached: two existsSync calls
            // per owned card keep removal/restore feedback immediate, and a
            // fleet has only a handful of rows by design. (M-P2 suggested a
            // short revivable TTL; deliberately NOT taken — the immediate
            // flip is a tested board contract, see revive.test.mjs; a cache
            // would need that test + a frontend sign-off. Coordination note.)
            revivable: spawnRowRevivable(sp),
          },
        } : {}),
      };
    });
    const repoMap = new Map();
    for (const s of sessions) {
      const key = s.repo_id ?? '(none)';
      if (!repoMap.has(key)) repoMap.set(key, { repo_id: s.repo_id, repo_name: s.repo_name, active: 0, total: 0 });
      const r = repoMap.get(key);
      r.total++;
      if (!s.endedAt) r.active++;
      if (s.repo_name) r.repo_name = s.repo_name;
    }
    const mailPending = {};
    for (const row of pendingBySid.values()) mailPending[row.to_session] = row.n;
    for (const s of sessions) if (!(s.session_id in mailPending)) mailPending[s.session_id] = 0;
    // mail_meta: per-session delivery truth for the board — how the next mail
    // would reach this session RIGHT NOW. Same cheap approximation as the
    // per-session mail_pending.deliverable: no tmux probe in a snapshot.
    const mailMeta = {};
    for (const s of sessions) {
      const p = pendingBySid.get(s.session_id);
      mailMeta[s.session_id] = {
        queued: p?.n ?? 0,
        oldest_at: p?.oldest_at ?? null,
        route: waiterBySid.get(s.session_id) ? 'watcher'
          : ownedPaneBySid.get(s.session_id) ? 'pane'
            : s.endedAt != null ? 'offline-queued'
              : 'turn-boundary',
      };
    }
    return {
      up_ms: now - t0,          // spike name, preserved
      uptime_ms: now - t0,      // contract addition
      sessions,
      repos: [...repoMap.values()],
      ticker: q.recentTicker.all(),
      // Callsigns resolved from EVERY session, not just the visible ones: a
      // conflict outlives its participants, and a banner shouting a raw UUID at
      // you is worse than one that says `comet-2d9d`.
      conflicts: q.recentConflicts.all().flatMap(c => {
        // M-B4: guarded parse (drop-on-corrupt), matching cleanup(). A single
        // corrupt sessions_json used to throw here and 500 EVERY /state and
        // every broadcast frame — poisoning the whole board off one bad row.
        let ids;
        try { ids = JSON.parse(c.sessions_json || '[]'); }
        catch { return []; }
        // R2-6: valid JSON of the WRONG shape ('{}', 'null', '"s"', 42) parses
        // without throwing, but then `ids.map(...)` below throws — reopening the
        // same 500-every-frame hole the guard was meant to close. Require an
        // array; anything else is dropped exactly like a corrupt row.
        if (!Array.isArray(ids)) return [];
        return [{
          at: c.at,
          repo_id: c.repo_id,
          rel_path: c.rel_path,
          file: c.rel_path,       // spike board reads .file
          severity: c.severity,
          sessions: ids,
          callsigns: ids.map(id => callsignById.get(id) ?? id),
        }];
      }),
      mail_pending: mailPending,
      mail_meta: mailMeta, // per-session {queued, oldest_at, route}
      questions: questions.listForState(), // F3: pending + last few resolved
      spawn: spawnCapability(),            // v1.2 capability flag
      spawn_orphans: spawnState.orphans,         // v1.2 "unadopted" scoped windows
      // v1.3 plan library: non-archived, newest first, cap 20. RAW plan_md
      // only — title derivation (first heading / first line) is a board
      // concern, never the daemon's.
      plans: q.plansForState.all().map(p => ({
        plan_id: p.plan_id,
        session_id: p.session_id,
        callsign: p.callsign,
        repo_id: p.repo_id,
        repo_name: p.repo_name,
        plan_md: p.plan_md,
        created_at: p.created_at,
        status: p.status,
      })),
    };
  }

  function fleetSize() {
    return q.countSessions.get().n;
  }

  // Live-terminal target resolver (CONTRACT): the HTTP/WS layer gets a row
  // only by opaque spawn id. It never accepts a pane/window target supplied
  // by a browser; termbridge performs the remaining fleet/status checks.
  function terminalSpawn(spawnId) {
    return q.getSpawn.get(spawnId) || null;
  }

  return { snapshot, fleetSize, terminalSpawn };
}

