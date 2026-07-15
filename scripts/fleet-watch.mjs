#!/usr/bin/env node
// fleet-watch.mjs — F3d-2 asyncRewake watcher, v1.1 any-mail wake (EXPERIMENTAL, shipped on).
//
// Spawned by the plugin's Stop command hook with `asyncRewake: true`
// (hooks/hooks.json): the CLI backgrounds it, hands it the full Stop payload
// on stdin, and — validated live on 2.1.206 — wakes the idle session when it
// exits 2, injecting
// stderr behind the hook's `rewakeMessage` prefix (now the neutral
// "[FLEETDECK] Fleet board mail for you:" — each mail carries its own frame,
// see below).
//
// v1.1 story: GET /api/watch is no longer "wait for the answer to the one
// freeform question this Stop may have created." It claims the OLDEST
// undelivered mail for the session from ANY sender — a human's answer, an
// orchestrator assignment, or plain board/session mail — at ANY time the
// session is idle, not only while a question happens to be outstanding. So
// this watcher keeps long-polling for as long as the session is alive,
// whether or not a question is pending; "nothing pending" is no longer a
// reason to give up (mail can land for an idle session at any moment — that
// is the whole feature).
//
// {status:'mail', mail_id, at, from, text} → exit 2 with the RAW `text` on
// stderr as-is: it now carries its own frame ([FLEETDECK ANSWER] /
// [FLEETDECK ASSIGNMENT] / plain), so no local reframing happens here — the
// hook's rewakeMessage prefix is deliberately neutral. Belt-and-braces: a
// server still mid-rollout may answer the old {status:'answer', text} shape
// instead of {status:'mail', ...} — treated identically (same exit 2 + raw
// stderr), so the watcher works against either server version.
//
// Behavior: long-poll fleetd GET /api/watch?session=<sid> (contract
// documented in scripts/fleetd/http.mjs — v1.1 orchestrator routing +
// mail-wake) until mail arrives for this session, then exit 2 with the text
// on stderr. Every other terminal path exits 0 silently.
//
// Self-termination requirements (validated: the CLI does NOT reap these —
// an asyncRewake hook that never exits would leak one process per Stop):
//   (a) single-flight per session, NEWEST WINS: write own pid to
//       FLEETDECK_HOME/watch-<sid>.pid, wait 150 ms, re-read — if the file
//       no longer holds our pid a newer watcher took over and we exit 0.
//       Ownership is re-checked before every poll, so a superseded older
//       watcher exits within one poll cycle. The pid file is removed on exit
//       only while it still holds our own pid (never clobber the successor's).
//       Best-effort: if two watchers briefly overlap mid-poll, /api/watch's
//       atomic mail claim still guarantees at-most-once delivery.
//   (b) exit 0 immediately when fleetd reports the session offline/ended
//       (session_alive:false), or when fleetd is unreachable for 3
//       consecutive polls. Unlike v1, a session with nothing pending is NOT
//       a reason to exit — watching an idle session is the point.
//   (c) total lifetime cap FLEETDECK_WATCH_MAX_MS (default 2 h — raised from
//       30 min so an idle session stays wakeable across a long lunch), then
//       exit 0. Each later Stop spawns a fresh watcher, so coverage
//       continues turn over turn; after the cap lapses, delivery falls back
//       to turn-boundary (still correct, just not instant). hooks/hooks.json's
//       Stop command-hook timeout is set above this cap so the CLI never
//       kills the process out from under its own lifecycle logic.
//   (d) stdout: NEVER written. stderr: carries ONLY the final mail text (the
//       CLI injects stderr||stdout on exit 2 — anything else here would
//       pollute the rewake).
//
// Env (tests shrink these): FLEETDECK_PORT (default 4711), FLEETDECK_HOME
// (default ~/.fleetdeck), FLEETDECK_WATCH_POLL_MS (per-request long-poll
// hold, default 25000, clamped 50..25000), FLEETDECK_WATCH_MAX_MS (lifetime
// cap, default 7200000 = 2h, clamped 500..86400000).

import fs from 'node:fs';
import path from 'node:path';
import { resolveHome, resolvePort, resolveBase } from './fleetd/config.mjs';

const PORT = resolvePort();
const BASE = resolveBase(PORT);
const HOME = resolveHome();

// FLEETDECK_REQUIRE_TOKEN support: /api/watch is neither a hook path nor the
// public shell, so under the flag it demands the token even on loopback. Read
// the persisted token ($FLEETDECK_HOME/token, resolved like every other path
// here) ONCE at startup and tolerate absence — in default loopback mode there
// is no file and the loopback exemption carries the poll. This watcher is only
// ever spawned by a Stop hook AFTER SessionStart booted the daemon, so the token
// file already exists by the time we read it. Harmless in default mode.
let TOKEN = null;
try { TOKEN = fs.readFileSync(path.join(HOME, 'token'), 'utf8').trim() || null; } catch { /* no token file — default loopback mode */ }

function envMs(name, dflt, min, max) {
  const raw = Number(process.env[name]);
  if (!Number.isFinite(raw) || raw <= 0) return dflt;
  return Math.max(min, Math.min(raw, max));
}
const POLL_MS = envMs('FLEETDECK_WATCH_POLL_MS', 25_000, 50, 25_000);
const MAX_MS = envMs('FLEETDECK_WATCH_MAX_MS', 7_200_000, 500, 24 * 3600_000);
const MAX_FAILURES = 3;
const MAX_STDIN_BYTES = 64_000;

const startedAt = Date.now();
const sleep = ms => new Promise(r => setTimeout(r, ms));
const jitter = base => base + Math.floor(Math.random() * 250);

// ---------------------------------------------------------------- stdin
// The CLI writes the Stop payload and closes stdin; guard with a timeout so
// a pathological parent can never wedge us before the loop's own caps apply.
function readStdin(timeoutMs = 5_000) {
  return new Promise(resolve => {
    const chunks = [];
    let bytes = 0;
    let settled = false;
    let timer;

    // WHY one finish path: the timeout, EOF, stream error and byte ceiling can
    // race. Resolving a Promise twice is harmless, but leaving even one data
    // listener behind lets a wedged parent feed this two-hour watcher forever.
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      process.stdin.removeListener('data', onData);
      process.stdin.removeListener('end', finish);
      process.stdin.removeListener('error', finish);
      process.stdin.pause();
      resolve(Buffer.concat(chunks, bytes).toString('utf8'));
    };
    const onData = chunk => {
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      const room = MAX_STDIN_BYTES - bytes;
      if (room > 0) {
        const kept = buf.length <= room ? buf : buf.subarray(0, room);
        chunks.push(kept);
        bytes += kept.length;
      }
      if (bytes >= MAX_STDIN_BYTES) finish();
    };

    timer = setTimeout(finish, timeoutMs);
    process.stdin.on('data', onData);
    process.stdin.on('end', finish);
    process.stdin.on('error', finish);
  });
}

// ------------------------------------------------------- single-flight pid
let pidFile = null;

function readPidFile() {
  try { return fs.readFileSync(pidFile, 'utf8').trim(); } catch { return null; }
}

function ownsPidFile() {
  return readPidFile() === String(process.pid);
}

function cleanupPidFile() {
  // Guarded removal: never delete a successor's claim.
  try { if (pidFile && ownsPidFile()) fs.unlinkSync(pidFile); } catch { /* best effort */ }
}
process.on('exit', cleanupPidFile);

// -------------------------------------------------------------------- main
const raw = await readStdin();
let payload = {};
try { payload = JSON.parse(raw || '{}'); } catch { process.exit(0); }
const sid = payload?.session_id;
if (!sid || typeof sid !== 'string') process.exit(0);

try {
  fs.mkdirSync(HOME, { recursive: true });
  pidFile = path.join(HOME, `watch-${sid.replace(/[^A-Za-z0-9_.-]/g, '_')}.pid`);
  fs.writeFileSync(pidFile, String(process.pid));
} catch { process.exit(0); } // no writable state dir → no single-flight → don't run
await sleep(150);
if (!ownsPidFile()) { pidFile = null; process.exit(0); } // a newer watcher took over

let failures = 0;
const deadline = startedAt + MAX_MS;

while (Date.now() < deadline) {
  if (!ownsPidFile()) { pidFile = null; process.exit(0); } // superseded mid-life

  // Per-request hold: whatever is left of the lifetime, capped at POLL_MS.
  const holdMs = Math.max(0, Math.min(POLL_MS, deadline - Date.now()));
  let out = null;
  try {
    const res = await fetch(
      `${BASE}/api/watch?session=${encodeURIComponent(sid)}&hold_ms=${holdMs}`,
      {
        headers: TOKEN ? { authorization: `Bearer ${TOKEN}` } : undefined,
        signal: AbortSignal.timeout(holdMs + 5_000),
      },
    );
    if (!res.ok) throw new Error(`watch ${res.status}`);
    out = await res.json();
  } catch {
    failures += 1;
    if (failures >= MAX_FAILURES) process.exit(0); // fleetd gone → stand down
    await sleep(jitter(Math.min(POLL_MS, 1_000)));
    continue;
  }
  failures = 0;

  // v2 'mail' (any sender, raw framed text) — and the transitional v1
  // 'answer' shape some in-rollout servers may still send — wake the session
  // the same way: exit 2, raw text on stderr, no local reframing.
  if ((out?.status === 'mail' || out?.status === 'answer') && typeof out.text === 'string') {
    cleanupPidFile();
    process.stderr.write(out.text); // the ONLY thing ever written anywhere
    process.exit(2);                // rewake: CLI injects stderr behind rewakeMessage
  }
  // idle
  if (out?.session_alive === false) process.exit(0); // tombstoned / unknown session
  // v1.1: keep polling even with nothing pending — mail can land for an idle
  // session at any time, and that's the whole feature (no exit-on-idle here,
  // unlike v1). A pending freeform question means the server likely answered
  // fast (no reason to hold), so retry sooner; otherwise the server already
  // held for ~holdMs, so a small jitter is enough before the next request.
  await sleep(out?.pending ? jitter(100) : jitter(Math.min(POLL_MS, 800)));
}
process.exit(0); // lifetime cap
