// Thin wrappers over the fleetd board/control API.
//
// LAN mode (v1.7): every request carries `Authorization: Bearer <token>` when a
// token is known. Loopback daemons ignore it; a LAN daemon refuses without it.
// A 401 anywhere latches the board's unauthorized state (token.js) and App
// swaps the board for the token gate — one clear state, however many calls fail.

import { authHeaders, markUnauthorized } from './token.js';
import { apiUrl } from './base.js';

// M-F1/H-X2 — the daemon speaks two error dialects ({reason} and the older
// {err}), and every caller used to hand-write the same `json?.reason ||
// json?.err || 'HTTP …'` dance (~16 of them). This is that dance, once. It also
// reads the top-level `reason` the request helpers set for a network failure,
// so "daemon unreachable" flows through the same path as a 4xx reason.
export function reasonOf(res, fallback) {
  if (!res) return fallback || 'daemon unreachable';
  return res.reason
    || res.json?.reason
    || res.json?.err
    || fallback
    || `HTTP ${res.status}`;
}

// One request path for both verbs. It NEVER rejects: a network drop, an abort,
// or the 15 s timeout come back as {ok:false, status:0, reason:'daemon
// unreachable'} — the same shape a 4xx does — so callers branch on `res.ok`
// and read `reasonOf(res)` without a parallel try/catch for the transport.
async function request(url, init) {
  let res;
  try {
    // Callers still write root-relative paths ('/api/spawn'); apiUrl resolves
    // them against the board's actual base so a path-based proxy works.
    res = await fetch(apiUrl(url), { ...init, signal: AbortSignal.timeout(15000) });
  } catch {
    // DNS/refused/reset/timeout/abort — the daemon is unreachable, not a status
    return { ok: false, status: 0, json: null, reason: 'daemon unreachable' };
  }
  if (res.status === 401) markUnauthorized();
  let json = null;
  try { json = await res.json(); } catch { /* non-JSON error body */ }
  return {
    ok: res.ok,
    status: res.status,
    json,
    reason: json?.reason || json?.err || null,
  };
}

function post(url, body) {
  return request(url, {
    method: 'POST',
    headers: authHeaders({ 'content-type': 'application/json' }),
    body: JSON.stringify(body ?? {}),
  });
}

// Same shape as post() — for GETs whose failure the CALLER must show (unlike
// /state, which fails silently into the LIVE pill).
function get(url) {
  return request(url, { headers: authHeaders() });
}

// GET /state — the board's paint-and-poll snapshot. Returns null on any
// failure (401 included: the gate, not the caller, reports that one).
export async function fetchState() {
  let res;
  try {
    res = await fetch(apiUrl('/state'), { headers: authHeaders(), signal: AbortSignal.timeout(15000) });
  } catch {
    return null; // unreachable/timeout — the WS retry loop owns recovery
  }
  if (res.status === 401) { markUnauthorized(); return null; }
  if (!res.ok) return null;
  try { return await res.json(); } catch { return null; }
}

export function answerQuestion(id, body) {
  return post(`/api/questions/${id}/answer`, body);
}

// v1.8 — dismiss a stale NEEDS YOU card (empty body). The daemon marks the
// question expired and sends the session NOTHING: this is for the question you
// already handled in the terminal, not an answer. 200 {ok:true} or 4xx
// {ok:false, reason}.
export function dismissQuestion(id) {
  return post(`/api/questions/${id}/dismiss`);
}

// to = session_id | callsign | 'all' | 'repo:<name>'
export function sendMail(to, text) {
  return post('/mail', { to, from: 'board', text });
}

export function sendCommand(text) {
  return post('/command', { text });
}

// v1.2 — board-spawned sessions. Spawn is an explicit human click ONLY.
// v1.6: body may carry remote_control:true — remote control on from birth.
export function spawnSession(body) {
  return post('/api/spawn', body);
}

export function killSpawn(spawnId, force) {
  return post(`/api/spawn/${encodeURIComponent(spawnId)}/kill`, { force: !!force });
}

// v1.3 — plan library. body = {status:'executed', via} | {status:'archived'};
// the daemon 409s bad transitions and the board surfaces that honestly.
export function markPlan(planId, body) {
  return post(`/api/plans/${encodeURIComponent(planId)}/mark`, body);
}

// Manual cleanup: archive offline cards, expire their undelivered mail, kill
// dead scoped panes. Responds {ok, archived, mail_expired, windows_killed,
// orphan_worktrees} — orphans are LISTED, never deleted by the daemon.
export function cleanup() {
  return post('/api/cleanup');
}

// v1.5 — revive a dead board-spawned agent whose worktree + transcript
// survive (spawn.revivable). 200 {ok:true, spawn_id, session_id,
// callsign, tmux:{session,window}} or 4xx {ok:false, reason}. On success the
// daemon moves the card to QUEUED — the board only fires and shows failures.
// v1.6: optional remoteControl boolean rides along as {remote_control};
// omitted (the default) means the revived agent inherits whatever the dead
// spawn had — today the daemon always inherits and ignores the override, so
// this is forward-compat plumbing, not a live affordance.
export function reviveSpawn(spawnId, remoteControl) {
  const body = typeof remoteControl === 'boolean' ? { remote_control: remoteControl } : {};
  return post(`/api/spawn/${encodeURIComponent(spawnId)}/revive`, body);
}

// v1.6 — enable remote control (/rc) on a RUNNING board-spawned session.
// Empty body. The daemon types /rc into the pane and harvests the claude.ai
// link, so the round-trip runs ~3-6 s — callers show a busy state. 200
// {ok:true, enabled:true, url: string|null, pending?: bool} (url null while
// the link is still being harvested) or {ok:false, reason} — 409 when the
// session is mid-turn ("busy") or the pane isn't live.
export function enableRemote(spawnId) {
  return post(`/api/spawn/${encodeURIComponent(spawnId)}/rc`);
}

// v2.0 "Move to tmux" — adopt a session the board did NOT spawn (source
// 'hooks'/'agents-cli') into a board-owned tmux pane. POST
// /api/sessions/<session_id>/adopt; the daemon is context-sensitive, so the
// BODY picks the intent and the response says which thing happened:
//   {} | {dangerously_skip_permissions:true}
//     · on an ENDED session → immediate adopt:
//         200 {ok:true, adopted:true, spawn_id, session_id, callsign,
//              tmux:{session, window}}
//     · on a LIVE session → ARM it (two processes can't drive one conversation,
//       so the move is deferred to the session's own SessionEnd):
//         200 {ok:true, armed:true, expires_at}
//       Re-arming just refreshes the deadline (idempotent), and the skip flag is
//       remembered for the deferred launch.
//   {disarm:true} → 200 {ok:true, armed:false}
// Refusals are {ok:false, reason} with 404/409/410 — 409 covers a still-alive
// session presumed dead ("presumed dead only — arm it instead") and an existing
// active spawn. On a successful adopt the daemon moves the card to QUEUED
// ("moving to tmux…"); the board only fires and reports the outcome (or the
// failure reason) on the shared feedback strip.
export function adoptSession(sessionId, body) {
  return post(`/api/sessions/${encodeURIComponent(sessionId)}/adopt`, body);
}

// v2.1 "Rename" (0.7.1) — give a session a custom callsign SUFFIX. The animal is
// minted by the daemon and is never chosen, so a rename only ever replaces the
// half after the first dash: 'wren-a9e1' → 'wren-docs-review'. POST
// /api/sessions/<session_id>/name — the BODY picks the intent:
//   {suffix: 'docs-review'} → 200 {ok:true, renamed:true,
//                                  callsign:'wren-docs-review', previous:'wren-a9e1'}
//   {clear: true}           → revert to the AUTOMATIC name (the ticket name when
//                             the session has a ticket, else the hex name) —
//                             same success shape
// `renamed:false` is a SUCCESS, not a failure: it means the name did not change
// (clearing a name that was already automatic), and the board says so plainly.
// Refusals are {ok:false, reason:'<human sentence>'} with 400 (bad charset,
// reserved word), 404 (no such session) or 409 (name already taken) — the reason
// is a finished sentence and is shown verbatim on the feedback strip.
// Valid suffix: ^[A-Za-z0-9][A-Za-z0-9-]{0,23}$ — validSuffix() in util.js is the
// client-side copy of that rule (see the WHY it can't be loosened there), but the
// DAEMON is the authority and gets the last word on every name.
export function renameSession(sessionId, body) {
  return post(`/api/sessions/${encodeURIComponent(sessionId)}/name`, body);
}

// v1.9 — the worktrees a spawn left behind. The daemon does the judging (the
// board must never guess whether a directory is safe to delete): each row
// arrives with a verdict and the evidence that produced it —
//   safe      nothing would be lost (clean, and merged or fully pushed)
//   has-work  dirty > 0 or unpushed > 0 — removing it destroys something
//   gone      the path is no longer there; only a stale row survives
//   unknown   git could not answer — dangerous, NEVER treated as safe
// 200 {ok:true, worktrees:[{path, callsign, session_id, session_alive,
// spawn_status, exists, branch, base, dirty, dirty_files, ahead, upstream,
// unpushed, merged, last_commit, verdict}]}. 404 on a daemon that predates the
// endpoint — the board hides the whole affordance rather than dangling it.
export function fetchWorktrees() {
  return get('/api/worktrees');
}

// Remove a worktree (and optionally its branch). The daemon REFUSES a
// 'has-work' worktree without force:true, and refuses ANY worktree whose
// session is still alive — so force:true only ever leaves the board from the
// confirmation dialog, which first spells out what it destroys.
// 200 {ok:true, removed:true, branch_deleted, rows_purged, path} or
// 4xx {ok:false, reason, verdict, dirty, unpushed} — the reason is shown verbatim.
export function removeWorktree(path, { force = false, deleteBranch = false } = {}) {
  return post('/api/worktrees/remove', {
    path,
    force: !!force,
    delete_branch: !!deleteBranch,
  });
}
