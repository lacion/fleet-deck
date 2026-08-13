// questions.ts — F3 needs-you relay: durable question rows + hold-open
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
//
// UX 2.1 adds life after (b): the expiry does not mean the question went away —
// the agent is parked on its NATIVE terminal prompt. After a short grace with
// no session activity (REARM_GRACE_MS, still-parked confirmation), the daemon
// raises a FRESH row for the same question, flagged payload.rearmed — NOT a
// hold (the hook socket is gone; re-parking is impossible). Its answer rides
// the mail pipeline from 'fleetdeck-answer' and lands at the next turn
// boundary, and the card copy says exactly that. Chains cap at MAX_REARMS and
// ANY session activity (the same signal expireOnActivity consumes) stops the
// chain permanently. Nothing on this path ever auto-answers a question —
// expiry still fails open to the terminal exactly as before.
//
// Plan-linked retirement seam (plan lifecycle contract): EVERY path that
// retires a PENDING row WITHOUT a board answer (hold timer, turn boundary,
// correlated PostToolUse, dismiss, orphan sweep, SessionEnd, the
// dead-hold 409 in answer()) also fires the onRetired ctx callback with the
// just-expired row, so derive.mjs can reconcile the linked plan. A
// retire-that-IS-activity (the turn-boundary path) settles the plan to
// 'handled-in-terminal' in the same tick; a bare timer expiry does NOT — a
// planner killed mid-hold must never be marked. See planRetired in
// derive.mjs for the full gate.
// Holds are in-memory only; question ROWS are durable (SQLite). After a
// daemon restart a pending hold-kind row has no socket left — the sweep (and
// any activity event from that session) expires it, because nobody can
// deliver its answer any more. The hook side times out non-blockingly.

import type { SqliteHandle } from './sqlite.ts';

// The durable `questions` row (db.ts schema). create() always writes
// session_id/kind/status/created_at non-null, so they are typed non-nullable;
// the remaining columns are the ones the schema deliberately leaves NULL
// (freeform has no hold deadline; a pending row has no answer yet).
interface QuestionRow {
  id: number;
  session_id: string;
  kind: string;
  payload_json: string | null;
  status: string;
  answer_json: string | null;
  created_at: number;
  expires_at: number | null;
  answered_at: number | null;
}

// An AskUserQuestion choice option / question, as parsed back out of a stored
// hold payload. Every field is optional and array elements are nullable: the
// payload is untrusted parsed JSON (a malformed or stale client is exactly what
// validChoiceAnswers guards against), so the runtime shape checks below stay
// honest rather than trusting the type.
interface ChoiceOption {
  label?: string;
}
interface ChoiceQuestion {
  question?: string;
  header?: string;
  options?: (ChoiceOption | null)[];
  multiSelect?: boolean;
}

// The parsed payload_json of a question row. Only the fields this module reads
// are named; the raw hook payload carries more (tool_use_id, etc.) that a spread
// preserves at runtime and structural typing ignores. All optional — a freeform
// row carries only {text}, a re-armed row adds rearmed/chain_root, a hold row
// carries the hook's tool_name/tool_input.
interface QuestionPayload {
  rearmed?: boolean;
  chain_root?: number;
  rearm_pending?: boolean;
  tool_name?: string;
  tool_input?: { questions?: (ChoiceQuestion | null)[] };
  text?: string;
}

// The parsed POST /answer body. Fields arrive off the wire, so each is optional
// and validated at the point of use; `content`/`answers` stay `unknown` because
// their shape is kind-specific and checked there.
interface AnswerBody {
  behavior?: string;
  action?: string;
  content?: unknown;
  text?: string;
  answers?: unknown;
}

// The daemon-authorized mail sender threaded in by derive.mjs.
type MailFn = (to: string, from: string, body: string) => void;

// The createQuestions options bag. Every field is optional with a default; the
// callbacks are the daemon-core seams (mail, plan wiring, board nudges) the
// factory closes over. Documented inline at the destructure below.
interface QuestionsOptions {
  holdMs?: number;
  mail?: MailFn;
  mailMaxLen?: number;
  tick?: (message: string) => void;
  callsignOf?: (sessionId: string) => string | null;
  onChange?: () => void;
  planIdFor?: (questionId: number) => number | null;
  planAnswered?: (questionId: number, behavior: string) => void;
  // A wired resolver may return a bad value in JS; the nullable return keeps the
  // `?? holdMs` fallback in create() honest (and non-redundant).
  resolveHoldWindow?: (() => number | null | undefined) | null;
  onRetired?: (row: QuestionRow | undefined, opts?: { activity?: boolean }) => void;
  rearmGraceMs?: number;
  rearmMax?: number;
}

// v1.3 plan library (CONTRACT "B. Plan library"): the mail sent to the
// planner when the board answers its ExitPlanMode question with
// {behavior:"capture"}. Text pinned VERBATIM by the contract. Delivery is the
// ordinary mail pipeline (turn boundary, or the v1.1 mail-wake for an idle
// planner) — mail() nudges watchers on insert.
const PLAN_CAPTURE_MAIL =
  '[FLEETDECK] Your plan was captured to the fleet plan library — do not execute it. Wrap up your turn.';

const DEFAULT_HOLD_MS = 600_000; // 10 min — an operator running a fleet answers on their own clock
const MAX_HOLDS_PER_SESSION = 4;
const SWEEP_MS = 5_000;
const RESOLVED_IN_STATE = 8; // "last few resolved" in GET /state
const HOLD_KINDS = new Set<string>(['permission', 'elicitation', 'choice']);
// UX 2.1 re-arm: when a hold expires with no answer, the agent is still parked
// on its NATIVE terminal prompt — the board card went dead, but the question
// hasn't gone anywhere. After a short still-parked grace (no activity from the
// session) the daemon raises a FRESH row whose answer rides the mail pipeline
// to the next turn boundary.
const REARM_GRACE_MS = 3_000; // parked-on-native-prompt confirmation window
// BUG-138: a board-answered hold leaves a short-lived completed-correlation
// record so its OWN completing PostToolUse is consumed against the ledger
// instead of expiring a still-pending identical twin (details on the ledger
// below). A real completion follows its answer by seconds at most; the TTL
// only bounds a false-positive window for a LATER identical call — a
// completed call never legitimately sends a second PostToolUse.
const COMPLETED_KEY_TTL_MS = 60_000;
// WHY a cap, and why 2: an un-rearmable dead card was the bug being fixed,
// but an agent parked behind a stack of questions (or a session nobody ever
// answers) must not re-raise cards forever — the rail filling with ghosts is
// the failure mode the freeform-expiry comment below already names. Two re-arms
// give the human three total chances (~30 min at the 600 s default) and then
// the daemon gets out of the way permanently — any activity ALSO stops it.
const MAX_REARMS = 2;

// A mail body / question snippet may arrive as `unknown` off the wire or out of
// untrusted parsed JSON. Faithful to the .mjs `String(x ?? '')`: null/undefined
// -> '', a string passes through, any other value takes its default
// stringification. Centralizes the one place a base-to-string coercion is
// intentional so the strict linter's guard stays scoped to it.
function asText(value: unknown): string {
  if (value == null) return '';
  if (typeof value === 'string') return value;
  // eslint-disable-next-line @typescript-eslint/no-base-to-string -- intentional String() coercion of untrusted input, matching the pre-migration .mjs behavior
  return String(value);
}

// FLEETDECK_HOLD_MS → the hold_ms SETTING (settings.mjs) → the 600 s default.
// THE LOCKSTEP INVARIANT: the daemon's hold window must stay under the shim
// watchdog (scripts/fleet-hook.mjs WATCHDOG_MS, 660 s for hold events), which
// must stay under the hooks.json `timeout` for the three hold hooks (720 s) —
// otherwise the board's answer lands on a dead socket and the hook fails open.
// Old-plugin/new-daemon installs (a 65 s or 120 s hook timeout with a 600 s
// daemon hold) fail OPEN the same way they always did: the shim's own
// watchdog answers {} and the terminal prompt owns the decision; the re-arm
// card is the recovery path. The env var is the OVERRIDE (an operator's
// deliberate choice may sit above the setting); the setting row arrives via
// `fallback` so questions.ts never has to know about the settings table.
export function resolveHoldMs(
  env: NodeJS.ProcessEnv | null = process.env,
  // The fallback reads a SETTING row, which comes back as a string (the hold_ms
  // k/v value) or null — the `Number(fallback?.())` below coerces it, so the
  // param type admits string as well as the pre-typed number. settings.ts's
  // resolveHoldMsRaw (a string-returning reader) is the real caller.
  fallback: (() => number | string | null | undefined) | null = null,
): number {
  const raw = Number(env?.['FLEETDECK_HOLD_MS']);
  if (Number.isFinite(raw) && raw > 0) {
    // 650 s ceiling keeps the resolved window under the 660 s shim watchdog —
    // the lockstep invariant above, enforced at the door.
    return Math.max(250, Math.min(raw, 650_000));
  }
  const stored = Number(fallback?.());
  if (Number.isFinite(stored) && stored > 0) return Math.max(250, Math.min(stored, 650_000));
  return DEFAULT_HOLD_MS;
}

// eslint-disable-next-line @typescript-eslint/no-unnecessary-type-parameters -- the generic centralizes the JSON.parse cast in one place so each call site reads `safeParse<Shape>(json)` instead of a bare `as`; it is a single-use param by design
function safeParse<T = unknown>(json: string | null | undefined): T | null {
  try {
    return JSON.parse(json ?? 'null') as T;
  } catch {
    return null;
  }
}

// Stable, order-independent JSON: the same value always yields the same string
// regardless of key insertion order, so two tool_inputs that are deeply equal
// compare equal even if a round-trip through SQLite reordered their keys.
function stableStringify(v: unknown): string {
  if (v === null || typeof v !== 'object') return JSON.stringify(v ?? null);
  if (Array.isArray(v)) return '[' + v.map(stableStringify).join(',') + ']';
  return (
    '{' +
    Object.keys(v)
      .sort()
      .map((k) => JSON.stringify(k) + ':' + stableStringify((v as Record<string, unknown>)[k]))
      .join(',') +
    '}'
  );
}

// The identity a held PermissionRequest and its completing PostToolUse actually
// share. NOT tool_use_id — real payloads omit it entirely (verified against
// tests/fixtures/post-tool-use-*.json and permission-request.json) — but the
// tool NAME plus its exact INPUT, which Claude Code sends verbatim on both the
// permission check and the tool's own PostToolUse. Two parallel calls of the
// same tool with different input get different keys, so completing one never
// retires the other's hold.
function toolCallKey(toolName: unknown, toolInput: unknown): string {
  // '\x00' (NUL) separator: it cannot appear in a tool name or in JSON output,
  // so the name/input boundary is unambiguous. Written as an ESCAPE, not a raw
  // NUL byte — a literal NUL makes the whole source read as binary, so grep and
  // audits silently skip the file. The runtime value is byte-identical (U+0000).
  return asText(toolName) + '\x00' + stableStringify(toolInput ?? null);
}

// Timer handle + the two in-memory hold/re-arm entry shapes the factory closes
// over. A re-arm entry's timer is null once the grace window fired and the
// entry survives only as a successor link.
type Timer = ReturnType<typeof setTimeout>;
type RespondFn = (response: unknown) => void;
interface HoldEntry {
  session_id: string;
  respond: RespondFn;
  timer: Timer;
}
interface RearmEntry {
  timer: Timer | null;
  chainRoot: number;
  armedAt?: number;
  successor?: number;
}

export function createQuestions(
  db: SqliteHandle,
  {
    holdMs = DEFAULT_HOLD_MS,
    mail = () => {
      /* no-op default */
    },
    // The mailbox's own clamp (mail.ts MAIL_MAX_LEN). Framed answers that exceed
    // it are REJECTED before settlement — see answerMailGuard below. Injectable
    // for tests; must mirror the real mailbox clamp in production (derive.mjs).
    mailMaxLen = 4000,
    tick = () => {
      /* no-op default */
    },
    callsignOf = () => null,
    onChange = () => {
      /* no-op default */
    },
    // v1.3 plan library wiring (derive.mjs owns the plans table; capture-on-
    // intake happens there, synchronously with the question row insert):
    //   planIdFor(questionId)            → plan_id | null (null = not a plan question)
    //   planAnswered(questionId, behavior) → flips the linked plan proposed →
    //       approved ('allow') | captured ('capture') | rejected ('deny')
    planIdFor = () => null,
    planAnswered = () => {
      /* no-op default */
    },
    // UX 2.1: live per-creation hold-window resolution. When set, create() calls
    // it for EVERY new hold-kind row, so a hold_ms settings write steers new
    // holds immediately instead of waiting out a daemon boot. Falls back to the
    // boot-resolved holdMs when unset (tests, bare createQuestions callers).
    resolveHoldWindow = null,
    // Plan-linked retirement seam: fired with the freshly-expired row by EVERY
    // path that retires a pending question WITHOUT a board answer. No-op for
    // non-plan questions (derive.mjs's planRetired finds no linked plan row and
    // returns). Deliberately fired AFTER the row is expired, outside any
    // transaction — a callback throw must never un-retire the question.
    onRetired = () => {
      /* no-op default */
    },
    // UX 2.1 re-arm knobs. rearmGraceMs is injectable for tests; 0 (or negative)
    // DISABLES the whole re-arm machinery — an expiry then behaves exactly as it
    // did before 2.1.
    rearmGraceMs = REARM_GRACE_MS,
    rearmMax = MAX_REARMS,
  }: QuestionsOptions = {},
) {
  const q = {
    insert:
      db.prepare(`INSERT INTO questions (session_id, kind, payload_json, status, created_at, expires_at)
      VALUES (?, ?, ?, 'pending', ?, ?)`),
    get: db.prepare<QuestionRow>('SELECT * FROM questions WHERE id = ?'),
    markAnswered:
      db.prepare(`UPDATE questions SET status = 'answered', answer_json = ?, answered_at = ?
      WHERE id = ? AND status = 'pending'`),
    markExpired: db.prepare(
      `UPDATE questions SET status = 'expired' WHERE id = ? AND status = 'pending'`,
    ),
    pending: db.prepare<QuestionRow>(
      `SELECT * FROM questions WHERE status = 'pending' ORDER BY id`,
    ),
    pendingBySession: db.prepare<QuestionRow>(
      `SELECT * FROM questions WHERE status = 'pending' AND session_id = ? ORDER BY id`,
    ),
    resolved: db.prepare<QuestionRow>(`SELECT * FROM questions WHERE status != 'pending'
      ORDER BY COALESCE(answered_at, expires_at, created_at) DESC, id DESC LIMIT ${RESOLVED_IN_STATE}`),
  };

  const holds = new Map<number, HoldEntry>(); // question id -> { session_id, respond, timer }
  // UX 2.1 re-arm ephemera (in-memory only — the same "a dead socket can never
  // be re-parked" reasoning that keeps holds in-memory; a daemon restart simply
  // forfeits any pending grace window, which fails safe to the pre-2.1 state):
  //   rearmById   — expired row id -> { timer, chainRoot, armedAt, successor? };
  //                 a LIVE timer means a grace window is armed. After the grace
  //                 fires, the entry stays as a link to the successor row, so
  //                 answering the successor cancels the chain through either id
  //   rearmMeta   — re-armed successor row id -> { sourceId } (the back-link)
  //   rearmChains — chain root id -> re-arm count so far (the MAX_REARMS cap)
  const rearmById = new Map<number, RearmEntry>();
  const rearmMeta = new Map<number, { sourceId: number }>();
  const rearmChains = new Map<number, number>();
  // BUG-138: completed-correlation ledger (in-memory — the same daemon-lifetime
  // ephemera class as `holds`; a daemon restart abandons every held socket
  // anyway, so the hook side can no longer complete through us). Identity:
  //   session_id -> Map<toolCallKey, count>   (count = board answers on that key)
  // WHY: with two parallel holds on IDENTICAL (tool_name, tool_input), answering
  // A retires its row to 'answered' — and expireOnActivity searches PENDING rows
  // only, so A's completing PostToolUse then sees only twin B and expires it,
  // even though B is still parked on the human. Recording each answered hold's
  // key here lets a correlated completion be consumed against the ledger FIRST
  // (each answer absorbs exactly one completion), so a twin's hold survives its
  // sibling's completion. Answered-in-terminal completions are unaffected: they
  // left no ledger entry and retire a matching pending hold exactly as before.
  const completedKeys = new Map<string, Map<string, number>>();

  // -------------------------------------------------------------- creation
  function create(
    kind: string,
    sessionId: string | null | undefined,
    payload?: unknown,
  ): QuestionRow {
    const now = Date.now();
    // The window resolves per creation when a resolver is wired: the hold_ms
    // settings row can change between daemon boots (settings.mjs), and a hold
    // parks with the window current at ITS birth. The resolver owns the clamp;
    // a throwing one degrades to the boot window rather than failing a hook.
    let windowMs = holdMs;
    if (HOLD_KINDS.has(kind) && resolveHoldWindow) {
      try {
        windowMs = resolveHoldWindow() ?? holdMs;
      } catch {
        windowMs = holdMs;
      }
    }
    // A RE-ARMED row gets NO expiry deadline: it is not a hold — there is no
    // parked socket to fail open and nothing about it expires on a timer (its
    // own grace chain is daemon ephemera, keyed off the source row). Stamping
    // one would make the board countdown promise a fail-open that never comes.
    const isRearm =
      typeof payload === 'object' &&
      payload !== null &&
      'rearmed' in payload &&
      payload.rearmed === true;
    const expiresAt = HOLD_KINDS.has(kind) && !isRearm ? now + windowMs : null; // freeform: no hold window
    const info = q.insert.run(
      sessionId ?? 'unknown',
      kind,
      JSON.stringify(payload ?? {}),
      now,
      expiresAt,
    );
    const row = q.get.get(Number(info.lastInsertRowid));
    // Impossible in practice — a same-tick read of the row we just inserted —
    // but honest: the caller (fireRearm, derive.mjs) uses the returned id, so a
    // vanished row must throw here rather than surface as `undefined.id`.
    if (!row) throw new Error('fleetd: question row vanished immediately after insert');
    return row;
  }

  // ----------------------------------------------------------------- holds
  // Register the parked HTTP response for a freshly created hold-kind row.
  // Called synchronously in the same tick as create() (no timer can
  // interleave), so a pending hold-kind row without a holds entry is always
  // an orphan (restart/disconnect), never a race.
  function attachHold(row: QuestionRow, respond: RespondFn) {
    // Held responses must not leak sockets: cap concurrent holds per session
    // at MAX_HOLDS_PER_SESSION; the OLDEST is failed open ({}) to make room.
    const mine = [...holds.keys()]
      .filter((id) => holds.get(id)?.session_id === row.session_id)
      .sort((a, b) => a - b);
    if (mine.length >= MAX_HOLDS_PER_SESSION) {
      const oldest = mine[0];
      if (oldest !== undefined) settleExpired(oldest);
    }

    // WHY the timer owns a second fail-open path: settleExpired deliberately
    // releases the HTTP response before touching SQLite, but persistence can
    // still throw (SQLITE_BUSY/IOERR), and an exception escaping a timer kills
    // the daemon. The catch repeats release/respond defensively in case a
    // future edit adds a throwing operation before settleExpired's release.
    const timer = setTimeout(
      () => {
        try {
          settleExpired(row.id);
        } catch (err) {
          const h = releaseHold(row.id);
          if (h) {
            try {
              h.respond({});
            } catch {
              /* socket already gone */
            }
          }
          console.error(`fleetd question #${row.id} expiry persistence error:`, err);
        }
      },
      Math.max(0, (row.expires_at ?? Date.now()) - Date.now()),
    );
    timer.unref();
    holds.set(row.id, { session_id: row.session_id, respond, timer });
  }

  function releaseHold(id: number): HoldEntry | null {
    const h = holds.get(id);
    if (!h) return null;
    clearTimeout(h.timer);
    holds.delete(id);
    return h;
  }

  // -------------------------------------------- BUG-138: completion ledger
  // noteCompleted / consumeCompleted implement the completedKeys ledger above.
  // TTL'd entries: the timer deletes ONLY this entry (verified by identity), so
  // answers and expirations interleaving on one key can never double-count.
  function noteCompleted(sessionId: string, key: string) {
    let byKey = completedKeys.get(sessionId);
    if (!byKey) completedKeys.set(sessionId, (byKey = new Map<string, number>()));
    byKey.set(key, (byKey.get(key) ?? 0) + 1);
    const entry = byKey;
    const timer = setTimeout(() => {
      if (completedKeys.get(sessionId) !== entry) return; // session ledger already dropped
      const n = entry.get(key);
      if (n == null) return;
      if (n <= 1) entry.delete(key);
      else entry.set(key, n - 1);
      if (entry.size === 0) completedKeys.delete(sessionId);
    }, COMPLETED_KEY_TTL_MS);
    timer.unref();
  }

  // A correlated PostToolUse landed: if a board answer on this exact key is
  // still awaiting ITS completion, consume it (one answer absorbs one
  // completion) and tell expireOnActivity to leave the pending twins alone.
  function consumeCompleted(sessionId: string, key: string): boolean {
    const byKey = completedKeys.get(sessionId);
    if (!byKey) return false;
    const n = byKey.get(key);
    if (!n) return false;
    if (n <= 1) byKey.delete(key);
    else byKey.set(key, n - 1);
    if (byKey.size === 0) completedKeys.delete(sessionId);
    return true;
  }

  // Path (b): hold window lapsed (timer) or evicted by the per-session cap —
  // answer the held request with {} so the normal flow resumes in the
  // terminal, and mark the row expired.
  function settleExpired(id: number) {
    const h = releaseHold(id);
    if (h) {
      try {
        h.respond({});
      } catch {
        /* socket already gone */
      }
    }
    if (q.markExpired.run(id).changes) {
      tick(`⌛ question #${id} expired unanswered — decide in the terminal`);
      onChange();
      // UX 2.1: the expiry MAY re-arm. With a grace timer armed the row's
      // payload carries rearm_pending:true (and expires_at extended to the
      // grace deadline) so the orphan sweep — which would otherwise see a
      // pending hold-kind row with no socket and expire it within 5 s — knows
      // this expiry is supervised. onRetired stays fired HERE (with no
      // activity flag — a bare timer expiry arms the plan gate, it does not
      // settle); the re-arm machinery only ever raises a successor, never
      // un-retires this row.
      scheduleRearm(q.get.get(id));
      onRetired(q.get.get(id));
    }
  }

  // ------------------------------------------------- UX 2.1: expiry re-arm
  // A hold expiry is NOT the question going away: the hook failed open and the
  // agent is still parked on its NATIVE terminal prompt, invisible to the
  // board. This path gives that question a survivable second (and third) life.
  // After REARM_GRACE_MS with NO activity from the session — still parked — a
  // FRESH row is created for the same question. It is deliberately NOT a hold:
  // the hook socket is gone, re-parking is impossible, so its answer rides the
  // ordinary mail pipeline to the next turn boundary (the freeform delivery
  // mechanism, proven since F3d). The payload flag rearmed:true is what Inbox
  // keys the honest "sent as a message — delivered at the next turn boundary"
  // copy on; the row keeps the original kind so the same buttons serialize the
  // same answer body.
  //
  // A re-armed row is question-rail state, not a held socket, so it does NOT
  // count against MAX_HOLDS_PER_SESSION — that cap exists to bound leaked
  // sockets, and there is no socket here. Chains are capped separately at
  // MAX_REARMS so a session nobody answers can't refill the rail forever.
  function scheduleRearm(row: QuestionRow | undefined): boolean {
    if (!(rearmGraceMs > 0)) return false; // disabled (tests, future kill-switch)
    if (!row || !HOLD_KINDS.has(row.kind)) return false; // freeform never expires on a timer
    const chainRoot = safeParse<QuestionPayload>(row.payload_json)?.chain_root ?? row.id;
    const chain = rearmChains.get(chainRoot) ?? 0;
    if (chain >= rearmMax) return false;
    // rearm_pending marks the just-expired row as re-arm-supervised. It does
    // two jobs: the orphan sweep skips it (a pending hold-kind row with no
    // socket is sweep-bait, and the 5 s sweep would outrun a test-length grace
    // window), and listForState extends expires_at to the grace deadline so
    // the board reads hold+grace as one continuous window instead of a
    // "expired" lie followed by a surprise card.
    db.prepare(`UPDATE questions SET payload_json = ? WHERE id = ? AND status = 'expired'`).run(
      JSON.stringify({
        ...(safeParse<QuestionPayload>(row.payload_json) ?? {}),
        rearm_pending: true,
      }),
      row.id,
    );
    const timer = setTimeout(() => {
      try {
        fireRearm(row.id, chainRoot);
      } catch {
        /* a re-arm is recovery, never a crash */
      }
    }, rearmGraceMs);
    timer.unref();
    rearmById.set(row.id, { timer, chainRoot, armedAt: Date.now() });
    return true;
  }

  // The grace window lapsed with no cancel signal: raise the successor. The
  // source row is already expired (settleExpired did that); a fresh pending
  // row appearing for the session inside the window (a new question raised by
  // a session that demonstrably moved on) suppresses the re-arm — don't pile
  // a recovery card on a session that is clearly talking again.
  function fireRearm(sourceId: number, chainRoot: number) {
    if (!rearmById.delete(sourceId)) return; // cancelled while the timer was queued
    const row = q.get.get(sourceId);
    if (row?.status !== 'expired') return;
    if (q.pendingBySession.all(row.session_id).length > 0) return;
    const payload = {
      ...(safeParse<QuestionPayload>(row.payload_json) ?? {}),
      rearmed: true,
      chain_root: chainRoot,
    };
    delete payload.rearm_pending;
    const fresh = create(row.kind, row.session_id, payload);
    rearmChains.set(chainRoot, (rearmChains.get(chainRoot) ?? 0) + 1);
    // The successor carries no socket and no timer — its OWN re-arm timer arms
    // when its card is answered (the answer path) and its chain continues or
    // closes there.
    rearmMeta.set(fresh.id, { sourceId });
    rearmById.set(row.id, { timer: null, chainRoot, successor: fresh.id });
    tick(
      `🔁 question #${fresh.id} re-armed (was #${row.id}) — answering sends it as a message at the next turn boundary`,
    );
    onChange();
  }

  // An unanswered re-armed row eventually leaves the recent-resolved window —
  // it was never answered, and the still-parked question deserves the chain's
  // remaining budget. Called from the sweep; expires the row (its card has had
  // its time on the rail), arms the NEXT grace window, and keeps the chain's
  // bookkeeping pointing at the row the timer hangs off.
  function recycleRearm(id: number): boolean {
    const meta = rearmMeta.get(id);
    rearmMeta.delete(id);
    if (!meta) return false;
    const row = q.get.get(id);
    if (row?.status !== 'pending') return false;
    if (Number(q.markExpired.run(id).changes) === 0) return false;
    const chainRoot = safeParse<QuestionPayload>(row.payload_json)?.chain_root ?? id;
    // The chain cap check lives in scheduleRearm's caller shape — inline here:
    // past the cap the row simply stays expired and the chain closes.
    if ((rearmChains.get(chainRoot) ?? 0) >= rearmMax) return true;
    rearmById.delete(meta.sourceId);
    const timer = setTimeout(() => {
      try {
        fireRearm(id, chainRoot);
      } catch {
        /* a re-arm is recovery, never a crash */
      }
    }, rearmGraceMs);
    timer.unref();
    rearmById.set(id, { timer, chainRoot });
    return true;
  }

  // Every cancel path below is a place where the question demonstrably left
  // the "parked and unanswered" state — the ONLY state a re-armed card
  // represents. Failing to cancel any one of them resurrects a dead question.
  // `id` may be a grace-window row's id (timer armed) or a re-armed
  // successor's id — the maps are cross-linked so either one finds both.
  function cancelRearm(id: number) {
    const src = rearmMeta.get(id)?.sourceId;
    const m = rearmById.get(id) ?? (src !== undefined ? rearmById.get(src) : undefined);
    if (m?.timer) clearTimeout(m.timer);
    if (m) rearmById.delete(src ?? id);
    if (m?.successor != null) rearmMeta.delete(m.successor);
    rearmMeta.delete(id);
  }

  // The session demonstrably moved on (a tool completed, a new turn began, the
  // session ended): everything re-arm related for it stands down. Cancel any
  // armed grace timers and expire any pending re-armed rows — whatever was
  // parked got answered (or abandoned) in the terminal, and the card must not
  // outlive the question it represents.
  function disarmRearmsForSession(sessionId: string): boolean {
    for (const [id, m] of [...rearmById]) {
      if (m.timer) clearTimeout(m.timer);
      rearmById.delete(id);
      if (m.successor != null) rearmMeta.delete(m.successor);
    }
    const retired: number[] = [];
    for (const r of q.pendingBySession.all(sessionId)) {
      if (!HOLD_KINDS.has(r.kind)) continue; // freeform follows its own rules
      if (safeParse<QuestionPayload>(r.payload_json)?.rearmed !== true) continue;
      if (q.markExpired.run(r.id).changes) retired.push(r.id);
    }
    // Same ordering rule as expireOnActivity: the batch retires before any
    // callback runs. These fire WITH activity:true — the session moving on is
    // exactly the signal, so a linked plan settles in the same tick.
    for (const id of retired) onRetired(q.get.get(id), { activity: true });
    return retired.length > 0;
  }

  // Path (c): the hook client disconnected before we responded. 'close' also
  // fires after a NORMAL completion, so only act while the hold still exists.
  function socketClosed(id: number) {
    if (!holds.has(id)) return;
    releaseHold(id);
    if (q.markExpired.run(id).changes) {
      onChange();
      onRetired(q.get.get(id));
    }
  }

  // -------------------------------------------------- answer mail size guard
  // BUG-137 (data loss): mail() clamps at MAIL_MAX_LEN and reports
  // {truncated:true}, but the freeform / re-armed answer paths used to ignore
  // the receipt and settle the row anyway — the human got "answer queued",
  // the agent got a truncated instruction, and the question was gone. Framed
  // answers ([FLEETDECK ANSWER] Q: … — A: …) that exceed the clamp are now
  // REJECTED before anything is stored or settled: the row stays pending, the
  // human is told to shorten, and nothing is lost silently. The 413 mirrors
  // the board API's other body rejections (400/409/422 are taken; the HTTP
  // layer passes our status through verbatim).
  const ANSWER_TOO_LONG_ERR =
    'answer too long — the mail pipeline would truncate it. Shorten it (or dismiss and answer in the terminal); the question is still pending.';
  function answerMailGuard(frame: string) {
    if (frame.length <= mailMaxLen) return null;
    return { status: 413, body: { ok: false, err: ANSWER_TOO_LONG_ERR } };
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
  // Mail-delivered answers (freeform + re-armed holds) pass the BUG-137 size
  // guard first: a framed answer that would exceed the mailbox clamp 413s and
  // the row stays pending — the mailbox never stores a truncated answer.
  // Returns { status, body } for the HTTP layer.
  function answer(id: string | number, body: AnswerBody | null | undefined) {
    const row = q.get.get(Number(id));
    if (!row) return { status: 404, body: { ok: false, err: 'no such question' } };
    if (row.status !== 'pending')
      return { status: 409, body: { ok: false, err: `question already ${row.status}` } };
    const now = Date.now();
    const who = callsignOf(row.session_id) ?? row.session_id;

    if (HOLD_KINDS.has(row.kind)) {
      // UX 2.1: a RE-ARMED row has no held socket by construction — the hook
      // failed open when the original hold expired and the native terminal
      // prompt owns the decision. Its answer rides the ordinary mail pipeline
      // (the freeform mechanism below, proven since F3d) and lands at the next
      // turn boundary — subject to the same BUG-137 size guard (an oversized
      // serialized answer 413s and the row stays pending). Never claim it
      // unblocks an agent parked on stdin — the board copy says exactly that.
      // This branch runs BEFORE any wire-schema validation: there is no
      // socket left to validate for, and the answer is serialized to plain
      // text either way.
      const payload = safeParse<QuestionPayload>(row.payload_json);
      if (payload?.rearmed === true) {
        const detail0 =
          row.kind === 'permission'
            ? body?.behavior
            : row.kind === 'choice'
              ? serializeChoiceAnswer(row, body)
              : body?.action === 'accept' || body?.action === 'decline'
                ? body.action
                : null;
        if (detail0 && typeof detail0 === 'object') {
          return {
            status: 400,
            body: {
              ok: false,
              err: `answer too long — ${detail0.over} code units exceeds the 2000-unit answer limit; shorten the answer or answer at the terminal`,
            },
          };
        }
        const detail = detail0;
        if (detail == null) {
          return {
            status: 400,
            body: {
              ok: false,
              err: 'body must match the question kind (behavior / answers|text / action)',
            },
          };
        }
        if (detail === 'capture') {
          return {
            status: 400,
            body: {
              ok: false,
              err: '"capture" needs the live hold — the window for it has closed',
            },
          };
        }
        const questionText = (
          payload.tool_input?.questions?.[0]?.question ??
          payload.text ??
          ''
        ).slice(0, 80);
        const frame = `[FLEETDECK ANSWER] ${row.kind} (answered after the hold expired) Q: ${questionText} — A: ${detail}`;
        // BUG-137: reject BEFORE the plan flip / row settle — an answer that
        // would be truncated in transit settles nothing.
        const rejected = answerMailGuard(frame);
        if (rejected) return rejected;
        if (planIdFor(row.id) != null) planAnswered(row.id, detail);
        mail(row.session_id, 'fleetdeck-answer', frame);
        cancelRearm(row.id); // idempotent — a sibling card answering first already cancelled
        q.markAnswered.run(JSON.stringify(body ?? {}), now, row.id);
        tick(
          `💬 ${who}: re-armed ${row.kind} answered (${detail}) — queued for the next turn boundary`,
        );
        onChange();
        return {
          status: 200,
          body: {
            ok: true,
            delivered: false,
            note: 'answer queued — delivered at next turn boundary',
          },
        };
      }
      let hookResponse: unknown;
      let detail: string;
      let planBehavior: string | null = null; // v1.3: set on a plan question's allow/capture/deny
      if (row.kind === 'permission') {
        const behavior = body?.behavior;
        const planId = planIdFor(row.id); // non-null only for an ExitPlanMode plan question
        if (behavior === 'capture' && planId == null) {
          return {
            status: 400,
            body: { ok: false, err: '"capture" is only valid for an ExitPlanMode plan question' },
          };
        }
        if (behavior !== 'allow' && behavior !== 'deny' && behavior !== 'capture') {
          return {
            status: 400,
            body: {
              ok: false,
              err:
                planId != null
                  ? 'body must be {"behavior":"allow"|"capture"|"deny"}'
                  : 'body must be {"behavior":"allow"|"deny"}',
            },
          };
        }
        // Verified schema (per the official hooks docs); no message/reason field documented.
        // 'capture' answers the held hook with a bare deny (CONTRACT v1.3) —
        // the pseudo-behavior never reaches the hook client.
        const wire = behavior === 'capture' ? 'deny' : behavior;
        hookResponse = {
          hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior: wire } },
        };
        detail = behavior;
        if (planId != null) planBehavior = behavior;
      } else if (row.kind === 'choice') {
        const serialized = serializeChoiceAnswer(row, body);
        if (serialized && typeof serialized === 'object') {
          return {
            status: 400,
            body: {
              ok: false,
              err: `answer too long — ${serialized.over} code units exceeds the 2000-unit answer limit; shorten the answer or answer at the terminal`,
            },
          };
        }
        if (!serialized) {
          return {
            status: 400,
            body: {
              ok: false,
              err: 'body must be {"answers":{"<question text>":"<label>"}} or {"text":"..."}',
            },
          };
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
          return {
            status: 400,
            body: {
              ok: false,
              err: 'body must be {"action":"accept","content":{...}} or {"action":"decline"}',
            },
          };
        }
        // UNVERIFIED schema — handoff guess, pending the Phase 3 live gate.
        hookResponse =
          action === 'accept'
            ? { action: 'accept', content: body?.content ?? {} }
            : { action: 'decline' };
        detail = action;
      }
      const h = releaseHold(row.id);
      if (!h) {
        // The held socket is gone (expired, client vanished, or daemon
        // restarted mid-hold): the decision cannot reach the session.
        if (q.markExpired.run(row.id).changes) {
          onChange();
          onRetired(q.get.get(row.id));
        }
        return {
          status: 409,
          body: { ok: false, err: 'hold expired — the terminal prompt owns this decision now' },
        };
      }
      try {
        h.respond(hookResponse);
      } catch {
        /* socket died as we answered */
      }
      // BUG-138: the board settled this hold, so its row now reads 'answered'
      // and a correlated PostToolUse can no longer tell THIS call's completion
      // from an identical twin's. Record the key so the completion is consumed
      // against this answer instead of expiring a still-pending twin.
      if (payload?.tool_name != null) {
        noteCompleted(row.session_id, toolCallKey(payload.tool_name, payload.tool_input));
      }
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
      const text = (body?.text ?? '').trim();
      if (!text) return { status: 400, body: { ok: false, err: 'body must be {"text":"..."}' } };
      const questionText = (safeParse<QuestionPayload>(row.payload_json)?.text ?? '').slice(0, 80);
      const frame = `[FLEETDECK ANSWER] Q: ${questionText} — A: ${text}`;
      // BUG-137: reject BEFORE settle — a frame mail() would clamp would
      // deliver a truncated instruction while the human loses the question.
      const rejected = answerMailGuard(frame);
      if (rejected) return rejected;
      // Turn-boundary delivery — the PROVEN mechanism (existing mail pipeline:
      // UserPromptSubmit additionalContext or Stop block). No new delivery
      // mechanisms here; asyncRewake is Phase 4.
      mail(row.session_id, 'fleetdeck-answer', frame);
      q.markAnswered.run(JSON.stringify({ text }), now, row.id);
      tick(`💬 answer for ${who} queued — lands at next turn boundary`);
      onChange();
      return {
        status: 200,
        body: {
          ok: true,
          delivered: false,
          note: 'answer queued — delivered at next turn boundary',
        },
      };
    }

    return { status: 400, body: { ok: false, err: `unknown question kind ${row.kind}` } };
  }

  // ------------------------------------------------------------ resolution
  // F3e auto-resolution: activity from a session settles the pending questions
  // it is no longer waiting on. Live holds are failed open ({} → the terminal
  // flow owns the decision) without waiting out the window; hold-less rows
  // (window lapsed, client gone, daemon restarted mid-hold) just flip to
  // expired. Two callers, two scopes:
  //
  //   • PostToolUse — a SINGLE tool call completed. With parallel tool calls,
  //     session identity alone is not enough: finishing tool A must NOT release
  //     tool B's hold, still parked on the human (M-B1). Real PostToolUse AND
  //     PermissionRequest payloads carry NO tool_use_id / request_id (verified
  //     against the hook fixtures), so the ONLY identity a held permission and
  //     its completing PostToolUse actually share is (tool_name, tool_input).
  //     Correlated activity therefore retires only the HOLD(s) whose stored
  //     payload names the same tool with the same input, and leaves every
  //     sibling hold (a different tool call) untouched. A freeform row (no
  //     tool_name) and an Elicitation hold (no tool_name either — it is MCP
  //     form input, not a tool call) never match a tool completion; they are
  //     settled by the turn-boundary path below, their own timer, or an answer.
  //
  //   • UserPromptSubmit — a TURN BOUNDARY, not one tool call. There is no tool
  //     identity to correlate and a new prompt means the session moved on from
  //     everything it was parked on, so this stays SESSION-WIDE: every pending
  //     hold AND freeform row for the session expires.
  //
  // FREEFORM ROWS EXPIRE ONLY ON THE SESSION-WIDE (turn-boundary) PATH, and
  // that is a correction, not a shortcut. A freeform question is raised at a
  // Stop — the session is idle, waiting on an answer. A new turn means it is
  // NOT waiting any more: the human answered in the terminal (or the board's
  // live terminal modal) and the session moved on. Leaving the row pending left
  // a NEEDS YOU card on the board forever, for a question answered ten minutes
  // ago in another window — the rail filled with ghosts and stopped meaning
  // anything. The board must reflect what is actually still owed.
  //
  // The resume path is untouched: expireAllForSession (SessionEnd) still spares
  // freeform, so an ENDED session's question survives for `claude --resume`.
  // Only a session that demonstrably kept going clears its own queue.
  //
  // Every retire here also fires onRetired, and the opts include `activity:
  // true`: this path IS session activity by definition (a tool call completed,
  // or a new turn began), so a plan question retired by it settles to
  // 'handled-in-terminal' in the same tick — the human visibly decided in the
  // terminal and the agent moved on. The same-turn guard lives on the plan
  // side (planRetired never touches a plan whose question is still pending).
  function expireOnActivity(
    sessionId: string,
    { toolName, toolInput }: { toolName?: unknown; toolInput?: unknown } = {},
  ): boolean {
    // A completed tool call correlates; a turn boundary (no toolName) is
    // session-wide.
    const activityKey =
      typeof toolName === 'string' && toolName !== '' ? toolCallKey(toolName, toolInput) : null;
    const correlated = activityKey !== null;
    let rows = q.pendingBySession.all(sessionId).filter((r) => {
      if (!correlated) return true; // turn boundary → session-wide (holds + freeform)
      if (!HOLD_KINDS.has(r.kind)) return false; // freeform has no tool identity
      const payload = safeParse<QuestionPayload>(r.payload_json);
      if (payload?.rearmed === true) return false; // re-armed rows disarm via disarmRearmsForSession below
      if (payload?.tool_name == null) return false; // elicitation & co. — not a tool call
      return toolCallKey(payload.tool_name, payload.tool_input) === activityKey;
    });
    // UX 2.1 grace-window guard (the BUG-138 twin-hold rule, made explicit):
    // activity keys on (tool_name, tool_input) ONLY — a twin hold for the SAME
    // tool call expires by timer even though its tool already completed. When
    // THIS activity lands inside that twin's re-arm grace window, the match is
    // not proof the twin is still parked — the twin must NOT re-arm. The twin's
    // row is already expired (settleExpired retired it; only its grace timer is
    // pending), so cancelling the timer IS the whole settlement here. The row
    // never read pending during the window, so pendingBySession can't see it —
    // walk the grace map (rearmById entries with a LIVE timer) instead.
    if (correlated) {
      const graceCancelled: number[] = [];
      for (const [id, m] of [...rearmById]) {
        if (!m.timer) continue; // a successor link, not an armed grace window
        const row = q.get.get(id);
        if (row?.session_id !== sessionId) continue;
        const payload = safeParse<QuestionPayload>(row.payload_json);
        if (payload?.tool_name == null) continue;
        if (toolCallKey(payload.tool_name, payload.tool_input) !== activityKey) continue;
        cancelRearm(id);
        graceCancelled.push(id);
      }
      for (const id of graceCancelled) onRetired(q.get.get(id), { activity: true });
    }
    // UX 2.1: activity from this session is also the re-arm stop-condition —
    // stand down every armed grace timer and retire every pending re-armed row
    // for the session (still-parked is disproven by definition here). Fires on
    // BOTH the correlated and the session-wide path.
    const rearmDisarmed = disarmRearmsForSession(sessionId);
    // BUG-138: a correlated completion that a board answer is still waiting on
    // belongs to the ANSWERED call, not to any pending twin. Consume the ledger
    // entry (one answer absorbs exactly one completion) and retire nothing —
    // without this, answering hold A made A's own PostToolUse expire B, the
    // still-pending identical twin, and the second permission vanished from the
    // board while the human had never decided it.
    if (activityKey !== null && consumeCompleted(sessionId, activityKey)) {
      if (rearmDisarmed) onChange();
      return rearmDisarmed;
    }
    // BUG 5: a single PostToolUse completes exactly ONE tool call. Two parallel
    // holds with IDENTICAL (tool_name, tool_input) share a toolCallKey, so the
    // filter above matches BOTH — and the old code expired both, releasing a
    // sibling still legitimately parked on the human. A correlated completion
    // must retire AT MOST ONE hold: the OLDEST still-live matching hold (or, if
    // none is still held — all are restart orphans — the oldest matching row).
    // pendingBySession is id-ascending, so the first live/first row IS the
    // oldest. Different-input siblings get different keys and never collide
    // here; the turn-boundary (non-correlated) path stays session-wide.
    if (correlated && rows.length > 1) {
      const first = rows[0];
      if (first) rows = [rows.find((r) => holds.has(r.id)) ?? first];
    }
    // Order matters: expire FIRST, then fire onRetired only for rows that
    // flipped. Firing inside the loop would interleave the plan-settle
    // callback with the retire loop, and a callback that consults the
    // pending-question set (derive.mjs's same-turn guard) would see a
    // half-retired batch — or worse, retire NEW rows (a plan question raised
    // in this same turn IS in the pending set right now) before its own loop
    // turn. One full pass first keeps the callback's world consistent.
    const retired: number[] = [];
    for (const r of rows) {
      const h = releaseHold(r.id);
      if (h) {
        try {
          h.respond({});
        } catch {
          /* socket already gone */
        }
      }
      if (q.markExpired.run(r.id).changes) retired.push(r.id);
    }
    for (const id of retired) onRetired(q.get.get(id), { activity: true });
    if (retired.length || rearmDisarmed) onChange();
    return retired.length > 0 || rearmDisarmed;
  }

  // "Clear" on the board: answered, expired and dismissed cards leave the rail
  // for good. PENDING rows are the human's actual queue and are never touched
  // here — Clear tidies the past, it does not silence the present.
  function purgeResolved(): number {
    const out = db.prepare("DELETE FROM questions WHERE status != 'pending'").run();
    if (out.changes) onChange();
    return Number(out.changes);
  }

  // The human already handled it elsewhere: retire the card, tell the session
  // nothing. Distinct from answering (which mails the answer to the session)
  // and from expiring a hold (which must fail a parked socket open).
  // opts.activity: a dismissal that happened alongside session activity (e.g.
  // BUG-041's execute-while-pending) is handed through to onRetired so the
  // linked plan settles in the same tick; a plain board dismissal is not
  // terminal activity, so the plan stays 'proposed' until the session next
  // moves (the activity gate — a human dismissing a card has NOT decided the
  // plan in the terminal).
  function dismiss(id: number, { activity = false }: { activity?: boolean } = {}) {
    const row = q.get.get(id);
    if (!row) return { ok: false, reason: 'no such question' };
    // The human declared the card handled — cancel any re-arm timer chained to
    // it FIRST, or the dismissed question resurrects one grace window later.
    // Runs before the status gate on purpose: a row inside its grace window
    // reads 'expired' (the old early-return), yet has exactly the armed timer
    // this cancel exists to kill.
    cancelRearm(row.id);
    if (row.status !== 'pending') return { ok: true, already: true };
    const h = releaseHold(row.id);
    if (h) {
      try {
        h.respond({});
      } catch {
        /* socket already gone */
      }
    }
    const changed = Number(q.markExpired.run(row.id).changes) > 0;
    if (changed) {
      onChange();
      onRetired(q.get.get(row.id), { activity });
    }
    // WHY resolve through the session adapter: `questions` deliberately has
    // no callsign column, so reading row.callsign always returned dead null.
    return { ok: true, callsign: callsignOf(row.session_id) ?? null };
  }

  // Restart hygiene (periodic sweep): a pending hold-kind row with NO live
  // hold can never deliver an answer — expire it. Never touches live holds,
  // and never expires a RE-ARMED row (payload.rearmed): it has no socket by
  // construction, its answer goes by mail, and it is exactly as deliverable as
  // a freeform row. Instead the sweep RECYCLES an aged re-armed row — a card
  // that sat unanswered long enough to fall out of the recent-resolved window
  // gets its chain's next grace window (recycleRearm), up to the cap.
  function expireOrphans(): boolean {
    let changed = false;
    for (const r of q.pending.all()) {
      if (!HOLD_KINDS.has(r.kind) || holds.has(r.id)) continue;
      if (safeParse<QuestionPayload>(r.payload_json)?.rearmed === true) {
        if (Date.now() - r.created_at >= rearmGraceMs && recycleRearm(r.id)) changed = true;
        continue;
      }
      if (q.markExpired.run(r.id).changes) {
        changed = true;
        onRetired(q.get.get(r.id));
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
  function expireAllForSession(
    sessionId: string,
    { includeFreeform = false }: { includeFreeform?: boolean } = {},
  ): number {
    let expired = 0;
    const retired: number[] = [];
    for (const r of q.pendingBySession.all(sessionId)) {
      if (!includeFreeform && !HOLD_KINDS.has(r.kind)) continue;
      const h = releaseHold(r.id);
      if (h) {
        try {
          h.respond({});
        } catch {
          /* gone */
        }
      }
      cancelRearm(r.id); // a dead session re-arms nothing
      if (q.markExpired.run(r.id).changes) {
        expired++;
        retired.push(r.id);
      }
    }
    // Grace timers belong to EXPIRED rows, so the loop above can't see them —
    // cancel any armed for this session or a dead session would still raise a
    // re-armed card one window later.
    for (const [id, m] of [...rearmById]) {
      if (!m.timer) continue; // a successor link, not an armed grace window
      if (q.get.get(id)?.session_id === sessionId) cancelRearm(id);
    }
    // BUG-138: a dead session's completions can no longer arrive — drop its
    // ledger so a later session reusing the id can't consume stale entries.
    completedKeys.delete(sessionId);
    // Same ordering rule as expireOnActivity: retire the whole batch before
    // any callback runs (these fire with no activity flag, but the plan
    // gate's pending-set reads must still see a consistent world).
    for (const id of retired) onRetired(q.get.get(id));
    if (expired) onChange();
    return expired;
  }

  function pendingOf(sessionId: string): QuestionRow[] {
    return q.pendingBySession.all(sessionId);
  }

  // Is this question's hold socket still parked? The holds map is private to
  // this module, but the plan-settle gate (derive.mjs planRetired) needs it:
  // a plan question created DURING the current activity event already reads
  // non-pending when the retirement callbacks run (hookHoldQuestion inserts
  // before the HTTP layer parks the socket), so the status column alone
  // cannot distinguish "retired prompt" from "chooser never rendered".
  function isHeld(id: number): boolean {
    return holds.has(id);
  }

  // -------------------------------------------------------------- snapshot
  // GET /state `questions`: all pending + the last few resolved, with enough
  // for the board to render countdowns (expires_at) and disable dead cards
  // (`held` false on a pending hold-kind row = restart orphan, sweep-bound).
  // v1.3: an ExitPlanMode question carries `plan_id` (its captured plan) so
  // the board can render it as a PLAN card and offer Approve / Capture &
  // release / Deny.
  function listForState() {
    return [...q.pending.all(), ...q.resolved.all()].map((r) => {
      const plan_id = planIdFor(r.id);
      // UX 2.1: a row inside its re-arm grace window reads expired in the DB
      // (settleExpired retired it — the hook failed open and that fact must
      // not be papered over) but its question is still live. Surface the grace
      // deadline as expires_at so the card's countdown runs to the END of the
      // window (hold + grace as one continuous open question) instead of
      // showing "expired" for the grace seconds and then surprising the human
      // with a fresh card. Keyed on the rearm_pending payload flag — a STALE
      // in-memory grace entry must never graft a fake deadline onto an
      // ordinary expired row (dismiss/activity/Sweep retire without arming).
      const payload = safeParse<QuestionPayload>(r.payload_json);
      const grace = payload?.rearm_pending === true ? rearmById.get(r.id)?.armedAt : undefined;
      return {
        id: r.id,
        kind: r.kind,
        session_id: r.session_id,
        callsign: callsignOf(r.session_id),
        payload,
        status: r.status,
        created_at: r.created_at,
        expires_at: grace != null ? grace + rearmGraceMs : r.expires_at,
        answered_at: r.answered_at,
        answer: safeParse(r.answer_json),
        held: holds.has(r.id),
        ...(plan_id != null ? { plan_id } : {}),
      };
    });
  }

  // Orphan sweep (restart hygiene). Live holds settle via their own timers.
  const sweep = setInterval(() => {
    try {
      expireOrphans();
    } catch {
      /* hygiene only */
    }
  }, SWEEP_MS);
  sweep.unref();

  return {
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
    isHeld,
    listForState,
  };
}

// --------------------------------------------------------------------------
// F3c choice-answer serialization (pure helper)
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
// nothing usable — the HTTP layer turns that into a 400 and the hold STAYS
// open for a corrected answer.
//
// An answers map is validated against the held question's own schema before a
// single label is serialized. Without this check a malformed or stale client
// (wrong key, an object value → "[object Object]") would settle the hold with
// a meaningless answer — the question flips to 'answered', irreversibly
// suppressing the native chooser. The rules:
//   • every key must be the `question` text of a payload question;
//   • every value must be a non-empty string, or an array of non-empty strings
//     (arrays only when that question is multiSelect);
//   • every label must come from that question's options[].
//
// An answer is the operator's decision, NOT a display string: it is relayed
// in full, never clipped by the 300-unit display clamp (BUG-139). Only a
// serialized answer over ANSWER_MAX code units is rejected outright (the
// HTTP layer turns the { over: n } marker into a 400) — settling the hold
// with a silently truncated answer would feed the agent a partial decision
// that cannot be recovered through the terminal chooser. The limit compares
// STRING CODE UNITS (Array.from never slices), so no surrogate pair is
// ever split. Any text a multi-question chooser legitimately produces is
// far under the cap; it guards against pathological bodies only.
const ANSWER_MAX = 2000;
function serializeChoiceAnswer(
  row: QuestionRow,
  body: AnswerBody | null | undefined,
): string | { over: number } | null {
  if (typeof body?.text === 'string' && body.text.trim()) {
    const t = body.text.trim();
    return t.length <= ANSWER_MAX ? t : { over: t.length };
  }
  const answers = body?.answers;
  if (!answers || typeof answers !== 'object' || Array.isArray(answers)) return null;
  const entries = Object.entries(answers as Record<string, unknown>);
  if (!entries.length) return null;
  const qs = safeParse<QuestionPayload>(row.payload_json)?.tool_input?.questions;
  if (Array.isArray(qs) && !validChoiceAnswers(qs, entries)) return null;
  const fmt = (v: unknown): string =>
    (Array.isArray(v) ? v.map(asText).join(', ') : asText(v)).trim();
  if (entries.some(([, v]) => fmt(v) === '')) return null;
  if (entries.length === 1) {
    const t = fmt(entries[0]?.[1]);
    return t.length <= ANSWER_MAX ? t : { over: t.length };
  }
  const headerOf = (qText: string): string | undefined =>
    Array.isArray(qs) ? qs.find((x) => x?.question === qText)?.header : undefined;
  const t = entries.map(([qText, v]) => `${headerOf(qText) ?? qText}: ${fmt(v)}`).join('; ');
  return t.length <= ANSWER_MAX ? t : { over: t.length };
}

// A label is valid only as one of the question's option labels.
function validChoiceLabel(question: ChoiceQuestion, label: string): boolean {
  if (!label) return false;
  return (Array.isArray(question.options) ? question.options : []).some((o) => o?.label === label);
}

function validChoiceAnswers(
  questions: (ChoiceQuestion | null)[],
  entries: [string, unknown][],
): boolean {
  return entries.every(([qText, v]) => {
    const question = questions.find((x) => x?.question === qText);
    if (!question) return false; // key must be a held question's text
    if (typeof v === 'string') return validChoiceLabel(question, v.trim());
    if (Array.isArray(v)) {
      // an array of labels only for a multiSelect question, all non-empty strings
      if (question.multiSelect !== true || !v.length) return false;
      return v.every((x) => typeof x === 'string' && validChoiceLabel(question, x.trim()));
    }
    return false; // never String()-coerce objects/numbers into the reason
  });
}

// --------------------------------------------------------------------------
// F3d detection helpers (pure functions; exported for tests)
// --------------------------------------------------------------------------

const CHOICE_RE =
  /\b(should I|do you want|would you like|which|prefer|option [AB1-9]|let me know)\b/i;

// Regex heuristic, NO model call. A trailing question is:
//   (1) the last non-empty line of the final paragraph ends with '?'
//       (allowing closing markdown/quotes/brackets after it), or
//   (2) the final paragraph matches a choice pattern (CHOICE_RE) AND
//       contains a '?' anywhere.
// Returns the question snippet, or null.
export function detectTrailingQuestion(text: unknown): string | null {
  const trimmed = asText(text).trim();
  if (!trimmed) return null;
  const paras = trimmed.split(/\n[ \t]*\n/);
  const lastPara = (paras[paras.length - 1] ?? '').trim();
  const lines = lastPara
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean);
  const lastLine = lines[lines.length - 1] ?? '';
  const TRAILING_Q = /\?[\s"'*_)\]`.]*$/;

  if (TRAILING_Q.test(lastLine)) return clipQuestion(lastLine);
  if (lastPara.includes('?') && CHOICE_RE.test(lastPara)) {
    return clipQuestion(sentenceWithLastQuestionMark(lastPara));
  }
  return null;
}

function sentenceWithLastQuestionMark(para: string): string {
  const idx = para.lastIndexOf('?');
  if (idx === -1) return para;
  let start = 0;
  for (let i = idx - 1; i >= 0; i--) {
    const ch = para[i];
    if (ch === '.' || ch === '!' || ch === '?' || ch === '\n') {
      start = i + 1;
      break;
    }
  }
  return para.slice(start, idx + 1).trim();
}

function clipQuestion(s: string): string {
  const t = s.trim();
  return t.length <= 300 ? t : t.slice(0, 297) + '…';
}
