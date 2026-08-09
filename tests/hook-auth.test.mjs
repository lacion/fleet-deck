// tests/hook-auth.test.mjs
//
// 0.16.0 — /hook/* now requires the bearer token in EVERY mode (hooks arrive
// through the authenticated command shims, scripts/fleet-hook.mjs et al.; a
// tokenless hook call is no longer "a CLI that cannot authenticate" — it is
// exactly the forgery the shims exist to stop). This suite pins the gate and
// the two cross-session attacks it closes:
//   - tokenless /hook/* → 401, and (fail-open contract) still never wedges a
//     session: it answers in the hook dialect, not an error page
//   - a forged UserPromptSubmit can no longer drain another session's mailbox
//     into the response or expire its pending holds
//   - a forged /clear SessionEnd+SessionStart can no longer graft succession
//   - the shim itself forwards payload + token and returns the daemon body

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson, rawRequest } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { waitUntil } from './helpers/wait.mjs';
import { openDb } from '../scripts/fleetd/db.ts';
import { createCore } from '../scripts/fleetd/derive.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SHIM = path.join(HERE, '..', 'scripts', 'fleet-hook.mjs');

function scratchCwd(prefix = 'fleetdeck-hookauth-') {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

test('tokenless /hook/* is refused with the upgrade whisper; the bearer opens it', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const sid = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // No token at all → refused, but answered in the hook dialect (a context
  // whisper telling the agent to ask the human for a restart) — never a bare
  // 401 page that would leave the old session silently dark.
  const bare = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  assert.equal(bare.status, 200, 'tokenless hook gets a dialect answer, not an error page');
  assert.match(bare.json?.hookSpecificOutput?.additionalContext ?? '', /restart/i, 'the whisper tells the human to restart');

  // Refused means REFUSED: no card was registered by the tokenless call.
  const stateAfterBare = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(!stateAfterBare.sessions.find(s => s.session_id === sid), 'tokenless hook changed no state');

  // A WRONG token gets the same treatment — forgery and legacy are not
  // distinguished, and neither carries any effect.
  const wrong = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: 'x'.repeat(64) });
  assert.equal(wrong.status, 200);
  const stateAfterWrong = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(!stateAfterWrong.sessions.find(s => s.session_id === sid), 'wrong token changed no state either');

  // The daemon's token → 200 with the SessionStart brief contract.
  const authed = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  assert.equal(authed.status, 200);
  assert.ok(authed.json?.ok, 'authenticated hook succeeds');
});

test('forged UserPromptSubmit can no longer drain a mailbox or expire holds', async (t) => {
  // Long hold window: the forgery must not expire the hold, and the board
  // answer must resolve it well before the window closes on its own.
  const holdMs = 6000;

  // Regression sabotage (BUG-166). The daemon's AUTHENTICATED activity path
  // (events.mjs hookUserPromptSubmit → expireOnActivity — F3e, expiry by
  // design) is deliberately weaponized in this daemon: every pending question
  // of the active session is expired before the real relay runs. This stands
  // in for the defect where the forged call REACHES that path: with the gate
  // intact the tokenless forgery is refused before any ingestion, the
  // sabotage never fires, and the hold survives; if the gate regresses the
  // forgery becomes session activity, the hold expires, and the assertions
  // below fail. The swap goes through a fleetd wrapper because the daemon's
  // core is module-private: the wrapper hands createHttp a Proxy of the whole
  // core whose `questions` getter returns a relay with a sabotaged
  // expireOnActivity (everything else delegates, so held sockets still settle
  // and stop() stays clean). events.mjs pins hookUserPromptSubmit to dispatch
  // through ctx.questions at call time, and the unit test below locks that
  // binding in place.
  // REGRESSION SEAM DEPENDENCY: events.mjs must dispatch the activity expiry
  // through ctx.questions at call time — if it destructures
  // `questions.expireOnActivity` at createCore time, the core Proxy below
  // routes around the real relay, the forged call expires the hold, and this
  // test FAILS even though the gate is intact. Rebind the seam in the wrapper
  // (and update the companion guard test below) if that ever changes.
  const wrapDir = scratchCwd('fleetdeck-hookauth-wrap-');
  t.after(() => rmSync(wrapDir, { recursive: true, force: true }));
  const FLEETD_DIR = path.join(HERE, '..', 'scripts', 'fleetd');
  const wrapper = path.join(wrapDir, 'fleetd-wrapper.mjs');
  writeFileSync(wrapper, `
import path from 'node:path';
import { writeFileSync } from 'node:fs';
import { openDb } from ${JSON.stringify(path.join(FLEETD_DIR, 'db.mjs'))};
import { createCore } from ${JSON.stringify(path.join(FLEETD_DIR, 'derive.mjs'))};
import { createHttp } from ${JSON.stringify(path.join(FLEETD_DIR, 'http.mjs'))};

const PORT = Number(process.env.FLEETDECK_PORT);
const HOME = process.env.FLEETDECK_HOME;
const db = openDb(path.join(HOME, 'fleetd.db'));
const core = createCore(db, { port: PORT });

// Wrap the WHOLE core: createHttp receives the proxy, so every hook handler
// reads ctx.questions through it and the sabotage is in the request path.
const sabotagedQuestions = new Proxy(core.questions, {
  get(target, prop, receiver) {
    if (prop === 'expireOnActivity') {
      return (sessionId, opts) => {
        for (const r of target.pendingOf(sessionId)) {
          db.prepare("UPDATE questions SET status = 'expired' WHERE id = ?").run(r.id);
        }
        return target.expireOnActivity(sessionId, opts);
      };
    }
    const value = Reflect.get(target, prop, receiver);
    return typeof value === 'function' ? value.bind(target) : value;
  },
});
const coreProxy = new Proxy(core, {
  get(target, prop, receiver) {
    if (prop === 'questions') return sabotagedQuestions;
    const value = Reflect.get(target, prop, receiver);
    return typeof value === 'function' ? value.bind(target) : value;
  },
});

// Mirror fleetd.mjs's boot contract: a persisted bearer token the test can
// read, then the HTTP surface.
const TOKEN = 't'.repeat(64);
writeFileSync(path.join(HOME, 'token'), TOKEN, { mode: 0o600 });
const { server } = createHttp(coreProxy, { port: PORT, token: TOKEN });
server.listen(PORT, '127.0.0.1');
`);
  // The wrapper lives outside the repo, so node_modules resolution for the
  // daemon's one runtime dep (ws) needs a hand.
  symlinkSync(path.join(HERE, '..', 'node_modules'), path.join(wrapDir, 'node_modules'), 'dir');

  const daemon = await startDaemon({
    scriptPath: wrapper,
    env: { FLEETDECK_HOLD_MS: String(holdMs) },
  });
  t.after(() => daemon.stop());

  const victim = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: victim, cwd }), { token: daemon });
  await postJson(`${daemon.baseUrl}/mail`, { to: victim, from: 'operator', text: 'secret instructions' }, { token: daemon.token });

  // Open a REAL long-lived hold on the victim session — the other thing a
  // forged UserPromptSubmit used to be able to sabotage. (Session activity
  // expires pending holds by design; a forged hook is refused before it can
  // count as activity, which is what the rest of this test pins.)
  const held = postHook(
    daemon.baseUrl, 'PermissionRequest',
    loadFixture('permission-request', { session_id: victim, cwd }),
    { token: daemon, timeout: holdMs + 5000 },
  );
  const q = await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    return (state.questions || []).find(x => x.session_id === victim && x.kind === 'permission');
  }, { label: 'permission hold to appear in /state' });
  assert.equal(q.status, 'pending', 'hold is live before the forgery');

  // The attack from the red-team report: one tokenless curl that used to
  // receive the victim's pending mail verbatim AND mark it delivered.
  const forged = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: victim, cwd }));
  assert.equal(forged.status, 200, 'tokenless forgery gets the whisper dialect');
  assert.ok(!forged.json?.hookSpecificOutput?.additionalContext?.includes('secret instructions'),
    'the response carries the whisper, never the mail');

  // The mailbox is intact: an authenticated drain still returns the mail.
  const drained = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(victim)}`);
  assert.equal(drained.json.mail?.length, 1, 'mail was neither stolen nor marked delivered');
  assert.equal(drained.json.mail[0].text, 'secret instructions');

  // The hold is intact too: still pending in /state, and the forged curl did
  // not resolve the open request (a spurious {} answer would settle it).
  const stateAfterForgery = (await getJson(`${daemon.baseUrl}/state`)).json;
  const qAfter = stateAfterForgery.questions.find(x => String(x.id) === String(q.id));
  assert.equal(qAfter?.status, 'pending', 'the forged UserPromptSubmit did not expire the pending hold');
  const settled = await Promise.race([
    held.then(() => 'resolved'),
    new Promise(r => setTimeout(r, 300)).then(() => 'still-held'),
  ]);
  assert.equal(settled, 'still-held', 'the forged UserPromptSubmit did not resolve the held hook');

  // And the hold is still genuinely answerable: a board answer resolves the
  // held request with the permission decision, long before the hold window
  // would have expired it to {}.
  const ansRes = await postJson(`${daemon.baseUrl}/api/questions/${q.id}/answer`, { behavior: 'allow' });
  assert.equal(ansRes.status, 200, 'the surviving hold still takes a board answer');
  const heldRes = await held;
  assert.deepEqual(heldRes.json, {
    hookSpecificOutput: {
      hookEventName: 'PermissionRequest',
      decision: { behavior: 'allow' },
    },
  }, 'the held PermissionRequest resolves with the board answer, not an expiry');
});

// Companion guard for the test above: it locks events.mjs's late binding —
// hookUserPromptSubmit must dispatch the activity-expiry through
// ctx.questions AT CALL TIME, never a create-time destructure. This test
// drives hookUserPromptSubmit DIRECTLY (unauthorized processing of the
// payload — exactly what the forged request must never cause) after swapping
// ctx.questions for one whose expireOnActivity expires the session's pending
// holds: the simulated activity-path regression. If a refactor rebinds the
// call, the swap stops firing, the row stays pending here, and the forged-
// forgery test above silently degrades back to a mailbox-only check.
test('unauthorized UserPromptSubmit processing expires a pending hold (regression simulation)', async (t) => {
  const db = openDb(':memory:');
  t.after(() => db.close());
  const core = createCore(db, { port: 21600, home: scratchCwd('fleetdeck-hookauth-core-') });

  const sid = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  core.hookSessionStart(loadFixture('session-start', { session_id: sid, cwd }));

  // Open a REAL hold and park a responder on it.
  const row = core.hookHoldQuestion(loadFixture('permission-request', { session_id: sid, cwd }), 'PermissionRequest');
  assert.ok(row && row.id != null, 'hold intake created a question row');
  let responded = null;
  core.questions.attachHold(row, obj => { responded = obj; });

  // Sabotage: a regression in the activity path that expires holds. Same
  // Proxy shape as the daemon wrapper in the test above — the expired-row
  // lookup goes through the relay's own prepared statements (pendingOf),
  // never a second prepare, so it sees the same live connection state.
  const real = core.questions;
  core.questions = new Proxy(real, {
    get(target, prop, receiver) {
      if (prop === 'expireOnActivity') {
        return (sessionId, opts) => {
          for (const r of target.pendingOf(sessionId)) {
            db.prepare("UPDATE questions SET status = 'expired' WHERE id = ?").run(r.id);
          }
          return target.expireOnActivity(sessionId, opts);
        };
      }
      const value = Reflect.get(target, prop, receiver);
      return typeof value === 'function' ? value.bind(target) : value;
    },
  });

  core.hookUserPromptSubmit(loadFixture('user-prompt-submit', { session_id: sid, cwd }));

  const after = db.prepare('SELECT status FROM questions WHERE id = ?').get(row.id);
  assert.equal(after.status, 'expired', 'the swapped-in expiry ran — the seam is live');
  assert.deepEqual(responded, {}, 'the held responder was released by the real relay, fail-open');
});

test('forged /clear succession graft is refused tokenless', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const victim = randomUUID();
  const heir = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: victim, cwd }), { token: daemon });
  const before = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = before.sessions.find(s => s.session_id === victim);
  assert.ok(card, 'victim on the board');

  // Tokenless SessionEnd(reason:'clear') + heir SessionStart(source:'clear'):
  // the two curls that used to steal the card's identity.
  const end = await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: victim, cwd }, { reason: 'clear' }));
  assert.equal(end.status, 200, 'refused, in dialect');
  const start = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: heir, cwd }, { source: 'clear' }));
  assert.equal(start.status, 200, 'refused, in dialect');

  const after = (await getJson(`${daemon.baseUrl}/state`)).json;
  const surviving = after.sessions.find(s => s.session_id === victim);
  assert.ok(surviving, 'victim card untouched by the forged clear');
  assert.notEqual(surviving.col, 'offline', 'victim was not tombstoned');
  assert.ok(!after.sessions.find(s => s.session_id === heir), 'forged heir never got a card');
});

test('the banner tracks legacy sessions and self-heals on their first authenticated hook', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const sid = randomUUID();
  const other = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // No legacy state before anything happens.
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.deepEqual(state.legacy_upgrade, { sessions: [], upgraded: 0 });

  // Two sessions call tokenless → both listed.
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: other, cwd }));
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.deepEqual(new Set(state.legacy_upgrade.sessions), new Set([sid, other]), 'both legacy sessions listed');

  // One restarts (its first AUTHENTICATED hook arrives) → it leaves the list
  // and the reconnected count moves. The board banner shrinks on its own.
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.deepEqual(state.legacy_upgrade.sessions, [other], 'restarted session cleared');
  assert.equal(state.legacy_upgrade.upgraded, 1, 'reconnected count moved');

  // A session that was never legacy just counts as upgraded.
  const fresh = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: fresh, cwd }), { token: daemon });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.legacy_upgrade.upgraded, 2);
  assert.deepEqual(state.legacy_upgrade.sessions, [other]);
});

test('a takeover registration carries the upgrade lines for the triggering session', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const sid = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // A legacy session is outstanding when the takeover registration lands.
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));

  const reg = await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', { session_id: randomUUID(), cwd }, { fleet_takeover: '0.15.0' }),
    { token: daemon });
  assert.equal(reg.status, 200);
  assert.ok(Array.isArray(reg.json?.upgrade_lines), 'upgrade lines present on a takeover registration');
  assert.match(reg.json.upgrade_lines.join('\n'), /0\.15\.0/, 'names the replaced version');
  assert.match(reg.json.upgrade_lines.join('\n'), /1 session\(s\)/, 'counts the outstanding legacy sessions');

  // No takeover flag → no upgrade lines (an ordinary SessionStart is unchanged).
  const plain = await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', { session_id: randomUUID(), cwd }), { token: daemon });
  assert.equal(plain.json?.upgrade_lines ?? null, null);
});

test('a legacy session that keeps calling gets ONE blocking restart instruction, then only whispers', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const sid = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // Legacy session emits tokenless events. Non-Stop events: whisper only.
  const evt = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }));
  assert.ok(evt.json?.hookSpecificOutput?.additionalContext, 'whisper on ordinary events');

  // First tokenless Stop from this session: the escalation — a turn-blocking
  // restart instruction.
  const stop1 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }));
  assert.equal(stop1.json?.decision, 'block', 'first legacy Stop blocks the turn');
  assert.match(stop1.json?.reason ?? '', /restart/i);

  // Every subsequent tokenless event, Stop included: whisper only — the block
  // is once per session per daemon boot, never a loop.
  const stop2 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }));
  assert.notEqual(stop2.json?.decision, 'block', 'no repeat block');
  assert.ok(stop2.json?.hookSpecificOutput?.additionalContext, 'whisper continues');

  // A DIFFERENT legacy session still gets its one block.
  const other = randomUUID();
  const stopOther = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: other, cwd }));
  assert.equal(stopOther.json?.decision, 'block', 'escalation is per-session');
});

test('fleet-hook.mjs shim forwards the payload with the token and relays the response', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const sid = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const payload = JSON.stringify(loadFixture('session-start', { session_id: sid, cwd }));
  const out = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SHIM, 'SessionStart'], {
      env: { ...process.env, FLEETDECK_PORT: String(daemon.port), FLEETDECK_HOME: daemon.home },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    child.stdout.on('data', d => { stdout += d; });
    child.on('error', reject);
    child.on('exit', () => resolve(stdout));
    child.stdin.end(payload);
  });
  const parsed = JSON.parse(out || '{}');
  assert.ok(parsed.ok, 'shim relayed the daemon SessionStart response');
  assert.ok(typeof parsed.callsign === 'string' && parsed.callsign, 'callsign came back through the shim');

  // And the card exists — the shim's POST was accepted as authenticated.
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(state.sessions.find(s => s.session_id === sid), 'session registered via shim');
});

test('fleet-hook.mjs fails open ({}) when the daemon is down', async (t) => {
  // No daemon at all on this port: the shim must still exit 0 with {} —
  // the foundational promise that a hook never breaks the session.
  const home = scratchCwd('fleetdeck-shim-down-');
  t.after(() => rmSync(home, { recursive: true, force: true }));
  writeFileSync(path.join(home, 'token'), 'x'.repeat(64), { mode: 0o600 });

  const out = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SHIM, 'Stop'], {
      env: { ...process.env, FLEETDECK_PORT: '21999', FLEETDECK_HOME: home },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    child.stdout.on('data', d => { stdout += d; });
    child.on('error', reject);
    child.on('exit', code => resolve({ stdout, code }));
    child.stdin.end('{}');
  });
  assert.equal(out.code, 0, 'shim exits 0 with the daemon down');
  assert.equal(out.stdout, '{}', 'shim emits the fail-open no-op');
});

// POST /hook/<Event> with arbitrary headers (Host/Origin), NO test-helper
// token attaching — for asserting the forged-proxy bypass is closed.
function rawHookPost(port, event, payload, headers = {}) {
  const body = JSON.stringify(payload);
  return rawRequest({
    port, path: `/hook/${event}`, method: 'POST',
    headers: { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body), ...headers },
    body,
  }).then(({ status, text }) => {
    let json = null;
    try { json = JSON.parse(text); } catch { /* leave null */ }
    return { status, json, text };
  });
}

// REGRESSION (0.16.1 adversarial review, HIGH): forged trusted Host/Origin must
// not authenticate a hook under PROXY_AUTH=trust. arrivedViaTrustedProxy() reads
// exactly those two headers and both are caller-controlled, so a direct loopback
// process can forge the configured trusted origin to look proxied. Under
// PROXY_AUTH=trust the proxy is the authenticator and a genuinely proxied request
// needs no token — but /hook/* must NEVER inherit that waiver, or one curl could
// impersonate any session (SessionStart/SessionEnd/Stop). authorized() gates
// /hook/* UNCONDITIONALLY, ahead of the proxy-trust exemption, so this holds in
// every mode. We prove it with TRUST_LOOPBACK both off and on (the flag is a
// red herring for this bypass — it changes nothing about the hook gate).
for (const trustLoopback of [false, true]) {
  test(`forged trusted Host/Origin cannot authenticate a hook under PROXY_AUTH=trust (TRUST_LOOPBACK ${trustLoopback ? 'on' : 'off'})`, async (t) => {
    const proxyOrigin = 'https://board.example.com';
    const forged = { host: 'board.example.com', origin: proxyOrigin };
    const env = { FLEETDECK_PROXY_AUTH: 'trust', FLEETDECK_TRUSTED_ORIGINS: proxyOrigin };
    if (trustLoopback) env.FLEETDECK_TRUST_LOOPBACK = 'on';
    const daemon = await startDaemon({ env });
    t.after(() => daemon.stop());

    const cwd = scratchCwd();
    t.after(() => rmSync(cwd, { recursive: true, force: true }));

    // The exact bypass: a tokenless SessionStart carrying the forged trusted
    // headers. It must be refused in the hook dialect and register NO card —
    // the handler is never invoked, no matter what those headers claim.
    const forgedSid = randomUUID();
    const res = await rawHookPost(daemon.port, 'SessionStart',
      loadFixture('session-start', { session_id: forgedSid, cwd }), forged);
    assert.equal(res.status, 200, 'answered in the hook dialect, not a proxy-authorized ok');
    assert.ok(!res.json?.ok, 'the forged hook did NOT register a session (no ok brief)');
    assert.match(res.json?.hookSpecificOutput?.additionalContext ?? '', /restart/i,
      'it took the tokenless-refusal path (the upgrade whisper), not the trust exemption');
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    assert.ok(!state.sessions.find(s => s.session_id === forgedSid),
      'forged proxy-trust hook changed no state');

    // Positive control: the SAME forged headers WITH the bearer succeed — the
    // gate is about authentication, not about blocking proxied hooks outright.
    const authedSid = randomUUID();
    const authed = await rawHookPost(daemon.port, 'SessionStart',
      loadFixture('session-start', { session_id: authedSid, cwd }),
      { ...forged, authorization: `Bearer ${daemon.token}` });
    assert.equal(authed.status, 200);
    assert.ok(authed.json?.ok, 'an authenticated hook still works under proxy trust');
    const state2 = (await getJson(`${daemon.baseUrl}/state`)).json;
    assert.ok(state2.sessions.find(s => s.session_id === authedSid),
      'the authenticated hook registered its session');
  });
}
