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
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

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
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const victim = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: victim, cwd }), { token: daemon });
  await postJson(`${daemon.baseUrl}/mail`, { to: victim, from: 'operator', text: 'secret instructions' }, { token: daemon.token });

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
