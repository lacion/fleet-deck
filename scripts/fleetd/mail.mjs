// mail.mjs — the mailbox (bounded, surrogate-safe), the /api/watch waiter
// registry, and owned-pane delivery. One module owns "how does a message reach
// a session": wake a live long-poll, type into a daemon-owned Claude pane, or
// queue it for a turn boundary / future --resume. Threaded ctx state: q, tick,
// logEvent, onMutate, the questions relay (watchInfo counts freeform), the tmux
// adapter + findScopedWindow (owned-pane probe), db (the atomic claim), and the
// PANE_MAIL_GRACE_MS knob.

// BUG 4: mail is pasted VERBATIM into a tmux paste-buffer, so it must stay
// bounded — but the old 500-char clamp silently truncated real messages (it
// cut the very bug report that surfaced these bugs to 500 chars) and reported
// {ok:true, delivered:1} as if nothing had been lost. 4 KB leaves room for a
// paragraph or a short stack trace while keeping the tmux buffer sane; the mail
// path now returns a `truncated` flag + the original length so POST /mail can
// tell the sender the truth instead of quietly dropping the tail.
// Exported so commands.mjs can validate a fully framed command against the
// very same cap BEFORE inserting any recipient rows (BUG-021) — one constant,
// no drift between the clamp and the pre-flight check — and so questions.mjs
// can reject framed answers that would be clamped (BUG-137) instead of
// settling the question over a truncated message.
export const MAIL_MAX_LEN = 4000;
// BUG 6: .slice() cuts by UTF-16 code UNIT, so a clamp landing between the two
// halves of an astral character (emoji, CJK extension B, …) keeps a lone high
// surrogate at the tail — a malformed, unpasteable string. Clamp to at most
// MAIL_MAX_LEN code units, then drop a trailing UNPAIRED high surrogate (its
// low-surrogate partner was the code unit we cut off, so it is guaranteed
// orphaned). The reported length semantics are UNCHANGED and code-unit-based:
// `original_length` stays raw.length and truncation is still `raw.length >
// MAIL_MAX_LEN` — only the STORED body loses the half-character (so a clamped
// astral message stores MAIL_MAX_LEN-1 units, never a broken surrogate).
function clampMail(raw) {
  if (raw.length <= MAIL_MAX_LEN) return raw;
  return dropOrphanSurrogate(raw.slice(0, MAIL_MAX_LEN));
}

// Shared BUG 6 tail-fix: a UTF-16 code-unit .slice() can leave a lone (unpaired)
// high surrogate at the cut — its low half was the very unit we dropped, so it
// is guaranteed orphaned. Shear it so no clamp ever stores or pastes a broken
// astral half-character. Both the text clamp and the `from` clamp go through it.
function dropOrphanSurrogate(cut) {
  const last = cut.charCodeAt(cut.length - 1);
  return (last >= 0xd800 && last <= 0xdbff) ? cut.slice(0, -1) : cut;
}

// BUG 12: the `from`/`from_id` is embedded VERBATIM into the owned-pane paste
// (`[FLEETDECK MAIL from ${from_id}] …`) and into every ticker/log line, but
// only `text` was ever bounded — a multi-MB `from` became a multi-MB paste +
// ticker row. Bound it at insert time to a short sane cap, surrogate-safe by
// the exact BUG 6 rule. Non-string values pass through untouched so the DB
// NULL-binding path is unchanged; only an oversized string is clamped. Unlike
// `text` no length is reported back: an over-long sender is malformed input,
// not a message body whose truncated tail we owe the caller.
const MAIL_FROM_MAX_LEN = 200;
function clampFrom(from) {
  if (typeof from !== 'string' || from.length <= MAIL_FROM_MAX_LEN) return from;
  return dropOrphanSurrogate(from.slice(0, MAIL_FROM_MAX_LEN));
}

// BUG-128: the mailbox is bounded not only per row (MAIL_MAX_LEN above) but
// per PENDING mailbox. Without a budget, a valid burst of 4 KB messages to
// one idle session accumulated without limit — and the owned-pane path then
// joined the WHOLE backlog into one unbounded tmux paste. An insert that
// would push a session past either bound is refused BEFORE the write
// (backpressure): postMail turns the refusal into a loud 429 instead of
// silently dropping the mail. Delivery channels are all FIFO-oldest-first
// (drainMail, claimMail/nextMail, claimAllMail/pendingMailPage), so the
// oldest messages are never starved by a refused new tail.
const MAIL_PENDING_MAX = 100;
const MAIL_PENDING_MAX_BYTES = 256 * 1024;
// The owned-pane claim is a bounded BATCH, not the whole mailbox: at most
// MAIL_PANE_BATCH rows / MAIL_PANE_BATCH_BYTES code units per paste (both
// comfortably above any single row, so one oversized-but-legal row still fits
// its own batch). Rows past the batch stay pending and are picked up by the
// next delivery round instead of one giant buffer.
const MAIL_PANE_BATCH = 25;
const MAIL_PANE_BATCH_BYTES = 64 * 1024;

// 0.16.0 SENDER/FRAME RESERVATION. The fleet doctrine teaches agents to treat
// [FLEETDECK ...] frames and the daemon's own sender names as carrying human
// authority — so they must be unforgeable. Only the daemon's internal mail()
// callers may send them; postMail (the external API) is forced unprivileged:
// reserved senders 422, and a reserved frame prefix at the start of ANY
// logical line of the text 422s as well (a frame MID-line renders as mail
// content, not an envelope, so only line-leading positions are checked).
// Ordinary callsign/session-id senders and plain text are unaffected.
const RESERVED_SENDERS = new Set(['orchestrator', 'fleetdeck', 'fleetdeck-answer', 'human']);
// Leading whitespace AND control/zero-width characters: a frame smuggled past
// as "\x00[FLEETDECK ANSWER]" renders identically in a pane to the real one.
// Multiline: a frame at the start of any LATER line (after \n, \r, \r\n, or a
// unicode line separator) is just as forged — delivery preserves newlines and
// the pane renders each line as its own row, so "preface\n[FLEETDECK ANSWER]"
// would render line two exactly like a daemon frame.
// eslint-disable-next-line no-control-regex
const RESERVED_FRAME_RE = /^[\s\x00-\x1f\x7f-\x9f]*\[FLEETDECK[ \]]/i;
// BUG-032: Unicode format characters (general category Cf — zero-width spaces
// and joiners, and EVERY bidi control: U+061C, U+200E/F, U+202A-E, U+2066-9)
// are visually ignorable in a receiving pane, so "human​" renders as the
// reserved `human` and "​[FLEETDECK ANSWER]" renders as a real authority
// frame while the exact-sender and leading-frame checks looked the other way.
// Reject Cf in sender names outright, and strip Cf from the text BEFORE the
// reserved-frame check so a zero-width character can't smuggle a frame past
// at any offset — leading ("​[FLEETDECK ANSWER]") or interior
// ("[FLEETDECK​ ANSWER]") alike.
const stripFormatChars = (s) => s.replace(/\p{Cf}/gu, '');
// BUG-036/BUG-063: delivery preserves linefeeds (watcher output is verbatim
// and sanitizePaneText keeps \n), so a frame at the start of ANY logical line
// — not just byte zero — renders as a genuine authority frame. Canonicalize
// EVERY JS line terminator the pane can break on — CRLF, lone CR, and the
// unicode line/paragraph separators U+2028/U+2029 (BUG-063's multiline anchor
// caught these; a CRLF-only split silently dropped them) — to LF, then test
// the start of every line (on the Cf-stripped text, per BUG-032 above).
function hasReservedFrame(text) {
  return stripFormatChars(String(text))
    .replace(/\r\n?|[\u2028\u2029]/g, '\n')
    .split('\n')
    .some(line => RESERVED_FRAME_RE.test(line));
}
// The pane envelope is a single line (`[FLEETDECK MAIL from <from>] <text>`):
// a newline in `from` lets the text forge a line-two frame, and `from` is
// interpolated VERBATIM between the envelope's own bracket delimiters — so a
// `]` closes the envelope early and a following `[FLEETDECK ASSIGNMENT`
// synthesizes an exact reserved frame inside a daemon-owned pane
// (`[FLEETDECK MAIL from peer] [FLEETDECK ASSIGNMENT] …`). Bracket delimiters
// are therefore as forbidden as controls. Control chars are already stripped
// from pane-bound text by sanitizePaneText, but `from` rides inside the same
// paste — refuse them at the door instead.
// eslint-disable-next-line no-control-regex
// BUG-032 adds \p{Cf} (format chars can smuggle a reserved sender name);
// BUG-035 adds [ and ] (brackets become envelope syntax in the pane paste).
const FROM_UNSAFE_RE = /[\r\n\x00-\x1f\x7f-\x9f\p{Cf}[\]]/u;

export function createMail(ctx) {
  const {
    db, q, tick, logEvent, onMutate, questions, tmuxAdapter,
    findScopedWindow, scopedPaneTarget, PANE_MAIL_GRACE_MS, MAIL_CLAIM_LEASE_MS,
    // BUG-128: test-only budget overrides (createCore passes these through
    // from its opts); production never sets them, so the constants rule.
    MAIL_PENDING_MAX: PENDING_MAX = MAIL_PENDING_MAX,
    MAIL_PENDING_MAX_BYTES: PENDING_MAX_BYTES = MAIL_PENDING_MAX_BYTES,
    MAIL_PANE_BATCH: PANE_BATCH = MAIL_PANE_BATCH,
    MAIL_PANE_BATCH_BYTES: PANE_BATCH_BYTES = MAIL_PANE_BATCH_BYTES,
  } = ctx;

  // BUG-034: a claim is now an EXPIRING IN-FLIGHT LEASE, not a delivery. The
  // three loss windows the audit pinned are all the same shape — the text left
  // the mailbox (delivered_at committed) BEFORE the consumer provably had it,
  // so a disconnect/restart dropped it with no retry path:
  //   - /api/watch claimed then wrote the response — a socket that closed
  //     mid-response lost the mail (http.mjs documented the window).
  //   - claimAllMail committed delivered_at for a whole box before the tmux
  //     paste — a daemon exit in between lost all of it.
  //   - the board's GET /mail drain finalized before the browser held the body.
  // The lease (mail.claimed_at = deadline, delivered_at still NULL) takes the
  // row out of every other claim path while it is in flight; delivery is
  // finalized only by explicit ack (the watcher's POST /mail/ack, the board's
  // ack mail_ids, a confirmed tmux Enter) or by the hook drains, whose reply
  // IS the side effect. retentionSweep hands back any lease whose deadline
  // passed, so a dead consumer or a daemon restart re-delivers — no mail can
  // be permanently claimed without acknowledgement ever again.

  // ------------------------------------------------------------------- mail
  // BUG 4: returns {truncated, original_length} so callers that surface a
  // delivery receipt (postMail → POST /mail) can tell the sender when the tail
  // was cut. The clamp itself lives here so every mail entry point — board
  // mail, orchestrator routing, question relays — is bounded identically.
  // BUG-128: returns {refused, reason} instead when the pending budget below
  // would be exceeded — the row is never inserted.
  function mail(toSession, from, text) {
    const raw = String(text ?? '');
    // BUG-128: backpressure. Price the would-be insert against the pending
    // budget BEFORE writing — stats only, never a full mailbox scan. The sum
    // uses the CLAMPED length, i.e. what would actually be stored.
    const stored = clampMail(raw);
    const stats = q.pendingMailStats.get(toSession);
    if (stats.n >= PENDING_MAX || stats.bytes + stored.length > PENDING_MAX_BYTES) {
      return { refused: true, reason: 'mailbox full' };
    }
    q.insertMail.run(toSession, clampFrom(from), stored, Date.now()); // BUG 6/12: surrogate-safe clamps
    // v1.1 mail-wake: ANY mail landing in the mailbox wakes any /api/watch
    // long-poll for that session — board answers, [FLEETDECK ASSIGNMENT]
    // routing and plain board/session mail alike (v1 nudged only on
    // fleetdeck-answer). The poll does its own undelivered check and never
    // claims for an offline session — this is only a nudge, never a delivery.
    notifyWatchers(toSession);
    // A live /api/watch waiter gets first refusal. After the grace window,
    // daemon-owned idle/queued panes gain the second delivery channel.
    // BUG-128: ONE coalesced timer per session, not one per row. Previously
    // every insert armed its own probe, so N queued rows fired N timers that
    // each forked tmux long after the first had drained the queue. The first
    // row's grace deadline stands — later rows ride the same probe; a probe
    // that leaves rows pending (a bounded batch) re-arms itself exactly once.
    armPaneMailTimer(toSession);
    return { truncated: raw.length > MAIL_MAX_LEN, original_length: raw.length };
  }

  // BUG-128: session_id -> the session's one pending grace timer. mail()
  // coalesces every insert onto a single probe, and tryOwnedPaneDelivery
  // re-arms it when a bounded batch left rows pending.
  const paneMailTimers = new Map();
  function armPaneMailTimer(sid) {
    if (paneMailTimers.has(sid)) return;
    const timer = setTimeout(() => {
      paneMailTimers.delete(sid);
      tryOwnedPaneDelivery(sid).catch(() => { /* fail-open; mail stays pending */ });
    }, PANE_MAIL_GRACE_MS);
    timer.unref?.();
    paneMailTimers.set(sid, timer);
  }
  // Re-arm after a batch claim: a probe is presumably RUNNING (this is called
  // from inside it), so drop the stale handle and schedule the next round.
  function rearmPaneMailTimer(sid) {
    paneMailTimers.delete(sid);
    armPaneMailTimer(sid);
  }

  // BUG-034: drainMail SPLIT into claim/finalize phases.
  //   - Hook paths (UserPromptSubmit additionalContext, Stop-block reason)
  //     keep finalizing at claim time: the text leaves the mailbox only as
  //     part of the hook reply that hands it to the session — that reply IS
  //     the acknowledgement, so finalize-at-claim loses nothing.
  //   - The board's GET /mail uses { lease: true } and must hand back
  //     ack_mail_ids once it holds the body; an unacked poll re-delivers when
  //     the lease lapses instead of losing the mail to a mid-response close.
  // `id` now rides every drained item so ack surfaces can name the rows.
  function drainMail(sid, { lease = false } = {}) {
    const now = Date.now();
    const box = q.pendingMail.all(sid, now);
    if (lease) {
      const deadline = now + MAIL_CLAIM_LEASE_MS;
      for (const m of box) q.claimMail.run(deadline, m.id);
    } else {
      for (const m of box) q.markDelivered.run(now, m.id);
    }
    return box.map(m => ({ id: m.id, from: m.from_id, text: m.text, at: m.at }));
  }

  // Explicit acknowledgement for leased mail (watch claim, board /mail GET).
  // The statements guard on delivered_at IS NULL, so a late or double ack
  // settles silently instead of touching a row that already moved on.
  function ackMail(ids) {
    if (!Array.isArray(ids)) return { acked: 0 };
    const now = Date.now();
    let acked = 0;
    for (const id of ids) if (Number.isSafeInteger(id)) acked += Number(q.ackMail.run(now, id).changes);
    return { acked };
  }

  // resolve a /mail "to" target to session ids
  function resolveTargets(to) {
    const all = q.visibleSessions.all();
    const active = all.filter(s => s.ended_at == null);
    const fanout = active.filter(s => s.source !== 'shell');
    if (to === 'all') return fanout.map(s => s.session_id);
    const m = /^repo:(.+)$/.exec(String(to ?? ''));
    if (m) {
      const key = m[1];
      return fanout
        .filter(s => s.repo_id === key || s.repo_name === key)
        .map(s => s.session_id);
    }
    // Direct match: session_id or CURRENT callsign wins. Only when nothing
    // matches there do we fall back to prev_callsign — the birth name a rename
    // left behind, still printed in this session's own brief and every peer's.
    // Fallback ONLY (never merged) so a reissued birth name never double-delivers
    // to both its new holder (matched above by current callsign) and the renamed
    // session that used to wear it. Both scopes are archived_at IS NULL (`all`),
    // so a dead-but-retained tombstone still catches mail to either of its names.
    //
    // Shell cards are excluded HERE, not just in postMail: every caller of this
    // resolver (postMail, and the orchestrator's `assign` in commands.mjs which
    // delivers via the raw mail() insert) must be unable to route text into a
    // shell pane — typed mail into a shell EXECUTES. An assign naming a shell
    // therefore resolves to nothing ("no such session"), and postMail turns the
    // same miss into its loud 409 below.
    const routable = all.filter(s => s.source !== 'shell');
    const direct = routable.filter(s => s.session_id === to || s.callsign === to);
    if (direct.length) return direct.map(s => s.session_id);
    const anchored = routable.filter(s => s.prev_callsign === to);
    if (anchored.length) return anchored.map(s => s.session_id);
    // BUG-107: last fallback — the alias history. A name dropped by ANY
    // supported rename sequence (ticket → re-ticket → clear → re-ticket) still
    // routes to the card that wore it. Same priority discipline as the anchor
    // fallback: reached only when no current name or anchor claims the name,
    // so a reissued name binds to its present holder and never forks.
    return q.aliasesMatch.all(to, to)
      .filter(s => s.source !== 'shell')
      .map(s => s.session_id);
  }

  // ---------------------------------------- F3d-2 /api/watch core surface
  // Consumed by http.mjs GET /api/watch (which documents the full response
  // contract) on behalf of scripts/fleet-watch.mjs, the asyncRewake watcher.
  const watchWaiters = new Map(); // session_id -> Set<fn>

  // BUG-105: per-session watcher generation. fleet-watch.mjs (single-flight,
  // NEWEST WINS) mints a random token per watcher and sends it as `wg` on
  // every /api/watch poll; registerWatchGen (called synchronously from the
  // route before any claim attempt) makes the newest token the current
  // generation. The mail claim then runs ONLY when the request's token still
  // IS the current generation — so a superseded watcher blocked in its poll
  // cannot claim mail out from under its successor even if the mail lands
  // mid-poll. The watcher mirrors this client-side: it re-checks pidfile
  // ownership before acting on any response, so a claim that slipped through
  // an old server's pre-generation code is never acted on (exit 0, not 2).
  // Read-only surfaces (watchInfo, hasWatchWaiter) stay untokened.
  const watchGens = new Map(); // session_id -> current generation token
  function registerWatchGen(sid, token) {
    if (typeof token !== 'string' || !token) return false;
    if (watchGens.get(sid) !== token) watchGens.set(sid, token);
    return true;
  }
  function isWatchGen(sid, token) {
    return typeof token === 'string' && token !== '' && watchGens.get(sid) === token;
  }

  function notifyWatchers(sid) {
    for (const fn of [...(watchWaiters.get(sid) ?? [])]) {
      try { fn(); } catch { /* a dead waiter must not break the notifier */ }
    }
  }

  // Register a nudge callback for a session's watch long-polls. Returns the
  // unregister function. Callbacks fire on ANY mail insert (v1.1 mail-wake)
  // and SessionEnd; they carry NO payload — the poll re-runs its own
  // undelivered check.
  function addWatchWaiter(sid, fn) {
    if (!watchWaiters.has(sid)) watchWaiters.set(sid, new Set());
    watchWaiters.get(sid).add(fn);
    return () => {
      const set = watchWaiters.get(sid);
      if (set) { set.delete(fn); if (!set.size) watchWaiters.delete(sid); }
    };
  }

  function hasWatchWaiter(sid) {
    return (watchWaiters.get(sid)?.size ?? 0) > 0;
  }

  function ownedPaneRow(sid) {
    const c = q.getSession.get(sid);
    if (!c || c.source === 'shell' || c.ended_at != null || !['queued', 'idle'].includes(c.col)) return null;
    const sp = q.spawnBySession.get(sid);
    if (!sp || !['spawning', 'stalled', 'live'].includes(sp.status)) return null;
    return { c, sp };
  }

  // Cheap mode is used only by snapshots and is explicitly approximate: a
  // qualifying spawn row implies a potentially deliverable owned pane, but
  // /state never forks tmux merely to render mail metadata.
  async function ownedPaneDeliverable(sid, { probe = true } = {}) {
    const pair = ownedPaneRow(sid);
    if (!pair) return false;
    if (!probe) return true;
    const win = await findScopedWindow(pair.sp.tmux_window);
    if (win === null) return false; // tmux lookup UNKNOWN: hold, never infer absence
    if (!win || win.pane_dead) return false;
    const pane = await tmuxAdapter.paneCurrentCommand(scopedPaneTarget(win));
    return !!pane && !pane.dead && pane.cmd === 'claude';
  }

  // BUG-034: claimAllMail now LEASES — it commits claimed_at (the lease
  // deadline), NOT delivered_at. The owned-pane path below finalizes delivery
  // only after Enter is confirmed, and releases the lease on any failure —
  // including the daemon dying between commit and paste, where the passed
  // deadline re-opens the rows for the next delivery path.
  // BUG-128: one bounded BATCH, not the whole mailbox. Claims at most
  // MAIL_PANE_BATCH rows AND MAIL_PANE_BATCH_BYTES of text (whichever bites
  // first), atomically; anything past the batch stays pending for the next
  // delivery round. Rows are oldest-first, so a batch boundary can only ever
  // leave a TAIL pending, never starve an old row. The +1 page row lets the
  // caller tell "empty after this" from "more waiting" without a count query.
  function claimAllMail(sid) {
    db.exec('BEGIN IMMEDIATE');
    try {
      const now = Date.now();
      const page = q.pendingMailPage.all(sid, now, PANE_BATCH + 1);
      const batch = [];
      let bytes = 0;
      for (const m of page) {
        if (batch.length >= PANE_BATCH || (batch.length && bytes + m.text.length > PANE_BATCH_BYTES)) break;
        batch.push(m);
        bytes += m.text.length;
      }
      const deadline = now + MAIL_CLAIM_LEASE_MS;
      for (const m of batch) q.claimMail.run(deadline, m.id);
      db.exec('COMMIT');
      return { batch, remaining: page.length > batch.length };
    } catch (err) {
      try { db.exec('ROLLBACK'); } catch { /* preserve original error */ }
      throw err;
    }
  }

  async function tryOwnedPaneDelivery(sid) {
    const pair = ownedPaneRow(sid);                         // session + spawn
    if (!pair || hasWatchWaiter(sid)) return false;         // watcher priority
    const win = await findScopedWindow(pair.sp.tmux_window); // live scoped pane
    if (win === null) return false;                         // UNKNOWN: leave mail queued
    if (!win || win.pane_dead) return false;
    const target = scopedPaneTarget(win);
    const pane = await tmuxAdapter.paneCurrentCommand(target);
    if (!pane || pane.dead || pane.cmd !== 'claude') return false;

    // Re-check waiter priority after the asynchronous probes, then atomically
    // claim every pending row before any text enters the pane.
    if (hasWatchWaiter(sid)) return false;
    // BUG 8: close the owned-pane TOCTOU. The eligibility gate at the top
    // (ownedPaneRow) read this session's turn-state/col BEFORE the awaited
    // findScopedWindow + paneCurrentCommand probes. During those awaits a
    // PermissionRequest/Notification hook can flip the card out of idle/queued
    // into needs-you/working — pasting now would inject text + Enter into a
    // permission or question TUI. Re-read the row FRESH through the same gate
    // and bail (claiming nothing) unless it is still an idle/queued owned pane.
    if (!ownedPaneRow(sid)) return false;
    const { batch: box, remaining } = claimAllMail(sid);
    if (!box.length) return false;
    // BUG-128: one bounded batch per paste. When rows remain pending, re-arm
    // exactly one grace timer so the rest follows in later rounds (and a burst
    // still coalesces to one probe per batch instead of one per row).
    if (remaining) rearmPaneMailTimer(sid);
    const text = box.map(m => `[FLEETDECK MAIL from ${m.from_id}] ${m.text}`).join('\n');
    const pasted = await tmuxAdapter.pasteText(target, text);
    if (!pasted) {                       // paste failed → redeliver at a later turn
      for (const m of box) q.releaseClaim.run(m.id);
      onMutate();
      return false;
    }
    // BUG 8 (last mile): the pasteText round-trip above yielded the event loop, so
    // re-read turn-state ONE more time before pressing Enter. If a hook flipped the
    // pane to needs-you/working in that window, do NOT auto-SUBMIT: the (sanitized,
    // bounded) text is already in the pane, but Enter would fire it into a
    // permission/question TUI. Leave it un-entered — recoverable — and keep it
    // marked delivered so it is never re-pasted.
    if (!ownedPaneRow(sid)) {
      const now = Date.now();
      for (const m of box) q.ackMail.run(now, m.id); // pasted = side effect landed
      onMutate();
      return true;
    }
    const entered = await tmuxAdapter.sendEnter(target);
    // BUG-033 × BUG-034: once pasteText succeeded the text is already IN the
    // pane, so a failed/uncertain Enter must NOT requeue the rows — re-pasting
    // would submit the same text twice (duplicated prompts, repeated
    // non-idempotent side effects). Under the lease model claimAllMail only
    // LEASED these rows, so a bare return would leave them claimed-but-unacked
    // and retentionSweep would later hand them back and re-deliver (a delayed
    // re-paste). FINALIZE delivery instead: the paste itself is the side effect
    // that landed, exactly like the "pane flipped" branch above. The pre-paste
    // releaseClaim remains the only requeue path.
    if (!entered) {
      const now = Date.now();
      for (const m of box) q.ackMail.run(now, m.id); // pasted = side effect landed; never re-paste
      logEvent(sid, 'MailPaneEnterFailed', null,
        `pasted ${box.length} mail into ${pair.sp.tmux_window} but Enter failed — left un-entered, NOT requeued (text already in pane)`);
      onMutate();
      return false;
    }
    // Enter confirmed: the text is submitted to the agent — THAT is the
    // acknowledgement for the pane channel. Finalize delivery only now.
    {
      const now = Date.now();
      for (const m of box) q.ackMail.run(now, m.id);
    }
    tick(`✉ delivered ${box.length} mail to ${pair.c.callsign} (typed into pane)`);
    logEvent(sid, 'MailPaneDelivery', null, `typed ${box.length} mail into ${pair.sp.tmux_window}`);
    onMutate();
    return true;
  }

  // ATOMIC claim of the oldest undelivered mail for a session — ANY sender
  // (/api/watch v2; v1 claimed board answers only). mail.delivered_at is THE
  // single source of truth for delivery, claimed_at (BUG-034) the in-flight
  // lease: this claim, the UserPromptSubmit drain, the Stop-block drain and
  // GET /mail all run synchronously on the daemon's only thread and all
  // filter on delivered_at IS NULL plus a live-lease check — whichever runs
  // first wins, and expired rows are excluded everywhere. No mail can ever
  // be delivered twice, and (new) no mail can be lost to a claim whose
  // consumer never acknowledged it: the claim below only LEASES the row; the
  // watcher acks with POST /mail/ack once it holds the body, and a claim
  // whose deadline passes becomes claimable again.
  // `text` is returned RAW, its own frame included ([FLEETDECK ANSWER] …,
  // [FLEETDECK ASSIGNMENT] …, or plain board/session mail) — v2's
  // rewakeMessage is neutral, so each mail must carry its own frame.
  // BUG-105: when a generation token is supplied it must still be the current
  // generation at claim time (registerWatchGen runs first, synchronously, in
  // the same tick); a superseded watcher's in-flight poll claims nothing.
  function claimMail(sid, gen = null) {
    if (gen !== null && !isWatchGen(sid, gen)) return null;
    const now = Date.now();
    const m = q.nextMail.get(sid, now);
    if (!m) return null;
    q.claimMail.run(now + MAIL_CLAIM_LEASE_MS, m.id);
    onMutate();
    return { mail_id: m.id, at: m.at, from: m.from_id, text: m.text };
  }

  function watchInfo(sid) {
    const c = q.getSession.get(sid);
    return {
      session_alive: !!c && c.ended_at == null,
      // Informational in v2: the watcher keeps polling while session_alive
      // is true even at pending:0, because mail can arrive for an idle
      // session at any time. Still counts FREEFORM questions only —
      // permission/elicitation/choice answers ride their held hook response
      // and never become mail (a choice whose hold expired belongs to the
      // native terminal chooser permanently; a late board answer 409s).
      pending: questions.pendingOf(sid).filter(r => r.kind === 'freeform').length,
    };
  }

  async function postMail({ to, from, text }) {
    // BUG-037: resolve the FINAL sender FIRST. The old flow validated the raw
    // input and defaulted LATER (`from || 'human'`), so an omitted/empty/zero/
    // false `from` sailed past the reserved check and was then stored — row and
    // pane envelope both — as the reserved `human` identity. Resolve first,
    // validate the resolved value, and default to the non-reserved 'board'.
    // External callers never wear the daemon's identities or its authority
    // frames — see RESERVED_SENDERS above. 422 like every other body rejection.
    const sender = from ?? 'board';
    if (typeof sender !== 'string' || sender.length === 0) {
      return { status: 422, body: { ok: false, reason: 'sender name must be a non-empty string' } };
    }
    // BUG-032: compare the Cf-stripped sender, so "human​" (zero-width space)
    // can't stand in for a reserved name. Cf characters are themselves refused
    // by FROM_UNSAFE_RE below; this catches the ones that would have mattered.
    if (RESERVED_SENDERS.has(stripFormatChars(sender).toLowerCase())) {
      return { status: 422, body: { ok: false, reason: `sender name '${from}' is reserved for the daemon` } };
    }
    if (FROM_UNSAFE_RE.test(sender)) {
      return { status: 422, body: { ok: false, reason: 'sender name may not contain control characters, newlines, or [ ] delimiters' } };
    }
    if (hasReservedFrame(text ?? '')) {
      return { status: 422, body: { ok: false, reason: 'mail text may not open any line with a [FLEETDECK ...] frame — those are reserved for the daemon' } };
    }
    // A direct send whose name belongs to a shell card is refused LOUDLY (mail
    // typed into a shell executes). Same current-name-wins priority as
    // resolveTargets: only when the CURRENT match set (session_id / callsign)
    // is empty does prev_callsign count — so a shell's abandoned birth name
    // never blocks a live claude that now wears it, and a reissued name
    // resolves to its present holder. The 409 fires only when everything the
    // name resolves to is a shell; resolveTargets below independently refuses
    // to route to shells, so this is the human-facing message, not the wall.
    const everyone = q.visibleSessions.all();
    const currentMatch = everyone.filter(s => s.session_id === to || s.callsign === to);
    const anchoredMatch = currentMatch.length ? currentMatch : everyone.filter(s => s.prev_callsign === to);
    // Mirror resolveTargets' last fallback (BUG-107): an alias-history name
    // held only by shells is still a loud refusal, not a silent drop.
    const namedByTo = anchoredMatch.length ? anchoredMatch : q.aliasesMatch.all(to, to);
    if (namedByTo.length && namedByTo.every(s => s.source === 'shell')) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `${namedByTo[0].callsign} is a shell pane — mail would be typed into a shell`,
        },
      };
    }
    const targets = resolveTargets(to);
    // Report delivery truth from the state immediately before insertion: a
    // live waiter wakes instantly ('watcher'), a verified owned Claude pane
    // gets typed into ('pane'); otherwise the mail is honestly queued for a
    // later turn ('turn-boundary') or a future --resume ('offline-queued').
    const routes = await Promise.all(targets.map(async sid => {
      if (hasWatchWaiter(sid)) return 'watcher';
      if (await ownedPaneDeliverable(sid)) return 'pane';
      return q.getSession.get(sid)?.ended_at != null ? 'offline-queued' : 'turn-boundary';
    }));
    // BUG-128: the per-mailbox pending budget can refuse an insert. That
    // refusal is loud, never silent: if EVERY fanout target refused, the send
    // 429s so the caller knows nothing landed; a partial fanout reports the
    // refused targets with route 'refused'.
    const outcomes = targets.map(sid => mail(sid, sender, text));
    const refusedAll = targets.length > 0 && outcomes.every(o => o.refused);
    if (refusedAll) {
      return {
        status: 429,
        body: {
          ok: false,
          reason: 'recipient mailbox is full — too much pending mail',
          pending_max: PENDING_MAX,
          pending_max_bytes: PENDING_MAX_BYTES,
        },
      };
    }
    tick(`✉ mail from ${sender} → ${to}`);
    onMutate();
    // BUG 4: report truncation to the sender. All targets receive the same
    // text and share MAIL_MAX_LEN, so the clamp is computed once from the raw
    // body (this also stays honest when there are zero targets). http.mjs's
    // /mail handler passes this object through verbatim (json(res, 200, out)),
    // so the flag surfaces without any change there — see coordination note.
    const raw = String(text ?? '');
    const truncated = raw.length > MAIL_MAX_LEN;
    return {
      ok: true,
      delivered: outcomes.filter(o => !o.refused).length,
      targets: targets.map((sid, i) => ({
        session_id: sid,
        callsign: q.getSession.get(sid)?.callsign ?? null,
        route: outcomes[i].refused ? 'refused' : routes[i],
      })),
      ...(truncated ? { truncated: true, original_length: raw.length, max_length: MAIL_MAX_LEN } : {}),
    };
  }

  return {
    mail, drainMail, ackMail, resolveTargets,
    notifyWatchers, addWatchWaiter, hasWatchWaiter,
    ownedPaneRow, ownedPaneDeliverable, tryOwnedPaneDelivery,
    claimMail, watchInfo, postMail, registerWatchGen,
  };
}
