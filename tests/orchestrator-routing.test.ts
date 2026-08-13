// tests/orchestrator-routing.test.ts
//
// v1.1 "orchestrator routing" contract (POST /command, orchestrator routing
// + mail-wake):
//
//   assign auto / assign auto:<repo_id-or-name>
//     Deterministic, zero model calls. Candidates = non-ended sessions whose
//     col is NOT offline/needsyou, optionally scoped to a repo (matched
//     against repo_id OR repo_name). Rank: col idle -> queued ->
//     (working|verifying); ties broken by fewest undelivered mail, then most
//     recent last_seen. Winner gets mail from 'orchestrator' with text
//     `[FLEETDECK ASSIGNMENT] <text>`. Response shapes are exact:
//       success  -> {ok:true, assigned_to:{session_id, callsign}}
//       unrouted -> {ok:false, unrouted:true}  (still logged: ticker + the
//                    commands table)
//
// Pure HTTP against a scratch daemon: sessions are built by replaying the
// same hook fixtures session-lifecycle.test.mjs uses to reach each derived
// column (col is never self-reported), so this file never has to guess at
// fleetd's internal derivation — it just walks the documented state machine:
//   SessionStart                              -> queued
//   SessionStart + UserPromptSubmit            -> working
//   ... + PostToolUse Bash "npm test"          -> verifying
//   ... + Stop                                 -> idle
//   SessionStart + Notification                -> needsyou
//   SessionStart + SessionEnd                  -> offline
//
// Coverage map (task brief bullet A -> tests below):
//   - idle beats queued beats working
//       -> "routing ladder: idle beats queued beats working/verifying..."
//   - needsyou and offline NEVER receive
//       -> same test (asserts their mailboxes stay empty throughout)
//   - verifying ranks with working, not above queued
//       -> "routing ladder: verifying ranks with working..."
//   - auto:<repo> scopes by repo_name AND by repo_id
//       -> "auto:<repo> scopes candidates by repo_name and by repo_id..."
//   - tie on col -> fewest undelivered mail wins
//       -> "tie on col: fewest undelivered mail wins..."
//   - response shapes {ok:true, assigned_to:{...}} / {ok:false, unrouted:true}
//     exactly; unrouted leaves no mail anywhere; command still logged
//       -> "response shapes are exact; unrouted leaves no mail anywhere..."
//   - winner's mailbox gets exactly one mail from 'orchestrator' framed
//     '[FLEETDECK ASSIGNMENT] '
//       -> "the winning session receives exactly one mail..."
//   - plain `assign <callsign> <text>` still works
//       -> "plain \"assign <callsign> <text>\" still delivers directly..."

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, type DaemonHandle } from './helpers/daemon.ts';
import { postHook, postJson, getJson, type JsonResponse } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { makeRepoWithWorktree } from './helpers/gitrepo.ts';

interface SessionInfo {
  sid: string;
  callsign: string;
}

interface SessionStartBody {
  callsign?: string;
}

interface AssignedTo {
  session_id: string;
  callsign: string;
}

interface CommandBody {
  ok?: boolean;
  assigned_to?: AssignedTo;
  unrouted?: boolean;
  text?: string;
  delivered?: number;
  reason?: string;
}

interface MailItem {
  from: string;
  text: string;
}

interface MailBody {
  mail?: MailItem[];
}

interface TickerLine {
  msg?: string;
}

interface StateBody {
  mail_pending?: Record<string, number>;
  ticker: TickerLine[];
}

function scratchCwd(): string {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
}

// ---------------------------------------------------------------------------
// Session builders — each replays the exact hook sequence
// session-lifecycle.test.mjs proves derives the named column, so this file
// never has to reach into fleetd internals to force a col.
// ---------------------------------------------------------------------------

async function makeQueued(daemon: DaemonHandle, cwd: string): Promise<SessionInfo> {
  const sid = randomUUID();
  const reg = await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    { token: daemon.token },
  );
  const body = reg.json as SessionStartBody | null;
  assert.ok(body?.callsign, 'setup: SessionStart should hand back a callsign');
  return { sid, callsign: body.callsign };
}

async function makeWorking(daemon: DaemonHandle, cwd: string): Promise<SessionInfo> {
  const { sid, callsign } = await makeQueued(daemon, cwd);
  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
    { token: daemon.token },
  );
  return { sid, callsign };
}

async function makeVerifying(daemon: DaemonHandle, cwd: string): Promise<SessionInfo> {
  const { sid, callsign } = await makeWorking(daemon, cwd);
  await postHook(
    daemon.baseUrl,
    'PostToolUse',
    loadFixture(
      'post-tool-use-bash',
      { session_id: sid, cwd },
      { tool_name: 'Bash', tool_input: { command: 'npm test' } },
    ),
    { token: daemon.token },
  );
  return { sid, callsign };
}

async function makeIdle(daemon: DaemonHandle, cwd: string): Promise<SessionInfo> {
  const { sid, callsign } = await makeWorking(daemon, cwd);
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }), {
    token: daemon.token,
  });
  return { sid, callsign };
}

async function makeNeedsyou(daemon: DaemonHandle, cwd: string): Promise<SessionInfo> {
  const { sid, callsign } = await makeQueued(daemon, cwd);
  await postHook(
    daemon.baseUrl,
    'Notification',
    loadFixture('notification', { session_id: sid, cwd }),
    { token: daemon.token },
  );
  return { sid, callsign };
}

async function makeOffline(daemon: DaemonHandle, cwd: string): Promise<SessionInfo> {
  const { sid, callsign } = await makeQueued(daemon, cwd);
  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    loadFixture('session-end', { session_id: sid, cwd }),
    { token: daemon.token },
  );
  return { sid, callsign };
}

async function mailboxOf(daemon: DaemonHandle, sid: string): Promise<MailItem[]> {
  const res = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(sid)}`);
  const body = res.json as MailBody | null;
  return body?.mail ?? [];
}

async function pendingCountOf(daemon: DaemonHandle, sid: string): Promise<number> {
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateBody;
  return state.mail_pending?.[sid] ?? 0;
}

function assignAuto(daemon: DaemonHandle, target: string, text: string): Promise<JsonResponse> {
  return postJson(`${daemon.baseUrl}/command`, { text: `assign ${target} ${text}` });
}

// ---------------------------------------------------------------------------
// Routing policy matrix
// ---------------------------------------------------------------------------

test('routing ladder: idle beats queued beats working; needsyou and offline are never candidates', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const needsyou = await makeNeedsyou(daemon, cwd);
  const offline = await makeOffline(daemon, cwd);

  // Round 1: only a working session is eligible -> it wins.
  const working = await makeWorking(daemon, cwd);
  let res = await assignAuto(daemon, 'auto', 'round one task');
  assert.equal(res.status, 200);
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: working.sid, callsign: working.callsign } },
    'with only a working session eligible, it must win',
  );

  // Round 2: add a queued session -> queued must outrank working.
  const queued = await makeQueued(daemon, cwd);
  res = await assignAuto(daemon, 'auto', 'round two task');
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: queued.sid, callsign: queued.callsign } },
    'queued must beat working',
  );

  // Round 3: add an idle session -> idle must outrank both queued and working.
  const idle = await makeIdle(daemon, cwd);
  res = await assignAuto(daemon, 'auto', 'round three task');
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: idle.sid, callsign: idle.callsign } },
    'idle must beat queued and working',
  );

  // needsyou/offline must never have received any of the three assignments,
  // even though they were present as candidates in every round above.
  assert.equal(await pendingCountOf(daemon, needsyou.sid), 0, 'needsyou must never be routed to');
  assert.equal(await pendingCountOf(daemon, offline.sid), 0, 'offline must never be routed to');
});

test('routing ladder: verifying ranks with working (both below queued), never above it', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // registered as an eligible-but-lower-ranked candidate (side effect only)
  await makeVerifying(daemon, cwd);
  const queued = await makeQueued(daemon, cwd);
  const res = await assignAuto(daemon, 'auto', 'verifying-vs-queued');
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: queued.sid, callsign: queued.callsign } },
    'queued must still beat a verifying session (verifying is not idle/queued)',
  );
});

test('auto:<repo> scopes candidates by repo_name and by repo_id; each repo stays isolated from the other', async (t) => {
  const daemon = await startDaemon();
  const repoA = makeRepoWithWorktree({ repoName: 'fleet-route-a' });
  const repoB = makeRepoWithWorktree({ repoName: 'fleet-route-b' });
  t.after(async () => {
    await daemon.stop();
    repoA.cleanup();
    repoB.cleanup();
  });

  const a = await makeIdle(daemon, repoA.root);
  const b = await makeIdle(daemon, repoB.root);

  // Scope by repo_name -> only A is eligible.
  let res = await assignAuto(daemon, `auto:${repoA.repoName}`, 'scoped by name');
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: a.sid, callsign: a.callsign } },
    'auto:<repo_name> should route within that repo only',
  );
  assert.equal(
    await pendingCountOf(daemon, b.sid),
    0,
    'repo-name scoping must not reach the other repo',
  );

  // Scope by repo_id (canonicalized git-common-dir) -> only B is eligible.
  res = await assignAuto(daemon, `auto:${repoB.gitCommonDir}`, 'scoped by id');
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: b.sid, callsign: b.callsign } },
    'auto:<repo_id> should route within that repo only',
  );
  assert.equal(
    await pendingCountOf(daemon, a.sid),
    1,
    'A should still show only its one earlier assignment, untouched by the repo_id-scoped round targeting B',
  );
});

test('tie on col: fewest undelivered mail wins, overriding recency', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const older = await makeIdle(daemon, cwd); // idle first: earlier last_seen
  await new Promise<void>((resolve) => {
    setTimeout(resolve, 30);
  });
  const newer = await makeIdle(daemon, cwd); // idle second: later last_seen — would win a pure recency tie-break

  await postJson(
    `${daemon.baseUrl}/mail`,
    { to: newer.sid, from: 'operator', text: 'unrelated noise' },
    { token: daemon.token },
  );

  const res = await assignAuto(daemon, 'auto', 'tie break check');
  assert.deepEqual(
    res.json,
    { ok: true, assigned_to: { session_id: older.sid, callsign: older.callsign } },
    'fewest undelivered mail must win the tie, even though the other session is more recently active',
  );
});

test('response shapes are exact; unrouted leaves no mail anywhere and still logs to the ticker', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // Only ineligible candidates present -> no winner possible (side effect only).
  await makeNeedsyou(daemon, cwd);
  await makeOffline(daemon, cwd);

  const before = ((await getJson(`${daemon.baseUrl}/state`)).json as StateBody).ticker.length;
  const res = await assignAuto(daemon, 'auto', 'nobody home');
  // v1.2 ("Unrouted CTA") adds `text` to the unrouted shape
  // so the board can prefill a "spawn a session for this" button — see
  // tests/spawn.test.mjs's dedicated coverage of that field. Updated here
  // (touch-up, not a behavior change to this v1.1 test's own intent) so the
  // exact-shape assertion doesn't false-fail on an additive field.
  assert.deepEqual(
    res.json,
    { ok: false, unrouted: true, text: 'nobody home' },
    "no eligible candidate must produce the exact unrouted response shape (incl. v1.2's verbatim text field)",
  );

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateBody;
  const pending: Record<string, number> = state.mail_pending ?? {};
  const totalPending = Object.values(pending).reduce((acc, n) => acc + n, 0);
  assert.equal(
    totalPending,
    0,
    'an unrouted command must leave no mail anywhere (needsyou/offline included)',
  );
  assert.ok(
    state.ticker.length > before,
    'the unrouted attempt must still be logged as a ticker line',
  );
  const newest = state.ticker[0];
  assert(newest);
  assert.match(
    newest.msg ?? '',
    /assign auto|no available|unrouted/i,
    `newest ticker line should reference the unrouted attempt (got: ${JSON.stringify(newest)})`,
  );
});

test('the winning session receives exactly one mail from "orchestrator" framed as [FLEETDECK ASSIGNMENT]', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const winner = await makeIdle(daemon, cwd);
  const text = 'refactor the widget loader';
  const res = await assignAuto(daemon, 'auto', text);
  assert.equal(
    (res.json as CommandBody | null)?.assigned_to?.session_id,
    winner.sid,
    'sanity: the idle session should win',
  );

  const box = await mailboxOf(daemon, winner.sid);
  assert.equal(box.length, 1, 'the winner must receive exactly one mail');
  const first = box[0];
  assert(first);
  assert.equal(first.from, 'orchestrator');
  assert.ok(
    first.text.startsWith('[FLEETDECK ASSIGNMENT] '),
    `mail text must start with the assignment frame (got: ${JSON.stringify(first.text)})`,
  );
  assert.ok(first.text.includes(text), 'mail text must carry the routed task text');
});

test('plain "assign <callsign> <text>" still delivers directly to that session', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const target = await makeQueued(daemon, cwd);
  const res = await postJson(`${daemon.baseUrl}/command`, {
    text: `assign ${target.callsign} handle the migration`,
  });
  const resBody = res.json as CommandBody | null;
  assert(resBody);
  assert.equal(resBody.ok, true);
  assert.equal(
    resBody.delivered,
    1,
    'callsign-targeted assign should deliver to exactly one session',
  );

  const box = await mailboxOf(daemon, target.sid);
  assert.equal(box.length, 1);
  const first = box[0];
  assert(first);
  assert.equal(first.from, 'orchestrator');
  assert.ok(first.text.includes('handle the migration'), 'mail should carry the assigned text');

  // Unknown target: delivered 0, no mail anywhere, still ok:true (unchanged
  // pre-v1.1 "no such session" behavior for a plain assign).
  const res2 = await postJson(`${daemon.baseUrl}/command`, {
    text: 'assign no-such-callsign do nothing',
  });
  const res2Body = res2.json as CommandBody | null;
  assert(res2Body);
  assert.equal(res2Body.ok, true);
  assert.equal(res2Body.delivered, 0, 'assigning to an unknown callsign should deliver to nobody');
});

test('an ambiguous direct assign (a callsign shared by two live sessions) is refused; the session id disambiguates', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // Same rotation collision tests/ticket-callsign.test.mjs pins: ticketless
  // births name from (countSessions % 12 → animal) + sid4, so session #1
  // (count 0 → falcon) and session #13 (count 12 → falcon) share a callsign
  // when both session ids start with the same 4 hex chars.
  const sessionStart = (sid: string): Promise<JsonResponse> =>
    postHook(
      daemon.baseUrl,
      'SessionStart',
      loadFixture('session-start', { session_id: sid, cwd }),
      {
        token: daemon.token,
      },
    );
  const sidA = 'dead' + randomUUID().slice(4);
  const startA = await sessionStart(sidA);
  for (let i = 0; i < 11; i++) await sessionStart(randomUUID()); // advance count 1 → 12
  const sidB = 'dead' + randomUUID().slice(4);
  const startB = await sessionStart(sidB);
  const aBody = startA.json as SessionStartBody | null;
  const bBody = startB.json as SessionStartBody | null;
  assert.equal(
    aBody?.callsign,
    bBody?.callsign,
    'precondition: two live ticketless sessions share a callsign (rotation collision)',
  );
  const shared = aBody?.callsign;

  // The shared callsign must NOT fan the task out to both holders.
  const res = await postJson(`${daemon.baseUrl}/command`, {
    text: `assign ${String(shared)} refactor the thing once`,
  });
  const resBody = res.json as CommandBody | null;
  assert(resBody);
  assert.equal(resBody.ok, false, 'an ambiguous direct assign is refused, not fanned out');
  assert.match(
    resBody.reason ?? '',
    /session id/i,
    'the refusal should point the human at the session id',
  );
  assert.equal(
    await mailboxOf(daemon, sidA).then((b) => b.length),
    0,
    'holder A must receive nothing',
  );
  assert.equal(
    await mailboxOf(daemon, sidB).then((b) => b.length),
    0,
    'holder B must receive nothing',
  );

  // The session id resolves to exactly one session — the task lands once.
  const byId = await postJson(`${daemon.baseUrl}/command`, {
    text: `assign ${sidA} refactor the thing once`,
  });
  const byIdBody = byId.json as CommandBody | null;
  assert(byIdBody);
  assert.equal(byIdBody.ok, true);
  assert.equal(byIdBody.delivered, 1, 'assign by session id delivers to exactly one session');
  assert.equal(await mailboxOf(daemon, sidA).then((b) => b.length), 1);
  assert.equal(
    await mailboxOf(daemon, sidB).then((b) => b.length),
    0,
    'the other holder stays untouched',
  );
});
