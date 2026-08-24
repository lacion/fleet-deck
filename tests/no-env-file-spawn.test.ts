import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

// P11.6 characterization. Bun auto-loads a Vite-shaped `.env*` stack from the
// process cwd and INJECTS unset keys. The production daemon spawn
// (scripts/fleet-sessionstart.ts) and the test-daemon helper
// (tests/helpers/daemon.ts) both launch bun as
//   spawn(process.execPath, ['--no-env-file', <script>], …)
// to close that accidental channel. Runtime flags are consumed by bun and never
// reach process.argv, so this pins the OBSERVABLE effect — a stray cwd `.env`
// leaks in without the flag and does not with it — on the exact argv shape both
// call sites use. Mutation: drop '--no-env-file' from either call site and the
// "blocks" test below fails.

interface Probe {
  dir: string;
  script: string;
}

function probeDir(t: TestContext): Probe {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-no-env-file-'));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  // A stray project/home `.env` — the class of file the SessionStart hook meets
  // in a Claude project cwd (FLEETDECK_BIND=0.0.0.0 is the LAN scar shape).
  writeFileSync(
    path.join(dir, '.env'),
    'FLEETDECK_BIND=0.0.0.0\nFLEETDECK_TOKEN=stray-dotenv-token-aaaaaaaaaaaa\n',
  );
  const script = path.join(dir, 'dump.ts');
  writeFileSync(
    script,
    'process.stdout.write(JSON.stringify({' +
      'bind: process.env.FLEETDECK_BIND ?? null, ' +
      'token: process.env.FLEETDECK_TOKEN ?? null}));\n',
  );
  return { dir, script };
}

function runChild(args: string[], cwd: string): { bind: string | null; token: string | null } {
  // Minimal env: PATH/HOME only, so the ONLY possible source of FLEETDECK_* is
  // the cwd `.env` (this very test process may carry FLEETDECK_* from a dev shell).
  const r = spawnSync(process.execPath, args, {
    cwd,
    encoding: 'utf8',
    env: { PATH: process.env['PATH'] ?? '', HOME: process.env['HOME'] ?? '' },
    timeout: 15000,
  });
  assert.equal(r.error, undefined);
  assert.equal(r.status, 0, r.stderr);
  return JSON.parse(r.stdout.trim());
}

test('without --no-env-file a cwd .env injects unset keys into a bun child (the hazard is real)', (t) => {
  const { dir, script } = probeDir(t);
  const out = runChild([script], dir);
  assert.equal(out.bind, '0.0.0.0', 'bun auto-loaded FLEETDECK_BIND from the cwd .env');
  assert.equal(out.token, 'stray-dotenv-token-aaaaaaaaaaaa', 'and FLEETDECK_TOKEN too');
});

test('spawn(process.execPath, ["--no-env-file", script]) blocks cwd .env auto-load (P11.6)', (t) => {
  // The exact argv shape used by fleet-sessionstart.ts and tests/helpers/daemon.ts.
  const { dir, script } = probeDir(t);
  const out = runChild(['--no-env-file', script], dir);
  assert.equal(out.bind, null, 'no FLEETDECK_BIND leaks in from the cwd .env');
  assert.equal(out.token, null, 'no FLEETDECK_TOKEN leaks in from the cwd .env');
});
