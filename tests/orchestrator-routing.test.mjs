// tests/orchestrator-routing.test.mjs
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

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeRepoWithWorktree } from './helpers/gitrepo.mjs';

function scratchCwd() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
}

// ---------------------------------------------------------------------------
// Session builders — each replays the exact hook sequence
// session-lifecycle.test.mjs proves derives the named column, so this file
// never has to reach into fleetd internals to force a col.
// ---------------------------------------------------------------------------

async function makeQueued(daemon, cwd) {
  const sid = randomUUID();
  const reg = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  assert.ok(reg.json?.callsign, 'setup: SessionStart should hand back a callsign');
  return { sid, callsign: reg.json.callsign };
}

async function makeWorking(daemon, cwd) {
  const { sid, callsign } = await makeQueued(daemon, cwd);
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
  return { sid, callsign };
}

async function makeVerifying(daemon, cwd) {
  const { sid, callsign } = await makeWorking(daemon, cwd);
  await postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-bash', { session_id: sid, cwd }, {
    tool_name: 'Bash', tool_input: { command: 'npm test' },
  }));
  return { sid, callsign };
}

async function makeIdle(daemon, cwd) {
  const { sid, callsign } = await makeWorking(daemon, cwd);
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }));
  return { sid, callsign };
}

async function makeNeedsyou(daemon, cwd) {
  const { sid, callsign } = await makeQueued(daemon, cwd);
  await postHook(daemon.baseUrl, 'Notification', loadFixture('notification', { session_id: sid, cwd }));
  return { sid, callsign };
}

async function makeOffline(daemon, cwd) {
  const { sid, callsign } = await makeQueued(daemon, cwd);
  await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sid, cwd }));
  return { sid, callsign };
}

async function mailboxOf(daemon, sid) {
  const res = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(sid)}`);
  return res.json?.mail ?? [];
}

async function pendingCountOf(daemon, sid) {
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  return state.mail_pending?.[sid] ?? 0;
}

function assignAuto(daemon, target, text) {
  return postJson(`${daemon.baseUrl}/command`, { text: `assign ${target} ${text}` });
}

// ---------------------------------------------------------------------------
// Routing policy matrix
// ---------------------------------------------------------------------------

test('routing ladder: idle beats queued beats working; needsyou and offline are never candidates', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const needsyou = await makeNeedsyou(daemon, cwd);
  const offline = await makeOffline(daemon, cwd);

  // Round 1: only a working session is eligible -> it wins.
  const working = await makeWorking(daemon, cwd);
  let res = await assignAuto(daemon, 'auto', 'round one task');
  assert.equal(res.status, 200);
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: working.sid, callsign: working.callsign } },
    'with only a working session eligible, it must win');

  // Round 2: add a queued session -> queued must outrank working.
  const queued = await makeQueued(daemon, cwd);
  res = await assignAuto(daemon, 'auto', 'round two task');
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: queued.sid, callsign: queued.callsign } },
    'queued must beat working');

  // Round 3: add an idle session -> idle must outrank both queued and working.
  const idle = await makeIdle(daemon, cwd);
  res = await assignAuto(daemon, 'auto', 'round three task');
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: idle.sid, callsign: idle.callsign } },
    'idle must beat queued and working');

  // needsyou/offline must never have received any of the three assignments,
  // even though they were present as candidates in every round above.
  assert.equal(await pendingCountOf(daemon, needsyou.sid), 0, 'needsyou must never be routed to');
  assert.equal(await pendingCountOf(daemon, offline.sid), 0, 'offline must never be routed to');
});

test('routing ladder: verifying ranks with working (both below queued), never above it', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const verifying = await makeVerifying(daemon, cwd);
  const queued = await makeQueued(daemon, cwd);
  const res = await assignAuto(daemon, 'auto', 'verifying-vs-queued');
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: queued.sid, callsign: queued.callsign } },
    'queued must still beat a verifying session (verifying is not idle/queued)');
});

test('auto:<repo> scopes candidates by repo_name and by repo_id; each repo stays isolated from the other', async (t) => {
  const daemon = await startDaemon();
  const repoA = makeRepoWithWorktree({ repoName: 'fleet-route-a' });
  const repoB = makeRepoWithWorktree({ repoName: 'fleet-route-b' });
  t.after(async () => { await daemon.stop(); repoA.cleanup(); repoB.cleanup(); });

  const a = await makeIdle(daemon, repoA.root);
  const b = await makeIdle(daemon, repoB.root);

  // Scope by repo_name -> only A is eligible.
  let res = await assignAuto(daemon, `auto:${repoA.repoName}`, 'scoped by name');
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: a.sid, callsign: a.callsign } },
    'auto:<repo_name> should route within that repo only');
  assert.equal(await pendingCountOf(daemon, b.sid), 0, 'repo-name scoping must not reach the other repo');

  // Scope by repo_id (canonicalized git-common-dir) -> only B is eligible.
  res = await assignAuto(daemon, `auto:${repoB.gitCommonDir}`, 'scoped by id');
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: b.sid, callsign: b.callsign } },
    'auto:<repo_id> should route within that repo only');
  assert.equal(await pendingCountOf(daemon, a.sid), 1,
    'A should still show only its one earlier assignment, untouched by the repo_id-scoped round targeting B');
});

test('tie on col: fewest undelivered mail wins, overriding recency', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const older = await makeIdle(daemon, cwd); // idle first: earlier last_seen
  await new Promise(r => setTimeout(r, 30));
  const newer = await makeIdle(daemon, cwd); // idle second: later last_seen — would win a pure recency tie-break

  await postJson(`${daemon.baseUrl}/mail`, { to: newer.sid, from: 'human', text: 'unrelated noise' });

  const res = await assignAuto(daemon, 'auto', 'tie break check');
  assert.deepEqual(res.json, { ok: true, assigned_to: { session_id: older.sid, callsign: older.callsign } },
    'fewest undelivered mail must win the tie, even though the other session is more recently active');
});

test('response shapes are exact; unrouted leaves no mail anywhere and still logs to the ticker', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  // Only ineligible candidates present -> no winner possible.
  const needsyou = await makeNeedsyou(daemon, cwd);
  const offline = await makeOffline(daemon, cwd);

  const before = (await getJson(`${daemon.baseUrl}/state`)).json.ticker.length;
  const res = await assignAuto(daemon, 'auto', 'nobody home');
  // v1.2 ("Unrouted CTA") adds `text` to the unrouted shape
  // so the board can prefill a "spawn a session for this" button — see
  // tests/spawn.test.mjs's dedicated coverage of that field. Updated here
  // (touch-up, not a behavior change to this v1.1 test's own intent) so the
  // exact-shape assertion doesn't false-fail on an additive field.
  assert.deepEqual(res.json, { ok: false, unrouted: true, text: 'nobody home' },
    'no eligible candidate must produce the exact unrouted response shape (incl. v1.2\'s verbatim text field)');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const totalPending = Object.values(state.mail_pending || {}).reduce((a, b) => a + b, 0);
  assert.equal(totalPending, 0, 'an unrouted command must leave no mail anywhere (needsyou/offline included)');
  assert.ok(state.ticker.length > before, 'the unrouted attempt must still be logged as a ticker line');
  assert.match(state.ticker[0]?.msg ?? '', /assign auto|no available|unrouted/i,
    `newest ticker line should reference the unrouted attempt (got: ${JSON.stringify(state.ticker[0])})`);
});

test('the winning session receives exactly one mail from "orchestrator" framed as [FLEETDECK ASSIGNMENT]', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const winner = await makeIdle(daemon, cwd);
  const text = 'refactor the widget loader';
  const res = await assignAuto(daemon, 'auto', text);
  assert.equal(res.json?.assigned_to?.session_id, winner.sid, 'sanity: the idle session should win');

  const box = await mailboxOf(daemon, winner.sid);
  assert.equal(box.length, 1, 'the winner must receive exactly one mail');
  assert.equal(box[0].from, 'orchestrator');
  assert.ok(box[0].text.startsWith('[FLEETDECK ASSIGNMENT] '),
    `mail text must start with the assignment frame (got: ${JSON.stringify(box[0].text)})`);
  assert.ok(box[0].text.includes(text), 'mail text must carry the routed task text');
});

test('plain "assign <callsign> <text>" still delivers directly to that session', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratchCwd();
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true }); });

  const target = await makeQueued(daemon, cwd);
  const res = await postJson(`${daemon.baseUrl}/command`, { text: `assign ${target.callsign} handle the migration` });
  assert.equal(res.json?.ok, true);
  assert.equal(res.json?.delivered, 1, 'callsign-targeted assign should deliver to exactly one session');

  const box = await mailboxOf(daemon, target.sid);
  assert.equal(box.length, 1);
  assert.equal(box[0].from, 'orchestrator');
  assert.ok(box[0].text.includes('handle the migration'), 'mail should carry the assigned text');

  // Unknown target: delivered 0, no mail anywhere, still ok:true (unchanged
  // pre-v1.1 "no such session" behavior for a plain assign).
  const res2 = await postJson(`${daemon.baseUrl}/command`, { text: 'assign no-such-callsign do nothing' });
  assert.equal(res2.json?.ok, true);
  assert.equal(res2.json?.delivered, 0, 'assigning to an unknown callsign should deliver to nobody');
});
