// questions.mjs — F3 needs-you relay: durable question rows + hold-open
// management (F3a PermissionRequest, F3b Elicitation, F3c AskUserQuestion) +
// free-text question detection at Stop (F3d). NO model calls anywhere:
// detection is regex, relay is plumbing.
//
// Four kinds:
//   'permission'  — PermissionRequest hook held open (F3a). Answer schema
//                   (verified against the official hooks docs):
//                     {"hookSpecificOutput":{"hookEventName":"PermissionRequest",
//                      "decision":{"behavior":"allow"|"deny"}}}
//                   the docs show NO message/reason field on the decision, so
//                   deny is sent bare (the board's full answer body is still
//                   recorded in answer_json). An `updatedPermissions` field
//                   exists in the docs schema but the v1 board never emits it.
//                   v1.3: an ExitPlanMode permission question (its plan was
//                   captured at intake, derive.mjs) also accepts the
//                   board-only pseudo-behavior "capture" — wire deny + the
//                   pinned PLAN_CAPTURE_MAIL + plan status 'captured'.
//   'elicitation' — Elicitation hook held open (F3b). The response schema is
//                   NOT documented; this wires a best-effort guess
//                   ({"action":"accept","content":{...}} / {"action":"decline"})
//                   — live semantics unproven until the Phase 3 gate (unproven
//                   live — validate before relying on it).
//   'choice'      — AskUserQuestion PreToolUse hook held open (F3c, validated
//                   live on CLI 2.1.206 on 2026-07-10, experiment 1).
//                   Payload keeps tool_input.questions[] ({question, header,
//                   options:[{label,description}], multiSelect}) + tool_use_id.
//                   Board answer body: {answers:{"<question text>":"<label>"}}
//                   (mirrors the CLI's own PostToolUse `answers` map) or
//                   {text:"..."} freeform fallback. Answer resolves the held
//                   PreToolUse response to a deny-with-reason:
//                     {"hookSpecificOutput":{"hookEventName":"PreToolUse",
//                      "permissionDecision":"deny",
//                      "permissionDecisionReason":"User answered via Fleet Deck: …"}}
//                   — validated GRACEFUL (model acts on the answer, no retry,
//                   no terminal chooser). Expiry/disconnect → {} → the native
//                   terminal chooser renders normally and OWNS the question
//                   from then on (a late board answer 409s, same as the other
//                   hold kinds). Activity/SessionEnd expiry semantics are
//                   identical to 'permission' (HOLD_KINDS-driven).
//   'freeform'    — trailing question detected at a passing Stop (F3d). No
//                   held socket: the answer is MAIL from 'fleetdeck-answer',
//                   carried in by the existing turn-boundary delivery
//                   (UserPromptSubmit additionalContext or Stop block) or —
//                   Phase 4 — claimed early by GET /api/watch for the
//                   asyncRewake watcher. Stays pending until answered or
//                   SessionEnd — it's the human's queue.
//
// Hold lifecycle (permission/elicitation/choice): the HTTP layer parks the
// hook response and registers a respond() callback here. Exactly one path
// settles each hold:
//   (a) board answer  → respond(decision), status 'answered'
//   (b) hold expiry   → respond({}), status 'expired' (normal flow resumes in
//                       the terminal — {} means "no decision" per docs §4)
//   (c) client gone   → no respond, status 'expired'
// Holds are in-memory only; question ROWS are durable (SQLite). After a
// daemon restart a pending hold-kind row has no socket left — the sweep (and
// any activity event from that session) expires it, because nobody can
// deliver its answer any more. The hook side times out non-blockingly.

// v1.3 plan library (CONTRACT "B. Plan library"): the mail sent to the
// planner when the board answers its ExitPlanMode question with
// {behavior:"capture"}. Text pinned VERBATIM by the contract. Delivery is the
// ordinary mail pipeline (turn boundary, or the v1.1 mail-wake for an idle
// planner) — mail() nudges watchers on insert.
export const PLAN_CAPTURE_MAIL = '[FLEETDECK] Your plan was captured to the fleet plan library — do not execute it. Wrap up your turn.';

export const DEFAULT_HOLD_MS = 50_000;
export const MAX_HOLDS_PER_SESSION = 4;
const SWEEP_MS = 5_000;
const RESOLVED_IN_STATE = 8; // "last few resolved" in GET /state
const HOLD_KINDS = new Set(['permission', 'elicitation', 'choice']);

// FLEETDECK_HOLD_MS, default 50 s. Clamped under the 65 s hook timeout wired
// in hooks/hooks.json — the held HTTP response must come back before the hook
// client gives up, or the board's answer lands on a dead socket.
export function resolveHoldMs(env = process.env) {
  const raw = Number(env?.FLEETDECK_HOLD_MS);
  if (!Number.isFinite(raw) || raw <= 0) return DEFAULT_HOLD_MS;
  return Math.max(250, Math.min(raw, 60_000));
}

function safeParse(json) {
  try { return JSON.parse(json ?? 'null'); } catch { return null; }
}

export function createQuestions(db, {
  holdMs = DEFAULT_HOLD_MS,
  mail = () => {},
  tick = () => {},
  callsignOf = () => null,
  onChange = () => {},
  // v1.3 plan library wiring (derive.mjs owns the plans table; capture-on-
  // intake happens there, synchronously with the question row insert):
  //   planIdFor(questionId)            → plan_id | null (null = not a plan question)
  //   planAnswered(questionId, behavior) → flips the linked plan proposed →
  //       approved ('allow') | captured ('capture') | rejected ('deny')
  planIdFor = () => null,
  planAnswered = () => {},
} = {}) {
  const q = {
    insert: db.prepare(`INSERT INTO questions (session_id, kind, payload_json, status, created_at, expires_at)
      VALUES (?, ?, ?, 'pending', ?, ?)`),
    get: db.prepare('SELECT * FROM questions WHERE id = ?'),
    markAnswered: db.prepare(`UPDATE questions SET status = 'answered', answer_json = ?, answered_at = ?
      WHERE id = ? AND status = 'pending'`),
    markExpired: db.prepare(`UPDATE questions SET status = 'expired' WHERE id = ? AND status = 'pending'`),
    pending: db.prepare(`SELECT * FROM questions WHERE status = 'pending' ORDER BY id`),
    pendingBySession: db.prepare(`SELECT * FROM questions WHERE status = 'pending' AND session_id = ? ORDER BY id`),
    resolved: db.prepare(`SELECT * FROM questions WHERE status != 'pending'
      ORDER BY COALESCE(answered_at, expires_at, created_at) DESC, id DESC LIMIT ${RESOLVED_IN_STATE}`),
  };

  const holds = new Map(); // question id -> { session_id, respond, timer }

  // -------------------------------------------------------------- creation
  function create(kind, sessionId, payload) {
    const now = Date.now();
    const expiresAt = HOLD_KINDS.has(kind) ? now + holdMs : null; // freeform: no hold window
    const info = q.insert.run(sessionId ?? 'unknown', kind, JSON.stringify(payload ?? {}), now, expiresAt);
    return q.get.get(Number(info.lastInsertRowid));
  }

  // ----------------------------------------------------------------- holds
  // Register the parked HTTP response for a freshly created hold-kind row.
  // Called synchronously in the same tick as create() (no timer can
  // interleave), so a pending hold-kind row without a holds entry is always
  // an orphan (restart/disconnect), never a race.
  function attachHold(row, respond) {
    // Held responses must not leak sockets: cap concurrent holds per session
    // at MAX_HOLDS_PER_SESSION; the OLDEST is failed open ({}) to make room.
    const mine = [...holds.keys()]
      .filter(id => holds.get(id).session_id === row.session_id)
      .sort((a, b) => a - b);
    if (mine.length >= MAX_HOLDS_PER_SESSION) settleExpired(mine[0]);

    const timer = setTimeout(() => settleExpired(row.id), Math.max(0, row.expires_at - Date.now()));
    timer.unref?.();
    holds.set(row.id, { session_id: row.session_id, respond, timer });
  }

  function releaseHold(id) {
    const h = holds.get(id);
    if (!h) return null;
    clearTimeout(h.timer);
    holds.delete(id);
    return h;
  }

  // Path (b): hold window lapsed (timer) or evicted by the per-session cap —
  // answer the held request with {} so the normal flow resumes in the
  // terminal, and mark the row expired.
  function settleExpired(id) {
    const h = releaseHold(id);
    if (h) { try { h.respond({}); } catch { /* socket already gone */ } }
    if (q.markExpired.run(id).changes) {
      tick(`⌛ question #${id} expired unanswered — decide in the terminal`);
      onChange();
    }
  }

  // Path (c): the hook client disconnected before we responded. 'close' also
  // fires after a NORMAL completion, so only act while the hold still exists.
  function socketClosed(id) {
    if (!holds.has(id)) return;
    releaseHold(id);
    if (q.markExpired.run(id).changes) onChange();
  }

  // --------------------------------------------------------------- answers
  // POST /api/questions/:id/answer body per kind:
  //   permission:  {behavior:"allow"|"deny"} — plus, for an ExitPlanMode plan
  //                question ONLY, the v1.3 board-only pseudo-behavior
  //                {behavior:"capture"}: the held hook gets a verified-schema
  //                bare deny, the planner gets PLAN_CAPTURE_MAIL from
  //                'fleetdeck', and the linked plan flips to 'captured'.
  //   elicitation: {action:"accept", content:{...}} | {action:"decline"}
  //   choice:      {answers:{"<question text>":"<label>"}} | {text:"..."}
  //   freeform:    {text:"..."}
  // Returns { status, body } for the HTTP layer.
  function answer(id, body) {
    const row = q.get.get(Number(id));
    if (!row) return { status: 404, body: { ok: false, err: 'no such question' } };
    if (row.status !== 'pending') return { status: 409, body: { ok: false, err: `question already ${row.status}` } };
    const now = Date.now();
    const who = callsignOf(row.session_id) || row.session_id;

    if (HOLD_KINDS.has(row.kind)) {
      let hookResponse;
      let detail;
      let planBehavior = null; // v1.3: set on a plan question's allow/capture/deny
      if (row.kind === 'permission') {
        const behavior = body?.behavior;
        const planId = planIdFor(row.id); // non-null only for an ExitPlanMode plan question
        if (behavior === 'capture' && planId == null) {
          return { status: 400, body: { ok: false, err: '"capture" is only valid for an ExitPlanMode plan question' } };
        }
        if (behavior !== 'allow' && behavior !== 'deny' && behavior !== 'capture') {
          return {
            status: 400,
            body: {
              ok: false,
              err: planId != null
                ? 'body must be {"behavior":"allow"|"capture"|"deny"}'
                : 'body must be {"behavior":"allow"|"deny"}',
            },
          };
        }
        // Verified schema (per the official hooks docs); no message/reason field documented.
        // 'capture' answers the held hook with a bare deny (CONTRACT v1.3) —
        // the pseudo-behavior never reaches the hook client.
        const wire = behavior === 'capture' ? 'deny' : behavior;
        hookResponse = { hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior: wire } } };
        detail = behavior;
        if (planId != null) planBehavior = behavior;
      } else if (row.kind === 'choice') {
        const serialized = serializeChoiceAnswer(row, body);
        if (!serialized) {
          return { status: 400, body: { ok: false, err: 'body must be {"answers":{"<question text>":"<label>"}} or {"text":"..."}' } };
        }
        // Validated schema + wording (validated live on CLI 2.1.206, exp. 1b): a
        // PreToolUse deny with this reason frame is honored GRACEFULLY — the
        // model proceeds with the relayed answer, no retry, no terminal
        // chooser. The reason renders as an `Error:` line in the terminal,
        // so it must read well in that frame.
        hookResponse = {
          hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: 'deny',
            permissionDecisionReason: `User answered via Fleet Deck: ${serialized}`,
          },
        };
        detail = serialized.length > 60 ? serialized.slice(0, 57) + '…' : serialized;
      } else {
        const action = body?.action;
        if (action !== 'accept' && action !== 'decline') {
          return { status: 400, body: { ok: false, err: 'body must be {"action":"accept","content":{...}} or {"action":"decline"}' } };
        }
        // UNVERIFIED schema — handoff guess, pending the Phase 3 live gate.
        hookResponse = action === 'accept'
          ? { action: 'accept', content: body?.content ?? {} }
          : { action: 'decline' };
        detail = action;
      }
      const h = releaseHold(row.id);
      if (!h) {
        // The held socket is gone (expired, client vanished, or daemon
        // restarted mid-hold): the decision cannot reach the session.
        if (q.markExpired.run(row.id).changes) onChange();
        return { status: 409, body: { ok: false, err: 'hold expired — the terminal prompt owns this decision now' } };
      }
      try { h.respond(hookResponse); } catch { /* socket died as we answered */ }
      q.markAnswered.run(JSON.stringify(body ?? {}), now, row.id);
      // v1.3 plan side effects — only after the hold actually settled (an
      // expired hold 409s above and the plan stays 'proposed', per contract):
      // allow → approved, capture → captured (+ the pinned mail), deny →
      // rejected. Hold expiry never lands here.
      if (planBehavior) {
        planAnswered(row.id, planBehavior);
        if (planBehavior === 'capture') mail(row.session_id, 'fleetdeck', PLAN_CAPTURE_MAIL);
      }
      tick(`✅ ${who}: ${row.kind} answered from the board (${detail})`);
      onChange();
      return { status: 200, body: { ok: true, delivered: true } };
    }

    if (row.kind === 'freeform') {
      const text = String(body?.text ?? '').trim();
      if (!text) return { status: 400, body: { ok: false, err: 'body must be {"text":"..."}' } };
      const questionText = String(safeParse(row.payload_json)?.text ?? '').slice(0, 80);
      // Turn-boundary delivery — the PROVEN mechanism (existing mail pipeline:
      // UserPromptSubmit additionalContext or Stop block). No new delivery
      // mechanisms here; asyncRewake is Phase 4.
      mail(row.session_id, 'fleetdeck-answer', `[FLEETDECK ANSWER] Q: ${questionText} — A: ${text}`);
      q.markAnswered.run(JSON.stringify({ text }), now, row.id);
      tick(`💬 answer for ${who} queued — lands at next turn boundary`);
      onChange();
      return { status: 200, body: { ok: true, delivered: false, note: 'answer queued — delivered at next turn boundary' } };
    }

    return { status: 400, body: { ok: false, err: `unknown question kind ${row.kind}` } };
  }

  // ------------------------------------------------------------ resolution
  // F3e auto-resolution: activity (UserPromptSubmit/PostToolUse) from a
  // session means it is not waiting on a permission/elicitation any more —
  // settle every pending hold-kind question it has: live holds are failed
  // open ({} → the terminal flow owns the decision) without waiting out the
  // window; hold-less rows (window lapsed, client gone, daemon restarted
  // mid-hold) just flip to expired.
  //
  // FREEFORM ROWS EXPIRE HERE TOO, and that is a correction, not a shortcut.
  // A freeform question is raised at a Stop — the session is idle, waiting on
  // an answer. Activity after that means it is NOT waiting any more: the human
  // walked over and answered in the terminal (or in the board's live terminal
  // modal), and the session moved on. Leaving the row pending left a NEEDS YOU
  // card on the board forever, for a question that was answered ten minutes ago
  // in another window — the rail filled with ghosts and stopped meaning
  // anything. The board must reflect what is actually still owed.
  //
  // The resume path is untouched: expireAllForSession (SessionEnd) still spares
  // freeform, so an ENDED session's question survives for `claude --resume`.
  // Only a session that demonstrably kept going clears its own queue.
  //
  // Watch item for live acceptance: if 2.1.206 ever fires PermissionRequest
  // hooks for PARALLEL tool calls, the first allowed tool's PostToolUse would
  // fail the sibling holds open here (unproven live — validate before relying
  // on it).
  function expireOnActivity(sessionId) {
    let changed = false;
    for (const r of q.pendingBySession.all(sessionId)) {
      const h = releaseHold(r.id);
      if (h) { try { h.respond({}); } catch { /* socket already gone */ } }
      if (q.markExpired.run(r.id).changes) changed = true;
    }
    if (changed) onChange();
    return changed;
  }

  // "Clear" on the board: answered, expired and dismissed cards leave the rail
  // for good. PENDING rows are the human's actual queue and are never touched
  // here — Clear tidies the past, it does not silence the present.
  function purgeResolved() {
    const out = db.prepare("DELETE FROM questions WHERE status != 'pending'").run();
    if (out.changes) onChange();
    return out.changes;
  }

  // The human already handled it elsewhere: retire the card, tell the session
  // nothing. Distinct from answering (which mails the answer to the session)
  // and from expiring a hold (which must fail a parked socket open).
  function dismiss(id) {
    const row = q.get.get(id);
    if (!row) return { ok: false, reason: 'no such question' };
    if (row.status !== 'pending') return { ok: true, already: true };
    const h = releaseHold(row.id);
    if (h) { try { h.respond({}); } catch { /* socket already gone */ } }
    const changed = q.markExpired.run(row.id).changes > 0;
    if (changed) onChange();
    return { ok: true, callsign: row.callsign ?? null };
  }

  // Restart hygiene (periodic sweep): a pending hold-kind row with NO live
  // hold can never deliver an answer — expire it. Never touches live holds.
  function expireOrphans() {
    let changed = false;
    for (const r of q.pending.all()) {
      if (HOLD_KINDS.has(r.kind) && !holds.has(r.id)) {
        if (q.markExpired.run(r.id).changes) changed = true;
      }
    }
    if (changed) onChange();
    return changed;
  }

  // SessionEnd: pending HOLD-kind questions die with the session (their held
  // sockets are moot), but freeform questions SURVIVE — an ended session is
  // resumable (`claude --resume`), the answer is delivered as mail at the
  // resumed session's first turn boundary, and the question is the human's
  // queue item, not the session's. (Proven live in the Phase 3 acceptance:
  // expiring freeform at SessionEnd orphaned the answer and the resumed
  // session went hunting for it through the board API.)
  // includeFreeform is the one sanctioned exception: manual cleanup archiving
  // a card is the human declaring "done with these" — its freeform items go too.
  // Returns the number of questions expired (truthy iff anything changed).
  function expireAllForSession(sessionId, { includeFreeform = false } = {}) {
    let expired = 0;
    for (const r of q.pendingBySession.all(sessionId)) {
      if (!includeFreeform && !HOLD_KINDS.has(r.kind)) continue;
      const h = releaseHold(r.id);
      if (h) { try { h.respond({}); } catch { /* gone */ } }
      if (q.markExpired.run(r.id).changes) expired++;
    }
    if (expired) onChange();
    return expired;
  }

  function pendingOf(sessionId) {
    return q.pendingBySession.all(sessionId);
  }

  // -------------------------------------------------------------- snapshot
  // GET /state `questions`: all pending + the last few resolved, with enough
  // for the board to render countdowns (expires_at) and disable dead cards
  // (`held` false on a pending hold-kind row = restart orphan, sweep-bound).
  // v1.3: an ExitPlanMode question carries `plan_id` (its captured plan) so
  // the board can render it as a PLAN card and offer Approve / Capture &
  // release / Deny.
  function listForState() {
    return [...q.pending.all(), ...q.resolved.all()].map(r => {
      const plan_id = planIdFor(r.id);
      return {
        id: r.id,
        kind: r.kind,
        session_id: r.session_id,
        callsign: callsignOf(r.session_id),
        payload: safeParse(r.payload_json),
        status: r.status,
        created_at: r.created_at,
        expires_at: r.expires_at,
        answered_at: r.answered_at,
        answer: safeParse(r.answer_json),
        held: holds.has(r.id),
        ...(plan_id != null ? { plan_id } : {}),
      };
    });
  }

  // Orphan sweep (restart hygiene). Live holds settle via their own timers.
  const sweep = setInterval(() => {
    try { expireOrphans(); } catch { /* hygiene only */ }
  }, SWEEP_MS);
  sweep.unref();

  return {
    holdMs,
    create,
    attachHold,
    socketClosed,
    answer,
    dismiss,
    purgeResolved,
    expireOnActivity,
    expireOrphans,
    expireAllForSession,
    pendingOf,
    listForState,
  };
}

// --------------------------------------------------------------------------
// F3c choice-answer serialization (pure; exported for tests)
// --------------------------------------------------------------------------
// Compacts a board answer body into the deny-reason tail. The frame the live
// validation proved graceful is "User answered via Fleet Deck: <answer>", so
// the serialization must stay SHORT and read well after an `Error:` prefix:
//   {text:"..."}                             → the text itself
//   {answers:{q:"argon2"}} (single entry)    → "argon2"
//   {answers:{q1:"a", q2:["x","y"]}} (multi) → "<header-or-question>: a; <…>: x, y"
// Answer keys are QUESTION TEXTS (the CLI's own PostToolUse `answers` map
// format); for multi-question calls each is swapped for the question's
// shorter `header` when the payload lets us match it. Values may be a string
// or an array of labels (multiSelect). Returns null when the body carries
// nothing usable — the HTTP layer turns that into a 400.
export function serializeChoiceAnswer(row, body) {
  if (typeof body?.text === 'string' && body.text.trim()) return clipQuestion(body.text.trim());
  const answers = body?.answers;
  if (!answers || typeof answers !== 'object' || Array.isArray(answers)) return null;
  const fmt = v => (Array.isArray(v) ? v.map(x => String(x)).join(', ') : String(v ?? '')).trim();
  const entries = Object.entries(answers).filter(([, v]) => fmt(v) !== '');
  if (!entries.length) return null;
  if (entries.length === 1) return clipQuestion(fmt(entries[0][1]));
  const qs = safeParse(row?.payload_json)?.tool_input?.questions;
  const headerOf = qText => (Array.isArray(qs) ? qs.find(x => x?.question === qText)?.header : null);
  return clipQuestion(entries.map(([qText, v]) => `${headerOf(qText) || qText}: ${fmt(v)}`).join('; '));
}

// --------------------------------------------------------------------------
// F3d detection helpers (pure functions; exported for tests)
// --------------------------------------------------------------------------

export const CHOICE_RE = /\b(should I|do you want|would you like|which|prefer|option [AB1-9]|let me know)\b/i;

// Regex heuristic, NO model call. A trailing question is:
//   (1) the last non-empty line of the final paragraph ends with '?'
//       (allowing closing markdown/quotes/brackets after it), or
//   (2) the final paragraph matches a choice pattern (CHOICE_RE) AND
//       contains a '?' anywhere.
// Returns the question snippet, or null.
export function detectTrailingQuestion(text) {
  const trimmed = String(text ?? '').trim();
  if (!trimmed) return null;
  const paras = trimmed.split(/\n[ \t]*\n/);
  const lastPara = (paras[paras.length - 1] || '').trim();
  const lines = lastPara.split('\n').map(l => l.trim()).filter(Boolean);
  const lastLine = lines[lines.length - 1] || '';
  const TRAILING_Q = /\?[\s"'*_)\]`.]*$/;

  if (TRAILING_Q.test(lastLine)) return clipQuestion(lastLine);
  if (lastPara.includes('?') && CHOICE_RE.test(lastPara)) {
    return clipQuestion(sentenceWithLastQuestionMark(lastPara));
  }
  return null;
}

function sentenceWithLastQuestionMark(para) {
  const idx = para.lastIndexOf('?');
  if (idx === -1) return para;
  let start = 0;
  for (let i = idx - 1; i >= 0; i--) {
    const ch = para[i];
    if (ch === '.' || ch === '!' || ch === '?' || ch === '\n') { start = i + 1; break; }
  }
  return para.slice(start, idx + 1).trim();
}

function clipQuestion(s) {
  const t = String(s).trim();
  return t.length <= 300 ? t : t.slice(0, 297) + '…';
}
