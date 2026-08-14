// snapshot.ts — the /state snapshot (spike field names preserved; adds repo
// fields, sparklines, per-card spawn ownership + mail routing), plus fleetSize
// and the live-terminal spawn resolver. Threaded ctx state: q, t0, the STALE /
// ledger-retention / per-session-file-cap knobs, the questions relay, the mail
// facts (hasWatchWaiter/ownedPaneRow), spawnCapability + spawnState from the
// spawn module. spawnRowRevivable is a pure helper.

import { spawnRowRevivable, sessionAdoptableNow, safeParse } from './helpers.ts';
import { scrubUrlCredentials } from './payload-capture.ts';
// F1a: the /state + /ws envelope carries the wire schema version so a board on
// an older build can detect skew. Imported from the shared TS contract (the
// bundle inlines it; the engine floor runs the bundle). See ts-migration.md.
import { WIRE_SCHEMA_VERSION } from '../../contracts/index.ts';
import type { Statements, SpawnRow } from './statements.ts';

// resolveSettings()'s /state block. snapshot.ts reads only browse_root.resolved
// (→ home_dir) and otherwise ships the object verbatim, so the index tail keeps
// this honest that settings.mjs returns more fields than this leaf models.
interface ResolvedSettings {
  browse_root: { resolved: string | null };
  [k: string]: unknown;
}

interface SnapshotCtx {
  // The prepared-statement bundle only (the daemon threads q onto ctx), same
  // idiom as commands.ts / mail.ts — Statements['q'], not the whole Statements.
  q: Statements['q'];
  t0: number;
  version: string;
  STALE_MS: number;
  RETAIN_LEDGER_MS: number;
  SNAPSHOT_FILES_PER_SESSION: number;
  questions: { listForState: () => unknown };
  hasWatchWaiter: (sid: string) => boolean;
  ownedPaneRow: (sid: string) => unknown;
  spawnCapability: () => unknown;
  spawnState: { orphans: unknown };
  resolveSettings: () => ResolvedSettings;
}

export function createSnapshot(ctx: SnapshotCtx) {
  const {
    q,
    t0,
    version,
    STALE_MS,
    RETAIN_LEDGER_MS,
    SNAPSHOT_FILES_PER_SESSION,
    questions,
    hasWatchWaiter,
    ownedPaneRow,
    spawnCapability,
    spawnState,
    resolveSettings,
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
    // BUG-149: the per-session cap is now enforced in SQL too (ROW_NUMBER
    // partition in statements.mjs, same SNAPSHOT_FILES_PER_SESSION passed as
    // the second arg) — a session with a six-figure distinct-touch ledger no
    // longer ships every grouped row across the boundary just to drop them
    // here. The JS check stays as a harmless backstop.
    const filesBySid = new Map<string, string[]>();
    for (const row of q.filesBySession.all(now - RETAIN_LEDGER_MS, SNAPSHOT_FILES_PER_SESSION)) {
      let list = filesBySid.get(row.session_id);
      if (!list) {
        list = [];
        filesBySid.set(row.session_id, list);
      }
      if (list.length < SNAPSHOT_FILES_PER_SESSION) list.push(row.abs_path);
    }
    const sparkBySid = new Map<string, number[]>();
    const nowMin = Math.floor(now / 60000);
    for (const row of q.sparkline.all(now - 30 * 60000)) {
      let bins = sparkBySid.get(row.session_id);
      if (!bins) {
        bins = new Array<number>(30).fill(0);
        sparkBySid.set(row.session_id, bins);
      }
      const idx = 29 - (nowMin - row.minute);
      if (idx >= 0 && idx < 30) bins[idx] = row.n;
    }
    // v1.2: per-card spawn ownership (spawn object when a spawns row owns the
    // session) and the stale badge (working/verifying with no events for
    // FLEETDECK_STALE_MS — derived from lastSeen, zero new machinery).
    const visible = q.visibleSessions.all();
    // BUG-150: every frame used to rebuild spawnBySid from ALL spawn history
    // (revive lineages accumulate forever) and callsignById from ALL sessions
    // ever seen, although spawnBySid is only read for the visible cards and
    // callsignById only for the ≤20 bounded conflict rows. Both scans grew
    // with daemon lifetime while projecting a small visible fleet; the two
    // statements below are scoped to exactly the rows this frame consumes.
    const spawnBySid = new Map<string, SpawnRow>();
    for (const r of q.spawnByVisibleSession.all()) spawnBySid.set(r.session_id, r);
    const pendingBySid = new Map(
      q.pendingCounts.all(Date.now()).map((r) => [r.to_session, r] as const),
    );
    const callsignById = new Map(
      q.conflictCallsigns.all().map((s) => [s.session_id, s.callsign] as const),
    );
    // M-P2: the owned-pane and watcher facts feed mail_meta.route below; compute
    // each ONCE per session here rather than re-running getSession +
    // spawnBySession inside the route derivation.
    const waiterBySid = new Map<string, boolean>();
    const ownedPaneBySid = new Map<string, boolean>();
    for (const s of visible) {
      waiterBySid.set(s.session_id, hasWatchWaiter(s.session_id));
      ownedPaneBySid.set(s.session_id, !!ownedPaneRow(s.session_id));
    }
    const sessions = visible.map((s) => {
      const sp = spawnBySid.get(s.session_id);
      // 0.7.0 Move-to-tmux (adopt) eligibility. ANY spawn row — dead or alive —
      // means the board owns this session's pane story: revive owns dead
      // lineages, so an adopted/spawned card is never ALSO adopt-eligible (a
      // second lineage would fight the first over the window and worktree
      // bookkeeping). Cost mirrors the spawn.revivable note below:
      // sessionAdoptableNow's two fs probes run ONLY on the offline branch (a
      // live card takes the no-fs 'arm' path), and a fleet has a handful of
      // rows by design — deliberately uncached so move-to-tmux feedback stays
      // immediate. The `adopt` object is ALWAYS emitted (armed/deadline are
      // meaningful even when eligible is null); board-owned cards simply carry
      // eligible:null.
      const adoptEligible =
        s.ended_at != null
          ? sessionAdoptableNow(s, !!sp)
            ? 'now'
            : null
          : s.source === 'hooks' && !sp
            ? 'arm'
            : null;
      const adopt = {
        eligible: adoptEligible,
        armed: s.adopt_armed_until != null && s.adopt_armed_until > now,
        armed_until: s.adopt_armed_until ?? null,
        armed_skip: !!s.adopt_armed_skip,
      };
      return {
        session_id: s.session_id,
        callsign: s.callsign,
        // 0.6.0 ticket callsigns: the board renders these verbatim (null when
        // unset). prev_callsign lets the ticker/mail UI recognise the birth name.
        ticket: s.ticket ?? null,
        ticket_source: s.ticket_source ?? null,
        prev_callsign: s.prev_callsign ?? null,
        model: s.model,
        cwd: s.cwd,
        branch: s.branch,
        col: s.col,
        note: s.note,
        task: s.task,
        files: filesBySid.get(s.session_id) ?? [],
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
        // D11: the per-session mail_pending block ({count, oldest_at, deliverable})
        // was removed — the top-level mail_meta[session_id] carries the same facts
        // ({queued, oldest_at, route}) and is the single per-session source of
        // truth. The top-level mail_pending map (a simple {sid: count}) is KEPT: it
        // is the documented /state count field orchestrators read (commands/fleet.md).
        sparkline: sparkBySid.get(s.session_id) ?? new Array<number>(30).fill(0),
        // last_seen is a write-path-faithful non-null (base column, all INSERTs
        // bind it, every updater stamps Date.now()), so the spike's defensive
        // `s.last_seen != null` guard is provably-true dead code — dropped, since
        // `true && X === X` keeps this behaviour-identical. See ts-migration-bugs.
        stale: (s.col === 'working' || s.col === 'verifying') && now - s.last_seen > STALE_MS,
        ...(sp
          ? {
              spawn: {
                spawn_id: sp.spawn_id,
                tmux_window: sp.tmux_window,
                status: sp.status,
                kind: sp.kind ?? 'claude',
                setup_cmd: sp.setup_cmd ?? null,
                stalled: sp.status === 'stalled', // watchdog chip ("never registered")
                stall_detail: sp.status === 'stalled' ? (sp.stall_detail ?? null) : null,
                // Why this gate is NOT a copy of stall_detail's `=== 'stalled'`: a
                // failed clone ends at 'gone' (spawnCompensate's tombstone) or, when
                // an unverified pane had to be cleaned up, at 'stalled' — so reusing
                // that predicate would null the field in exactly the case it exists
                // for. And why it is gated at all rather than emitted raw:
                // resurrectableSpawns can flip a 'gone' row back to 'live', and the
                // gate makes a stale failure expander disappear from a healthy card
                // immediately, without waiting on a write. It is only half the
                // control though — the gate HIDES, it does not FORGET, so a row that
                // went 'stalled' with a detail, then registered 'live', then died
                // 'gone' would wear the old failure again. Both 'live' transitions
                // (events.mjs first hook, spawns.mjs resurrectSpawn) therefore NULL
                // the column out. origin_url stays unexposed here — it is the one
                // field that can carry credentials verbatim.
                fail_detail:
                  sp.status === 'gone' || sp.status === 'stalled' ? (sp.fail_detail ?? null) : null,
                skip_permissions: !!sp.skip_permissions, // v1.3 unsupervised chip
                remote: { enabled: !!sp.remote_control, url: sp.remote_url ?? null },
                // Which provider is serving this pane. A boolean, never the profile:
                // the card needs a badge, and the gateway's URL/credential are not
                // the card's business (see settings.mjs's masking note).
                gateway: !!sp.gateway,
                requested_branch: sp.requested_branch ?? null,
                branch_mode: sp.branch_mode ?? null,
                // Snapshot cost is intentionally uncached: two existsSync calls
                // per owned card keep removal/restore feedback immediate, and a
                // fleet has only a handful of rows by design. (M-P2 suggested a
                // short revivable TTL; deliberately NOT taken — the immediate
                // flip is a tested board contract, see revive.test.mjs; a cache
                // would need that test + a frontend sign-off. Coordination note.)
                revivable: sp.kind === 'shell' ? false : spawnRowRevivable(sp),
              },
            }
          : {}),
        // 0.7.0 Move-to-tmux: {eligible:'now'|'arm'|null, armed, armed_until,
        // armed_skip}. 'now' = adopt this offline card immediately; 'arm' = live
        // card, remember the click and move it when the CLI exits; null = not a
        // move-to-tmux candidate right now (board-owned, presumed dead, or a
        // live non-hooks card). armed reflects a live, unexpired arm.
        adopt,
      };
    });
    const repoMap = new Map<
      string,
      { repo_id: string | null; repo_name: string | null; active: number; total: number }
    >();
    for (const s of sessions) {
      const key = s.repo_id ?? '(none)';
      let r = repoMap.get(key);
      if (!r) {
        r = { repo_id: s.repo_id, repo_name: s.repo_name, active: 0, total: 0 };
        repoMap.set(key, r);
      }
      r.total++;
      if (!s.endedAt) r.active++;
      if (s.repo_name) r.repo_name = s.repo_name;
    }
    const mailPending: Record<string, number> = {};
    for (const row of pendingBySid.values()) mailPending[row.to_session] = row.n;
    for (const s of sessions) if (!(s.session_id in mailPending)) mailPending[s.session_id] = 0;
    // mail_meta: per-session delivery truth for the board — how the next mail
    // would reach this session RIGHT NOW. Same cheap approximation as the
    // per-session mail_pending.deliverable: no tmux probe in a snapshot.
    const mailMeta: Record<string, { queued: number; oldest_at: number | null; route: string }> =
      {};
    for (const s of sessions) {
      const p = pendingBySid.get(s.session_id);
      mailMeta[s.session_id] = {
        queued: p?.n ?? 0,
        oldest_at: p?.oldest_at ?? null,
        route: waiterBySid.get(s.session_id)
          ? 'watcher'
          : ownedPaneBySid.get(s.session_id)
            ? 'pane'
            : s.endedAt != null
              ? 'offline-queued'
              : 'turn-boundary',
      };
    }
    // Resolved ONCE per frame: home_dir below is derived from the same object,
    // and the two must never disagree within a single snapshot.
    const settings = resolveSettings();
    return {
      schema_version: WIRE_SCHEMA_VERSION, // F1a wire-version stamp (contracts/state.ts)
      up_ms: now - t0, // spike name, preserved
      uptime_ms: now - t0, // contract addition
      // Which build is serving the board — pairs with /health's version so an
      // upgrade takeover is visible in the footer without a second fetch.
      version,
      sessions,
      repos: [...repoMap.values()],
      repo_catalog: q.catalogRepos.all().map((repo) => ({
        repo_id: repo.repo_id,
        repo_name: repo.repo_name,
        root: repo.root,
        // The persisted origin comes verbatim from `git remote get-url origin`
        // (repos.mjs touchRepo backfill) and can carry credentials —
        // `https://user:PAT@host/org/repo.git`, `?access_token=…`. The spawn-row
        // snapshot withholds its origin outright for exactly this reason (see
        // the fail_detail gate note above); the catalog needs the URL itself for
        // spawn-form completion, so it goes out through the same positional
        // userinfo + secret-query-param scrub every other board-facing git
        // string gets (payload-capture.mjs). The raw value stays server-side.
        origin_url: repo.origin_url == null ? null : scrubUrlCredentials(repo.origin_url),
        default_branch: repo.default_branch ?? null,
        last_used_at: repo.last_used_at,
      })),
      settings, // {repos_dir, repo_transport, browse_root, fav_dirs}
      // home_dir means "the absolute root the /api/fs endpoints serve" — it is
      // NOT the user's home any more. A stale board (the previously committed
      // board-dist) composes its global-explorer paths against this field, so
      // it MUST track the same root the endpoints actually serve: shipping
      // os.homedir() while /api/fs served a Coder /workspace would make the
      // stale picker build /home/…/<sel> for rows listed from /workspace. The
      // key keeps its old name for one release of stale-board compatibility;
      // new boards read settings.browse_root.resolved.
      home_dir: settings.browse_root.resolved,
      ticker: q.recentTicker.all(),
      // Callsigns resolved from EVERY session, not just the visible ones: a
      // conflict outlives its participants, and a banner shouting a raw UUID at
      // you is worse than one that says `comet-2d9d`.
      conflicts: q.recentConflicts.all().flatMap((c) => {
        // M-B4: guarded parse (drop-on-corrupt), matching cleanup(). A single
        // corrupt sessions_json used to throw here and 500 EVERY /state and
        // every broadcast frame — poisoning the whole board off one bad row.
        // safeParse folds corrupt/absent sessions_json to `null`; the R2-6 guard
        // below drops it. Corrupt rows drop exactly as the old try/catch did.
        // One deliberate, writer-unreachable delta: a SQL-NULL sessions_json —
        // which the old `?? '[]'` turned into a valid empty array and EMITTED as
        // a zero-session conflict row — now folds to null and is dropped. ledger
        // .ts only ever writes a populated array, and the json_valid callsign
        // gate already excludes such rows, so dropping is the consistent choice.
        const ids: unknown = safeParse(c.sessions_json);
        // R2-6: valid JSON of the WRONG shape ('{}', 'null', '"s"', 42) parses
        // without throwing, but then `ids.map(...)` below throws — reopening the
        // same 500-every-frame hole the guard was meant to close. Require an
        // array; anything else is dropped exactly like a corrupt row.
        if (!Array.isArray(ids)) return [];
        // Array.isArray narrows `unknown` to `any[]`; re-bind to unknown[] so the
        // element callback below is not an unsafe-any pipeline (the elements are
        // session-id strings, but a corrupt row could hold anything).
        const sessionIds = ids as unknown[];
        return [
          {
            at: c.at,
            repo_id: c.repo_id,
            rel_path: c.rel_path,
            file: c.rel_path, // spike board reads .file
            severity: c.severity,
            sessions: sessionIds,
            // Elements are session-id strings; a non-string element simply misses
            // the callsign map and falls back to its raw self, as in the .mjs.
            callsigns: sessionIds.map((id) => callsignById.get(id as string) ?? id),
          },
        ];
      }),
      mail_pending: mailPending,
      mail_meta: mailMeta, // per-session {queued, oldest_at, route}
      questions: questions.listForState(), // F3: pending + last few resolved
      spawn: spawnCapability(), // v1.2 capability flag
      spawn_orphans: spawnState.orphans, // v1.2 "unadopted" scoped windows
      // v1.3 plan library: non-archived, newest first, cap 20. RAW plan_md
      // only — title derivation (first heading / first line) is a board
      // concern, never the daemon's.
      plans: q.plansForState.all().map((p) => ({
        plan_id: p.plan_id,
        session_id: p.session_id,
        callsign: p.callsign,
        repo_id: p.repo_id,
        repo_name: p.repo_name,
        plan_md: p.plan_md,
        created_at: p.created_at,
        status: p.status,
        via: p.executed_via, // optional {via} recorded by POST /api/plans/:id/mark
      })),
    };
  }

  // Health semantics (BUG-193): the CURRENT fleet — the cards /state can
  // show. Dismissed and retention-archived rows are history, not fleet, so an
  // unscoped COUNT(*) left /health.fleet and `fleetdeck status` permanently
  // overstating the fleet by every card ever archived.
  function fleetSize(): number {
    // COUNT(*) always returns exactly one row; the ?? 0 is a dead type-guard.
    return q.countVisibleSessions.get()?.n ?? 0;
  }

  // Live-terminal target resolver (CONTRACT): the HTTP/WS layer gets a row
  // only by opaque spawn id. It never accepts a pane/window target supplied
  // by a browser; termbridge performs the remaining fleet/status checks.
  function terminalSpawn(spawnId: string) {
    return q.getSpawn.get(spawnId) ?? null;
  }

  return { snapshot, fleetSize, terminalSpawn };
}
