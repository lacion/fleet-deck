// tests/max-turns-abort.test.ts
//
// Sharp edge: a --max-turns abort skips the Stop hook entirely. SessionEnd
// must still land and must be the only thing cleanup
// ever keys off of. This test never sends a Stop for the session at all.
//
// The SessionEnd below is delivered the way the CLI actually delivers it:
// through hooks/hooks.json's SessionEnd command entry into the
// scripts/fleet-hook.mjs shim (the only hook path that can present the daemon
// token). POSTing /hook/SessionEnd directly from the test would leave the
// manifest→shim delivery chain free to rot while this test stayed green — a
// misconfigured or deleted SessionEnd entry, or a shim that dies on exit,
// would leave a real card live until retention presumes it dead hours later.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon, REPO_ROOT, type DaemonHandle } from './helpers/daemon.ts';
import { postHook, getJson } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import type { StateResponse } from '../contracts/state.ts';

const HOOKS_JSON = path.join(REPO_ROOT, 'hooks', 'hooks.json');
const SHIM = path.join(REPO_ROOT, 'scripts', 'fleet-hook.mjs');

// hooks/hooks.json — the subset this test reads to prove SessionEnd routes
// through the fleet-hook.mjs command shim.
interface HookCommand {
  type: string;
  command: string;
}
interface HookGroup {
  hooks?: HookCommand[];
}
interface HooksManifest {
  hooks?: { SessionEnd?: HookGroup[] };
}

interface ShimResult {
  stdout: string;
  code: number | null;
}

// Run the SessionEnd hook exactly as the CLI would: node <shim> SessionEnd
// with the hook payload on stdin and FLEETDECK_PORT/HOME pointing at the
// isolated test daemon. Resolves { stdout, code }.
function runSessionEndShim(daemon: DaemonHandle, payload: string): Promise<ShimResult> {
  return new Promise<ShimResult>((resolve, reject) => {
    const child = spawn(process.execPath, [SHIM, 'SessionEnd'], {
      env: { ...process.env, FLEETDECK_PORT: String(daemon.port), FLEETDECK_HOME: daemon.home },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    child.stdout.on('data', (d: Buffer) => {
      stdout += d.toString();
    });
    child.on('error', reject);
    child.on('exit', (code) => {
      resolve({ stdout, code });
    });
    child.stdin.end(payload);
  });
}

test('SessionEnd tombstones a session even when Stop was never sent (max-turns abort shape)', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // The delivery contract this test protects: hooks.json must route
  // SessionEnd through the fleet-hook.mjs command shim. If this entry is
  // removed or misconfigured, a max-turns exit delivers nothing and the
  // tombstone below could never happen in production.
  const manifest = JSON.parse(readFileSync(HOOKS_JSON, 'utf8')) as HooksManifest;
  const sessionEndCmds = (manifest.hooks?.SessionEnd ?? [])
    .flatMap((group) => group.hooks ?? [])
    .filter((h) => h.type === 'command');
  const sessionEndCmd = sessionEndCmds.find((h) => h.command.includes('fleet-hook.mjs'));
  assert.ok(sessionEndCmd, 'hooks.json must route SessionEnd through scripts/fleet-hook.mjs');
  assert.match(
    sessionEndCmd.command,
    /fleet-hook\.mjs"?\s+SessionEnd/,
    'the SessionEnd hook command must pass SessionEnd as the shim argv',
  );

  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    { token: daemon },
  );
  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
    { token: daemon },
  );
  await postHook(
    daemon.baseUrl,
    'PostToolUse',
    loadFixture(
      'post-tool-use-edit',
      { session_id: sid, cwd },
      {
        tool_input: { file_path: path.join(cwd, 'util.js'), old_string: 'a', new_string: 'b' },
      },
    ),
    { token: daemon },
  );

  let state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  let card = state.sessions.find((s) => s.session_id === sid);
  assert.ok(card, 'sanity: session present before SessionEnd');
  assert.notEqual(
    card.col,
    'offline',
    'sanity: session should not already be offline before SessionEnd',
  );

  // No Stop is ever sent -- this is the point of the test. The SessionEnd
  // goes through the manifest's own delivery chain (the shim), not a direct
  // test POST, so a broken chain fails here instead of in production.
  const payload = JSON.stringify(
    loadFixture('session-end', { session_id: sid, cwd }, { reason: 'other' }),
  );
  const shim = await runSessionEndShim(daemon, payload);
  assert.equal(shim.code, 0, 'fleet-hook.mjs SessionEnd must exit 0 (never break the session)');
  assert.deepEqual(
    JSON.parse(shim.stdout || '{}'),
    {},
    'SessionEnd should respond {} through the shim',
  );

  state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  card = state.sessions.find((s) => s.session_id === sid);
  assert.ok(card, 'session should still be present (tombstoned, not deleted)');
  assert.equal(
    card.col,
    'offline',
    'SessionEnd alone must derive col=offline, with no Stop ever having fired',
  );
  assert.ok(card.endedAt, 'SessionEnd alone must stamp endedAt');
});
