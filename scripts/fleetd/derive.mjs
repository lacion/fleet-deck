// derive.mjs — fleetd core: the spike's applyEvent state derivation ported
// onto SQLite, plus file ledger / conflicts (F4), mail (F2), the SessionStart
// brief (F1) and the /state snapshot.
//
// Columns are DERIVED, never self-reported:
//   queued → working → verifying → needsyou → idle → offline
// Transition rules are a faithful port of fleetdeck-spike/server/fleetd.mjs.

import fs from 'node:fs';
import path from 'node:path';
import { branchOf } from './repo-identity.ts';
import { ticketFromBranch, animalOf } from './tickets.ts';
import { createQuestions, resolveHoldMs } from './questions.ts';
import { lastAssistantModel } from './transcript.ts';
import * as defaultTmuxAdapter from './spawn.mjs';
import { createStatements } from './statements.ts';
import { createWorktrees } from './worktrees.ts';
import { createRepos } from './repos.ts';
import { createSettings } from './settings.ts';
import { createFiles } from './files.ts';
import { pasteImage } from './paste.ts';
import { createMail, MAIL_MAX_LEN } from './mail.ts';
import { createLedger } from './ledger.ts';
import { createIngest } from './ingest.ts';
import { createCommands } from './commands.ts';
import { createPlans } from './plans.ts';
import { createSpawns } from './spawns.mjs';
import { createEvents } from './events.ts';
import { createSnapshot } from './snapshot.ts';
import { createRetention } from './retention.ts';
import { createKeyedMutex, envInt } from './helpers.ts';

// Public re-exports: these helpers moved to helpers.mjs, but tests and other
// scripts import them from derive.mjs — keep them importable from here.
export {
  mungeClaudeProjectCwd, claudeTranscriptPath, claudeEnvArgvPrefix, spawnRowRevivable,
} from './helpers.ts';

const CALLSIGNS = ['falcon', 'otter', 'raven', 'lynx', 'orca', 'wren', 'viper', 'heron', 'badger', 'comet', 'ember', 'drift'];
// CONFLICT_WINDOW_MS lives in ledger.mjs now.
// MAIL_MAX_LEN + clampMail (the surrogate-safe bound) live in mail.ts now.
// EDIT_TOOLS + TEST_RUNNER_RE live in events.ts now.

// v1.2 env knobs (resolved once per core; tests spawn fresh daemons):
//   FLEETDECK_STALE_MS    — stale badge threshold for working/verifying cards
//                           with no events (default 600 000 = 10 min)
//   FLEETDECK_NUDGE_MS               — bring-up nudge delay (default 8 s)
//   FLEETDECK_SPAWN_REGISTER_MS      — pane registration deadline (90 s)
//   FLEETDECK_SETUP_REGISTER_MS      — setup-spawn registration deadline (10 m)
//   FLEETDECK_PANE_MAIL_GRACE_MS     — watcher-first mail grace (1.5 s)
//   FLEETDECK_PRESUME_DEAD_MS        — silent hook-session timeout (3 h)
//   FLEETDECK_PRESUME_DEAD_WORKING_MS — same, for mid-turn columns (9 h)
//   FLEETDECK_RETAIN_OFFLINE_MS      — offline retention window (24 h)
//   FLEETDECK_REPOS_DIR              — default root for managed repository clones
//   FLEETDECK_CLONE_TIMEOUT_MS        — git clone timeout (default 600 000 = 10 min)
//   FLEETDECK_CLONE_CONCURRENCY       — max repos cloning at once (default 3)
//   FLEETDECK_REARM_GRACE_MS         — UX 2.1 re-arm grace after hold expiry
//                                      (default 3 s; 0 disables re-arming)
// envInt (the reader) + mungeClaudeProjectCwd / claudeTranscriptPath /
// spawnRowRevivable / claudeEnvArgvPrefix now live in helpers.mjs (re-exported
// above).

export function createCore(db, {
  port = 4711,
  home = process.env.FLEETDECK_HOME || '',
  // The default defers: q doesn't exist until createStatements runs below, so
  // the hold_ms settings row is threaded in after (see the questions wiring).
  holdMs = null,
  tmuxAdapter = defaultTmuxAdapter,
  // Daemon version, threaded from fleetd.mjs's package.json read so the
  // snapshot can tell the board which build is serving it (upgrade-takeover
  // observability). '0.0.0' mirrors /health's standalone-install fallback.
  version = '0.0.0',
  // BUG-128: test-only overrides for the mail pending budget and pane batch
  // bounds (mail.ts defaults to its own constants when these are undefined).
  MAIL_PENDING_MAX, MAIL_PENDING_MAX_BYTES, MAIL_PANE_BATCH, MAIL_PANE_BATCH_BYTES,
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
  const SETUP_REGISTER_MS = envInt('FLEETDECK_SETUP_REGISTER_MS', 600_000, { min: 1 });
  const PANE_MAIL_GRACE_MS = envInt('FLEETDECK_PANE_MAIL_GRACE_MS', 1_500, { min: 0 });
  // BUG-034: how long an in-flight mail claim (watch response, owned-pane
  // paste, board /mail GET) stays exclusively leased waiting for its ack
  // before the retention sweep hands it back for redelivery. 30 s covers a
  // slow hook/HTTP round-trip with headroom; a daemon that dies mid-flight
  // simply lets the deadline pass and the next daemon re-claims.
  const MAIL_CLAIM_LEASE_MS = envInt('FLEETDECK_MAIL_CLAIM_LEASE_MS', 30_000, { min: 1 });
  const PRESUME_DEAD_MS = envInt('FLEETDECK_PRESUME_DEAD_MS', 10_800_000, { min: 1 });
  // Mid-turn silence horizon: 3x the ordinary one (9 h). A card in 'working' or
  // 'verifying' may be legitimately quiet through one very long tool call, so
  // this must stay well clear of real work — but without SOME horizon a session
  // that dies mid-turn without a SessionEnd is unclearable forever (see
  // presumeDeadWorkingSessions). Retirement is still evidence-based: a spawned
  // row is adjudicated by tmux, never by the clock alone.
  const PRESUME_DEAD_WORKING_MS = envInt('FLEETDECK_PRESUME_DEAD_WORKING_MS', PRESUME_DEAD_MS * 3, { min: 1 });
  const RETAIN_OFFLINE_MS = envInt('FLEETDECK_RETAIN_OFFLINE_MS', 86_400_000, { min: 1 });
  const RC_HARVEST_MS = envInt('FLEETDECK_RC_HARVEST_MS', 2_500, { min: 0 });
  // 0.7.0 Move-to-tmux (adopt) knobs:
  //   FLEETDECK_ADOPT_ARM_MS   — how long a "move to tmux" arm on a LIVE card
  //                              stays valid before it self-expires (default
  //                              30 min). Stored as a deadline on the card, so
  //                              expiry needs no sweep — consumers check
  //                              `adopt_armed_until > Date.now()`.
  //   FLEETDECK_ADOPT_DELAY_MS — grace after the armed session's SessionEnd
  //                              before the deferred adopt launches, letting the
  //                              CLI flush the final transcript lines the resume
  //                              reads (default 750 ms; set 0 in tests).
  const ADOPT_ARM_MS = envInt('FLEETDECK_ADOPT_ARM_MS', 1_800_000, { min: 1 });
  const ADOPT_DELAY_MS = envInt('FLEETDECK_ADOPT_DELAY_MS', 750, { min: 0 });
  // M-G1 ledger retention: file_touches / commands / conflicts / settled mail
  // are aged out of SQLite after this window, and the snapshot only aggregates
  // file touches newer than it. Defaults to 24h (matching the events prune).
  // Daemon-internal only — deliberately NOT in the claudeEnvArgvPrefix scrub
  // list, since a spawned `claude` child never reads it. envInt's below-min
  // semantics fall back to the DEFAULT (24h), never to the min, so the
  // dangerous range is an ACCEPTED horizon between one minute and the fixed
  // 30-minute conflict window — retention.mjs therefore clamps the file-touch
  // pruning cutoff to CONFLICT_WINDOW_MS itself (BUG-144).
  const RETAIN_LEDGER_MS = envInt('FLEETDECK_RETAIN_LEDGER_MS', 86_400_000, { min: 60_000 });
  // 0.7.1 /clear succession: how long after a session's /clear a brand-new
  // session id starting in the SAME cwd is read as that session continuing.
  // The real gap is milliseconds (the CLI fires both hooks in the same second);
  // 30s is generous cover for a slow hook round-trip without being long enough
  // to swallow a genuinely new session the human started right after clearing.
  const CLEAR_SUCCESSION_MS = envInt('FLEETDECK_CLEAR_SUCCESSION_MS', 30_000, { min: 0 });
  // Two /clears in one directory closer together than this are indistinguishable
  // as "the one that just happened", so the daemon refuses to pair them by time
  // and falls back to the pane as evidence (or to no merge at all).
  const CLEAR_AMBIGUITY_MS = 1_000;
  // The forward succession (a /clear claiming an heir whose SessionStart already
  // landed) is IRREVERSIBLE, so momentary uniqueness is not proof: SessionEnd is
  // an async hook, and a second session's /clear in the same cwd can still be in
  // flight when the first one arrives. The claim therefore settles through this
  // bounded interval and re-reads the predecessor set before merging — a rival
  // clear that lands inside it cancels the claim, leaving the split for the boot
  // heal to pair correctly once every clear is on record. Sized on the same
  // "the CLI fires both hooks in the same second" fact as CLEAR_AMBIGUITY_MS —
  // several hook round-trips of cover, far too short for a human's deliberate
  // second /clear to be swallowed. Set 0 in tests to keep the synchronous fast
  // path.
  const CLEAR_SETTLE_MS = envInt('FLEETDECK_CLEAR_SETTLE_MS', 250, { min: 0 });
  const SNAPSHOT_FILES_PER_SESSION = 50; // M-P2/M-G1: per-card cap on the ledger file list

  // D7: the single way to resolve one of THIS fleet's scoped tmux windows by
  // its exact name. Owned-pane mail delivery, the bring-up nudge, revive's
  // window reuse and /rc enablement all asked the same question — collapse the
  // five copies of (await listScopedWindows(port)).find(w => w.window === X)
  // here so "which window is this" is asked exactly one way.
  async function findScopedWindow(name) {
    const wins = await tmuxAdapter.listScopedWindows(port);
    // The test override launches no tmux pane by contract, so a missing test
    // server is authoritative absence. Production lookups remain fail-closed.
    if (wins === null && tmuxAdapter.spawnOverrideCmd?.()) return undefined;
    return wins === null ? null : wins.find(w => w.window === name);
  }

  // Production pane operations bind to the exact fleet session + exact window
  // name found above, not the reusable @window_id. Injected test adapters from
  // before this hardening have no exactWindowTarget helper and retain their
  // stable-id contract through the fallback.
  function scopedPaneTarget(win) {
    return tmuxAdapter.exactWindowTarget
      ? tmuxAdapter.exactWindowTarget(port, win.window)
      : win.window_id;
  }

  // ----------------------------------------------------------- statements
  // The prepared-statement map (q) + the cached-UPDATE updateSession writer
  // live in statements.mjs now.
  const { q, updateSession } = createStatements(db);

  // In-terminal plan settlement (UX 2.2, CONTRACT "B. Plan library"): plan
  // capture is unconditional (events.ts hookHoldQuestion), but the
  // proposed → approved/captured/rejected flip only exists on the BOARD-answer
  // path (planAnswered below). Approve the plan in the TERMINAL instead and
  // the question row retires by turn boundary or hold timer while the plan
  // row used to sit at 'proposed' forever, still offering Execute/Assign.
  //
  // Settlement is ACTIVITY-BASED, not hook-matcher-based — an ExitPlanMode
  // approval may never emit a PostToolUse the daemon could correlate. The rule:
  // a 'proposed' plan flips to the terminal status 'handled-in-terminal' when
  // its linked question has been retired without a board answer AND the
  // session shows activity. Two shapes:
  //   • retire IS activity (turn-boundary/correlated expireOnActivity — the
  //     session demonstrably moved on past its native prompt): settles in the
  //     SAME tick, via the questions.mjs onRetired seam below.
  //   • bare timer expiry / dismiss / orphan sweep (NOT activity): only ARM
  //     the flip; settleTerminalPlans re-scans at the session's NEXT activity
  //     event. Expiry alone NEVER settles — a planner killed mid-hold (daemon
  //     restart, dead socket, unanswered hold) keeps its plan at 'proposed',
  //     which is still the truth: nobody decided anything.
  // Guards, both deliberate: the flip is guarded to 'proposed' (a board verdict
  // or a library mark always wins), and pendingTerminalPlans skips plans whose
  // question is still pending (a NEW plan question raised in the same turn is
  // untouched).
  //
  // planHoldOpen: the plan-linked hold registry is questions.mjs's in-memory
  // `holds` map, which nothing outside that module can see — so the module
  // itself answers "is this question's hold socket still parked?" here. WHY
  // it matters: a plan question created DURING the current activity event
  // (hookHoldQuestion inserts the row BEFORE the HTTP layer parks the
  // socket) already reads non-pending by the time expireOnActivity's
  // post-loop callback pass runs — the status guard alone cannot tell it
  // from a genuinely retired prompt. The parked socket can: nobody has
  // decided a question whose chooser never rendered.
  function planRetired(row, { activity = false } = {}) {
    if (!row || !activity) return;
    const p = q.planByQuestion.get(row.id);
    if (!p || questions.isHeld(row.id)) return;
    if (q.settleTerminalPlan.run(p.plan_id).changes) {
      tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) handled in terminal`);
      onMutate();
    }
  }

  function settleTerminalPlans(sid) {
    let changed = false;
    for (const p of q.pendingTerminalPlans.all(sid)) {
      if (p.question_id != null && questions.isHeld(p.question_id)) continue;
      if (q.settleTerminalPlan.run(p.plan_id).changes) {
        changed = true;
        tick(`📚 plan #${p.plan_id} (${p.callsign ?? p.session_id}) handled in terminal`);
      }
    }
    if (changed) onMutate();
  }

  // F3 needs-you relay (questions.mjs): question rows + hold-open management.
  // mail/tick are function declarations below — hoisted, so passing them here
  // is safe; onMutate is captured lazily through the arrow.
  const questions = createQuestions(db, {
    // UX 2.1: FLEETDECK_HOLD_MS stays the override; the hold_ms settings row
    // is the board-tunable fallback (read live from SQLite here, at boot).
    holdMs: holdMs ?? resolveHoldMs(process.env, () => q.getSetting.get('hold_ms')?.value ?? null),
    // UX 2.1: and re-resolved per hold creation, so a hold_ms settings write
    // steers NEW holds without a daemon restart. The fallback re-reads the row
    // live (settings.mjs's resolveHoldMsRaw, threaded through ctx below — the
    // arrow defers the lookup until the first hold, by which time createSettings
    // has run).
    resolveHoldWindow: () => resolveHoldMs(process.env, () => ctx.resolveHoldMsRaw?.() ?? null),
    // UX 2.1 re-arm: how long after a hold expiry with NO session activity the
    // question re-raises as a mail-delivered card (0 disables — some existing
    // test suites would otherwise meet an unexpected second card).
    rearmGraceMs: envInt('FLEETDECK_REARM_GRACE_MS', 3_000, { min: 0 }),
    mail: (sid, from, text) => ctx.mail(sid, from, text),
    // BUG-137: the questions relay rejects framed answers that would exceed
    // the mailbox clamp (reject-before-settle instead of silent truncation).
    // Threaded from mail.ts so the two can never drift apart.
    mailMaxLen: MAIL_MAX_LEN,
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
    // UX 2.2: the retirement seam — every unanswered retirement of a
    // plan-linked question flows through planRetired (defined above; same-tick
    // settle only when the retire itself was session activity).
    onRetired: (row, opts) => planRetired(row, opts),
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
      q.rememberAlias.run(sid, callsign, now); // BUG-107: the birth name is an alias from day one
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

  // applyTicket — the ONE rename path, shared by branch auto-detect (events.ts)
  // and the manual `ticket` command (commands.mjs). Returns
  // { ok:true, renamed, callsign, ticket, previous? } (previous only on a real
  // rename) or { ok:false, reason }. Runs fully synchronously (no await), so it
  // preserves the naming-collision invariant: read current name, compute the
  // next, write it, all in one tick.
  // 0.7.1: the ONE write that renames a card, extracted from applyTicket so the
  // ticket rename, the human `name` command, and the /clear succession heal all
  // rename identically. Synchronous by contract (read-current-name → compute →
  // write, one tick), so two concurrent renames can never both see a free name.
  //
  // What it deliberately does NOT touch: spawns.callsign / spawns.tmux_window.
  // Those are frozen at spawn birth and every pane operation (mail-to-pane,
  // kill, revive, liveness, termbridge) reads the ROW, so a renamed card keeps
  // driving its real window. Renaming the tmux window instead would open a
  // crash window where the row names a window tmux no longer has — and
  // reconcileSpawns would then condemn a live pane to 'gone'. The row stays
  // authoritative; the window name is an internal handle, not a label.
  function renameCallsign(sid, c, next, { tickMsg, extra = {} }) {
    const previous = c.callsign;
    // BUG-107: prev_callsign has ONE slot and must stay the birth anchor, so
    // every DROPPED name is also recorded in the alias table — that is the set
    // target resolution falls back to after the anchor.
    q.rememberAlias.run(sid, previous, Date.now());
    updateSession(sid, {
      callsign: next,
      // prev_callsign is WRITE-ONCE: set to the birth callsign on the first
      // rename (c.prev_callsign ?? c.callsign), never overwritten by a later
      // one. WHY: the birth name is the longest-lived stale reference — printed
      // into this session's own SessionStart brief and every peer's brief — so
      // it must remain the mail-routing anchor even after a second rename.
      prev_callsign: c.prev_callsign ?? c.callsign,
      ...extra,
    });
    // ONE tick carrying BOTH names: the board ticker filters by callsign
    // substring, so a line naming old AND new appears in both cards' timelines
    // — the handoff is visible from either name.
    tick(tickMsg(previous, next));
    onMutate();
    return { ok: true, renamed: true, callsign: next, previous };
  }

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
    // 0.7.1: a human-chosen name outranks branch auto-detection, always. The
    // rule across the whole naming system is one sentence — the most recent
    // EXPLICIT human action wins, and automation never overwrites a human name.
    // (A manual `ticket` command still may: that is also an explicit human act,
    // and it clears custom_suffix on its way through.)
    if (source === 'branch' && c.custom_suffix != null) {
      return { ok: false, reason: 'session has a custom name — auto-detect does not override it' };
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
    // A ticket rename is an explicit human/branch naming act, so it takes the
    // name over from any custom suffix (cleared here — the card is now
    // ticket-named, and `name <target> clear` reverts to exactly this name).
    const out = renameCallsign(sid, c, next, {
      tickMsg: (previous, name) => `🎫 ${previous} is now ${name} (ticket ${ticket})`,
      extra: { ticket, ticket_source: source, custom_suffix: null },
    });
    return { ...out, ticket };
  }

  // 0.7.1 custom names: rename the SUFFIX, keep the animal. The animal is the
  // fleet's stable identity (12 of them, one per ticket); the suffix is the
  // part that should say what the session is doing. `suffix: null` reverts to
  // the automatic name — the ticket name if the card has a ticket, else the
  // birth <animal>-<sid4>.
  function applyCustomName(sid, suffix) {
    const c = q.getSession.get(sid);
    if (!c || c.ended_at != null) return { ok: false, reason: 'no live session for that target' };
    const animal = animalOf(c.callsign);
    // Clearing a name that was never set is a NO-OP, and it has to be: the
    // "automatic" name cannot be recomputed from the session id after a /clear
    // succession (the heir's id is not the id its inherited name was minted
    // from), so reconstructing <animal>-<sid4> here would rename a card to a
    // name no human has ever seen — and, for a paned card, away from the
    // fd<port>-<callsign> its window is actually called.
    if (suffix == null && c.custom_suffix == null) {
      return { ok: true, renamed: false, callsign: c.callsign };
    }
    // The automatic name to revert TO: the ticket name when the card has a
    // ticket, else the name it carried before the human renamed it (the
    // write-once anchor — the birth name, or the callsign inherited across a
    // /clear). If that is gone or taken, keep the current name rather than
    // invent one.
    const revertTo = c.ticket
      ? `${animal}-${c.ticket}`
      : (c.prev_callsign && !q.callsignTaken.get(c.prev_callsign, c.prev_callsign, sid)
        ? c.prev_callsign
        : c.callsign);
    const next = suffix == null ? revertTo : `${animal}-${suffix}`;
    if (next === c.callsign) {
      // Name unchanged (renaming to what it already is, or clearing a card that
      // never had a custom name). Record the column so a clear is still a clear,
      // but no rename tick and no prev_callsign churn.
      if ((c.custom_suffix ?? null) !== (suffix ?? null)) {
        updateSession(sid, { custom_suffix: suffix ?? null });
        onMutate();
      }
      return { ok: true, renamed: false, callsign: c.callsign };
    }
    if (q.callsignTaken.get(next, next, sid)) {
      return { ok: false, reason: `${next} is already taken by another session` };
    }
    return renameCallsign(sid, c, next, {
      tickMsg: (previous, name) => (suffix == null
        ? `✎ ${previous} is now ${name} (custom name cleared)`
        : `✎ ${previous} is now ${name}`),
      extra: { custom_suffix: suffix ?? null },
    });
  }

  // ------------------------------------------- 0.7.1 /clear session succession
  // THE FACT THIS EXISTS FOR: the CLI mints a NEW session id on /clear. The old
  // id fires SessionEnd(reason='clear') and, in the same second and the same
  // cwd, a brand-new id fires SessionStart(source='clear'). Nothing in either
  // payload names the other (SessionStart carries only cwd/session_id/source/
  // model/transcript_path, and the new transcript's header has no parent), so
  // the daemon used to see two unrelated sessions: the OLD card kept the spawn
  // row and the tmux window (and therefore the terminal/kill chips) while going
  // permanently silent, and a SECOND card collected all the telemetry with no
  // pane to drive. A human clicking "terminal" on one card and watching the
  // other card update is the visible symptom; a board-spawned worker that runs
  // /clear stranding its own pane is the expensive one.
  //
  // So: correlate them, and make the successor CONTINUE the card. It inherits
  // the callsign (which is also why nothing has to rename a tmux window — the
  // frozen spawns.tmux_window still matches windowName(port, callsign)), the
  // ticket, the mail anchor, the arm, the pane, the pending mail and questions.
  // The predecessor is retired as 'superseded': archived, so its name is freed
  // for the heir, and never resurrectable (its transcript is still on disk).
  //
  // findClearedPredecessor is the whole inference. It is deliberately narrow —
  // a wrong merge is worse than a duplicate card, so an ambiguous match is
  // simply not a match.
  const hasLivePane = sid => {
    const sp = q.spawnBySession.get(sid);
    return !!sp && ['provisioning', 'spawning', 'stalled', 'live'].includes(sp.status);
  };

  function findClearedPredecessor(sid, cwd, now) {
    if (!cwd) return null;
    // Ordered newest-clear-first.
    const candidates = q.clearedPredecessors.all(cwd, now - CLEAR_SUCCESSION_MS, sid);
    if (!candidates.length) return null;
    if (candidates.length === 1) return candidates[0];
    // Several sessions in this cwd cleared inside the window. The heir is born
    // MILLISECONDS after its own /clear, so the freshest clear is its parent —
    // unless two clears are so close together that "freshest" is meaningless.
    const [first, second] = candidates;
    if (first.cleared_at - second.cleared_at > CLEAR_AMBIGUITY_MS) return first;
    // Near-simultaneous. Prefer the one holding a live pane (a stranded pane is
    // the expensive case, and the pane corroborates the link); otherwise refuse
    // to guess — an unhealed split is recoverable, a wrong merge is not.
    const paned = candidates.filter(c => hasLivePane(c.session_id));
    return paned.length === 1 ? paned[0] : null;
  }

  // The other direction. SessionEnd is an ASYNC hook (hooks.json) while
  // SessionStart is not, so the heir's birth can reach the daemon BEFORE its
  // predecessor's /clear — and the agents-cli poller can even create the heir's
  // row first. Either way the hook-time interception misses, and without this
  // the pair silently stays split until the next boot heal. So when a /clear
  // lands, also look FORWARD: is the heir already here, orphaned, waiting?
  // `settled` marks the deferred re-check (see the settlement interval below):
  // the first pass only SCHEDULES the claim, the second pass — after every
  // in-flight hook has had its round-trip — is the one allowed to commit it.
  function succeedForwardFromClear(prevSid, cwd, { settled = false } = {}) {
    const prev = q.getSession.get(prevSid);
    if (!prev || prev.succeeded_by != null || !cwd) return null;
    const now = Date.now();
    const born = q.clearBornSessionsSince.all(now - CLEAR_SUCCESSION_MS, now + 1_000);
    const cands = [];
    for (const b of born) {
      if (b.session_id === prevSid) continue;
      const heir = q.getSession.get(b.session_id);
      if (!heir || heir.archived_at != null || heir.ended_at != null) continue;
      if (heir.cwd !== cwd || heir.succeeded_by != null) continue;
      if (q.successorClaimed.get(heir.session_id)) continue; // already continues someone
      if (q.spawnBySession.get(heir.session_id)) continue;   // owns a pane → not a stranded heir
      cands.push(heir);
    }
    // 0 candidates is the NORMAL case (the heir simply has not started yet — the
    // hook-time interception will catch it). More than one: refuse to guess.
    if (cands.length !== 1) return null;
    // Predecessor-ambiguity guard, the mirror image of findClearedPredecessor's.
    // The backward path refuses a heir when SEVERAL predecessors could be its
    // parent; this path must refuse the symmetric case — a LONE orphan heir that
    // SEVERAL predecessors are competing to claim. `prev` just cleared, but so
    // may have another agent sharing this cwd: two co-located sessions both
    // running /clear at once. If A′ arrived early (the reorder this whole forward
    // pass exists for) and is sitting orphaned while BOTH A and B are cleared,
    // not-yet-succeeded predecessors, nothing in A′'s SessionStart says which one
    // it continues — and matching only on (time-window, cwd) cannot tell them
    // apart. Whichever /clear's SessionEnd lands first would grab the lone heir
    // and graft ITS callsign, pane, mail and questions onto a conversation that
    // is really the sibling's. That merge is irreversible; the split it would
    // "fix" is not — the boot heal re-derives the pairing from the event log once
    // every clear is finally on record. So the instant a SECOND cleared,
    // un-succeeded predecessor in this cwd could equally claim this heir, refuse
    // and stay split. (Reuses q.clearedPredecessors — the same cleared-in-window
    // query the backward path reads — excluding `prev` itself; any survivor is a
    // rival for the same heir. prev's own cleared_at was stamped by applyEvent
    // just before this ran, so prev is a predecessor here, not a rival.)
    const rivals = q.clearedPredecessors.all(cwd, now - CLEAR_SUCCESSION_MS, prevSid);
    if (rivals.length) return null;
    // Still not proof. The rival check above sees only clears already COMMITTED,
    // but SessionEnd is an async hook: a second session's /clear in this same cwd
    // can be mid-flight right now (the heir beat BOTH ends here — that reorder is
    // the entire reason this forward pass exists). Committing on momentary
    // uniqueness would graft prev's callsign, pane, mail, questions and file
    // ledger onto a conversation that may be the sibling's, irreversibly — the
    // later SessionEnd cannot undo the claim, and the supplied live-daemon
    // ordering (B's heir, A's end, B's end) ended with A.succeeded_by = heirB.
    // So defer through a bounded settlement interval and require the evidence to
    // be unique at BOTH ends of it: re-read everything (the predecessor set, the
    // heir, the whole candidate set) after the delay and only merge if nothing
    // new arrived. A rival clear landing inside the window cancels the claim;
    // the pair stays split and the boot heal pairs it correctly once every clear
    // is on record. A genuinely new orphan arriving inside the window makes
    // cands.length !== 1 on the re-read, which is the existing refuse-to-guess
    // path. Tests set FLEETDECK_CLEAR_SETTLE_MS=0 and take the synchronous fast
    // path below, so the settlement only runs when the knob is nonzero.
    if (!settled && CLEAR_SETTLE_MS > 0) {
      setTimeout(() => {
        try { succeedForwardFromClear(prevSid, cwd, { settled: true }); } catch { /* the boot heal remains the backstop */ }
      }, CLEAR_SETTLE_MS).unref();
      return null;
    }
    return succeedSession(prev, cands[0].session_id, { rename: true });
  }

  // Move everything that binds a card to its work from the predecessor to the
  // heir. Synchronous: the caller runs it before the successor's first event is
  // derived, so the card the rest of applyEvent sees is already the continued one.
  function succeedSession(prev, sid, { rename = false } = {}) {
    const now = Date.now();
    const callsign = prev.callsign;
    // One transaction: a half-done succession is the one outcome worse than the
    // bug — the predecessor archived (its card gone from the board) with no heir
    // wearing its identity would delete a live session from the fleet's view.
    db.exec('BEGIN IMMEDIATE');
    try {
    // Retire the predecessor FIRST: archiving frees its callsign (callsignTaken
    // scopes on archived_at IS NULL), which is what lets the heir take the name.
    // 'superseded' is a PROVEN end (the CLI told us it cleared) but must never be
    // adopt- or revive-eligible: the pane it used to own now belongs to the heir,
    // and its transcript is a closed chapter (see sessionAdoptableNow). The arm
    // is cleared here and re-hung on the heir below — an arm left on a ghost
    // would be found by retention's orphaned-arm sweep and fired at the
    // abandoned transcript.
    updateSession(prev.session_id, {
      col: 'offline',
      ended_at: now,
      end_reason: 'superseded',
      succeeded_by: sid,
      archived_at: now,
      cleared_at: null,
      note: `context cleared → continued as ${callsign}`,
      adopt_armed_until: null,
      adopt_armed_skip: null,
    });
    if (rename) {
      // Heal mode: the heir already exists under its own auto-assigned name (a
      // stranded card the board has been showing alongside the real one). Rename
      // it onto the lineage's callsign.
      //
      // Its throwaway name is deliberately NOT kept as the mail anchor. That
      // name only ever existed BECAUSE of the bug, while the lineage's own name
      // is the one in every peer's roster brief, in the ticker, and on the tmux
      // window. prev_callsign has exactly one slot (write-once), so spending it
      // on the artifact would leave the real name routable by nobody the next
      // time the card is renamed. Inherit the lineage's anchor instead.
      const heir = q.getSession.get(sid);
      if (heir && heir.callsign !== callsign) {
        updateSession(sid, { callsign });
      }
      updateSession(sid, {
        prev_callsign: prev.prev_callsign ?? (prev.callsign === callsign ? null : prev.callsign),
        ticket: prev.ticket ?? null,
        ticket_source: prev.ticket_source ?? null,
        custom_suffix: prev.custom_suffix ?? null,
        adopt_armed_until: prev.adopt_armed_until ?? null,
        adopt_armed_skip: prev.adopt_armed_skip ?? null,
      });
    } else {
      // Birth mode: insert the heir already wearing the inherited identity, so
      // card() finds an existing row and never mints a fresh animal (or ticks
      // "joined the fleet" for what is, to the human, the same session).
      q.insertSession.run(sid, callsign, now, now);
      updateSession(sid, {
        ticket: prev.ticket ?? null,
        ticket_source: prev.ticket_source ?? null,
        prev_callsign: prev.prev_callsign ?? (prev.callsign === callsign ? null : prev.callsign),
        custom_suffix: prev.custom_suffix ?? null,
        adopt_armed_until: prev.adopt_armed_until ?? null,
        adopt_armed_skip: prev.adopt_armed_skip ?? null,
      });
    }
    // The pane, the undelivered mail, the human's surviving question queue and
    // the file ledger all follow the card. Terminal spawn history stays with the
    // id that lived it. The ledger move is NOT bookkeeping: the conflict radar
    // treats any other still-existing session row as a rival, so touches left on
    // the archived predecessor would have the heir colliding with its own past.
    q.reassignActiveSpawns.run(sid, prev.session_id);
    q.reassignPendingMail.run(sid, prev.session_id);
    q.reassignPendingQuestions.run(sid, prev.session_id);
    q.reassignTouches.run(sid, prev.session_id);
    // BUG-107: the lineage's whole name history follows the card too, so mail
    // to any name the lineage ever wore reaches the heir.
    q.reassignAliases.run(sid, prev.session_id);
      db.exec('COMMIT');
    } catch (err) {
      try { db.exec('ROLLBACK'); } catch { /* the transaction is already gone */ }
      console.error('fleetd /clear succession error:', err);
      return null;
    }
    modelMemo.delete(prev.session_id); // the heir stamps its own floor at SessionStart
    // Wake the retired id's /api/watch long-poll so its watcher sees
    // session_alive:false and exits now instead of hanging to its timeout.
    notifyWatchers(prev.session_id);
    tick(`🧹 ${callsign} cleared its context — same card, new session id (${String(sid).slice(0, 8)})`);
    onMutate();
    return callsign;
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
  // Canonical worktree-path mutex (BUG-060): removal holds a path's claim
  // from inspection through filesystem, branch, and DB cleanup; every launch
  // or revive into that directory acquires the same claim first, so it can
  // never interleave into a removal's destructive window and launch a pane
  // whose cwd has already disappeared. One claim per canonical path, FIFO.
  const acquireWorktreePathLock = createKeyedMutex();

  const ctx = {
    // holdMs resolves WITH the settings fallback at the questions wiring above
    // (q didn't exist at parameter-default time); ctx carries that same value.
    db, port, home, holdMs: holdMs ?? resolveHoldMs(process.env, () => q.getSetting.get('hold_ms')?.value ?? null), t0, version,
    STALE_MS, NUDGE_MS, SPAWN_REGISTER_MS, SETUP_REGISTER_MS, PANE_MAIL_GRACE_MS,
    MAIL_PENDING_MAX, MAIL_PENDING_MAX_BYTES, MAIL_PANE_BATCH, MAIL_PANE_BATCH_BYTES, // BUG-128 test-only
    MAIL_CLAIM_LEASE_MS,
    PRESUME_DEAD_MS, PRESUME_DEAD_WORKING_MS, RETAIN_OFFLINE_MS, RC_HARVEST_MS, RETAIN_LEDGER_MS,
    ADOPT_ARM_MS, ADOPT_DELAY_MS, // 0.7.0 Move-to-tmux (spawns arms, events fires)
    SNAPSHOT_FILES_PER_SESSION,
    q, updateSession, onMutate, tmuxAdapter, questions, settleTerminalPlans,
    findScopedWindow, scopedPaneTarget, tick, logEvent, card, assignCallsign, applyTicket,
    modelMemo, stampTranscriptFloor, readTranscriptModel,
    // 0.7.1: naming + /clear succession, shared with events.ts (the hook-time
    // interception), commands.mjs / http.mjs (the `name` surfaces), and
    // spawns.mjs (the boot heal for pairs stranded before this shipped).
    CLEAR_SUCCESSION_MS, applyCustomName, hasLivePane,
    findClearedPredecessor, succeedSession, succeedForwardFromClear,
    acquireWorktreePathLock,
  };

  // Mail + /api/watch waiter registry + owned-pane delivery → mail.ts.
  Object.assign(ctx, createMail(ctx));
  const {
    mail, drainMail, ackMail, resolveTargets, notifyWatchers, addWatchWaiter,
    hasWatchWaiter, ownedPaneRow, ownedPaneDeliverable, tryOwnedPaneDelivery,
    claimMail, watchInfo, postMail, registerWatchGen,
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

  // Repository catalog, persisted repos-root setting, clone and branch
  // materialization. It must precede ingest/events/spawns: all three are
  // catalog writers or consumers.
  Object.assign(ctx, createRepos(ctx));
  // Whitelisted settings surface (repos_dir/repo_transport/browse_root/fav_dirs)
  // rides ON TOP of the repos catalog: createSettings reads resolveReposDir /
  // setReposDir from ctx and re-exports the `setSettings` name (so http.mjs's
  // POST route is untouched) plus resolveSettings, browseRootChoice (files.mjs)
  // and persistRepoTransport (spawns.mjs). It must precede files/spawns.
  Object.assign(ctx, createSettings(ctx));
  const { resolveReposDir, setSettings, resolveSettings, setRepoSetupEntry } = ctx;

  // File-touch ledger + conflict radar → ledger.mjs.
  Object.assign(ctx, createLedger(ctx));
  const { recordFile, whisperText } = ctx;

  // agents-cli ingest (F1) → ingest.mjs.
  Object.assign(ctx, createIngest(ctx));
  const { ingestAgentsPoll } = ctx;

  // POST /command → commands.mjs.
  Object.assign(ctx, createCommands(ctx));
  const { command } = ctx;

  // v1.3 plan library mark + BUG-039 assign → plans.mjs.
  Object.assign(ctx, createPlans(ctx));
  const { planMark, assignPlan } = ctx;

  // Worktree custody (inspection + allow-listed removal) → worktrees.mjs. The
  // custody lease below must be threaded BEFORE both worktrees and spawns are
  // built: it is the shared state removeWorktree and revive() serialize on.
  // It lives HERE, on the per-core ctx — not in helpers.mjs (whose contract is
  // closure-free purity) and not module-scoped in worktrees.mjs (module scope
  // is per-PROCESS, so two cores in one test process would share one map and
  // block each other's unrelated removals across separate databases).
  //
  // remove and revive previously had no shared path lease: every liveness
  // re-check inside remove is only a snapshot, so a revive could slip in
  // BETWEEN the pre-remove check and the awaited `git worktree remove` —
  // validate the still-existing cwd, insert its 'provisioning' row and launch
  // a pane in a directory removal was about to delete (the final gate would
  // then report success for a live card pointing at a nonexistent checkout).
  // One claim per canonical path, taken with NO await between check and set,
  // held through git removal + optional branch delete + row purge (remove) and
  // through window reconciliation + the synchronous provisional-row insert
  // (revive), makes exactly one of them win.
  const custodyLeases = new Map(); // canonical worktree path -> lease owner
  const canonicalWorktreePath = p => {
    try { return fs.realpathSync(p); } catch { return path.resolve(p); }
  };
  ctx.claimWorktreeCustody = (p, owner) => {
    const canonical = canonicalWorktreePath(p);
    if (custodyLeases.has(canonical)) return null;
    custodyLeases.set(canonical, owner);
    let released = false;
    return () => { if (!released) { released = true; custodyLeases.delete(canonical); } };
  };
  Object.assign(ctx, createWorktrees(ctx));
  const { worktrees, removeWorktree } = ctx;

  // Read-only session working-tree browsing + the global home explorer → files.mjs.
  Object.assign(ctx, createFiles(ctx));
  const { fsList, fsRead, fsSearch, fsListHome, fsReadHome, fsSearchHome } = ctx;

  // v1.2/v1.3 board-spawned session lifecycle → spawns.mjs.
  Object.assign(ctx, createSpawns(ctx));
  const {
    spawn, revive, adoptSession, enableRemote, spawnKill, spawnCapability,
    spawnLivenessTick, reconcileSpawns, reconcileClearForks,
    scheduleRegistrationRemoteHarvest, forgetSpawn, spawnState, armUnsupervised,
  } = ctx;

  // Hook state machine (applyEvent + hook endpoints + brief + plan capture) → events.ts.
  Object.assign(ctx, createEvents(ctx));
  const {
    applyEvent, hookSessionStart, hookUserPromptSubmit, hookPostToolUse,
    hookStop, hookSessionEnd, hookHoldQuestion, takeoverBriefLines,
  } = ctx;

  // /state snapshot + fleetSize + live-terminal resolver → snapshot.mjs.
  Object.assign(ctx, createSnapshot(ctx));
  const { snapshot, fleetSize, terminalSpawn } = ctx;

  // Retention sweep + manual cleanup → retention.mjs.
  Object.assign(ctx, createRetention(ctx));
  const { retentionSweep, cleanup, dismissSession, dismissRetry } = ctx;

  // Run retention once at core boot, then alongside event pruning every 10m.
  // BUG 2: retentionSweep is async now (it may probe tmux for spawned silence),
  // but with no spawned candidate it completes SYNCHRONOUSLY before returning
  // its already-resolved promise, so the boot sweep's DB effects still land
  // synchronously for the common case. .catch() contains any tmux-probe
  // rejection so a fire-and-forget sweep can never become an unhandled reject.
  const bootRetention = retentionSweep().catch(err => console.error('fleetd retention sweep error:', err));
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
    takeoverBriefLines, // 0.16.0 upgrade banner lines for the takeover SessionStart brief
    questions, // F3 relay surface: attachHold / socketClosed / answer / …
    addWatchWaiter,  // F3d-2 watch surface (GET /api/watch v2)
    // hasWatchWaiter is used only INTERNALLY (mail routing + snapshot); no
    // http.mjs/fleetd.mjs caller consumes it, so it is not re-exported here.
    claimMail,       // "
    watchInfo,       // "
    registerWatchGen, // " (BUG-105: newest-wins watcher generation for the atomic claim)
    drainMail,
    ackMail,           // POST /mail/ack — BUG-034 lease finalization
    // BUG-128: the raw internal insert is on the surface so in-memory tests
    // can queue a burst without the postMail routing/probe ceremony (commands
    // .mjs and question relays already reach it through ctx).
    mail,
    postMail,
    tryOwnedPaneDelivery,
    command,
    snapshot,
    fleetSize,
    terminalSpawn,
    pasteImage,        // POST /api/paste-image → {status, body} (stateless; paste.mjs)
    ingestAgentsPoll,
    // v1.2 dynamic fleet
    spawn,             // POST /api/spawn flow → {status, body}
    revive,            // POST /api/spawn/:id/revive → {status, body}
    adoptSession,      // POST /api/sessions/:session_id/adopt → {status, body}
    enableRemote,      // POST /api/spawn/:id/rc → {status, body}
    spawnKill,         // POST /api/spawn/:id/kill → {status, body}
    spawnCapability,   // /health + /state `spawn` object
    armUnsupervised,   // POST /api/spawn/arm-unsupervised → one-time arm token
    spawnLivenessTick, // owned-pane liveness, rides the agents-poll cadence
    reconcileSpawns,   // fleetd boot: rows ↔ tmux windows
    reconcileClearForks, // fleetd boot: heal cards split by a /clear before 0.7.1
    // retentionSweep also runs internally (boot + the 10m interval above). It
    // is re-exported so tests can drive the tmux-verified presume-dead path
    // (BUG 2) deterministically; production callers keep using the interval.
    retentionSweep,
    bootRetention,     // the boot sweep's settle promise — fleetd folds it into boot readiness
    cleanup,
    dismissSession,     // POST /api/sessions/:id/dismiss — per-card cleanup → {status, body}
    dismissRetry,       // POST /api/sessions/:id/dismiss/retry — re-attempt the window kills of a partial dismiss (BUG-145)
    worktrees,          // GET /api/worktrees — bounded live git inspection
    removeWorktree,     // POST /api/worktrees/remove — allow-listed destruction
    resolveReposDir,    // repos-root resolver (still consumed via resolveSettings)
    resolveSettings,    // GET /api/settings + POST response + /state snapshot
    setSettings,        // POST /api/settings (whitelisted; settings.mjs)
    setRepoSetupEntry,  // daemon-side per-repo setup-default writer (BUG-147)
    fsList,             // GET /api/sessions/:id/fs/list → {status, body}
    fsRead,             // GET /api/sessions/:id/fs/read → {status, body}
    fsSearch,           // GET /api/sessions/:id/fs/search → {status, body}
    fsListHome,         // GET /api/fs/list → {status, body} (home-rooted)
    fsReadHome,         // GET /api/fs/read → {status, body}
    fsSearchHome,       // GET /api/fs/search → {status, body}
    // v1.3 plan library
    planMark,          // POST /api/plans/:id/mark → {status, body}
    assignPlan,        // POST /api/plans/:id/assign → {status, body} (BUG-039)
    // 0.7.1 custom names: POST /api/sessions/:id/name and the `name` command
    // both land here (one rename write, one set of rules).
    applyCustomName,
    // Exposed for fleetd.mjs's boot banner (upgrade takeover: "vX replaced
    // vY" must reach the board feed, not just fleetd.log).
    tick,
    set onMutate(fn) { onMutateImpl = fn; },
  };
}
