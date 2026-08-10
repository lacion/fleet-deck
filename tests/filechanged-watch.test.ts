// tests/filechanged-watch.test.ts — BUG-104 regression coverage.
//
// The defect: hooks.json registered a FileChanged hook with NO matcher and
// nothing emitted hookSpecificOutput.watchPaths, so the plugin established an
// empty watch list — an external or shell-side edit to a repository file never
// reached fleet-hook.mjs, and the recent-file ledger/conflict radar stayed
// blind to it. The fix wires the harness's dynamic registration:
//   - SessionStart (fleet-sessionstart.mjs) prints the SessionStart JSON
//     contract with watchPaths = [cwd] and the roster brief in
//     additionalContext (previously plain stdout, which cannot carry watchPaths);
//   - fleet-hook.mjs re-pins watchPaths from CwdChanged (whose watchPaths
//     REPLACE the dynamic list) and from every delivered FileChanged;
//   - the daemon routes /hook/CwdChanged like FileChanged (telemetry only);
//   - hooks.json's FileChanged matcher is a literal, non-empty static watch
//     list, so the plugin never establishes an empty registration again.

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startDaemon } from './helpers/daemon.ts';
import { getJson } from './helpers/http.ts';
import type { StateResponse } from '../contracts/state.ts';

interface HooksJson {
  hooks?: { FileChanged?: { matcher?: string }[] };
}
interface HookJsonOutput {
  hookSpecificOutput?: {
    hookEventName?: string;
    watchPaths?: string[];
    additionalContext?: string;
  };
}

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '..');
const HOOKS_JSON = path.join(REPO_ROOT, 'hooks/hooks.json');
const SESSIONSTART = path.join(REPO_ROOT, 'scripts/fleet-sessionstart.mjs');
const SHIM = path.join(REPO_ROOT, 'scripts/fleet-hook.mjs');

function scratch(t: TestContext, prefix = 'fleetdeck-cwd-') {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

function runHook(
  script: string,
  event: string | null,
  payload: unknown,
  env: Record<string, string> = {},
  timeoutMs = 8000,
) {
  const child = spawn(process.execPath, event ? [script, event] : [script], {
    env: { ...process.env, ...env },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stdout = '';
  child.stdout.on('data', (d: Buffer) => {
    stdout += d.toString();
  });
  child.stdin.on('error', () => {
    /* EPIPE if the hook exits early */
  });
  child.stdin.end(JSON.stringify(payload));
  return Promise.race([
    new Promise<{ code: number | null; stdout: string }>((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', (code) => {
        resolve({ code, stdout });
      });
    }),
    new Promise<never>((_, reject) =>
      setTimeout(() => {
        child.kill('SIGKILL');
        reject(new Error('hook did not exit in time'));
      }, timeoutMs),
    ),
  ]);
}

test('hooks.json FileChanged has a matcher that cannot silently rot', () => {
  const hooks = JSON.parse(readFileSync(HOOKS_JSON, 'utf8')) as HooksJson | null;
  const matcher = hooks?.hooks?.FileChanged?.[0]?.matcher;
  assert.ok(
    typeof matcher === 'string' && matcher.length > 0,
    'a matcher-less FileChanged registers NO watched files (BUG-104)',
  );
  // FileChanged matchers are literal filenames, not regexes: every segment
  // must stay inside the narrow exact-match alphabet (letters, digits, _, |)
  // or the harness stops treating it as a watch list.
  assert.match(
    matcher,
    /^[A-Za-z0-9_|]+$/,
    'a FileChanged matcher outside the exact-match alphabet registers no literal watches',
  );
  // The plugin's own structural surfaces must always be watched, even with a
  // dead hook: Claude config files, env files, and fleet decks.
  for (const segment of ['claude', 'json', 'envrc', 'env', 'fleet', 'deck']) {
    assert.ok(
      matcher.split('|').includes(segment),
      `the static watch list must keep the '${segment}' surface`,
    );
  }
});

test('fleet-sessionstart prints the SessionStart JSON contract with watchPaths and the brief', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratch(t);
  t.after(() => daemon.stop());
  const sid = randomUUID();

  // Drive the REAL SessionStart hook end to end: it must find the daemon,
  // register the session, and print valid JSON whose hookSpecificOutput both
  // registers the cwd watch and still delivers the roster brief.
  const { code, stdout } = await runHook(
    SESSIONSTART,
    null,
    { session_id: sid, hook_event_name: 'SessionStart', cwd, source: 'startup' },
    { FLEETDECK_HOME: daemon.home, FLEETDECK_PORT: String(daemon.port) },
  );
  assert.equal(code, 0);

  const out = JSON.parse(stdout) as HookJsonOutput; // throws if the hook fell back to plain-text stdout
  const hso = out.hookSpecificOutput;
  assert.ok(hso, 'SessionStart must print hookSpecificOutput JSON, not bare brief text');
  assert.equal(hso.hookEventName, 'SessionStart');
  assert.deepEqual(
    hso.watchPaths,
    [cwd],
    'SessionStart must register the session cwd so FileChanged can fire at all',
  );
  assert.match(
    hso.additionalContext ?? '',
    /fleet/i,
    'the roster brief must survive the move into additionalContext',
  );

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.ok(
    state.sessions.some((s) => s.session_id === sid),
    'the hook must still register the session with the daemon',
  );
});

test('fleet-hook re-pins watchPaths on CwdChanged and FileChanged', async (t) => {
  const daemon = await startDaemon();
  const cwd = scratch(t);
  t.after(() => daemon.stop());
  const sid = randomUUID();
  const env = { FLEETDECK_HOME: daemon.home, FLEETDECK_PORT: String(daemon.port) };

  // CwdChanged watchPaths REPLACE the dynamic list — a bare daemon '{}'
  // forwarded verbatim would silently clear every registration after a `cd`.
  const cwdRes = await runHook(
    SHIM,
    'CwdChanged',
    { session_id: sid, hook_event_name: 'CwdChanged', cwd },
    env,
  );
  assert.equal(cwdRes.code, 0);
  assert.deepEqual(
    (JSON.parse(cwdRes.stdout) as HookJsonOutput).hookSpecificOutput?.watchPaths,
    [cwd],
    'CwdChanged must re-pin the dynamic watch list, not forward a bare {}',
  );

  // Every delivered FileChanged re-pins the list too, so the registration
  // survives whatever else replaced it.
  const file = path.join(cwd, 'notes.txt');
  const fcRes = await runHook(
    SHIM,
    'FileChanged',
    { session_id: sid, hook_event_name: 'FileChanged', cwd, file_path: file, event: 'change' },
    env,
  );
  assert.equal(fcRes.code, 0);
  assert.deepEqual(
    (JSON.parse(fcRes.stdout) as HookJsonOutput).hookSpecificOutput?.watchPaths,
    [cwd],
    'FileChanged must re-pin the dynamic watch list',
  );

  // Telemetry: the daemon saw both events (the FileChanged fed the ledger).
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = state.sessions.find((s) => s.session_id === sid);
  assert.ok(card, 'hook events through the shim must reach the daemon');
  assert.equal(card.note, 'changed notes.txt');
});
