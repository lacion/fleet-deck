// spawns.mjs — the v1.2/v1.3 board-spawned session lifecycle: spawn / revive /
// enableRemote / spawnKill, the ~10s owned-pane liveness tick, boot
// reconciliation, and the RC-URL harvest + bring-up nudge ephemera. Threaded
// ctx state: q, updateSession, tick, logEvent, onMutate, assignCallsign,
// notifyWatchers, tombstoneCard (the shared offline-tombstone write),
// findScopedWindow, the tmux adapter, port/home, and the NUDGE/REGISTER/
// RC-harvest knobs. spawnState.orphans (the unadopted-window list) is exposed
// on ctx for the snapshot.

import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { deriveRepo, branchOf } from './repo-identity.mjs';
import { ticketFromBranch, animalOf } from './tickets.mjs';
import { execFileP, claudeEnvArgvPrefix, claudeTranscriptPath, SHELL_RE } from './helpers.mjs';

export function createSpawns(ctx) {
  const {
    q, updateSession, tick, logEvent, onMutate, assignCallsign,
    notifyWatchers, tombstoneCard, findScopedWindow, tmuxAdapter, port, home,
    NUDGE_MS, SPAWN_REGISTER_MS, RC_HARVEST_MS,
    ADOPT_ARM_MS, // 0.7.0 Move-to-tmux: arm-deadline window (default 30 min)
  } = ctx;

  // ------------------------------------- v1.2 board-spawned sessions (spawns)
  // CONTRACT "v1.2 — dynamic fleet". Spawn = explicit human click ONLY; the
  // spawned command is interactive `claude` behind a deterministic `env`
  // argv wrapper; no auto-respawn, ever. All tmux construction is argv arrays.
  //
  // 0.7.0 Move-to-tmux (adopt) does NOT break "explicit human click ONLY" even
  // in its armed, fires-later form: the click that ARMED the card WAS the human
  // decision. The deferred auto-adopt on the session's own SessionEnd is that
  // single click cashing in — one-shot (the arm is durable intent, CONSUMED by
  // adoptSession inside its single-flight claim; a failed launch never
  // retries, and a deferred call that finds the arm gone or the session live
  // again stands down instead of acting), cancellable (disarm any time,
  // including inside the post-SessionEnd grace window, or it self-expires
  // after FLEETDECK_ADOPT_ARM_MS), visible (the armed state + deadline ride
  // the snapshot's per-session `adopt` object), and expiring (a deadline, not
  // a standing order). The retention sweep completes an armed move orphaned by
  // a daemon death inside the grace window — still the same single click. It
  // is not auto-respawn: nothing the daemon decides on its own ever launches a
  // pane — a human clicked, once.
  //
  // Row state machine:
  //   'spawning'  insert at POST /api/spawn
  //     → 'stalled'    registration deadline passed while claude pane lives
  //     → 'live'       first hook event for the pre-issued session_id
  //                    (the source flip in applyEvent above)
  //     → 'pane-dead'  liveness tick saw #{pane_dead}=1 or a bare shell
  //                    (crash, no SessionEnd), OR SessionEnd landed
  //                    (hookSessionEnd above — graceful end, pane kept)
  //     → 'gone'       boot reconciliation: window (exact scoped name) missing
  //     → 'killed'     POST /api/spawn/:id/kill succeeded (name-verified)
  //   'stalled' → live (late hook) | pane-dead | gone | killed
  //   'live' → pane-dead | gone | killed (same triggers)
  //   'pane-dead' → killed (the dead pane's window still exists until killed)
  //               → gone   (kill found the window already gone: 410)
  //   'pane-dead' / 'killed' / 'gone' are terminal. A stalled live pane still
  //   counts against the cap; the daemon never auto-kills or auto-respawns it.

  // Capability flag (/health and /state). available=false when tmux is absent
  // or FLEETDECK_SPAWN=off; the FLEETDECK_SPAWN_CMD override reports
  // available:true with reason 'test-override'. The board hides ALL spawn UI
  // when available is false.
  //
  // `active` is a COUNT, not a budget: there is no cap on how many agents may
  // be live at once. The board shows it so you know how big your fleet is, and
  // the spawn form makes you look at exactly what you are about to launch —
  // that is the guardrail, not a refusal from the daemon.
  function spawnCapability() {
    const base = { active: q.countActiveSpawns.get().n };
    if (String(process.env.FLEETDECK_SPAWN ?? '').toLowerCase() === 'off') {
      return { available: false, reason: 'disabled (FLEETDECK_SPAWN=off)', ...base };
    }
    if (tmuxAdapter.spawnOverrideCmd()) {
      return { available: true, reason: 'test-override', ...base };
    }
    if (!tmuxAdapter.hasTmux()) {
      return { available: false, reason: 'tmux not found on PATH', ...base };
    }
    return { available: true, ...base };
  }

  // CONTRACT flow step 1: pre-create the card so the callsign exists before
  // the pane does. Note is EXACTLY "spawning…"; col queued; source 'spawned'.
  function createSpawnedCard(sid, cwd, prompt) {
    const repo = deriveRepo(cwd);
    // Ticket inheritance is a naming moment: derive the ticket from the SOURCE
    // cwd's branch (fresh — bypass the 20s cache) BEFORE naming, so a spawn off a
    // ticket branch is born ticket-first exactly like a hook/agents birth. The
    // whole chain here runs in spawn()'s synchronous pre-await prefix, preserving
    // the naming-collision invariant.
    const branch = cwd ? branchOf(cwd, { fresh: true }) : null;
    const ticket = ticketFromBranch(branch);
    const callsign = assignCallsign(sid, ticket);
    const now = Date.now();
    q.insertSpawnedSession.run(
      sid, callsign, cwd, repo.repo_id ?? null, repo.repo_name ?? null,
      branch ?? null, repo.worktree ?? null,
      prompt ? String(prompt).slice(0, 80) : null, now, now,
    );
    if (ticket) updateSession(sid, { ticket, ticket_source: 'branch' });
    return q.getSession.get(sid);
  }

  function spawnFailed(sid, callsign, reason) {
    // D8: this tombstone now also wakes watchers (via tombstoneCard's default),
    // consistent with every other terminal transition — a failed spawn is a
    // card that just went offline, and a watch long-poll should learn that now
    // rather than at its timeout. forgetModel drops the transcript memo (M-G2:
    // terminal — a revive re-stamps the floor).
    tombstoneCard(sid, {
      note: `spawn failed: ${reason}`.slice(0, 80),
      tickMsg: `✗ spawn failed for ${callsign}: ${reason.slice(0, 60)}`,
      forgetModel: true,
      mutate: true,
    });
  }

  // Bring-up nudge — one of four sanctioned pane injections (the others are
  // owned-pane mail and verbatim human typing relayed by the live-terminal
  // modal, plus explicit human-requested /rc enablement): if no hook lands
  // within NUDGE_MS and the pane is alive, send ONE Enter for the trust
  // dialog. Never again for that spawn.
  const nudged = new Set();
  function scheduleNudge(spawn_id, window, callsign) {
    const t = setTimeout(async () => {
      try {
        if (nudged.has(spawn_id)) return;
        const row = q.getSpawn.get(spawn_id);
        if (!row || row.status !== 'spawning') return; // hook event already landed, or terminal
        const win = await findScopedWindow(window);
        if (!win || win.pane_dead) return; // pane not alive → no keystroke
        nudged.add(spawn_id);
        await tmuxAdapter.sendBringupEnter(win.window_id);
        logEvent(row.session_id, 'SpawnNudge', null, 'bring-up Enter sent (trust dialog)');
        tick(`⏎ nudged ${callsign} through the trust dialog`);
        onMutate();
      } catch { /* nudge is best-effort; never disturb the daemon */ }
    }, NUDGE_MS);
    t.unref?.();
  }

  const RC_URL_RE = /https:\/\/claude\.ai\/\S+/;
  const registrationRemoteHarvests = new Map();

  // HIGH (revive single-flight): the SYNCHRONOUS claim that makes revive()
  // single-flight per session. revive() crosses async awaits (window
  // inspection, kill) before it inserts its provisional owner row, so two
  // near-simultaneous revives for one session could BOTH pass the DB guard and
  // BOTH launch a pane. A revive checks-and-adds its session id here with NO
  // await in between, so the second revive is refused (409) until the first
  // settles (the try/finally in revive always releases it). Scoped to this
  // core; a restart naturally clears it (a mid-flight revive is a provisioning
  // row that boot reconciliation settles).
  const revivingSessions = new Set();

  // BUG 3 (condemn hysteresis): consecutive dead-read counter per spawn, keyed
  // by spawn_id. The liveness tick requires CONDEMN_DEAD_READS consecutive dead
  // reads before it flips a LIVE spawn 'pane-dead', so a single transient dead
  // read (a momentary tmux glitch / split-pane flap) cannot condemn a card the
  // resurrect loop would only flip back next tick. Cleared on any live read and
  // on every terminal transition (forgetSpawn), so it never carries stale.
  const condemnStreak = new Map();

  // M-G2: per-spawn in-memory ephemera (the bring-up nudge dedupe set and the
  // registration remote-harvest promise) used to be cleared on no terminal
  // path at all, leaking one entry per spawn for the daemon's life. Drop them
  // on EVERY terminal spawn transition (kill / gone / pane-dead / spawn-fail /
  // reconciliation). modelMemo is keyed by session, not spawn, and is cleared
  // alongside session-terminal transitions. condemnStreak (BUG 3) rides along:
  // a spawn that just went terminal has no live streak to remember.
  function forgetSpawn(spawn_id) {
    nudged.delete(spawn_id);
    registrationRemoteHarvests.delete(spawn_id);
    condemnStreak.delete(spawn_id);
  }

  // BUG 3: restore a spawn that tmux just proved is a live claude after it was
  // wrongly condemned to 'pane-dead'/'gone' (a /clear tombstone, a 3h-silence
  // presume-dead, a transient tmux misread). Flip the row back 'live' and lift
  // the card off the offline shelf — clear ended_at/archived_at and re-derive a
  // live lane — so the board shows the terminal again. 'idle' reflects a live
  // claude sitting at its prompt; the next real hook refines col/note. Shared
  // by the liveness tick (automatic, next poll) and revive() (human-driven,
  // immediate). The caller has already confirmed pane_dead=0 and
  // pane_current_command='claude' AND that no newer row owns the window.
  function resurrectSpawn(row) {
    q.setSpawnStatus.run('live', row.spawn_id);
    const c = q.getSession.get(row.session_id);
    updateSession(row.session_id, {
      col: 'idle', ended_at: null, archived_at: null,
      // BUG 4: a card condemned while it was 'spawn_stalled'/'permission_prompt'
      // must not come back wearing a stale needs-you reason chip — clear
      // notification_type. And a resurrected card IS being seen right now, so
      // refresh last_seen: it is 'idle' at its prompt, not silent since death
      // (otherwise the very next retention sweep could presume it dead again on
      // the pre-death last_seen).
      notification_type: null, last_seen: Date.now(),
      note: 'pane is a live claude — restored to the board',
    });
    forgetSpawn(row.spawn_id); // drop any stale nudge/harvest ephemera from the death (BUG 3: also clears condemnStreak)
    tick(`✨ ${c?.callsign ?? row.callsign} restored — its pane was a live claude all along`);
    notifyWatchers(row.session_id);
    onMutate();
  }

  async function harvestRemote(spawn_id) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { url: null };
    let text = null;
    try { text = await tmuxAdapter.capturePane(row.tmux_window); } catch { /* best effort */ }
    const url = typeof text === 'string' ? (text.match(RC_URL_RE)?.[0] ?? null) : null;
    // M-B8: setSpawnRemote/tick/onMutate can throw (SQLITE_BUSY/IOERR, or a bug
    // in a broadcast handler). Harvest runs from an unref timer / detached
    // promise, so an escaping throw becomes an unhandled rejection. fleetd.mjs
    // now installs a global process.on('unhandledRejection') handler that logs
    // rather than crashing, but we do not lean on it: contain the throw HERE so
    // a failed harvest just leaves remote_url unset, which the next /rc attempt
    // or registration hook can retry.
    try {
      q.setSpawnRemote.run(url, spawn_id);
      // Live callsign, not the frozen spawn row: a manual re-ticket renames the
      // session but never the immutable spawn.callsign (like :180/:764).
      const c = q.getSession.get(row.session_id);
      tick(`📱 ${c?.callsign ?? row.callsign} remote control enabled${url ? '' : ' (URL not found)'}`);
      onMutate();
    } catch (err) {
      console.error('fleetd remote harvest persist error:', err);
    }
    return { url };
  }

  // CONTRACT: born-remote sessions expose their URL only in TUI scrollback.
  // The first hook schedules exactly one delayed capture, after the status
  // panel has had time to render. The unref timer never keeps fleetd alive.
  function delayedRemoteHarvest(spawn_id) {
    // A zero test knob means "capture on the next microtask"; production's
    // non-zero delay always uses the required unref timer.
    // M-B8: the timer path already funnels rejections into resolve({url:null});
    // the zero path returned a bare promise whose rejection nothing observed.
    // harvestRemote is now internally guarded, but keep the .catch() as the
    // documented belt-and-suspenders against an unhandled rejection.
    if (RC_HARVEST_MS === 0) {
      return Promise.resolve().then(() => harvestRemote(spawn_id)).catch(() => ({ url: null }));
    }
    let timer;
    const promise = new Promise(resolve => {
      timer = setTimeout(() => harvestRemote(spawn_id).then(resolve, () => resolve({ url: null })), RC_HARVEST_MS);
      timer.unref?.();
    });
    return promise;
  }

  function scheduleRegistrationRemoteHarvest(spawn_id) {
    if (registrationRemoteHarvests.has(spawn_id)) return registrationRemoteHarvests.get(spawn_id);
    const promise = delayedRemoteHarvest(spawn_id);
    registrationRemoteHarvests.set(spawn_id, promise);
    return promise;
  }

  // H-R6 compensation: unwind a spawn's partial EXTERNAL state and settle its
  // durable row+card as failed. A fleet-created worktree is fresh and holds no
  // human work, so force-remove it (git first, then rmSync + prune as the
  // half-created fallback); kill any half-created scoped window by verified
  // name. Then flip the provisional row to a terminal 'gone' (so neither
  // liveness nor boot reconciliation ever adopts it) and tombstone the card.
  // Best-effort throughout: cleanup failure must not mask the launch failure.
  async function spawnCompensate({ spawn_id, session_id, callsign, cwd, worktree_path, tmux_window, reason }) {
    if (worktree_path) {
      try {
        const rm = await execFileP('git', ['-C', cwd, 'worktree', 'remove', '--force', worktree_path], { timeout: 30_000 });
        if (!rm.ok) { try { fs.rmSync(worktree_path, { recursive: true, force: true }); } catch { /* best effort */ } }
        await execFileP('git', ['-C', cwd, 'worktree', 'prune'], { timeout: 30_000 });
      } catch { /* best effort — a stranded fleet worktree is surfaced by cleanup() */ }
    }
    if (tmux_window) { try { await tmuxAdapter.killWindowVerified(tmux_window); } catch { /* best effort */ } }
    q.setSpawnStatus.run('gone', spawn_id);
    forgetSpawn(spawn_id);
    spawnFailed(session_id, callsign, reason);
  }

  // POST /api/spawn — control API, fail-loud (CONTRACT: 4xx {ok:false,
  // reason} for no-tmux / bad-cwd / cap / bad-worktree; 409 when worktree is
  // requested outside git). Returns {status, body} for the HTTP layer.
  async function spawn(body) {
    const cap = spawnCapability();
    if (!cap.available) {
      return { status: 400, body: { ok: false, reason: `spawning unavailable: ${cap.reason}` } };
    }
    for (const k of ['cwd', 'prompt', 'model', 'permission_mode']) {
      if (body?.[k] != null && typeof body[k] !== 'string') {
        return { status: 400, body: { ok: false, reason: `${k} must be a string` } };
      }
    }
    if (body?.worktree != null && typeof body.worktree !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'worktree must be a boolean' } };
    }
    if (body?.dangerously_skip_permissions != null && typeof body.dangerously_skip_permissions !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'dangerously_skip_permissions must be a boolean' } };
    }
    if (body?.remote_control != null && typeof body.remote_control !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'remote_control must be a boolean' } };
    }
    // v1.3 unsupervised spawns (CONTRACT "A"): either form of bypass —
    // dangerously_skip_permissions:true (its own CLI flag) or
    // permission_mode:"bypassPermissions" (plain passthrough) — marks the row
    // and the /state spawn object. Both together are allowed (CLI semantics
    // decide what wins in the session).
    const skipPermissions = body?.dangerously_skip_permissions === true
      || body?.permission_mode === 'bypassPermissions';
    const cwd = body?.cwd || '';
    let st = null;
    try { st = fs.statSync(cwd); } catch { /* missing */ }
    if (!cwd || !st?.isDirectory()) {
      return { status: 400, body: { ok: false, reason: 'cwd missing or not a directory' } };
    }
    if (body?.worktree === true && !deriveRepo(cwd).is_git) {
      return { status: 409, body: { ok: false, reason: 'cwd is not a git repository — cannot spawn into a worktree' } };
    }

    // Step 1: pre-issue the session UUID and create the card (callsign exists
    // from here on; the first real hook event flips source to 'hooks').
    const session_id = randomUUID();
    const spawn_id = randomUUID();
    const c = createSpawnedCard(session_id, cwd, body?.prompt);
    const callsign = c.callsign;

    // Step 2 (H-R6): insert the DURABLE spawn row BEFORE any external op. The
    // tmux names are deterministic from the callsign, so the provisional row
    // already knows the window it is about to create — which lets compensation
    // clean up by verified name and lets boot reconciliation finish/clean a
    // row whose external ops never completed. It is born 'provisioning'
    // (excluded from activeSpawns), and only flips to 'spawning' once the pane
    // exists. worktree_path is filled in once the worktree is actually created.
    const tmux_session = tmuxAdapter.sessionName(port);
    const tmux_window = tmuxAdapter.windowName(port, callsign);
    q.insertProvisionalSpawn.run(spawn_id, session_id, callsign, tmux_session, tmux_window, cwd,
      null, Date.now(), skipPermissions ? 1 : 0, body?.remote_control === true ? 1 : 0);

    // Step 3: optional fresh worktree — the session cwd becomes the new path.
    let worktree_path = null;
    if (body?.worktree === true) {
      // Ticket-first artifact names so sibling worktree dirs and branch lists
      // group by ticket: <repo>--fd-PROJ-123-otter / fd/PROJ-123-otter.
      // Gated on the callsign actually BEING ticket-named: on a saturated
      // ticket assignCallsign falls back to hex while c.ticket is still
      // recorded, and composing from c.ticket there would mint PROJ-123-raven —
      // the canonical dir of the OTHER live raven that caused the saturation —
      // colliding every time and naming a dir after a foreign session. Hex or
      // ticketless callsigns keep today's <repo>--fd-<callsign> / fd/<callsign>,
      // byte-for-byte. The tmux window + --remote-control keep the plain
      // callsign (set above / below).
      const ticketNamed = c.ticket && String(callsign).endsWith(`-${c.ticket}`);
      const baseName = ticketNamed ? `${c.ticket}-${animalOf(callsign)}` : callsign;
      const pathFor = name => path.join(path.dirname(cwd), `${path.basename(cwd)}--fd-${name}`);
      // Leftover-artifact collision: a long-archived same-ticket session can free
      // the animal (taken-scope is archived_at) while its worktree dir / fd/
      // branch still sit on disk. If the target dir already exists, go straight
      // to a sid-disambiguated name; otherwise try the clean name and retry ONCE
      // on failure (also covers a stale branch of the same name). Hex-suffixed
      // ticketless names made this astronomically unlikely; ticket-first names
      // make it merely rare — hence the single deterministic retry, no loop.
      const dedup = `${baseName}-${session_id.slice(0, 4)}`;
      const names = fs.existsSync(pathFor(baseName)) ? [dedup] : [baseName, dedup];
      let candidate;
      let res;
      for (const workname of names) {
        candidate = pathFor(workname);
        res = await execFileP('git', ['-C', cwd, 'worktree', 'add', '-b', `fd/${workname}`, candidate]);
        if (res.ok) break;
      }
      worktree_path = candidate;
      if (!res.ok) {
        // No window was created yet; compensate cleans the (possibly partial)
        // worktree and settles the provisional row.
        await spawnCompensate({ spawn_id, session_id, callsign, cwd, worktree_path, tmux_window: null,
          reason: `git worktree add: ${res.err}` });
        return { status: 409, body: { ok: false, reason: `git worktree add failed: ${res.err}`.slice(0, 300) } };
      }
      q.setSpawnWorktree.run(worktree_path, spawn_id);
      const repo = deriveRepo(worktree_path);
      updateSession(session_id, {
        cwd: worktree_path, repo_id: repo.repo_id, repo_name: repo.repo_name,
        worktree: repo.worktree, branch: branchOf(worktree_path),
      });
    }
    const runCwd = worktree_path ?? cwd;

    // Step 4: deterministic interactive `claude`: scrub inherited agent and
    // fleet variables, then pin this daemon's port/home. The claude flag order
    // remains contract-pinned: --session-id, --model, --permission-mode,
    // permission bypass, remote-control name, then the prompt as ONE
    // positional argv element. permission_mode "bypassPermissions" needs no
    // extra argv change — it rides --permission-mode as plain passthrough.
    const argv = [
      ...claudeEnvArgvPrefix(port, home),
      'claude', '--session-id', session_id,
    ];
    if (body?.model) argv.push('--model', body.model);
    if (body?.permission_mode) argv.push('--permission-mode', body.permission_mode);
    if (body?.dangerously_skip_permissions === true) argv.push('--dangerously-skip-permissions');
    // Name the Remote Control session after the CALLSIGN, not the tmux window:
    // this is the string the human reads in the claude.ai session list on their
    // phone, and the board speaks callsigns everywhere. fd<port>- is internal
    // plumbing that means nothing to them.
    if (body?.remote_control === true) argv.push('--remote-control', callsign);
    if (body?.prompt) argv.push(body.prompt);

    const override = tmuxAdapter.spawnOverrideCmd();
    if (override) {
      // Test override: argv [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)].
      const spec = {
        spawn_id, session_id, callsign, port,
        cwd: runCwd, requested_cwd: cwd,
        prompt: body?.prompt ?? null, model: body?.model ?? null,
        permission_mode: body?.permission_mode ?? null, worktree_path,
        dangerously_skip_permissions: body?.dangerously_skip_permissions === true,
        skip_permissions: skipPermissions,
        remote_control: body?.remote_control === true,
        tmux: { session: tmux_session, window: tmux_window },
        argv, // the full env-wrapped argv tmux would have run
      };
      // launchOverride is fire-and-forget; onError fires only if the child
      // process cannot start at all (no child ⇒ no SessionStart to race), so
      // compensating there is safe.
      tmuxAdapter.launchOverride(override, spec, err =>
        spawnCompensate({ spawn_id, session_id, callsign, cwd, worktree_path, tmux_window,
          reason: `spawn override: ${err.message || err}` }).catch(() => {}));
    } else {
      try {
        await tmuxAdapter.ensureSession(port);
        await tmuxAdapter.newWindow({ port, callsign, cwd: runCwd, argv });
      } catch (err) {
        // The window failed to launch; compensate removes the worktree we may
        // have just created and settles the provisional row (H-R6).
        await spawnCompensate({ spawn_id, session_id, callsign, cwd, worktree_path, tmux_window,
          reason: String(err.message || err) });
        return { status: 500, body: { ok: false, reason: `tmux spawn failed: ${err.message || err}` } };
      }
    }

    // Step 5: the pane now exists (or the override was handed off) — flip the
    // durable row live-eligible ('provisioning' → 'spawning') and arm the
    // bring-up nudge timer.
    q.setSpawnStatus.run('spawning', spawn_id);
    tick(`🚀 spawned ${callsign} — tmux window ${tmux_window}${skipPermissions ? ' (unsupervised)' : ''}`);
    scheduleNudge(spawn_id, tmux_window, callsign);
    onMutate();
    return {
      status: 200,
      body: { ok: true, spawn_id, session_id, callsign, tmux: { session: tmux_session, window: tmux_window } },
    };
  }

  // POST /api/spawn/:id/revive — resume a terminal board-owned Claude
  // conversation into a NEW durable spawn row. Historical rows are immutable
  // evidence; the same session id/callsign/window identity is reused.
  async function revive(spawn_id, body = {}) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    if (!['pane-dead', 'killed', 'gone'].includes(row.status)) {
      return { status: 409, body: { ok: false, reason: `spawn is ${row.status}, not revivable` } };
    }
    if (body?.remote_control != null && typeof body.remote_control !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'remote_control must be a boolean' } };
    }
    // Remote control survives death: inherit the dead row's wish unless the
    // human overrides it on this revive.
    const remoteWanted = body?.remote_control ?? !!row.remote_control;
    // HIGH (revive single-flight), part 1 — the DB guard. A live-eligible OR a
    // PROVISIONING spawn for this session means someone is already bringing a
    // pane up; refuse. Including 'provisioning' matters because a revive's own
    // durable row is 'provisioning' from the instant it is inserted until its
    // pane is up, so a later revive request cannot slip past a still-in-flight
    // one (activeSpawnBySession alone would not see it).
    const active = q.activeSpawnBySession.get(row.session_id)
      || q.provisioningSpawnBySession.get(row.session_id);
    if (active) {
      return { status: 409, body: { ok: false, reason: `session already has active spawn ${active.spawn_id}` } };
    }
    // HIGH (revive single-flight), part 2 — the SYNCHRONOUS claim. The DB guard
    // above cannot stop TWO near-simultaneous revives: both read the same
    // pre-insert state, both cross the async window inspection below, and both
    // insert a provisional row + launch a pane → two live panes for one
    // session. Claim the session id here with no await between the check and
    // the add, so the second concurrent revive is refused until the first
    // settles. The try/finally guarantees the claim is released on EVERY exit
    // (success, refusal, or throw). R2-5 stale-kill protection is untouched —
    // the provisional owner row is still inserted below before newWindow.
    if (revivingSessions.has(row.session_id)) {
      return { status: 409, body: { ok: false, reason: `session ${row.session_id.slice(0, 8)} is already being revived` } };
    }
    revivingSessions.add(row.session_id);
    try {
      // H-R7: validate ALL resume eligibility BEFORE touching tmux. Reviving
      // reuses the deterministic window name, and the old code killed whatever
      // occupied it and THEN checked cwd/transcript — so a missing transcript
      // would 410 only AFTER an unrelated pane had already been destroyed.
      // Prove the resume can actually happen first; only then reconcile the
      // window.
      const runCwd = row.worktree_path ?? row.cwd;
      let st = null;
      try { st = fs.statSync(runCwd); } catch { /* missing */ }
      if (!runCwd || !st?.isDirectory()) {
        return { status: 410, body: { ok: false, reason: 'revive cwd no longer exists' } };
      }
      if (!fs.existsSync(claudeTranscriptPath(runCwd, row.session_id))) {
        return { status: 410, body: { ok: false, reason: 'resume transcript no longer exists' } };
      }

      // Exact scoped-name collision defense, now that eligibility is proven. A
      // live Claude pane is ownership proof and must never be duplicated. Only a
      // pane PROVEN dead, or an expected bare shell (claude exited, leaving the
      // login shell in a remain-on-exit window), is a safe remnant to remove by
      // verified name before reusing the window. A live pane running ANYTHING
      // ELSE (the human repurposed the window) is never destroyed — refuse.
      const existing = await findScopedWindow(row.tmux_window);
      if (existing && !existing.pane_dead && existing.pane_cmd === 'claude') {
        // BUG 3: the deterministic window ALREADY hosts a live claude pane for
        // this session — it was wrongly condemned (BUG 1 /clear, BUG 2 silence)
        // while the agent kept running, and the board hid the terminal. A human
        // clicking Revive here used to hit a dead-end 409 ("already has a live
        // claude pane") and stay stuck until the next liveness poll. There is
        // nothing to resume: the pane IS the live session. ADOPT it — resurrect
        // the row to 'live' and lift the card back onto the board — and return
        // success so the terminal shows NOW. No tmux launch, no kill: we never
        // duplicate a live billed session (the same safety the 409 protected).
        //
        // MED (adoption must mirror the liveness-tick's resurrection guards):
        // the tick only resurrects a 'pane-dead'/'gone' row (never 'killed')
        // AND only when currentWindowOwner still names THAT row. Adoption did
        // NEITHER, which let two things go wrong:
        //   (a) a 'killed' row (a human decision) could be flipped back to
        //       'live' by adoption — breaking "a human kill never resurrects".
        //   (b) reviving a NON-newest 'pane-dead'/'gone' row whose reused window
        //       is a live claude could resurrect the OLDER row while a newer
        //       pane-dead row still outranks it in currentWindowOwner: BOTH end
        //       up 'live' (countActiveSpawns double-counts) and the adopted row
        //       becomes un-killable (spawnKill's owner check points at the other
        //       one). Guard both here before resurrecting.
        if (row.status !== 'pane-dead' && row.status !== 'gone') {
          // 'killed': the window hosts a live claude, but a human kill is never
          // undone by adoption. Refuse rather than resurrect or duplicate.
          return { status: 409, body: { ok: false, reason: `spawn ${spawn_id} was killed — its window hosts a live claude, but a killed spawn is never resurrected by adoption` } };
        }
        const owner = q.currentWindowOwner.get(row.tmux_window);
        if (owner && owner.spawn_id !== row.spawn_id) {
          return {
            status: 409,
            body: { ok: false, reason: `window ${row.tmux_window} is owned by spawn ${owner.spawn_id} — revive that one`, current_spawn_id: owner.spawn_id },
          };
        }
        resurrectSpawn(row);
        return {
          status: 200,
          body: {
            ok: true, adopted: true, spawn_id: row.spawn_id, session_id: row.session_id,
            callsign: row.callsign, tmux: { session: tmuxAdapter.sessionName(port), window: row.tmux_window },
          },
        };
      }
      if (existing && !existing.pane_dead && !SHELL_RE.test(existing.pane_cmd)) {
        return { status: 409, body: { ok: false, reason: `window ${row.tmux_window} hosts a live '${existing.pane_cmd}' pane — not a dead remnant; refusing to kill it` } };
      }
      if (existing) {
        const killed = await tmuxAdapter.killWindowVerified(row.tmux_window);
        if (!killed.ok && !killed.gone) {
          return { status: 500, body: { ok: false, reason: killed.error || 'tmux kill-window failed' } };
        }
      }

      // The launch discipline (R2-5 pre-launch owner re-check, provisional owner
      // row, override/newWindow, live-flip, card update, nudge) is shared with
      // adopt now — launchResume() below is the single source of truth. Reviving
      // reuses the dead row's window name and callsign, carries the dead row's
      // requested cwd + worktree_path + skip-permissions, inherits the remote
      // wish, and excludes ITS OWN terminal row (spawn_id) from the owner
      // re-check (a 'pane-dead' row still naming its window is not a rival).
      return await launchResume({
        session_id: row.session_id,
        callsign: row.callsign,
        tmux_window: row.tmux_window,
        runCwd,
        requested_cwd: row.cwd,
        worktree_path: row.worktree_path,
        skip_permissions: !!row.skip_permissions,
        remoteWanted,
        excludeSpawnId: spawn_id,
        overrideExtra: { revive_of: spawn_id },
        note: 'reviving…',
        tickMsg: `⟲ reviving ${row.callsign} (resume ${row.session_id.slice(0, 8)})`,
        failReason: 'tmux revive failed',
      });
    } finally {
      // Release the single-flight claim on EVERY exit path.
      revivingSessions.delete(row.session_id);
    }
  }

  // Shared resume-launch discipline — the tail extracted VERBATIM from revive()
  // so revive and adopt build the pane exactly one way. Given a session id +
  // callsign + a scoped window name it: builds the env-wrapped
  // `claude --resume <sid>` argv (+ optional --dangerously-skip-permissions /
  // --remote-control), does the R2-5 pre-launch owner re-check, inserts the
  // PROVISIONAL owner row BEFORE any pane exists (H-R6), launches (test override
  // or tmux), flips the row live-eligible, updates the card (archived_at:null,
  // col:queued, note — ended_at left for the first hook), nudges, and returns
  // the control-API {status, body}. Callers own everything BEFORE this: state
  // gate, single-flight claim, H-R7 cwd/transcript validation, and window
  // collision defense. The caller's try/finally still owns the revivingSessions
  // release — launchResume never touches that Set.
  async function launchResume({
    session_id, callsign, tmux_window, runCwd, requested_cwd, worktree_path,
    skip_permissions, remoteWanted, excludeSpawnId, overrideExtra = {},
    note, tickMsg, failReason, bodyExtra = {},
  }) {
    const new_spawn_id = randomUUID();
    const argv = [...claudeEnvArgvPrefix(port, home), 'claude', '--resume', session_id];
    if (skip_permissions) argv.push('--dangerously-skip-permissions');
    if (remoteWanted) argv.push('--remote-control', callsign);
    const tmux_session = tmuxAdapter.sessionName(port);

    // HIGH (revive single-flight), part 3 / R2-5 — re-check ownership right
    // before we claim the window. The caller's guards ran BEFORE its awaits
    // (listScopedWindows / killWindowVerified). Re-confirm no live-eligible or
    // provisioning row has taken this window in the meantime (belt-and-suspenders
    // behind the single-flight claim; also catches a concurrent spawn that
    // reused this exact scoped name). currentWindowOwner ranks the window's rows
    // newest-first over provisioning|spawning|stalled|live|pane-dead: the
    // rightful owner right now must be either the caller's own excluded row (a
    // revive's 'pane-dead' row still naming its window — excludeSpawnId) or
    // nobody (an adopt onto a fresh callsign-derived name, or a 'gone'/'killed'
    // row that released it). A DIFFERENT row in a live-eligible state means a
    // second pane is already coming up — refuse rather than launch a duplicate.
    // A stale 'pane-dead' SIBLING is not live-eligible and never blocks.
    const preLaunchOwner = q.currentWindowOwner.get(tmux_window);
    if (preLaunchOwner && preLaunchOwner.spawn_id !== excludeSpawnId
      && ['provisioning', 'spawning', 'stalled', 'live'].includes(preLaunchOwner.status)) {
      return {
        status: 409,
        body: { ok: false, reason: `window ${tmux_window} is now owned by active spawn ${preLaunchOwner.spawn_id}`, current_spawn_id: preLaunchOwner.spawn_id },
      };
    }

    // R2-5 / H-R6: insert the new spawn's durable row as a PROVISIONAL owner of
    // the window name BEFORE the pane is created. Without a live owning row, a
    // forced stale-id spawnKill arriving during the newWindow await would see no
    // current owner and kill the just-launched pane, leaving a 'spawning' row
    // for a dead pane. currentWindowOwner counts 'provisioning', so the new row
    // owns the window the instant it exists and any OTHER-id kill is refused.
    // remote_url is deliberately NOT carried over: a fresh one is harvested from
    // the resumed pane. Flip to 'spawning' (live-eligible) only once the pane is
    // up. (adopt passes worktree_path:null — it never creates a worktree.)
    q.insertProvisionalSpawn.run(new_spawn_id, session_id, callsign, tmux_session,
      tmux_window, requested_cwd, worktree_path, Date.now(), skip_permissions ? 1 : 0,
      remoteWanted ? 1 : 0);

    const override = tmuxAdapter.spawnOverrideCmd();
    if (override) {
      tmuxAdapter.launchOverride(override, {
        spawn_id: new_spawn_id, ...overrideExtra,
        session_id, callsign, port,
        cwd: runCwd, requested_cwd, prompt: null, model: null,
        permission_mode: null, worktree_path,
        dangerously_skip_permissions: !!skip_permissions,
        skip_permissions: !!skip_permissions,
        remote_control: remoteWanted,
        tmux: { session: tmux_session, window: tmux_window },
        argv,
      }, err => spawnFailed(session_id, callsign, `spawn override: ${err.message || err}`));
    } else {
      try {
        await tmuxAdapter.ensureSession(port);
        await tmuxAdapter.newWindow({ port, callsign, cwd: runCwd, argv });
      } catch (err) {
        // The pane never launched — settle the provisional row terminal so it
        // stops owning the window (and is never liveness-checked), then fail.
        q.setSpawnStatus.run('gone', new_spawn_id);
        forgetSpawn(new_spawn_id);
        return { status: 500, body: { ok: false, reason: `${failReason}: ${err.message || err}` } };
      }
    }

    // Pane exists (or the override was handed off): flip the durable row
    // live-eligible ('provisioning' → 'spawning').
    q.setSpawnStatus.run('spawning', new_spawn_id);
    updateSession(session_id, { archived_at: null, col: 'queued', note });
    tick(tickMsg);
    scheduleNudge(new_spawn_id, tmux_window, callsign);
    onMutate();
    return {
      status: 200,
      body: {
        ok: true, ...bodyExtra, spawn_id: new_spawn_id, session_id, callsign,
        tmux: { session: tmux_session, window: tmux_window },
      },
    };
  }

  // POST /api/sessions/:session_id/adopt — "Move to tmux". Adopt a session the
  // board did NOT spawn (source 'hooks' / 'agents-cli' / a remote ccd-cli run)
  // into a board-owned tmux pane via `claude --resume <sid>`. Context-sensitive:
  //   • live session   → ARM (200 {armed, expires_at}); re-arm refreshes; the
  //                      pane is launched by hookSessionEnd when the CLI exits
  //                      (two processes can't drive one conversation).
  //   • ended session  → ADOPT NOW (200 {adopted, spawn_id, …}).
  //   • body {disarm}  → clear a pending arm (200 {armed:false}).
  // Stale-snapshot races degrade correctly in BOTH directions (a card that
  // ended between snapshot and this POST adopts now instead of arming, and vice
  // versa) and the response says which happened. Never sets --remote-control in
  // v1 (/rc is available from the live card once the pane is up).
  async function adoptSession(session_id, body = {}, { deferred = false } = {}) {
    const c = q.getSession.get(session_id);
    if (!c) return { status: 404, body: { ok: false, reason: 'no such session' } };

    // Body validation. dangerously_skip_permissions is the two-step unsupervised
    // gate; disarm cancels a pending arm.
    if (body?.dangerously_skip_permissions != null && typeof body.dangerously_skip_permissions !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'dangerously_skip_permissions must be a boolean' } };
    }
    if (body?.disarm != null && typeof body.disarm !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'disarm must be a boolean' } };
    }
    const skip = body?.dangerously_skip_permissions === true;

    // Disarm FIRST, in any state: the click that disarms is the human revoking
    // the earlier arm click, and it must NEVER fall through into an adopt (a
    // card that ended between the armed snapshot and this disarm POST would
    // otherwise be adopted by a cancel click). Because the arm now survives
    // until CONSUMED (see the ended fork), a disarm landing inside the deferred
    // grace window genuinely cancels the scheduled move — the deferred call
    // finds the arm gone and stands down. Idempotent.
    if (body?.disarm === true) {
      updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
      tick(`⇥ ${c.callsign} move-to-tmux disarmed`);
      onMutate();
      return { status: 200, body: { ok: true, armed: false, disarmed: true } };
    }

    // Board-owned never adopts — and never ARMS. ANY spawn lineage (dead or
    // alive) means the board already owns this session's pane story: revive
    // owns dead lineages, and arming a board-owned session would fire a second
    // `claude --resume` lineage at its next SessionEnd, fighting the first over
    // the window name and worktree bookkeeping. This sits BEFORE the live/ended
    // fork so neither path can slip past it. A deferred call landing here means
    // a manual adopt/revive won the race and created the row — benign (the
    // caller treats 409 as "someone else already did it").
    const lineage = q.spawnBySession.get(session_id);
    if (lineage) {
      return { status: 409, body: { ok: false, reason: `session is board-owned (spawn ${lineage.spawn_id}, ${lineage.status}) — revive owns its pane story` } };
    }

    // LIVE fork: the CLI is still running (ended_at null). Two processes can't
    // drive one conversation, so we can't grab it now.
    if (c.ended_at == null) {
      // A DEFERRED call reaching a live session is the resurrection race: the
      // CLI exited (arming the move) and something resumed it inside the grace
      // window. The human's click was one-shot — re-arming here would plant a
      // standing 30-minute arm they never asked for, firing a surprise move at
      // the NEXT exit. Cancel instead: consume the arm, say so once.
      if (deferred) {
        updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
        tick(`↷ move-to-tmux canceled for ${c.callsign} — session came back live before the move`);
        onMutate();
        return { status: 200, body: { ok: true, canceled: true } };
      }
      // Manual click → ARM: remember it as a durable deadline; hookSessionEnd
      // fires the adopt the instant the CLI exits. Re-arm refreshes the
      // deadline + bypass choice.
      const expires_at = Date.now() + ADOPT_ARM_MS;
      updateSession(session_id, { adopt_armed_until: expires_at, adopt_armed_skip: skip ? 1 : 0 });
      tick(`⧗ ${c.callsign} armed for move-to-tmux — exit the CLI to move it${skip ? ' (unsupervised)' : ''}`);
      onMutate();
      return { status: 200, body: { ok: true, armed: true, expires_at } };
    }

    // ENDED fork: the card is offline — adopt NOW.
    // A deferred call whose arm is already gone stands down silently: the human
    // disarmed inside the grace window (their cancel must win), or another
    // actor already consumed the arm. Not a failure — no ticker line.
    if (deferred && c.adopt_armed_until == null) {
      return { status: 200, body: { ok: true, canceled: true } };
    }
    // Immediate adopt requires a hook-PROVEN end, as an ALLOWLIST: end_reason
    // NULL means no provenance was ever stamped (pre-0.7.0 rows, the agents-cli
    // absence guess, tombstone writers that condemn without proof), and
    // retention's silence guess stamps 'presumed'. Either way there is no proof
    // the CLI actually exited, and `claude --resume` against a still-live CLI
    // is a duplicate billed session (the exact doctrine violation) → refuse;
    // arm it instead (truly dead cards just let the arm expire; silently-alive
    // ones complete the move at their real exit).
    if (c.end_reason == null || c.end_reason === 'presumed') {
      return { status: 409, body: { ok: false, reason: 'session has no hook-proven end (presumed/unstamped) — arm it instead' } };
    }
    // Single-flight: reuse the revive Set so a manual adopt, an armed auto-adopt,
    // and a revive can never race two panes onto one session. The try/finally
    // releases it on EVERY exit path.
    if (revivingSessions.has(session_id)) {
      return { status: 409, body: { ok: false, reason: `session ${session_id.slice(0, 8)} is already being moved/revived` } };
    }
    revivingSessions.add(session_id);
    try {
      // Consume the arm INSIDE the single-flight claim, before any external op:
      // one-shot by construction — whoever acts first (deferred timer, manual
      // click, sweep) burns it, and a failed launch never retries. Skipped when
      // the columns are already clear (the common manual adopt-now).
      if (c.adopt_armed_until != null || c.adopt_armed_skip != null) {
        updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
      }
      // H-R7: validate ALL resume eligibility BEFORE touching tmux. runCwd is
      // the hook-reported sessions.cwd (the dir claudeTranscriptPath munges) —
      // NEVER sessions.worktree (the git worktree root); adopt creates no
      // worktree, so the spawn row's worktree_path stays NULL.
      const runCwd = c.cwd;
      let st = null;
      try { st = fs.statSync(runCwd); } catch { /* missing */ }
      if (!runCwd || !st?.isDirectory()) {
        return { status: 410, body: { ok: false, reason: 'session cwd no longer exists' } };
      }
      if (!fs.existsSync(claudeTranscriptPath(runCwd, session_id))) {
        return { status: 410, body: { ok: false, reason: 'resume transcript no longer exists' } };
      }

      // Window collision defense — mirrors revive minus resurrection. The window
      // name comes from the LIVE callsign (windowName(port, callsign), the
      // enableRemote precedent). Adopt has NO prior ownership claim, so unlike
      // revive there is no self-row to ADOPT: a live claude pane on this name is
      // some other live session → 409; a live pane running anything else (the
      // human repurposed the window) → 409; only a pane PROVEN dead or an
      // expected bare shell is a safe remnant to kill by verified name and reuse.
      const tmux_window = tmuxAdapter.windowName(port, c.callsign);
      const existing = await findScopedWindow(tmux_window);
      if (existing && !existing.pane_dead && existing.pane_cmd === 'claude') {
        return { status: 409, body: { ok: false, reason: `window ${tmux_window} already hosts a live claude pane` } };
      }
      if (existing && !existing.pane_dead && !SHELL_RE.test(existing.pane_cmd)) {
        return { status: 409, body: { ok: false, reason: `window ${tmux_window} hosts a live '${existing.pane_cmd}' pane — not a dead remnant; refusing to kill it` } };
      }
      if (existing) {
        const killed = await tmuxAdapter.killWindowVerified(tmux_window);
        if (!killed.ok && !killed.gone) {
          return { status: 500, body: { ok: false, reason: killed.error || 'tmux kill-window failed' } };
        }
      }

      // Launch. adopt carries the LIVE callsign, the hook cwd as both effective
      // and requested cwd, NULL worktree_path, the human's bypass choice, and
      // NEVER remote_control (v1). excludeSpawnId is null — adopt has no prior
      // row, so ANY active owner on the window is a rival to refuse.
      return await launchResume({
        session_id,
        callsign: c.callsign,
        tmux_window,
        runCwd,
        requested_cwd: runCwd,
        worktree_path: null,
        skip_permissions: skip,
        remoteWanted: false,
        excludeSpawnId: null,
        overrideExtra: { adopt_of: session_id },
        note: 'moving to tmux…',
        tickMsg: `⇥ moving ${c.callsign} to tmux (resume ${session_id.slice(0, 8)})${skip ? ' (unsupervised)' : ''}`,
        failReason: 'tmux adopt failed',
        bodyExtra: { adopted: true },
      });
    } finally {
      // Release the single-flight claim on EVERY exit path.
      revivingSessions.delete(session_id);
    }
  }

  // POST /api/spawn/:id/rc — an explicit human board action, relayed as
  // literal TUI input. Never inject into a working/needsyou turn boundary.
  async function enableRemote(spawn_id) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    if (row.status !== 'live') {
      return { status: 409, body: { ok: false, reason: `spawn is ${row.status}, not live` } };
    }
    if (row.remote_control && row.remote_url) {
      return { status: 200, body: { ok: true, enabled: true, url: row.remote_url, pending: false } };
    }
    const session = q.getSession.get(row.session_id);
    if (!session || !['queued', 'idle'].includes(session.col)) {
      return { status: 409, body: { ok: false, reason: `session is ${session?.col ?? 'missing'}, not queued or idle` } };
    }
    const win = await findScopedWindow(row.tmux_window);
    if (!win || win.pane_dead || win.pane_cmd !== 'claude') {
      const observed = !win ? 'missing' : win.pane_dead ? 'dead' : `running ${win.pane_cmd || 'unknown'}`;
      return { status: 409, body: { ok: false, reason: `claude pane is not alive (${observed})` } };
    }
    // `/rc <name>` — named by callsign so the session the human finds on
    // claude.ai carries the same name as the card on the board. Verified live
    // on CLI 2.1.207: no confirmation dialog, and the
    // https://claude.ai/code/session_… URL lands in the pane's scrollback.
    // Use the LIVE session callsign (a manual re-ticket renames the card but not
    // the frozen spawn.callsign) so claude.ai shows today's name.
    const typed = await tmuxAdapter.typeKeys(win.window_id, `/rc ${session?.callsign ?? row.callsign}`);
    const entered = typed ? await tmuxAdapter.sendEnter(win.window_id) : false;
    if (!typed || !entered) {
      return { status: 500, body: { ok: false, reason: 'failed to type remote-control command into pane' } };
    }
    const harvest = delayedRemoteHarvest(spawn_id);
    let timeout;
    const timed = new Promise(resolve => {
      timeout = setTimeout(() => resolve({ pending: true, url: null }), 6_000);
      timeout.unref?.();
    });
    const result = await Promise.race([
      harvest.then(({ url }) => ({ pending: false, url })),
      timed,
    ]);
    clearTimeout(timeout);
    return { status: 200, body: { ok: true, enabled: true, url: result.url, pending: result.pending } };
  }

  // POST /api/spawn/:id/kill — name-verified tmux kill-window (404 unknown
  // id, 409 card not offline without force, 410 window already gone).
  // "Stop" needs no endpoint: the board mails the session instead.
  async function spawnKill(spawn_id, force) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    // H-R5: a revive reuses the dead row's tmux_window, so one physical window
    // can be named by several rows across a session's lifetime. Killing by a
    // STALE id would kill the window the NEWEST (revived) row now owns while
    // marking only the old row 'killed' — liveness then disagrees with reality
    // (the new row still says 'live', its pane is dead). Refuse a historical
    // id even under force: the window belongs to whichever non-terminal row
    // most recently claimed it, and that is the only id allowed to kill it.
    const owner = q.currentWindowOwner.get(row.tmux_window);
    if (owner && owner.spawn_id !== spawn_id) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `spawn ${spawn_id} is a historical row; tmux window ${row.tmux_window} is now owned by spawn ${owner.spawn_id} — kill that one`,
          current_spawn_id: owner.spawn_id,
        },
      };
    }
    const c = q.getSession.get(row.session_id);
    if (c && c.col !== 'offline' && force !== true) {
      return {
        status: 409,
        body: { ok: false, reason: `session ${c.callsign} is ${c.col}, not offline — pass force:true to kill anyway` },
      };
    }
    const res = await tmuxAdapter.killWindowVerified(row.tmux_window);
    if (res.gone) {
      // Discovery: the pane is already gone — settle the row AND the card
      // (same tombstone every terminal row state applies; only reachable for
      // a non-offline card via an explicit force:true).
      if (['spawning', 'stalled', 'live', 'pane-dead'].includes(row.status)) {
        q.setSpawnStatus.run('gone', spawn_id);
        forgetSpawn(spawn_id); // M-G2
        if (c && c.ended_at == null) {
          tombstoneCard(row.session_id, { note: 'spawned pane window gone' }); // D8
        }
        onMutate();
      }
      return { status: 410, body: { ok: false, reason: 'window already gone' } };
    }
    if (!res.ok) return { status: 500, body: { ok: false, reason: res.error || 'tmux kill-window failed' } };
    q.setSpawnStatus.run('killed', spawn_id);
    forgetSpawn(spawn_id); // M-G2
    if (c && c.ended_at == null) {
      tombstoneCard(row.session_id, { note: 'pane killed from the board' }); // D8
    }
    tick(`🗡 killed pane ${row.tmux_window}${force === true ? ' (forced)' : ''}`);
    onMutate();
    return { status: 200, body: { ok: true, spawn_id, status: 'killed' } };
  }

  // Owned-pane liveness (CONTRACT) — rides the agents-poll tick (~10 s), for
  // spawn rows in spawning|stalled|live:
  //   pane_dead or a bare shell (sh|bash|zsh|zsh-*)  → confidently dead:
  //     row 'pane-dead', card offline with the --resume note
  //   'claude'                                        → alive
  //   anything else (incl. window/tmux unreachable)   → UNKNOWN, NO action —
  //     never confidently dead (firstmate's hard-won rule; a wrong "dead"
  //     costs a duplicate billed session, a wrong "alive" costs nothing).
  // The pane_dead flag matters because remain-on-exit panes keep reporting
  // the ORIGINAL command name after death (verified on tmux 3.7b) — the
  // command string alone would read "claude" forever. (SHELL_RE — the
  // bare-shell command matcher — is a shared helper now; see helpers.mjs.)
  // BUG 3 (hysteresis): consecutive dead reads a LIVE spawn must accumulate
  // before the condemn loop flips it 'pane-dead'. Two is enough to swallow a
  // single transient dead read (~one tick) while still condemning a genuinely
  // dead pane on the very next tick.
  const CONDEMN_DEAD_READS = 2;
  async function spawnLivenessTick() {
    const rows = q.activeSpawns.all();
    // BUG 3: resurrection candidates count too — if EVERY spawn is already
    // 'pane-dead'/'gone' (rows empty) we must still probe tmux, or a fleet
    // that was wholly (and wrongly) condemned could never come back.
    const resurrectable = q.resurrectableSpawns.all();
    if (!rows.length && !resurrectable.length && !spawnState.orphans.length) return;
    const wins = await tmuxAdapter.listScopedWindows(port);
    for (const row of rows) {
      const win = wins.find(w => w.window === row.tmux_window);
      if (!win) continue; // gone/unreachable at runtime = unknown; boot reconciliation owns 'gone'
      // BUG 3 (consistent probe): decide liveness with the SAME signal the
      // resurrect loop below trusts. pane_dead is authoritative; otherwise the
      // ACTIVE-pane command (paneCurrentCommand) is the truth. The scoped
      // win.pane_cmd is the LOWEST-index pane and reads stale on remain-on-exit
      // / split panes — letting it CONDEMN while the resurrect loop RESURRECTS
      // off a different pane was the thrash: a split or flapping window
      // oscillated a card live↔pane-dead every ~10s tick (feed spam, re-waking
      // every watcher/broadcast). Trust one pane for both decisions.
      //   deadSignal: true = looks dead, false = live claude, null = some other
      //   command → UNKNOWN, no action (unchanged firstmate rule).
      let deadSignal;
      if (win.pane_dead) {
        deadSignal = true;
      } else if (win.pane_cmd === 'claude') {
        deadSignal = false; // fast path: lowest pane already reads claude, no extra probe needed
      } else {
        const pane = await tmuxAdapter.paneCurrentCommand(win.window_id);
        if (pane && !pane.dead && pane.cmd === 'claude') deadSignal = false;      // active pane IS a live claude
        else if (!pane || pane.dead || SHELL_RE.test(pane.cmd)) deadSignal = true; // dead / bare shell remnant
        else deadSignal = null;                                                    // repurposed to some other cmd → unknown
      }
      if (deadSignal === false) {
        condemnStreak.delete(row.spawn_id); // a live read resets the hysteresis counter
        if (row.status === 'spawning' && Date.now() - row.requested_at > SPAWN_REGISTER_MS) {
          const note = `pane up but never registered — env/port issue? window ${row.tmux_window}`;
          q.setSpawnStatus.run('stalled', row.spawn_id);
          // Loud lane, deliberately: a stalled spawn is a human's problem now
          // (fail loud, never auto-respawn). The first late hook re-derives col.
          updateSession(row.session_id, { col: 'needsyou', notification_type: 'spawn_stalled', note });
          const c = q.getSession.get(row.session_id);
          tick(`⚠ ${c?.callsign ?? row.callsign} pane is up but never phoned home`);
          logEvent(row.session_id, 'SpawnStalled', null, note);
          onMutate();
        }
        continue; // alive; stalled is fail-loud state only, never remediation
      }
      if (deadSignal === null) continue; // unknown → no action
      // deadSignal === true: HYSTERESIS — require CONDEMN_DEAD_READS consecutive
      // dead reads before condemning a LIVE spawn, so a single transient dead
      // read cannot condemn a card the resurrect loop would only flip back next
      // tick. A genuinely dead pane just condemns one tick later.
      const streak = (condemnStreak.get(row.spawn_id) ?? 0) + 1;
      if (streak < CONDEMN_DEAD_READS) {
        condemnStreak.set(row.spawn_id, streak);
        continue;
      }
      q.setSpawnStatus.run('pane-dead', row.spawn_id);
      forgetSpawn(row.spawn_id); // M-G2 (also clears the condemnStreak entry)
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        tombstoneCard(row.session_id, { // D8
          note: `pane idle — resume with claude --resume ${row.session_id}`,
          tickMsg: `💀 ${c.callsign} pane died (claude no longer running) — window kept for scrollback`,
        });
      }
      onMutate();
    }
    // BUG 3 — resurrection: re-validate the terminal-but-recoverable rows
    // ('pane-dead'/'gone', never 'killed') against tmux and bring back any
    // whose window is a live claude. This is the ONLY exit from those states
    // back to 'live'; without it BUG 1/BUG 2 (and any transient misread) are a
    // permanent one-way door, and revive() deadlocks on the very-alive pane.
    for (const row of resurrectable) {
      const win = wins.find(w => w.window === row.tmux_window);
      if (!win || win.pane_dead) continue; // gone/dead/unreachable → stays condemned (firstmate rule)
      // A NEWER row may now own this reused window name (a revive lineage);
      // only resurrect when this row is still the window's rightful owner.
      // currentWindowOwner ranks provisioning|spawning|stalled|live|pane-dead
      // newest-first: for a 'pane-dead' row it returns that row unless a newer
      // live row exists; for a 'gone' row (excluded from that query) it returns
      // null when no other row claims the window — both mean "resurrect me".
      const owner = q.currentWindowOwner.get(row.tmux_window);
      if (owner && owner.spawn_id !== row.spawn_id) continue;
      // Second probe confirms 'claude' (the scoped row's pane_cmd can read
      // stale on remain-on-exit panes; pane_dead already screened above). This
      // is the exact liveness test ownedPaneDeliverable trusts to type mail in.
      const pane = await tmuxAdapter.paneCurrentCommand(win.window_id);
      if (!pane || pane.dead || pane.cmd !== 'claude') continue; // bare shell / not claude → stays condemned
      resurrectSpawn(row);
    }
    // Keep the boot-computed orphan list honest: windows that disappear
    // stop being listed (informational only — no ops are ever offered).
    const owned = new Set(q.allSpawns.all().map(r => r.tmux_window));
    const orphans = wins.filter(w => !owned.has(w.window)).map(w => ({ window: w.window }));
    if (JSON.stringify(orphans) !== JSON.stringify(spawnState.orphans)) {
      spawnState.orphans = orphans;
      onMutate();
    }
  }

  // H-R2: listScopedWindows() returns [] indistinguishably for "tmux is
  // reachable but this fleet owns no windows" (→ genuinely gone) and "tmux
  // timed out / is unreachable" (→ UNKNOWN). Boot reconciliation is the only
  // path that tombstones absent windows, so on a transient tmux hiccup at
  // restart it would mark the WHOLE live fleet gone+offline — the exact
  // "unreachable → confidently dead" mistake spawnLivenessTick already refuses
  // (`if (!win) continue`). The clean fix threads an explicit unknown-signal
  // out of the adapter; that lives in spawn.mjs (see coordination note), so
  // here we probe reachability instead: when the scoped list is empty AND we
  // still hold active rows, confirm tmux is actually reachable before trusting
  // the emptiness. ensureSession() is the only exported call that distinguishes
  // reachable (resolves) from unreachable (throws); its create-on-absence side
  // effect is exactly what the next spawn would do anyway.
  async function tmuxReachableForReconcile() {
    // Test-override mode has no tmux server to reach — the fixture drives
    // spawns and the empty scoped list is authoritative (existing contract).
    if (tmuxAdapter.spawnOverrideCmd()) return true;
    // No tmux binary at all: there is no server that could be "unreachable",
    // and rows created under a since-removed backend should reconcile normally.
    if (typeof tmuxAdapter.hasTmux === 'function' && !tmuxAdapter.hasTmux()) return true;
    try { await tmuxAdapter.ensureSession(port); return true; }
    catch { return false; } // binary present but server wedged / timed out
  }

  // Restart reconciliation (fleetd boot): spawn rows outlive the daemon in
  // SQLite while the panes outlive it in tmux — re-join the two. Rows in
  // spawning|stalled|live whose exact scoped window is gone → 'gone' + card
  // offline; provisioning rows (H-R6, launch interrupted) → gone + offline;
  // scoped windows with no row at all → spawn_orphans ("unadopted" on the
  // board; surfaced, never operated on).
  const spawnState = { orphans: [] };
  async function reconcileSpawns() {
    // BOOT TOCTOU: snapshot the rows to reconcile BEFORE awaiting the tmux
    // window list. reconcile exists to settle rows that PRE-EXIST this boot
    // against the current windows; it runs fire-and-forget the instant the
    // server starts listening, so a human's POST /api/spawn (or a revive) can
    // land DURING the listScopedWindows await below. That new spawn inserts its
    // row and flips it 'spawning' synchronously — and if we read `active` AFTER
    // the await, the brand-new row is in the set but its window is absent from a
    // `wins` snapshot taken before the window could exist, so the loop condemns
    // it 'gone' and tombstones the just-created card. (Observed as the
    // intermittent Node-24 "spawn never reaches live": the later first hook
    // finds the row already 'gone' and the live-flip gate skips it.) Reading the
    // candidate rows first excludes anything created concurrently — that spawn
    // is owned by the live spawn/liveness path, never by boot reconciliation.
    // This mirrors spawnLivenessTick, which already snapshots its rows before
    // its own await. Provisioning rows are captured here for the same reason
    // (staleProvisioningSpawns has no age floor, so a fresh provisional row
    // read post-await would be condemned identically).
    const active = q.activeSpawns.all();
    const staleProvisioning = q.staleProvisioningSpawns.all();
    const wins = await tmuxAdapter.listScopedWindows(port);
    if (!wins.length && active.length && !(await tmuxReachableForReconcile())) {
      // Unreachable at boot → leave every row UNKNOWN, tombstone nothing.
      tick(`⚠ tmux unreachable at restart — leaving ${active.length} spawn row(s) as-is (unknown, not gone)`);
      onMutate();
      return;
    }
    const names = new Set(wins.map(w => w.window));
    for (const row of active) {
      if (names.has(row.tmux_window)) continue;
      q.setSpawnStatus.run('gone', row.spawn_id);
      forgetSpawn(row.spawn_id); // M-G2
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        tombstoneCard(row.session_id, { // D8 (reconcile never woke watchers)
          note: 'spawned pane gone (daemon restart reconciliation)',
          tickMsg: `${c.callsign} pane gone — noticed at daemon restart`,
          notify: false,
        });
      }
      onMutate();
    }
    // H-R6: a 'provisioning' row is a spawn that died between its durable
    // insert and the completion of its external ops (worktree/window). It
    // never had a live pane; boot is where we finish the job — settle the row
    // and tombstone the half-born card. (Snapshot taken pre-await above so a
    // spawn created concurrently with reconcile is not swept up as stale.)
    for (const row of staleProvisioning) {
      q.setSpawnStatus.run('gone', row.spawn_id);
      forgetSpawn(row.spawn_id);
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        tombstoneCard(row.session_id, { // D8 (reconcile never woke watchers)
          note: 'spawn interrupted before launch (daemon restart)',
          tickMsg: `${c.callsign} spawn was interrupted before launch — cleaned up at restart`,
          notify: false,
        });
      }
      onMutate();
    }
    const owned = new Set(q.allSpawns.all().map(r => r.tmux_window));
    spawnState.orphans = wins.filter(w => !owned.has(w.window)).map(w => ({ window: w.window }));
    if (spawnState.orphans.length) {
      tick(`⚠ ${spawnState.orphans.length} unadopted fleetdeck window(s) in tmux (fd${port}-* with no spawn row)`);
      onMutate();
    }
  }

  return {
    spawn, revive, adoptSession, enableRemote, spawnKill, spawnCapability,
    spawnLivenessTick, reconcileSpawns, scheduleRegistrationRemoteHarvest,
    forgetSpawn, spawnState,
  };
}

