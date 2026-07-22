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
const MAIL_MAX_LEN = 4000;
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

// 0.16.0 SENDER/FRAME RESERVATION. The fleet doctrine teaches agents to treat
// [FLEETDECK ...] frames and the daemon's own sender names as carrying human
// authority — so they must be unforgeable. Only the daemon's internal mail()
// callers may send them; postMail (the external API) is forced unprivileged:
// reserved senders 422, and a reserved frame prefix at the start of the text
// 422s as well (a frame MID-text renders as mail content, not an envelope, so
// only the leading position is checked). Ordinary callsign/session-id senders
// and plain text are unaffected.
const RESERVED_SENDERS = new Set(['orchestrator', 'fleetdeck', 'fleetdeck-answer', 'human']);
// Leading whitespace AND control/zero-width characters: a frame smuggled past
// as "\x00[FLEETDECK ANSWER]" renders identically in a pane to the real one.
// eslint-disable-next-line no-control-regex
const RESERVED_FRAME_RE = /^[\s\x00-\x1f\x7f-\x9f]*\[FLEETDECK[ \]]/i;
// The pane envelope is a single line (`[FLEETDECK MAIL from <from>] <text>`):
// a newline in `from` lets the text forge a line-two frame. Control chars are
// already stripped from pane-bound text by sanitizePaneText, but `from` rides
// inside the same paste — refuse them at the door instead.
// eslint-disable-next-line no-control-regex
const FROM_UNSAFE_RE = /[\r\n\x00-\x1f\x7f-\x9f]/;

export function createMail(ctx) {
  const {
    db, q, tick, logEvent, onMutate, questions, tmuxAdapter,
    findScopedWindow, scopedPaneTarget, PANE_MAIL_GRACE_MS,
  } = ctx;

  // ------------------------------------------------------------------- mail
  // BUG 4: returns {truncated, original_length} so callers that surface a
  // delivery receipt (postMail → POST /mail) can tell the sender when the tail
  // was cut. The clamp itself lives here so every mail entry point — board
  // mail, orchestrator routing, question relays — is bounded identically.
  function mail(toSession, from, text) {
    const raw = String(text ?? '');
    q.insertMail.run(toSession, clampFrom(from), clampMail(raw), Date.now()); // BUG 6/12: surrogate-safe clamps
    // v1.1 mail-wake: ANY mail landing in the mailbox wakes any /api/watch
    // long-poll for that session — board answers, [FLEETDECK ASSIGNMENT]
    // routing and plain board/session mail alike (v1 nudged only on
    // fleetdeck-answer). The poll does its own undelivered check and never
    // claims for an offline session — this is only a nudge, never a delivery.
    notifyWatchers(toSession);
    // A live /api/watch waiter gets first refusal. After the grace window,
    // daemon-owned idle/queued panes gain the second delivery channel.
    const timer = setTimeout(() => {
      tryOwnedPaneDelivery(toSession).catch(() => { /* fail-open; mail stays pending */ });
    }, PANE_MAIL_GRACE_MS);
    timer.unref?.();
    return { truncated: raw.length > MAIL_MAX_LEN, original_length: raw.length };
  }

  function drainMail(sid) {
    const box = q.pendingMail.all(sid);
    const now = Date.now();
    for (const m of box) q.markDelivered.run(now, m.id);
    return box.map(m => ({ from: m.from_id, text: m.text, at: m.at }));
  }

  // resolve a /mail "to" target to session ids
  function resolveTargets(to) {
    const all = q.visibleSessions.all();
    const active = all.filter(s => s.ended_at == null);
    if (to === 'all') return active.map(s => s.session_id);
    const m = /^repo:(.+)$/.exec(String(to ?? ''));
    if (m) {
      const key = m[1];
      return active
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
    const direct = all.filter(s => s.session_id === to || s.callsign === to);
    if (direct.length) return direct.map(s => s.session_id);
    return all.filter(s => s.prev_callsign === to).map(s => s.session_id);
  }

  // ---------------------------------------- F3d-2 /api/watch core surface
  // Consumed by http.mjs GET /api/watch (which documents the full response
  // contract) on behalf of scripts/fleet-watch.mjs, the asyncRewake watcher.
  const watchWaiters = new Map(); // session_id -> Set<fn>

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
    if (!c || c.ended_at != null || !['queued', 'idle'].includes(c.col)) return null;
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

  function claimAllMail(sid) {
    db.exec('BEGIN IMMEDIATE');
    try {
      const box = q.pendingMail.all(sid);
      const now = Date.now();
      for (const m of box) q.markDelivered.run(now, m.id);
      db.exec('COMMIT');
      return box;
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
    const box = claimAllMail(sid);
    if (!box.length) return false;
    const text = box.map(m => `[FLEETDECK MAIL from ${m.from_id}] ${m.text}`).join('\n');
    const pasted = await tmuxAdapter.pasteText(target, text);
    if (!pasted) {                       // paste failed → redeliver at a later turn
      for (const m of box) q.unmarkDelivered.run(m.id);
      onMutate();
      return false;
    }
    // BUG 8 (last mile): the pasteText round-trip above yielded the event loop, so
    // re-read turn-state ONE more time before pressing Enter. If a hook flipped the
    // pane to needs-you/working in that window, do NOT auto-SUBMIT: the (sanitized,
    // bounded) text is already in the pane, but Enter would fire it into a
    // permission/question TUI. Leave it un-entered — recoverable — and keep it
    // marked delivered so it is never re-pasted.
    if (!ownedPaneRow(sid)) { onMutate(); return true; }
    const entered = await tmuxAdapter.sendEnter(target);
    if (!entered) {
      for (const m of box) q.unmarkDelivered.run(m.id);
      onMutate();
      return false;
    }
    tick(`✉ delivered ${box.length} mail to ${pair.c.callsign} (typed into pane)`);
    logEvent(sid, 'MailPaneDelivery', null, `typed ${box.length} mail into ${pair.sp.tmux_window}`);
    onMutate();
    return true;
  }

  // ATOMIC claim of the oldest undelivered mail for a session — ANY sender
  // (/api/watch v2; v1 claimed board answers only). mail.delivered_at is THE
  // single source of truth for delivery: this claim, the UserPromptSubmit
  // drain, the Stop-block drain and GET /mail all run synchronously on the
  // daemon's only thread and all filter on delivered_at IS NULL — whichever
  // runs first wins, and expired rows are excluded everywhere. No mail can
  // ever be delivered twice.
  // `text` is returned RAW, its own frame included ([FLEETDECK ANSWER] …,
  // [FLEETDECK ASSIGNMENT] …, or plain board/session mail) — v2's
  // rewakeMessage is neutral, so each mail must carry its own frame.
  function claimMail(sid) {
    const m = q.nextMail.get(sid);
    if (!m) return null;
    q.markDelivered.run(Date.now(), m.id);
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
    // External callers never wear the daemon's identities or its authority
    // frames — see RESERVED_SENDERS above. 422 like every other body rejection.
    if (from != null && RESERVED_SENDERS.has(String(from).toLowerCase())) {
      return { status: 422, body: { ok: false, reason: `sender name '${from}' is reserved for the daemon` } };
    }
    if (from != null && FROM_UNSAFE_RE.test(String(from))) {
      return { status: 422, body: { ok: false, reason: 'sender name may not contain control characters or newlines' } };
    }
    if (RESERVED_FRAME_RE.test(String(text ?? ''))) {
      return { status: 422, body: { ok: false, reason: 'mail text may not open with a [FLEETDECK ...] frame — those are reserved for the daemon' } };
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
    targets.forEach(sid => mail(sid, from || 'human', text));
    tick(`✉ mail from ${from || 'human'} → ${to}`);
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
      delivered: targets.length,
      targets: targets.map((sid, i) => ({
        session_id: sid,
        callsign: q.getSession.get(sid)?.callsign ?? null,
        route: routes[i],
      })),
      ...(truncated ? { truncated: true, original_length: raw.length, max_length: MAIL_MAX_LEN } : {}),
    };
  }

  return {
    mail, drainMail, resolveTargets,
    notifyWatchers, addWatchWaiter, hasWatchWaiter,
    ownedPaneRow, ownedPaneDeliverable, tryOwnedPaneDelivery,
    claimMail, watchInfo, postMail,
  };
}
