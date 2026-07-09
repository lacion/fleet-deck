// http.mjs — fleetd HTTP + WebSocket surface.
//
// Hook endpoints answer with hook-output JSON directly; every
// handler fails open — an internal error still returns 200 {} so a hook can
// never break a session. Board/control API: /health /state /mail /command,
// static board at / + /assets/* (built React app from board-dist), the spike
// board at /plain, and WS /ws (snapshot on connect, on every mutation, 5 s
// heartbeat).

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer } from 'ws';

const MAX_BODY = 1e6;

// ------------------------------------------------------------ board static
// GET / and /assets/* serve the built React board from board-dist, resolved
// relative to THIS file's directory at runtime — the esbuild bundle keeps
// import.meta.url pointing at scripts/fleetd/, so both the source run
// (fleetd.mjs) and the bundle run (fleetd.bundle.mjs) find the same dist.
// The spike board stays available verbatim at GET /plain.
const BOARD_DIST = path.join(path.dirname(fileURLToPath(import.meta.url)), 'board-dist');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.txt': 'text/plain; charset=utf-8',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
};

// Serve one file from board-dist. Traversal-safe: the decoded request path is
// resolved against BOARD_DIST and must stay strictly inside it (any '..' —
// raw or percent-encoded — normalizes outside and 404s).
function serveBoardAsset(res, pathname, notFound) {
  let decoded;
  try { decoded = decodeURIComponent(pathname); } catch { return notFound(); }
  const rel = decoded === '/' ? 'index.html' : decoded.replace(/^\/+/, '');
  const abs = path.resolve(BOARD_DIST, rel);
  if (abs !== BOARD_DIST && !abs.startsWith(BOARD_DIST + path.sep)) return notFound();
  let data;
  try { data = fs.readFileSync(abs); } catch { return notFound(); }
  res.writeHead(200, {
    'content-type': MIME[path.extname(abs).toLowerCase()] || 'application/octet-stream',
    'content-length': data.length,
  });
  return res.end(data);
}

export function createHttp(core, { port, boardFile, version = '0.0.0', capture = () => {} }) {
  function json(res, code, obj) {
    const body = JSON.stringify(obj);
    res.writeHead(code, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
    res.end(body);
  }

  // PermissionRequest / Elicitation / AskUserQuestion are handled OUT of this
  // table (Phase 3/4 hold-open relay — the response is parked, see the hook
  // branch below).
  const hookHandlers = {
    SessionStart: ev => core.hookSessionStart(ev),
    UserPromptSubmit: ev => core.hookUserPromptSubmit(ev),
    PostToolUse: ev => core.hookPostToolUse(ev),
    PreToolUse: ev => core.hookPostToolUse(ev), // same derivation branch as the spike
    Stop: ev => core.hookStop(ev),
    SessionEnd: ev => core.hookSessionEnd(ev),
    Notification: ev => (core.applyEvent({ ...ev, hook_event_name: 'Notification' }), {}),
    FileChanged: ev => (core.applyEvent({ ...ev, hook_event_name: 'FileChanged' }), {}),
  };

  // F3a/F3b/F3c hold-open relay: create the durable question row, then park
  // the HTTP response until the board answers, the hold window lapses
  // (respond {} — normal flow resumes in the terminal), or the client
  // disconnects. questions.mjs owns the arbitration; this only wires the
  // socket to it. Fail open like every hook path: intake errors still 200 {}.
  function holdHook(res, ev, name) {
    let row = null;
    try { row = core.hookHoldQuestion(ev, name); } catch (err) { console.error('fleetd hold intake error:', err); }
    if (!row) return json(res, 200, {});
    core.questions.attachHold(row, obj => json(res, 200, obj));
    res.on('close', () => {
      try { core.questions.socketClosed(row.id); } catch { /* hold hygiene only */ }
    });
    // response intentionally left open
  }

  // GET /api/watch v2 — long-poll consumed by scripts/fleet-watch.mjs (the
  // asyncRewake watcher). v2 (orchestrator routing + mail-wake): claims mail
  // from ANY sender, not just board answers, and the watcher stays alive on
  // session_alive alone.
  //
  //   GET /api/watch?session=<sid>[&hold_ms=<0..25000>]   → always 200 JSON
  //
  //   {status:'mail', mail_id, at, from, text}
  //     The OLDEST undelivered mail for <sid> — from ANY sender — existed
  //     (or arrived during the hold) and was ATOMICALLY claimed by this
  //     response: the mail row's delivered_at is set in the same synchronous
  //     step that resolves the poll, so the turn-boundary path
  //     (UserPromptSubmit/Stop-block/GET /mail drains, which all filter
  //     delivered_at IS NULL) can never re-deliver it. `text` is the RAW
  //     mail text including its own frame ([FLEETDECK ANSWER] …,
  //     [FLEETDECK ASSIGNMENT] …, or plain board/session mail) — no prefix
  //     stripping; the Stop hook's rewakeMessage is neutral in v2 and each
  //     mail carries its own frame. `from` is the sender id
  //     (fleetdeck-answer, orchestrator, human, a callsign, …).
  //   {status:'idle', session_alive, pending}
  //     Nothing deliverable. Sent IMMEDIATELY when the session is offline or
  //     unknown (session_alive:false → watcher must exit 0; any queued mail
  //     is deliberately NOT claimed so a resumed session still gets it at
  //     its first turn boundary). For a LIVE session the poll always holds —
  //     even at pending:0, because mail can arrive for an idle session at
  //     any time — and this is sent when hold_ms (default and max 25 s)
  //     lapses with no mail; the watcher keeps polling while session_alive
  //     is true. `pending` counts pending FREEFORM questions only
  //     (informational in v2 — no longer a watcher exit condition).
  //
  //   Waiter nudges fire on ANY mail insert and on SessionEnd (derive.mjs
  //   mail() / hookSessionEnd → notifyWatchers). Nudges carry no payload —
  //   the poll re-runs its own claim attempt. Permission/elicitation/choice
  //   answers still never resolve a watch: they ride the held hook response
  //   and never become mail.
  //
  //   Races: mailbox drained first → delivered_at already set → the poll's
  //   claim finds nothing and the hold simply lapses to idle. Watcher socket
  //   gone → 'close' unregisters the waiter, nothing claimed. Accepted
  //   window: a claim whose response the watcher never reads loses
  //   auto-delivery (the mail row is marked delivered either way).
  function watchHook(req, res, url) {
    const sid = url.searchParams.get('session') || '';
    const holdRaw = Number(url.searchParams.get('hold_ms'));
    const holdMs = Number.isFinite(holdRaw) ? Math.max(0, Math.min(holdRaw, 25_000)) : 25_000;

    const attempt = () => {
      const info = core.watchInfo(sid);
      if (!info.session_alive) return { status: 'idle', ...info };
      const claimed = core.claimMail(sid);
      if (claimed) return { status: 'mail', ...claimed };
      return null; // session alive, no undelivered mail → hold (mail-wake)
    };

    const immediate = attempt();
    if (immediate) return json(res, 200, immediate);

    let settled = false;
    let unregister = () => {};
    const finish = obj => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unregister();
      try { json(res, 200, obj); } catch { /* socket gone */ }
    };
    const timer = setTimeout(() => finish({ status: 'idle', ...core.watchInfo(sid) }), holdMs);
    timer.unref?.();
    unregister = core.addWatchWaiter(sid, () => {
      if (settled || res.writableEnded || res.destroyed) return;
      const out = attempt();
      if (out) finish(out);
    });
    res.on('close', () => { settled = true; clearTimeout(timer); unregister(); });
    // response intentionally left open
  }

  const server = http.createServer((req, res) => {
    try {
      const url = new URL(req.url, `http://127.0.0.1:${port}`);
      if (req.method === 'GET') {
        if (url.pathname === '/health') {
          // v1.2: spawn capability rides /health so the launcher/board can
          // hide all spawn UI when unavailable.
          return json(res, 200, { ok: true, fleet: core.fleetSize(), pid: process.pid, version, spawn: core.spawnCapability() });
        }
        if (url.pathname === '/state') return json(res, 200, core.snapshot());
        if (url.pathname === '/mail') {
          const sid = url.searchParams.get('session') || '';
          const box = core.drainMail(sid);
          if (box.length) broadcast();
          return json(res, 200, { mail: box });
        }
        if (url.pathname === '/api/watch') return watchHook(req, res, url); // F3d-2 long-poll
        if (url.pathname === '/plain') {
          // the Phase 1 spike board, served verbatim
          res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
          return res.end(fs.readFileSync(boardFile));
        }
        if (url.pathname === '/' || url.pathname.startsWith('/assets/')) {
          // built React board (Phase 5) from board-dist
          return serveBoardAsset(res, url.pathname, () => json(res, 404, { err: 'nope' }));
        }
        return json(res, 404, { err: 'nope' });
      }

      if (req.method === 'POST') {
        let body = '';
        req.on('data', d => { body += d; if (body.length > MAX_BODY) req.destroy(); });
        req.on('end', () => {
          let ev = {};
          try { ev = JSON.parse(body || '{}'); } catch {
            // hooks fail open: a bad body on a hook path is still 200 {}
            return url.pathname.startsWith('/hook/')
              ? json(res, 200, {})
              : json(res, 400, { err: 'bad json' });
          }
          try {
            const hook = /^\/hook\/([A-Za-z]+)$/.exec(url.pathname);
            if (hook) {
              const name = hook[1];
              // payload capture (validation aid): first 3 raw payloads per
              // hook event name, best-effort, never affects the response
              try { capture(name, ev); } catch { /* best-effort */ }
              // F3c CRITICAL (validated live on CLI 2.1.206):
              // AskUserQuestion rides the permission machinery — after the
              // /hook/AskUserQuestion hold resolves {}, the CLI fires
              // PermissionRequest for the SAME tool call. NEVER hold that
              // one: an unanswered question would chain two full hold
              // windows (~50 s each) before the terminal user ever sees the
              // chooser. Ingest telemetry, answer {} immediately.
              if (name === 'PermissionRequest' && ev?.tool_name === 'AskUserQuestion') {
                core.applyEvent({ ...ev, hook_event_name: 'PermissionRequest' });
                return json(res, 200, {});
              }
              if (name === 'PermissionRequest' || name === 'Elicitation' || name === 'AskUserQuestion') {
                return holdHook(res, ev, name); // Phase 3/4 hold-open relay
              }
              const handler = hookHandlers[name];
              if (!handler) {
                // unknown hook event: ingest telemetry anyway, respond no-op
                core.applyEvent({ hook_event_name: name, ...ev });
                return json(res, 200, {});
              }
              return json(res, 200, handler(ev) ?? {});
            }
            if (url.pathname === '/mail') return json(res, 200, core.postMail(ev));
            if (url.pathname === '/command') return json(res, 200, core.command(ev.text));
            if (url.pathname === '/api/spawn') {
              // v1.2 board spawn (CONTRACT). Control API like the questions
              // answer path: real status codes, fail-loud — never a silent
              // no-op. The whole flow (validate → card → worktree → tmux →
              // row → nudge) lives in derive.mjs. v1.3 adds
              // dangerously_skip_permissions: bool and permission_mode
              // "bypassPermissions" (validated/applied in derive.spawn too).
              core.spawn(ev)
                .then(out => json(res, out.status, out.body))
                .catch(err => {
                  console.error('fleetd spawn error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const killMatch = /^\/api\/spawn\/([A-Za-z0-9-]+)\/kill$/.exec(url.pathname);
            if (killMatch) {
              // v1.2 name-verified kill: 404 unknown id, 409 card not offline
              // without force:true, 410 window already gone.
              core.spawnKill(killMatch[1], ev?.force === true)
                .then(out => json(res, out.status, out.body))
                .catch(err => {
                  console.error('fleetd spawn kill error:', err);
                  json(res, 500, { ok: false, reason: 'internal' });
                });
              return;
            }
            const answerMatch = /^\/api\/questions\/(\d+)\/answer$/.exec(url.pathname);
            if (answerMatch) {
              // Board answer API (F3). NOT a hook path — real status codes.
              // v1.3: for an ExitPlanMode plan question the body may also be
              // {behavior:"capture"} (board-only pseudo-behavior) — the
              // branching lives in questions.mjs answer().
              const out = core.questions.answer(Number(answerMatch[1]), ev);
              return json(res, out.status, out.body);
            }
            const planMatch = /^\/api\/plans\/(\d+)\/mark$/.exec(url.pathname);
            if (planMatch) {
              // v1.3 plan library mark (CONTRACT): {status:"executed"|"archived",
              // via?} — 404 unknown id, 409 bad transition. Matrix documented
              // at core.planMark (derive.mjs).
              const out = core.planMark(Number(planMatch[1]), ev);
              return json(res, out.status, out.body);
            }
            return json(res, 404, { err: 'nope' });
          } catch (err) {
            console.error('fleetd handler error:', err);
            // fail open on hook paths; visible error elsewhere
            if (url.pathname.startsWith('/hook/')) return json(res, 200, {});
            return json(res, 500, { err: 'internal' });
          }
        });
        return;
      }

      json(res, 404, { err: 'nope' });
    } catch (err) {
      console.error('fleetd request error:', err);
      try { json(res, String(req.url || '').startsWith('/hook/') ? 200 : 500, {}); } catch { /* socket gone */ }
    }
  });

  // ---------------------------------------------------------------- ws
  const wss = new WebSocketServer({ server, path: '/ws' });
  // ws re-emits http server errors (e.g. EADDRINUSE) on the wss; without a
  // listener that throws and masks the election exit-3 path in fleetd.mjs.
  wss.on('error', () => { /* the http server's 'error' listener owns this */ });
  function broadcast() {
    if (!wss.clients.size) return;
    const msg = JSON.stringify({ type: 'snapshot', ...core.snapshot() });
    for (const c of wss.clients) if (c.readyState === 1) c.send(msg);
  }
  wss.on('connection', ws => {
    try { ws.send(JSON.stringify({ type: 'snapshot', ...core.snapshot() })); } catch { /* client gone */ }
  });
  const heartbeat = setInterval(broadcast, 5000); // ages on the board refresh
  heartbeat.unref();
  core.onMutate = broadcast;

  return { server, wss, broadcast };
}
