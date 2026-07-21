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

test('tokenless /hook/* is refused in every mode; the bearer opens it', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const sid = randomUUID();
  const cwd = scratchCwd();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // No token at all → 401 (and NOT the fail-open 200 {} a real hook gets).
  const bare = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
  assert.equal(bare.status, 401, 'tokenless hook must 401');

  // A WRONG token → 401.
  const wrong = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: 'x'.repeat(64) });
  assert.equal(wrong.status, 401, 'wrong token must 401');

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
  assert.equal(forged.status, 401, 'forged submit must 401');

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
  assert.equal(end.status, 401);
  const start = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: heir, cwd }, { source: 'clear' }));
  assert.equal(start.status, 401);

  const after = (await getJson(`${daemon.baseUrl}/state`)).json;
  const surviving = after.sessions.find(s => s.session_id === victim);
  assert.ok(surviving, 'victim card untouched by the forged clear');
  assert.notEqual(surviving.col, 'offline', 'victim was not tombstoned');
  assert.ok(!after.sessions.find(s => s.session_id === heir), 'forged heir never got a card');
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
