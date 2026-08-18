// tests/fleet-command.test.ts
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

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, rmSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FLEET_MD = path.join(REPO_ROOT, 'commands', 'fleet.md');

function inlineCommand(): string {
  const md = readFileSync(FLEET_MD, 'utf8');
  const m = /^!`(.+)`$/m.exec(md);
  assert.ok(m, 'commands/fleet.md must contain an inline !`...` command');
  return m[1] ?? '';
}

// Run the inline command with a fake `curl` that logs its argv to
// <dir>/curl-args and emits a minimal /state body. Returns the logged args.
function runInlineWithShimmedCurl(t: TestContext, env: NodeJS.ProcessEnv): string[] {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleet-cmd-'));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const argsFile = path.join(dir, 'curl-args');
  const shim = path.join(dir, 'curl');
  writeFileSync(
    shim,
    `#!/usr/bin/env bash\nprintf '%s\\n' "$@" > "${argsFile}"\necho '{"sessions":[],"conflicts":[],"mail_pending":0}'\n`,
  );
  chmodSync(shim, 0o755);

  const r = spawnSync('bash', ['-c', inlineCommand()], {
    encoding: 'utf8',
    env: {
      ...process.env,
      ...env,
      PATH: `${dir}:${String(process.env['PATH'])}`,
      FLEETDECK_HOME: dir,
    },
  });
  assert.equal(r.status, 0, r.stderr);
  assert.notEqual((r.stdout || '').trim(), 'FLEET_DAEMON_DOWN', 'shimmed curl must succeed');
  const args = readFileSync(argsFile, 'utf8').trim().split('\n');
  const stateUrl = fetchedUrl(args);
  assert.equal(
    r.stdout.split('\n')[0],
    `FLEET_BOARD_URL=${stateUrl.replace(/state$/, '')}`,
    '/fleet must inject the resolved board URL instead of asking the model to expand shell syntax',
  );
  return args;
}

function fetchedUrl(args: string[]): string {
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

for (const invalid of ['0', '65536', '999999999999999999999999']) {
  test(`BUG-088: out-of-range FLEETDECK_PORT ${invalid} falls back safely`, (t) => {
    const args = runInlineWithShimmedCurl(t, { FLEETDECK_PORT: invalid });
    assert.equal(fetchedUrl(args), 'http://127.0.0.1:4711/state');
  });
}

test('BUG-088: an unset FLEETDECK_PORT defaults to 4711 in the fetch URL', (t) => {
  // Control the REAL process.env: runInlineWithShimmedCurl spreads ...process.env
  // into the child, so deleting the key only from a copy cannot unset it there.
  // Under bun's shared process an earlier file may have left FLEETDECK_PORT set;
  // delete it for the duration and restore after. No-op under node (never leaked).
  const saved = process.env['FLEETDECK_PORT'];
  delete process.env['FLEETDECK_PORT'];
  t.after(() => {
    if (saved !== undefined) process.env['FLEETDECK_PORT'] = saved;
  });
  const args = runInlineWithShimmedCurl(t, {});
  assert.equal(fetchedUrl(args), 'http://127.0.0.1:4711/state');
});

test('BUG-088: no board URL or fetch in commands/fleet.md hardcodes 4711 outside the FLEETDECK_PORT default', () => {
  const md = readFileSync(FLEET_MD, 'utf8');
  const hardcoded = md
    .split('\n')
    .filter((line) => line.includes(':4711') && !line.includes('${FLEETDECK_PORT:-4711}'));
  assert.deepEqual(hardcoded, []);
});
