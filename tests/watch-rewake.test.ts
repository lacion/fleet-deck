// tests/watch-rewake.test.ts
//
// Phase 4 F3d-2 delivery path, now v1.1 "any-mail wake" (orchestrator
// routing + mail-wake). Two surfaces under test:
//
//   1. fleetd GET /api/watch?session=<sid> — the long-poll (contract
//      documented in scripts/fleetd/http.mjs). v2 claims the OLDEST
//      undelivered mail for the session from ANY sender (not just board
//      answers), and returns it RAW — the mail's own frame
//      ([FLEETDECK ANSWER] / [FLEETDECK ASSIGNMENT] / no frame at all) is
//      never stripped, because hooks.json's rewakeMessage is now neutral
//      ("[FLEETDECK] Fleet board mail for you:") and each mail must carry
//      its own meaning. Response status flipped from v1's 'answer' to
//      'mail'. ATOMIC claim is unchanged: resolving a poll with mail marks
//      it delivered, so the turn-boundary path (UserPromptSubmit/Stop
//      drains) can never re-deliver it, and vice versa — a mailbox drained
//      first leaves the poll idle. mail.delivered_at is the single source
//      of truth. A LIVE session with nothing claimable now HOLDS for the
//      full hold_ms (v1 returned idle immediately) — mail can land for an
//      idle session at any time, so there is no more "nothing to wait for"
//      short-circuit while the session is alive; only an offline/unknown
//      session still answers immediately.
//
//   2. scripts/fleet-watch.mjs — the watcher a Stop command hook spawns with
//      asyncRewake:true. v2: it keeps long-polling for as long as the
//      session is alive, whether or not a freeform question happens to be
//      pending (mail can arrive for an idle session at any time — that's
//      the whole feature; v1 used to give up once nothing was pending past
//      a startup grace, which no longer exists). Exit contract: 2 + the RAW
//      mail text on stderr ONLY for delivered mail (from any sender, not
//      just answers); 0 for superseded / tombstoned / unreachable /
//      lifetime-cap. stdout must stay empty forever (stderr feeds the
//      rewake injection). Lifetime cap raised to FLEETDECK_WATCH_MAX_MS
//      default 2h (was 30 min).

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, rmSync, readFileSync, existsSync } from 'node:fs';
import type { AddressInfo, Socket } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { startDaemon, REPO_ROOT, type DaemonHandle } from './helpers/daemon.ts';
import { postHook, postJson, getJson } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { makeTranscriptDir, writeTranscript } from './helpers/transcript.ts';
import { waitUntil, scaleMs } from './helpers/wait.ts';
import { getState, scratchCwd } from './helpers/state.ts';
import type { StateResponse, QuestionEntry } from '../contracts/state.ts';

const WATCH_SCRIPT = path.join(REPO_ROOT, 'scripts/fleet-watch.mjs');

const noop = (): void => {
  /* stub error handler */
};

// The GET /api/watch v2 response body facet — every field the tests read.
interface WatchResponse {
  status?: string;
  session_alive?: boolean;
  pending?: number;
  from?: string;
  mail_id?: string;
  text?: string;
}

// POST /command `assign auto ...` acknowledgement facet.
interface AssignResult {
  ok?: boolean;
  assigned_to?: { session_id?: string; callsign?: string };
}

// The UserPromptSubmit hook response facet — only additionalContext is read.
interface UpsResponse {
  hookSpecificOutput?: { additionalContext?: string };
}

// hooks/hooks.json (only the Stop entry shape the last test asserts on).
interface HookCommand {
  command?: unknown;
  asyncRewake?: boolean;
  timeout?: number;
}
interface HookGroup {
  hooks: HookCommand[];
}
interface HooksFile {
  hooks: { Stop: HookGroup[] };
}

// Register a session and park a pending FREEFORM question for it (the only
// kind whose answer ever becomes claimable mail) via the F3d Stop detection
// path.
async function pendingFreeform(
  daemon: DaemonHandle,
  sid: string,
  cwd: string,
  transcriptDir: string,
  questionText = 'Should the project use bcrypt or argon2?',
): Promise<QuestionEntry> {
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );
  const transcriptPath = writeTranscript(transcriptDir, {
    sessionId: sid,
    assistantText: questionText,
  });
  await postHook(
    daemon.baseUrl,
    'Stop',
    { session_id: sid, hook_event_name: 'Stop', cwd, transcript_path: transcriptPath },
    { token: daemon.token },
  );
  const state = await getState<StateResponse>(daemon.baseUrl);
  const q = state.questions.find(
    (x) => x.session_id === sid && x.kind === 'freeform' && x.status === 'pending',
  );
  assert.ok(q, 'setup: a pending freeform question should exist');
  return q;
}

// Register a plain live session (no question, no mail) — the v1.1 case the
// watcher must now keep polling for instead of exiting.
async function liveSession(daemon: DaemonHandle, sid: string, cwd: string): Promise<void> {
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );
}

interface SpawnWatcherOptions {
  port: number;
  home: string;
  sid: string;
  env?: Record<string, string>;
}

interface Watcher {
  child: ChildProcess;
  readonly stdout: string;
  readonly stderr: string;
  exitWithin: (timeoutMs: number, label: string) => Promise<number | null>;
}

// Spawn scripts/fleet-watch.mjs the way the Stop command hook does: env for
// the scratch daemon, Stop payload on stdin. Timings shrunk for tests. v2 has
// no FLEETDECK_WATCH_GRACE_MS — "nothing pending" is no longer an exit
// reason, so there is nothing left to shrink there.
function spawnWatcher({ port, home, sid, env = {} }: SpawnWatcherOptions): Watcher {
  const child = spawn(process.execPath, [WATCH_SCRIPT], {
    env: {
      ...process.env,
      FLEETDECK_PORT: String(port),
      FLEETDECK_HOME: home,
      FLEETDECK_WATCH_POLL_MS: '400',
      FLEETDECK_WATCH_MAX_MS: '20000',
      ...env,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (d: Buffer) => {
    stdout += d.toString();
  });
  child.stderr.on('data', (d: Buffer) => {
    stderr += d.toString();
  });
  child.stdin.write(
    JSON.stringify({ session_id: sid, hook_event_name: 'Stop', stop_hook_active: false }),
  );
  child.stdin.end();
  const exited = new Promise<number | null>((resolve) => {
    child.once('exit', (code) => {
      resolve(code);
    });
  });
  return {
    child,
    get stdout() {
      return stdout;
    },
    get stderr() {
      return stderr;
    },
    exitWithin(timeoutMs: number, label: string): Promise<number | null> {
      return Promise.race([
        exited,
        new Promise<number | null>((_, reject) => {
          setTimeout(() => {
            reject(new Error(`watcher did not exit within ${timeoutMs}ms (${label})`));
          }, scaleMs(timeoutMs)).unref();
        }),
      ]);
    },
  };
}

function pidFileOf(home: string, sid: string): string {
  return path.join(home, `watch-${sid}.pid`);
}

function stillAlive(child: ChildProcess): boolean {
  return child.exitCode === null && child.signalCode === null;
}

// ---------------------------------------------------------------------------
// GET /api/watch v2 endpoint contract
// ---------------------------------------------------------------------------

test('/api/watch: unknown/offline session -> {status:"idle", session_alive:false} immediately, ignoring hold_ms (unchanged in v2)', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
  });

  const t0 = Date.now();
  const res = await getJson(`${daemon.baseUrl}/api/watch?session=${randomUUID()}&hold_ms=8000`);
  const elapsed = Date.now() - t0;
  const json = res.json as WatchResponse;
  assert.equal(res.status, 200);
  assert.equal(json.status, 'idle');
  assert.equal(json.session_alive, false);
  assert.ok(
    elapsed < 1000,
    `an unknown session must respond immediately, not hold (took ${elapsed}ms)`,
  );
});

test('/api/watch v2 FLIP: a live session with nothing claimable now HOLDS for the full hold_ms instead of returning idle immediately', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);

  const holdMs = 700;
  const t0 = Date.now();
  const res = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=${holdMs}`, {
    timeout: holdMs + 5000,
  });
  const elapsed = Date.now() - t0;
  const json = res.json as WatchResponse;
  assert.equal(json.status, 'idle');
  assert.equal(json.session_alive, true);
  assert.equal(json.pending, 0);
  assert.ok(
    elapsed >= holdMs - 150,
    `v1.1: a live session with nothing pending must hold for ~hold_ms (mail can land at any time) — resolved in ${elapsed}ms, expected >= ~${holdMs}ms`,
  );
});

test('/api/watch v2: claims assignment mail — status "mail", from "orchestrator", RAW text with the [FLEETDECK ASSIGNMENT] prefix PRESENT (not stripped)', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);
  // Reserved senders/frames 422 from POST /mail now — assignments arrive via
  // the legitimate internal path: POST /command `assign <target> <text>`.
  await postJson(`${daemon.baseUrl}/command`, { text: `assign ${sid} ship the release notes` });

  const res = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=2000`);
  const json = res.json as WatchResponse;
  assert.equal(json.status, 'mail', 'v2 status name is "mail", not the v1 "answer"');
  assert.equal(json.from, 'orchestrator');
  assert.ok(json.mail_id, 'response should reference the claimed mail row');
  assert.match(
    json.text ?? '',
    /^\[FLEETDECK ASSIGNMENT\] /,
    'the assignment frame must be PRESENT in the raw text (v2 removes prefix stripping)',
  );
  assert.ok(json.text?.includes('ship the release notes'));
});

test('/api/watch v2: claims plain board mail too (no frame), same as any other sender', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);
  await postJson(
    `${daemon.baseUrl}/mail`,
    { to: sid, from: 'operator', text: 'no rush, just checking in' },
    { token: daemon.token },
  );

  const res = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=2000`);
  const json = res.json as WatchResponse;
  assert.equal(
    json.status,
    'mail',
    'plain mail must resolve the watch, not just answer/assignment mail',
  );
  assert.equal(json.from, 'operator');
  assert.equal(
    json.text,
    'no rush, just checking in',
    'plain mail carries no frame and must not gain one',
  );
});

test('/api/watch v2: never claims mail for an offline session — preserved for --resume, delivered at the resumed first turn boundary', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);
  // Internal assignment path (POST /mail refuses reserved senders/frames).
  await postJson(`${daemon.baseUrl}/command`, { text: `assign ${sid} pick this up later` });
  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    loadFixture('session-end', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const res = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=500`);
  const json = res.json as WatchResponse;
  assert.equal(json.status, 'idle');
  assert.equal(
    json.session_alive,
    false,
    'an offline session must never have its mail claimed by the watch poll',
  );

  // Resume: SessionStart(resume) + the first UserPromptSubmit must carry it.
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }, { source: 'resume' }),
    { token: daemon },
  );
  const upRes = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }),
    { token: daemon },
  );
  const ctx = (upRes.json as UpsResponse | null)?.hookSpecificOutput?.additionalContext ?? '';
  assert.match(
    ctx,
    /\[FLEETDECK ASSIGNMENT\]/,
    `resumed session's first turn must carry the preserved assignment (got: ${ctx.slice(0, 150)})`,
  );
  assert.ok(ctx.includes('pick this up later'));
});

test('/api/watch v2: a pending freeform answered during the poll resolves as "mail" (from fleetdeck-answer, RAW [FLEETDECK ANSWER] text), and the next UserPromptSubmit carries NO duplicate', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  const q = await pendingFreeform(daemon, sid, cwd, transcriptDir);

  // start the long-poll BEFORE the answer exists
  const t0 = Date.now();
  const poll = getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=10000`, {
    timeout: 15000,
  });
  await new Promise((r) => setTimeout(r, 300)); // let the poll park

  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    text: 'argon2id',
  });
  assert.equal(ansRes.status, 200);

  const res = await poll;
  const json = res.json as WatchResponse;
  const elapsed = Date.now() - t0;
  assert.equal(json.status, 'mail', 'the parked poll must resolve with status "mail" (v2 name)');
  assert.equal(json.from, 'fleetdeck-answer');
  assert.ok(
    elapsed < 5000,
    `the poll must resolve on the mail-insert event, not its 10s hold (took ${elapsed}ms)`,
  );
  assert.match(json.text ?? '', /A: argon2id/, 'answer text should carry the Q/A body');
  assert.match(
    json.text ?? '',
    /^\[FLEETDECK ANSWER\]/,
    'v2 FLIP: text now carries the [FLEETDECK ANSWER] frame PRESENT (v1 stripped it; rewakeMessage is neutral in v2)',
  );
  assert.ok(json.mail_id, 'answer should reference the claimed mail row');

  // the claim marked the mail delivered -> turn-boundary path must NOT re-deliver
  const upRes = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }),
    { token: daemon },
  );
  const ctx = (upRes.json as UpsResponse | null)?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(
    !ctx.includes('[FLEETDECK ANSWER]'),
    `a watch-claimed answer must never re-deliver at the turn boundary (got: ${ctx.slice(0, 120)})`,
  );
});

test('/api/watch: mailbox drained first (turn boundary) -> the poll stays idle after its hold (no double delivery in either direction)', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  const q = await pendingFreeform(daemon, sid, cwd, transcriptDir);

  // answer, then let the TURN-BOUNDARY path drain it first
  await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: 'argon2id' });
  const upRes = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }),
    { token: daemon },
  );
  const ctx = (upRes.json as UpsResponse | null)?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(
    ctx.includes('[FLEETDECK ANSWER]') && ctx.includes('argon2id'),
    'sanity: the turn boundary delivered the answer',
  );

  // the poll must now find nothing claimable — delivered_at is the single
  // source of truth. v2: since the session is still alive, this now holds
  // for the (short) hold_ms rather than returning instantly, but the
  // resolved status must still be idle, never a second delivery.
  const res = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=500`);
  const json = res.json as WatchResponse;
  assert.equal(
    json.status,
    'idle',
    'a drained mailbox must leave the watch poll idle — never a second delivery',
  );
});

test('BUG-105: a superseded watcher generation cannot claim mail — the newest generation wins mid-poll and the mail survives for it', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);

  // Watcher A parks in a long poll with generation gen-old. Mail arriving
  // during the hold would previously resolve THIS poll with status 'mail' —
  // claimed (marked delivered) for a watcher that may already be superseded.
  const stale = getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=5000&wg=gen-old`, {
    timeout: 10000,
  });
  await new Promise((r) => setTimeout(r, 300)); // let A's poll park and register gen-old

  // Watcher B takes over: its first poll registers gen-new (newest wins) and
  // itself holds with nothing to claim yet.
  const fresh = getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=5000&wg=gen-new`, {
    timeout: 10000,
  });
  await new Promise((r) => setTimeout(r, 300)); // let B's poll park and supersede gen-old

  // Mail lands mid-poll: it must wake the CURRENT generation, never A's.
  await postJson(
    `${daemon.baseUrl}/mail`,
    { to: sid, from: 'operator', text: 'mid-poll takeover mail' },
    { token: daemon.token },
  );

  const freshRes = await fresh;
  const freshJson = freshRes.json as WatchResponse;
  assert.equal(freshJson.status, 'mail', 'the current generation must claim the mail');
  assert.equal(freshJson.text, 'mid-poll takeover mail');

  const staleRes = await stale;
  const staleJson = staleRes.json as WatchResponse;
  assert.equal(
    staleJson.status,
    'idle',
    'a superseded generation must never resolve with claimed mail',
  );

  // The mail was claimed exactly once, by B — the turn-boundary drain must
  // find nothing left to re-deliver.
  const upRes = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'continue' }),
    { token: daemon },
  );
  const ctx = (upRes.json as UpsResponse | null)?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(
    !ctx.includes('mid-poll takeover mail'),
    `the generation-guarded claim must still mark the mail delivered (got: ${ctx.slice(0, 150)})`,
  );
});

// ---------------------------------------------------------------------------
// fleet-watch.mjs v2 lifecycle, against a scratch daemon
// ---------------------------------------------------------------------------

test('fleet-watch v2: a live session with NO pending questions and NO mail KEEPS POLLING (does not exit)', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);

  const w = spawnWatcher({
    port: daemon.port,
    home: daemon.home,
    sid,
    env: { FLEETDECK_WATCH_POLL_MS: '300' },
  });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(daemon.home, sid)), { label: 'watcher pid file' });

  // Several poll cycles' worth of wall-clock time (v1 would have exited by
  // now once past its startup grace with nothing pending; v2 must not).
  await new Promise((r) => setTimeout(r, 2200));
  assert.ok(
    stillAlive(w.child),
    'v1.1: a live session with nothing pending must keep the watcher alive (mail can land at any time)',
  );
  assert.equal(w.stdout, '');
  assert.equal(w.stderr, '');
  assert.ok(
    existsSync(pidFileOf(daemon.home, sid)),
    'pid file should still be held while the watcher is alive',
  );
});

test('fleet-watch v2: assignment mail arrives -> exit 2 with "[FLEETDECK ASSIGNMENT]" in stderr (stdout silent), pid file removed', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);

  const w = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(daemon.home, sid)), { label: 'watcher pid file' });

  // Internal assignment path (POST /mail refuses reserved senders/frames).
  await postJson(`${daemon.baseUrl}/command`, { text: `assign ${sid} build the thing` });

  const code = await w.exitWithin(6000, 'after assignment mail');
  assert.equal(code, 2, 'delivered assignment mail must exit 2 — the asyncRewake wake signal');
  assert.match(
    w.stderr,
    /\[FLEETDECK ASSIGNMENT\]/,
    'stderr must carry the assignment frame (rewakeMessage is neutral in v2 — mail carries its own frame)',
  );
  assert.match(w.stderr, /build the thing/);
  assert.equal(w.stdout, '', 'stdout must stay empty in all cases');
  assert.ok(
    !existsSync(pidFileOf(daemon.home, sid)),
    'the watcher must remove its pid file on exit',
  );
});

test('fleet-watch: a board answer makes the watcher exit 2 with the RAW answer (frame PRESENT) on stderr, stdout silent', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  const q = await pendingFreeform(daemon, sid, cwd, transcriptDir);

  const w = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(daemon.home, sid)), { label: 'watcher pid file' });

  await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, {
    text: 'argon2id, and rotate the salt per user',
  });

  const code = await w.exitWithin(6000, 'after board answer');
  assert.equal(code, 2, 'a delivered answer must exit 2 — the asyncRewake wake signal');
  assert.match(
    w.stderr,
    /argon2id, and rotate the salt per user/,
    'stderr must carry the answer text (it becomes the rewake system-reminder)',
  );
  assert.match(
    w.stderr,
    /\[FLEETDECK ANSWER\]/,
    "v2 FLIP: stderr must carry the [FLEETDECK ANSWER] frame (v1 stripped it to avoid duplicating rewakeMessage; v2's rewakeMessage is neutral so the mail must carry its own frame)",
  );
  assert.equal(w.stdout, '', 'stdout must stay empty in all cases');
  assert.ok(
    !existsSync(pidFileOf(daemon.home, sid)),
    'the watcher must remove its pid file on exit',
  );
});

test('fleet-watch: single-flight per session — a newer watcher supersedes the older (older exits 0), newest gets the mail', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  const q = await pendingFreeform(daemon, sid, cwd, transcriptDir);
  const pidFile = pidFileOf(daemon.home, sid);

  const w1 = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w1.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(
    () => existsSync(pidFile) && readFileSync(pidFile, 'utf8').trim() === String(w1.child.pid),
    {
      label: 'watcher #1 owns the pid file',
    },
  );

  const w2 = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w2.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });

  const code1 = await w1.exitWithin(6000, 'older watcher after takeover');
  assert.equal(code1, 0, 'the superseded (older) watcher must exit 0 — newest wins');
  assert.equal(w1.stderr, '', 'a superseded watcher must not write to stderr');
  assert.equal(
    readFileSync(pidFile, 'utf8').trim(),
    String(w2.child.pid),
    'the pid file must belong to the newest watcher',
  );

  await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { text: 'ship it' });
  const code2 = await w2.exitWithin(6000, 'newest watcher after board answer');
  assert.equal(code2, 2, 'the surviving (newest) watcher gets the answer');
  assert.match(w2.stderr, /ship it/);
});

test('BUG-105: a watcher superseded while its poll is in flight never wakes on the mail — it exits 0 and the successor claims and delivers instead', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);
  const pidFile = pidFileOf(daemon.home, sid);

  const w1 = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w1.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(
    () => existsSync(pidFile) && readFileSync(pidFile, 'utf8').trim() === String(w1.child.pid),
    {
      label: 'watcher #1 owns the pid file',
    },
  );
  // W1 is now blocked in a long poll (hold 400 ms per cycle).

  const w2 = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w2.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(
    () => existsSync(pidFile) && readFileSync(pidFile, 'utf8').trim() === String(w2.child.pid),
    {
      label: 'watcher #2 owns the pid file',
    },
  );

  // Mail arrives while w1's poll is (very likely) still in flight. Whether
  // or not the timing lands inside w1's hold, the invariant must hold: the
  // superseded watcher never emits the wake.
  await postJson(
    `${daemon.baseUrl}/mail`,
    { to: sid, from: 'operator', text: 'mid-poll handoff mail' },
    { token: daemon.token },
  );

  const code1 = await w1.exitWithin(6000, 'superseded watcher with mail in flight');
  assert.equal(code1, 0, 'the superseded watcher must exit 0 — never the wake signal');
  assert.equal(w1.stderr, '', 'the superseded watcher must never write mail text to stderr');
  assert.equal(w1.stdout, '');

  const code2 = await w2.exitWithin(6000, 'successor watcher after mail');
  assert.equal(code2, 2, 'the successor claims the mail and wakes (exit 2)');
  assert.match(w2.stderr, /mid-poll handoff mail/);
  assert.equal(w2.stdout, '');
});

test('fleet-watch: SessionEnd tombstone makes the watcher exit 0 promptly (freeform question may still be pending)', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  const transcriptDir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(transcriptDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await pendingFreeform(daemon, sid, cwd, transcriptDir);

  const w = spawnWatcher({ port: daemon.port, home: daemon.home, sid });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(daemon.home, sid)), { label: 'watcher pid file' });

  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    loadFixture('session-end', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );

  const code = await w.exitWithin(6000, 'after SessionEnd');
  assert.equal(code, 0, 'a tombstoned session must release its watcher with exit 0');
  assert.equal(w.stderr, '', 'no answer, no stderr');
  assert.equal(w.stdout, '');

  // the freeform question SURVIVES the tombstone (human queue, resume path) —
  // only the watcher stands down
  const state = await getState<StateResponse>(daemon.baseUrl);
  const q = state.questions.find((x) => x.session_id === sid && x.kind === 'freeform');
  assert.equal(
    q?.status,
    'pending',
    'the freeform question still awaits its answer for a resumed session',
  );
});

test('hooks.json: the asyncRewake Stop hook timeout exceeds the watcher lifetime ceiling with shutdown margin', () => {
  // BUG-103: fleet-watch accepts FLEETDECK_WATCH_MAX_MS up to 24 h, but the
  // hook timeout was fixed at 7230 s (~2 h) — any configured lifetime above
  // that was killed by the CLI out from under the watcher's own cap logic.
  // The timeout must sit above the full accepted maximum, with margin for
  // the watcher's shutdown tail (final long-poll hold + fetch grace).
  const hooks = JSON.parse(
    readFileSync(path.join(REPO_ROOT, 'hooks/hooks.json'), 'utf8'),
  ) as HooksFile;
  const entry = hooks.hooks.Stop.flatMap((group) => group.hooks).find(
    (h) => typeof h.command === 'string' && h.command.includes('fleet-watch.mjs'),
  );
  assert.ok(entry, 'hooks.json must register scripts/fleet-watch.mjs as a Stop hook');
  assert.equal(entry.asyncRewake, true, 'the watcher hook must stay asyncRewake');
  assert.ok(typeof entry.timeout === 'number', 'the watcher hook must declare a numeric timeout');
  const WATCH_MAX_CEILING_S = 24 * 3600; // scripts/fleet-watch.mjs MAX_MS clamp ceiling
  assert.ok(
    entry.timeout > WATCH_MAX_CEILING_S,
    `hook timeout ${entry.timeout}s must exceed the watcher lifetime ceiling ${WATCH_MAX_CEILING_S}s`,
  );
});

test('fleet-watch v2: lifetime cap (FLEETDECK_WATCH_MAX_MS) makes the watcher exit 0 even with a live session and nothing pending', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  await liveSession(daemon, sid, cwd);

  const w = spawnWatcher({
    port: daemon.port,
    home: daemon.home,
    sid,
    env: { FLEETDECK_WATCH_POLL_MS: '200', FLEETDECK_WATCH_MAX_MS: '700' },
  });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(daemon.home, sid)), { label: 'watcher pid file' });

  const code = await w.exitWithin(6000, 'lifetime cap');
  assert.equal(code, 0, 'the lifetime cap must end the watcher with exit 0, not a wake');
  assert.equal(w.stderr, '', 'a cap exit is silent, not a delivery');
  assert.equal(w.stdout, '');
  assert.ok(!existsSync(pidFileOf(daemon.home, sid)), 'pid file cleaned up on the cap exit too');
});

test('fleet-watch: fleetd unreachable -> exits 0 after exactly 3 consecutive failed polls, alive through failures 1-2, silently', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-watch-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // A dedicated bound stub that deliberately hangs every poll — each request
  // fails on the watcher's own timeout, so no port collision can yield a
  // false pass, and attempts are countable. Bound to a real port, so the
  // watcher definitely reaches a live TCP endpoint that never answers.
  let attempts = 0;
  const sockets = new Set<Socket>();
  const stub = http.createServer((_req, res) => {
    attempts += 1;
    // hold the request open; the watcher's AbortSignal.timeout ends it
    res.on('error', noop);
    _req.on('error', noop);
  });
  stub.on('connection', (s) => {
    sockets.add(s);
    s.on('close', () => {
      sockets.delete(s);
    });
  });
  await new Promise<void>((resolve, reject) => {
    stub.once('error', reject);
    stub.listen(0, '127.0.0.1', resolve);
  });
  t.after(() => {
    for (const s of sockets) s.destroy();
    stub.close();
  });
  const port = (stub.address() as AddressInfo).port;

  const sid = randomUUID();
  const w = spawnWatcher({ port, home, sid, env: { FLEETDECK_WATCH_POLL_MS: '200' } });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(home, sid)), { label: 'watcher pid file' });

  // Survives failure one...
  await waitUntil(() => attempts >= 2 && stillAlive(w.child), {
    timeoutMs: 15000,
    intervalMs: 25,
    label: 'watcher alive after failure #1 (attempt #2 started)',
  });
  // ...and failure two...
  await waitUntil(() => attempts >= 3 && stillAlive(w.child), {
    timeoutMs: 15000,
    intervalMs: 25,
    label: 'watcher alive after failure #2 (attempt #3 started)',
  });

  // ...and exits 0 ONLY after failure three completes.
  const code = await w.exitWithin(8000, 'after the third consecutive failure');
  assert.equal(code, 0, 'an unreachable fleetd must never keep a watcher alive');
  assert.equal(w.stderr, '', 'failures are silent — stderr is reserved for the mail text');
  assert.equal(w.stdout, '');
  assert.equal(
    attempts,
    3,
    `exactly 3 polls must be attempted before standing down (saw ${attempts})`,
  );
  await new Promise((r) => setTimeout(r, scaleMs(1200))); // no 4th attempt sneaks in after exit
  assert.equal(attempts, 3, 'the watcher must stop polling once it exits');
  assert.ok(!existsSync(pidFileOf(home, sid)), 'pid file cleaned up on the failure exit too');
});

// ---------------------------------------------------------------------------
// D) End-to-end loop: assign auto -> idle session's live watcher wakes it —
// no `claude` involved anywhere, entirely synthetic HTTP + a real spawned
// fleet-watch.mjs.
// ---------------------------------------------------------------------------

test('E2E: an idle session with a running fleet-watch wakes on `assign auto`, framed task on stderr, no duplicate at the next turn', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const sid = randomUUID();
  // Idle session: SessionStart + Stop (Stop unconditionally derives col=idle).
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    {
      token: daemon,
    },
  );
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }), {
    token: daemon,
  });
  const state = await getState<StateResponse>(daemon.baseUrl);
  assert.equal(
    state.sessions.find((s) => s.session_id === sid)?.col,
    'idle',
    'sanity: the session should be idle before the watcher spawns',
  );

  const w = spawnWatcher({
    port: daemon.port,
    home: daemon.home,
    sid,
    env: { FLEETDECK_WATCH_POLL_MS: '300' },
  });
  t.after(() => {
    try {
      w.child.kill('SIGKILL');
    } catch {
      /* gone */
    }
  });
  await waitUntil(() => existsSync(pidFileOf(daemon.home, sid)), { label: 'watcher pid file' });

  // The one and only eligible candidate wins unscoped auto-routing.
  const cmdRes = await postJson(`${daemon.baseUrl}/command`, {
    text: 'assign auto build the thing',
  });
  const cmdJson = cmdRes.json as AssignResult;
  assert.deepEqual(cmdRes.json, {
    ok: true,
    assigned_to: { session_id: sid, callsign: cmdJson.assigned_to?.callsign },
  });

  const code = await w.exitWithin(5000, 'after assign auto');
  assert.equal(code, 2, 'the watcher must wake (exit 2) once the routed assignment mail lands');
  assert.match(w.stderr, /\[FLEETDECK ASSIGNMENT\]/);
  assert.match(w.stderr, /build the thing/);
  assert.equal(w.stdout, '');

  // The watcher's claim already marked the mail delivered — the next turn
  // boundary must carry NO duplicate.
  const upRes = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }, { prompt: 'on it' }),
    { token: daemon },
  );
  const ctx = (upRes.json as UpsResponse | null)?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(
    !ctx.includes('[FLEETDECK ASSIGNMENT]'),
    `a watch-claimed assignment must never re-deliver at the turn boundary (got: ${ctx.slice(0, 150)})`,
  );
});
