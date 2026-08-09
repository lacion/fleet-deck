// tests/run-nonce.test.mjs
//
// THE RUN NONCE MUST IDENTIFY THE CLI PROCESS, NOT THE HOOK INVOCATION.
//
// BUG-025's guard refuses to tombstone on a SessionEnd whose nonce is not the
// card's active run — that is what stops a delayed async end from killing a
// live `claude --resume`. It only works if every hook of ONE CLI process
// reports the SAME nonce, and a DIFFERENT process reports a different one.
//
// The nonce used to be keyed on `process.ppid`. Claude Code runs each hook
// through a fresh intermediate shell, so that key changed on every single hook:
// SessionStart stored nonce A, the SessionEnd that followed minted B, and
// B !== A made EVERY tagged end look stale. Measured on one real session before
// the fix: 510 nonce files, 510 distinct values, none matching the card's run.
// Consequences: hook-only sessions never tombstoned on exit, an armed
// move-to-tmux could never fire, and HOME grew a file per hook.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readdirSync, rmSync, writeFileSync, utimesSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pruneRunNonces, runFileFor, runKey, runNonce } from '../scripts/fleetd/run-nonce.ts';

function home(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-runnonce-'));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

test('every hook of ONE CLI process gets the SAME nonce, whatever its parent shell is', (t) => {
  const dir = home(t);
  // Three hooks of one CLI, each spawned under a different throwaway shell —
  // the exact shape that used to mint three different nonces.
  const a = runNonce(dir, { CLAUDE_PID: '4242' }, 1001);
  const b = runNonce(dir, { CLAUDE_PID: '4242' }, 1002);
  const c = runNonce(dir, { CLAUDE_PID: '4242' }, 1003);

  assert.ok(a, 'a nonce must be minted');
  assert.equal(a, b, 'a second hook of the same CLI must reuse the nonce');
  assert.equal(b, c, 'and a third');
  const files = readdirSync(dir).filter(f => f.startsWith('run-'));
  assert.deepEqual(files, [`run-4242`],
    `exactly ONE nonce file per CLI process; got ${JSON.stringify(files)}`);
});

test('a --resume (new CLI process, same session id) gets a NEW nonce — the guard still works', (t) => {
  const dir = home(t);
  const first = runNonce(dir, { CLAUDE_PID: '4242' }, 1001);
  const resumed = runNonce(dir, { CLAUDE_PID: '9999' }, 1001);
  assert.notEqual(resumed, first,
    'a different CLI process must not inherit the previous run nonce, or a delayed '
    + 'SessionEnd from the dead process would tombstone the live resumed one');
});

test('CLAUDE_PID is preferred; ppid is only the last-resort fallback', () => {
  assert.deepEqual(runKey({ CLAUDE_PID: '4242' }, 77), { key: 4242, source: 'CLAUDE_PID' });
  // Garbage never silently becomes a key.
  for (const bad of ['', '0', '-3', 'nope', undefined]) {
    const k = runKey({ CLAUDE_PID: bad }, 77);
    assert.notEqual(k.source, 'CLAUDE_PID', `CLAUDE_PID=${JSON.stringify(bad)} must be rejected`);
    assert.ok(k.key > 0, 'a usable key is still produced');
  }
});

test('an unusable HOME leaves the event UNTAGGED rather than throwing (fail open)', () => {
  assert.equal(runNonce('', { CLAUDE_PID: '4242' }, 1), null);
  assert.equal(runNonce('/proc/nonexistent-and-unwritable', { CLAUDE_PID: '4242' }, 1), null);
});

test('prune removes nonce files of dead CLIs and never touches a live one', (t) => {
  const dir = home(t);
  const old = Date.now() / 1000 - 7200; // two hours ago

  // A live CLI: our own pid, aged so only liveness protects it.
  const live = runFileFor(dir, process.pid);
  writeFileSync(live, 'live-nonce', { mode: 0o600 });
  utimesSync(live, old, old);

  // A dead CLI: a pid that cannot exist.
  const dead = runFileFor(dir, 4194304);
  writeFileSync(dead, 'dead-nonce', { mode: 0o600 });
  utimesSync(dead, old, old);

  // A freshly minted file for a dead pid — too young to touch, because a hook
  // may be about to read the nonce it just wrote.
  const fresh = runFileFor(dir, 4194303);
  writeFileSync(fresh, 'fresh-nonce', { mode: 0o600 });

  const removed = pruneRunNonces(dir);

  assert.equal(removed, 1, 'exactly the aged dead file is collected');
  assert.ok(existsSync(live), 'a LIVE CLI must keep the nonce its next hook will read');
  assert.ok(!existsSync(dead), 'an aged file whose pid is gone is collectable');
  assert.ok(existsSync(fresh), 'a just-minted file is never yanked out from under its process');
});

test('prune is safe on a missing or empty HOME', () => {
  assert.equal(pruneRunNonces(''), 0);
  assert.equal(pruneRunNonces('/definitely/not/a/dir'), 0);
});
