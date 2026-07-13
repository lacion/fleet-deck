// derive.mjs — fleetd core: the spike's applyEvent state derivation ported
// onto SQLite, plus file ledger / conflicts (F4), mail (F2), the SessionStart
// brief (F1) and the /state snapshot.
//
// Columns are DERIVED, never self-reported:
//   queued → working → verifying → needsyou → idle → offline
// Transition rules are a faithful port of fleetdeck-spike/server/fleetd.mjs.

import fs from 'node:fs';
import { branchOf } from './repo-identity.mjs';
import { ticketFromBranch, animalOf } from './tickets.mjs';
import { createQuestions, resolveHoldMs } from './questions.mjs';
import { lastAssistantModel } from './transcript.mjs';
import * as defaultTmuxAdapter from './spawn.mjs';
import { createStatements } from './statements.mjs';
import { createWorktrees } from './worktrees.mjs';
import { createMail } from './mail.mjs';
import { createLedger } from './ledger.mjs';
import { createIngest } from './ingest.mjs';
import { createCommands } from './commands.mjs';
import { createPlans } from './plans.mjs';
import { createSpawns } from './spawns.mjs';
import { createEvents } from './events.mjs';
import { createSnapshot } from './snapshot.mjs';
import { createRetention } from './retention.mjs';
import { envInt } from './helpers.mjs';

// Public re-exports: these helpers moved to helpers.mjs, but tests and other
// scripts import them from derive.mjs — keep them importable from here.
export {
  mungeClaudeProjectCwd, claudeTranscriptPath, claudeEnvArgvPrefix, spawnRowRevivable,
} from './helpers.mjs';

const CALLSIGNS = ['falcon', 'otter', 'raven', 'lynx', 'orca', 'wren', 'viper', 'heron', 'badger', 'comet', 'ember', 'drift'];
// CONFLICT_WINDOW_MS lives in ledger.mjs now.
// MAIL_MAX_LEN + clampMail (the surrogate-safe bound) live in mail.mjs now.
// EDIT_TOOLS + TEST_RUNNER_RE live in events.mjs now.

// v1.2 env knobs (resolved once per core; tests spawn fresh daemons):
//   FLEETDECK_STALE_MS    — stale badge threshold for working/verifying cards
//                           with no events (default 600 000 = 10 min)
//   FLEETDECK_NUDGE_MS               — bring-up nudge delay (default 8 s)
//   FLEETDECK_SPAWN_REGISTER_MS      — pane registration deadline (90 s)
//   FLEETDECK_PANE_MAIL_GRACE_MS     — watcher-first mail grace (1.5 s)
//   FLEETDECK_PRESUME_DEAD_MS        — silent hook-session timeout (3 h)
//   FLEETDECK_RETAIN_OFFLINE_MS      — offline retention window (24 h)
// envInt (the reader) + mungeClaudeProjectCwd / claudeTranscriptPath /
// spawnRowRevivable / claudeEnvArgvPrefix now live in helpers.mjs (re-exported
// above).

export function createCore(db, {
  port = 4711,
  home = process.env.FLEETDECK_HOME || '',
  holdMs = resolveHoldMs(),
  tmuxAdapter = defaultTmuxAdapter,
} = {}) {
  const t0 = Date.now();
  // onMutate is reassignable through the setter on the returned surface; the
  // extracted modules capture the STABLE wrapper (ctx.onMutate below), which
  // always calls the current impl — so a late setter reassignment still reaches
  // every module. In-scope callers keep calling onMutate() unchanged.
  let onMutateImpl = () => {};
  const onMutate = () => onMutateImpl();
  const STALE_MS = envInt('FLEETDECK_STALE_MS', 600_000, { min: 1 });
  const NUDGE_MS = envInt('FLEETDECK_NUDGE_MS', 8_000, { min: 1 });
  const SPAWN_REGISTER_MS = envInt('FLEETDECK_SPAWN_REGISTER_MS', 90_000, { min: 1 });
  const PANE_MAIL_GRACE_MS = envInt('FLEETDECK_PANE_MAIL_GRACE_MS', 1_500, { min: 0 });
  const PRESUME_DEAD_MS = envInt('FLEETDECK_PRESUME_DEAD_MS', 10_800_000, { min: 1 });
  const RETAIN_OFFLINE_MS = envInt('FLEETDECK_RETAIN_OFFLINE_MS', 86_400_000, { min: 1 });
  const RC_HARVEST_MS = envInt('FLEETDECK_RC_HARVEST_MS', 2_500, { min: 0 });
  // M-G1 ledger retention: file_touches / commands / conflicts / settled mail
  // are aged out of SQLite after this window, and the snapshot only aggregates
  // file touches newer than it. Defaults to 24h (matching the events prune).
  // Daemon-internal only — deliberately NOT in the claudeEnvArgvPrefix scrub
  // list, since a spawned `claude` child never reads it.
  const RETAIN_LEDGER_MS = envInt('FLEETDECK_RETAIN_LEDGER_MS', 86_400_000, { min: 60_000 });
  const SNAPSHOT_FILES_PER_SESSION = 50; // M-P2/M-G1: per-card cap on the ledger file list

  // D7: the single way to resolve one of THIS fleet's scoped tmux windows by
  // its exact name. Owned-pane mail delivery, the bring-up nudge, revive's
  // window reuse and /rc enablement all asked the same question — collapse the
  // five copies of (await listScopedWindows(port)).find(w => w.window === X)
  // here so "which window is this" is asked exactly one way.
  async function findScopedWindow(name) {
    return (await tmuxAdapter.listScopedWindows(port)).find(w => w.window === name);
  }

  // ----------------------------------------------------------- statements
  // The prepared-statement map (q) + the cached-UPDATE updateSession writer
  // live in statements.mjs now.
  const { q, updateSession } = createStatements(db);

  // F3 needs-you relay (questions.mjs): question rows + hold-open management.
  // mail/tick are function declarations below — hoisted, so passing them here
  // is safe; onMutate is captured lazily through the arrow.
  const questions = createQuestions(db, {
    holdMs,
    mail: (sid, from, text) => ctx.mail(sid, from, text),
    tick: msg => tick(msg),
    callsignOf: sid => q.getSession.get(sid)?.callsign ?? null,
    onChange: () => onMutate(),
    // v1.3 plan library: the plans table lives here; questions.mjs only needs
    // the link (plan_id for /state) and the answer-path status flips. The
    // flip is guarded to 'proposed' — the answer paths describe the freshly
    // captured plan, and a plan the human already archived/marked from the
    // library keeps that verdict.
    planIdFor: questionId => q.planByQuestion.get(questionId)?.plan_id ?? null,
    planAnswered: (questionId, behavior) => {
      const p = q.planByQuestion.get(questionId);
      if (!p || p.status !== 'proposed') return;
      const status = behavior === 'allow' ? 'approved'
        : behavior === 'capture' ? 'captured'
        : 'rejected';
      q.setPlanStatus.run(status, p.plan_id);
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) ${status}`);
    },
  });

  // ------------------------------------------------------------- model tracking
  // Claude Code reports `model` in the SessionStart payload and nowhere else, so
  // a /model switch mid-session would otherwise leave the badge frozen at the
  // launch model forever. The transcript records the model on every assistant
  // line, so we re-read its tail — carefully:
  //
  //   floor — the transcript's size at SessionStart. `claude --resume` appends to
  //           the SAME file, so a resumed session's tail is full of the PREVIOUS
  //           run's assistant lines. Without this, the first post-SessionStart
  //           event (a UserPromptSubmit, which appends only a user line) would
  //           read the old model and stomp the fresh one from the payload. No
  //           floor recorded (daemon restarted mid-session) → 0 → the transcript
  //           is the truth, which is exactly right after a restart.
  //   size  — the file size at our last read. Transcripts only grow, so an
  //           unchanged size means there is nothing new to see: skip the read.
  const modelMemo = new Map(); // sid -> { floor, size, model }

  function stampTranscriptFloor(sid, transcriptPath) {
    let floor = 0;
    try { if (transcriptPath) floor = fs.statSync(transcriptPath).size; } catch { /* not written yet → 0 */ }
    modelMemo.set(sid, { floor, size: -1, model: null });
  }

  function readTranscriptModel(sid, transcriptPath) {
    const memo = modelMemo.get(sid) ?? { floor: 0, size: -1, model: null };
    let size;
    try { size = fs.statSync(transcriptPath).size; } catch { return null; }
    if (size === memo.size) return memo.model;
    const model = lastAssistantModel(transcriptPath, { minOffset: memo.floor });
    modelMemo.set(sid, { ...memo, size, model: model ?? memo.model });
    return model;
  }

  // ------------------------------------------------------------------ cards
  // With a ticket, the callsign becomes <animal>-<TICKET> instead of
  // <animal>-<sid4>. Multiple sessions on one ticket is the NORMAL case (a fleet
  // on one branch), so each gets a different animal: scan the 12-animal rotation
  // from the count-based start and take the first <animal>-<ticket> no other
  // un-archived row holds. All 12 held → the ticket is saturated → fall back to
  // today's hex format (a stable, unique name; recorded once so detection never
  // retries). Without a ticket → today's behaviour, byte-for-byte.
  function assignCallsign(sid, ticket = null) {
    const start = q.countSessions.get().n % CALLSIGNS.length;
    if (ticket) {
      for (let i = 0; i < CALLSIGNS.length; i++) {
        const cand = CALLSIGNS[(start + i) % CALLSIGNS.length] + '-' + ticket;
        if (!q.callsignTaken.get(cand, cand, sid)) return cand;
      }
    }
    return CALLSIGNS[start] + '-' + String(sid).slice(0, 4);
  }

  // `cwd` is passed ONLY by the birth callers that know it (the applyEvent
  // entry); the hookStop/detectFreeform/ledger callers pass none because they
  // only ever hit an existing row. When a fresh card is born with a cwd, detect
  // its ticket from the SERVER-derived branch — fresh:true so a 20s-stale cache
  // can't misname a just-switched checkout — and name it ticket-first. The
  // ticketFromBranch → assignCallsign → insert chain has NO await in it: the
  // naming synchrony invariant (see plan) requires choose-name + insert to land
  // in one tick, which is why fresh branchOf stays execFileSync.
  function card(sid, cwd = null) {
    let c = q.getSession.get(sid);
    if (!c) {
      const ticket = ticketFromBranch(cwd ? branchOf(cwd, { fresh: true }) : null);
      const callsign = assignCallsign(sid, ticket);
      const now = Date.now();
      q.insertSession.run(sid, callsign, now, now);
      // Birth is NOT a rename: the card is inserted already ticket-named, so
      // there is no prev_callsign and a single "joined" tick. Record ticket +
      // source even on the hex fallback — detection was consumed, and the auto
      // path (guarded on ticket_source IS NULL) must never fire again.
      if (ticket) updateSession(sid, { ticket, ticket_source: 'branch' });
      c = q.getSession.get(sid);
      tick(`${callsign} joined the fleet`);
    }
    return c;
  }

  // applyTicket — the ONE rename path, shared by branch auto-detect (events.mjs)
  // and the manual `ticket` command (commands.mjs). Returns
  // { ok:true, renamed, callsign, ticket, previous? } (previous only on a real
  // rename) or { ok:false, reason }. Runs fully synchronously (no await), so it
  // preserves the naming-collision invariant: read current name, compute the
  // next, write it, all in one tick.
  function applyTicket(sid, ticket, source) {
    const c = q.getSession.get(sid);
    if (!c || c.ended_at != null) return { ok: false, reason: 'no live session for that target' };
    // Auto-rename fires AT MOST ONCE: a branch detection is refused the moment
    // a ticket/source already exists (a later branch switch changes nothing, and
    // a manual ticket_source permanently blocks the auto path). Manual may fire
    // any number of times.
    if (source === 'branch' && (c.ticket != null || c.ticket_source != null)) {
      return { ok: false, reason: 'ticket already set — auto-detect fires once' };
    }
    // Prefer keeping the current animal so a rename is the least-surprising
    // <sameAnimal>-<ticket>; only when that exact name is already held by
    // another row do we rotation-scan for a free animal.
    const preferred = `${animalOf(c.callsign)}-${ticket}`;
    const next = q.callsignTaken.get(preferred, preferred, sid) ? assignCallsign(sid, ticket) : preferred;
    // assignCallsign returns the hex fallback only when all 12 animals for this
    // ticket are taken. Saturation: keep the current name, but still record
    // ticket + source so the auto path never retries (manual is the recovery).
    if (!next.endsWith('-' + ticket)) {
      updateSession(sid, { ticket, ticket_source: source });
      tick(`🎫 ${c.callsign} stays on ticket ${ticket} — every callsign for it is taken`);
      onMutate();
      return { ok: true, renamed: false, callsign: c.callsign, ticket };
    }
    if (next === c.callsign) {
      // Name unchanged (a manual re-ticket with the current key). Record the
      // fields (e.g. a manual pin over a branch-detected ticket) without a
      // rename tick or touching prev_callsign.
      updateSession(sid, { ticket, ticket_source: source });
      onMutate();
      return { ok: true, renamed: false, callsign: c.callsign, ticket };
    }
    // prev_callsign is WRITE-ONCE: set to the birth callsign on the first rename
    // (c.prev_callsign ?? c.callsign), never overwritten by a later manual
    // rename. WHY: the birth name is the longest-lived stale reference — printed
    // into this session's own SessionStart brief and every peer's brief — so it
    // must remain the mail-routing anchor even after a second re-ticket.
    const previous = c.callsign;
    updateSession(sid, {
      callsign: next,
      prev_callsign: c.prev_callsign ?? c.callsign,
      ticket,
      ticket_source: source,
    });
    // ONE tick carrying BOTH names + the ticket: the board ticker filters by
    // callsign substring, so a line naming old AND new appears in both cards'
    // timelines — the handoff is visible from either name.
    tick(`🎫 ${previous} is now ${next} (ticket ${ticket})`);
    onMutate();
    return { ok: true, renamed: true, callsign: next, ticket, previous };
  }

  function tick(msg) {
    q.insertTicker.run(Date.now(), msg);
    q.trimTicker.run();
  }

  function logEvent(sid, hookEvent, toolName, note) {
    q.insertEvent.run(sid, hookEvent ?? null, toolName ?? null, note ?? null, Date.now());
  }

  // ------------------------------------------------------------------- ctx
  // The shared closure state threaded into every extracted module-factory.
  // Primitives (statements, the session writer, tick/logEvent/card, model
  // tracking, the scoped-window resolver, env knobs, the tmux adapter, the
  // questions relay) live here; each createX(ctx) factory below reads what it
  // needs and Object.assign's its own surface back onto ctx so later factories
  // (and the return surface) can reach it. onMutate is the STABLE wrapper, so
  // the setter on the returned object reaches code in every module.
  const ctx = {
    db, port, home, holdMs, t0,
    STALE_MS, NUDGE_MS, SPAWN_REGISTER_MS, PANE_MAIL_GRACE_MS,
    PRESUME_DEAD_MS, RETAIN_OFFLINE_MS, RC_HARVEST_MS, RETAIN_LEDGER_MS,
    SNAPSHOT_FILES_PER_SESSION,
    q, updateSession, onMutate, tmuxAdapter, questions,
    findScopedWindow, tick, logEvent, card, assignCallsign, applyTicket,
    modelMemo, stampTranscriptFloor, readTranscriptModel,
  };

  // Mail + /api/watch waiter registry + owned-pane delivery → mail.mjs.
  Object.assign(ctx, createMail(ctx));
  const {
    mail, drainMail, resolveTargets, notifyWatchers, addWatchWaiter,
    hasWatchWaiter, ownedPaneRow, ownedPaneDeliverable, tryOwnedPaneDelivery,
    claimMail, watchInfo, postMail,
  } = ctx;

  // D8: the offline-tombstone write, in one place. Every terminal transition
  // (spawn-fail, kill, liveness/silence condemn, reconcile-gone) flips a card
  // to offline with an ended_at + note; most also drop the transcript memo,
  // feed a line, and wake watchers. Callers pass the flags matching their exact
  // prior effects — `at` (Date.now() vs the sweep's `now`), whether to drop the
  // model memo, the feed line, the watcher wake, and onMutate (several callers
  // batch it, or place it outside a liveness guard, so it stays theirs).
  function tombstoneCard(sid, { note, at = Date.now(), tickMsg = null, notify = true, forgetModel = false, mutate = false }) {
    updateSession(sid, { col: 'offline', ended_at: at, note });
    if (forgetModel) modelMemo.delete(sid);
    if (tickMsg) tick(tickMsg);
    if (notify) notifyWatchers(sid);
    if (mutate) onMutate();
  }
  ctx.tombstoneCard = tombstoneCard;

  // File-touch ledger + conflict radar → ledger.mjs.
  Object.assign(ctx, createLedger(ctx));
  const { recordFile, whisperText } = ctx;

  // agents-cli ingest (F1) → ingest.mjs.
  Object.assign(ctx, createIngest(ctx));
  const { ingestAgentsPoll } = ctx;

  // POST /command → commands.mjs.
  Object.assign(ctx, createCommands(ctx));
  const { command } = ctx;

  // v1.3 plan library mark → plans.mjs.
  Object.assign(ctx, createPlans(ctx));
  const { planMark } = ctx;

  // Worktree custody (inspection + allow-listed removal) → worktrees.mjs.
  const { worktrees, removeWorktree } = createWorktrees(ctx);

  // v1.2/v1.3 board-spawned session lifecycle → spawns.mjs.
  Object.assign(ctx, createSpawns(ctx));
  const {
    spawn, revive, enableRemote, spawnKill, spawnCapability,
    spawnLivenessTick, reconcileSpawns, scheduleRegistrationRemoteHarvest,
    forgetSpawn, spawnState,
  } = ctx;

  // Hook state machine (applyEvent + hook endpoints + brief + plan capture) → events.mjs.
  Object.assign(ctx, createEvents(ctx));
  const {
    applyEvent, hookSessionStart, hookUserPromptSubmit, hookPostToolUse,
    hookStop, hookSessionEnd, hookHoldQuestion,
  } = ctx;

  // /state snapshot + fleetSize + live-terminal resolver → snapshot.mjs.
  Object.assign(ctx, createSnapshot(ctx));
  const { snapshot, fleetSize, terminalSpawn } = ctx;

  // Retention sweep + manual cleanup → retention.mjs.
  Object.assign(ctx, createRetention(ctx));
  const { retentionSweep, cleanup } = ctx;

  // Run retention once at core boot, then alongside event pruning every 10m.
  // BUG 2: retentionSweep is async now (it may probe tmux for spawned silence),
  // but with no spawned candidate it completes SYNCHRONOUSLY before returning
  // its already-resolved promise, so the boot sweep's DB effects still land
  // synchronously for the common case. .catch() contains any tmux-probe
  // rejection so a fire-and-forget sweep can never become an unhandled reject.
  retentionSweep().catch(err => console.error('fleetd retention sweep error:', err));
  setInterval(() => {
    try { q.pruneEvents.run(Date.now() - 24 * 3600 * 1000); } catch { /* hygiene only */ }
    retentionSweep().catch(() => { /* hygiene only */ });
  }, 10 * 60 * 1000).unref();

  return {
    applyEvent,
    hookSessionStart,
    hookUserPromptSubmit,
    hookPostToolUse,
    hookStop,
    hookSessionEnd,
    hookHoldQuestion,
    questions, // F3 relay surface: attachHold / socketClosed / answer / …
    addWatchWaiter,  // F3d-2 watch surface (GET /api/watch v2)
    // hasWatchWaiter is used only INTERNALLY (mail routing + snapshot); no
    // http.mjs/fleetd.mjs caller consumes it, so it is not re-exported here.
    claimMail,       // "
    watchInfo,       // "
    drainMail,
    postMail,
    tryOwnedPaneDelivery,
    command,
    snapshot,
    fleetSize,
    terminalSpawn,
    ingestAgentsPoll,
    // v1.2 dynamic fleet
    spawn,             // POST /api/spawn flow → {status, body}
    revive,            // POST /api/spawn/:id/revive → {status, body}
    enableRemote,      // POST /api/spawn/:id/rc → {status, body}
    spawnKill,         // POST /api/spawn/:id/kill → {status, body}
    spawnCapability,   // /health + /state `spawn` object
    spawnLivenessTick, // owned-pane liveness, rides the agents-poll cadence
    reconcileSpawns,   // fleetd boot: rows ↔ tmux windows
    // retentionSweep also runs internally (boot + the 10m interval above). It
    // is re-exported so tests can drive the tmux-verified presume-dead path
    // (BUG 2) deterministically; production callers keep using the interval.
    retentionSweep,
    cleanup,
    worktrees,          // GET /api/worktrees — bounded live git inspection
    removeWorktree,     // POST /api/worktrees/remove — allow-listed destruction
    // v1.3 plan library
    planMark,          // POST /api/plans/:id/mark → {status, body}
    set onMutate(fn) { onMutateImpl = fn; },
  };
}
