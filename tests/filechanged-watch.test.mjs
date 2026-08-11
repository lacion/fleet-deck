// tests/filechanged-watch.test.mjs — BUG-104 regression coverage.
//
// BUG-104 fixed a dead FileChanged hook by adding static and dynamic watches.
// Both can recursively traverse directories, so FileChanged is now disabled,
// legacy requests are ignored, and SessionStart emits no dynamic watchPaths.
// CwdChanged remains telemetry-only.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { REPO_ROOT, startDaemon } from './helpers/daemon.mjs';
import { getJson } from './helpers/http.mjs';
import { scaleMs } from './helpers/wait.mjs';

const HOOKS_JSON = path.join(REPO_ROOT, 'hooks/hooks.json');
const SESSIONSTART = path.join(REPO_ROOT, 'scripts/fleet-sessionstart.mjs');
const SHIM = path.join(REPO_ROOT, 'scripts/fleet-hook.mjs');
const DEMO_HOOK_TABLES = [
  'demo/render-smoke-settings.mjs',
  'demo/run-smoke.sh',
  'demo/run-accept-plan.sh',
  'demo/run-accept-phase3.sh',
  'demo/run-accept-spawn.sh',
];

function scratch(t, prefix = 'fleetdeck-cwd-') {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

function runHook(script, event, payload, env = {}, timeoutMs = 8000) {
  const child = spawn(process.execPath, event ? [script, event] : [script], {
    env: { ...process.env, ...env },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stdout = '';
  child.stdout.on('data', d => { stdout += d; });
  child.stdin.on('error', () => {}); // EPIPE if the hook exits early
  child.stdin.end(JSON.stringify(payload));
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error('hook did not exit in time'));
    }, scaleMs(timeoutMs));
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', code => { clearTimeout(timer); resolve({ code, stdout }); });
  });
}

test('production and demo hook tables do not register FileChanged', () => {
  const hooks = JSON.parse(readFileSync(HOOKS_JSON, 'utf8'));
  assert.equal(Object.hasOwn(hooks.hooks, 'FileChanged'), false,
    'production must not start Claude Code\'s recursive file watcher');
  for (const rel of DEMO_HOOK_TABLES) {
    assert.doesNotMatch(readFileSync(path.join(REPO_ROOT, rel), 'utf8'), /FileChanged["']?\s*:/,
      `${rel} must not register FileChanged`);
  }
});

test('legacy FileChanged hook requests are accepted without ingesting telemetry', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratch(t);
  t.after(() => daemon.stop());
  const sid = randomUUID();

  const { code, stdout } = await runHook(SHIM, 'FileChanged',
    { session_id: sid, hook_event_name: 'FileChanged', cwd, file_path: path.join(cwd, 'large-tree') },
    { FLEETDECK_HOME: daemon.home, FLEETDECK_PORT: String(daemon.port) });
  assert.equal(code, 0);
  assert.deepEqual(JSON.parse(stdout), {});

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.sessions.some(s => s.session_id === sid), false,
    'legacy FileChanged traffic must not create or mutate a session card');
});

test('fleet-sessionstart returns plain context without structured watch output', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratch(t);
  t.after(() => daemon.stop());
  const sid = randomUUID();

  // Drive the real SessionStart hook end to end: it must register the session
  // and deliver the roster brief without handing the cwd to the file watcher.
  const { code, stdout } = await runHook(SESSIONSTART, null,
    { session_id: sid, hook_event_name: 'SessionStart', cwd, source: 'startup' },
    { FLEETDECK_HOME: daemon.home, FLEETDECK_PORT: String(daemon.port) });
  assert.equal(code, 0);

  assert.match(stdout, /fleet/i, 'SessionStart must still deliver the roster brief');
  assert.throws(() => JSON.parse(stdout),
    'SessionStart must use plain stdout, not structured hook output that can register watches');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.ok(state.sessions.some(s => s.session_id === sid),
    'the hook must still register the session with the daemon');
});

test('fleet-hook forwards CwdChanged telemetry without dynamic watchPaths', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratch(t);
  t.after(() => daemon.stop());
  const sid = randomUUID();
  const env = { FLEETDECK_HOME: daemon.home, FLEETDECK_PORT: String(daemon.port) };

  // CwdChanged remains visible to the daemon, but its hook response must not
  // introduce a dynamic file watch.
  const cwdRes = await runHook(SHIM, 'CwdChanged',
    { session_id: sid, hook_event_name: 'CwdChanged', cwd }, env);
  assert.equal(cwdRes.code, 0);
  assert.equal(JSON.parse(cwdRes.stdout).hookSpecificOutput?.watchPaths, undefined,
    'CwdChanged must not emit dynamic watchPaths');

  // Telemetry: the daemon saw the cwd change.
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = state.sessions.find(s => s.session_id === sid);
  assert.ok(card, 'hook events through the shim must reach the daemon');
  assert.equal(card.note, `cwd → ${path.basename(cwd)}`);
});
