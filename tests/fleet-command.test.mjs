// tests/fleet-command.test.mjs
//
// BUG-088 regression: the built-in /fleet command (commands/fleet.md) used to
// hardcode port 4711 in its inline curl and both board URLs, so on a
// custom-port fleet (FLEETDECK_PORT) it summarized the wrong daemon and
// printed an unusable board link.
//
// These tests extract the inline !`...` command from the markdown and run it
// with a shimmed `curl` on PATH that records the URL it was handed (the test
// environment cannot rely on loopback sockets to ephemeral ports, so the
// fetch itself is stubbed — what is under test is the URL the command
// builds). A static check asserts no hardcoded 4711 URL remains.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, rmSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FLEET_MD = path.join(REPO_ROOT, 'commands', 'fleet.md');

function inlineCommand() {
  const md = readFileSync(FLEET_MD, 'utf8');
  const m = md.match(/^!`(.+)`$/m);
  assert.ok(m, 'commands/fleet.md must contain an inline !`...` command');
  return m[1];
}

// Run the inline command with a fake `curl` that logs its argv to
// <dir>/curl-args and emits a minimal /state body. Returns the logged args.
function runInlineWithShimmedCurl(t, env) {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleet-cmd-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const argsFile = path.join(dir, 'curl-args');
  const shim = path.join(dir, 'curl');
  writeFileSync(shim, `#!/usr/bin/env bash\nprintf '%s\\n' "$@" > "${argsFile}"\necho '{"sessions":[],"conflicts":[],"mail_pending":0}'\n`);
  chmodSync(shim, 0o755);

  const r = spawnSync('bash', ['-c', inlineCommand()], {
    encoding: 'utf8',
    env: { ...process.env, ...env, PATH: `${dir}:${process.env.PATH}`, FLEETDECK_HOME: dir },
  });
  assert.equal(r.status, 0, r.stderr);
  assert.notEqual((r.stdout || '').trim(), 'FLEET_DAEMON_DOWN', 'shimmed curl must succeed');
  return readFileSync(argsFile, 'utf8').trim().split('\n');
}

function fetchedUrl(args) {
  const url = args.find((a) => a.startsWith('http://'));
  assert.ok(url, `no URL in curl args: ${args.join(' ')}`);
  return url;
}

test('BUG-088: /fleet inline command fetches /state from FLEETDECK_PORT, not hardcoded 4711', (t) => {
  const args = runInlineWithShimmedCurl(t, { FLEETDECK_PORT: '18471' });
  assert.equal(fetchedUrl(args), 'http://127.0.0.1:18471/state');
});

test('BUG-088: an invalid FLEETDECK_PORT falls back to the default port in the fetch URL', (t) => {
  const args = runInlineWithShimmedCurl(t, { FLEETDECK_PORT: 'not-a-port' });
  assert.equal(fetchedUrl(args), 'http://127.0.0.1:4711/state');
});

test('BUG-088: an unset FLEETDECK_PORT defaults to 4711 in the fetch URL', (t) => {
  const env = { ...process.env };
  delete env.FLEETDECK_PORT;
  const args = runInlineWithShimmedCurl(t, env);
  assert.equal(fetchedUrl(args), 'http://127.0.0.1:4711/state');
});

test('BUG-088: no board URL or fetch in commands/fleet.md hardcodes 4711 outside the FLEETDECK_PORT default', () => {
  const md = readFileSync(FLEET_MD, 'utf8');
  const hardcoded = md
    .split('\n')
    .filter((line) => line.includes(':4711') && !line.includes('${FLEETDECK_PORT:-4711}'));
  assert.deepEqual(hardcoded, []);
});
