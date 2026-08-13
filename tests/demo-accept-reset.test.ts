// tests/demo-accept-reset.test.ts
//
// BUG-009 regression: the acceptance scripts' reset must never kill by port.
// The old discipline (`curl /health | grep -q '"ok"'` then `fuser -k PORT/tcp`)
// could kill EVERY process associated with the port — fuser -k signals clients
// too — and the substring grep treats any body containing "ok" (including
// {"ok":false}) as a Fleet Deck health proof. The reset must instead stop only
// a daemon proven by all three identities (identity-bound JSON pidfile, a
// /health reply on the port reporting the same pid, and a live node+fleetd
// process shape) and ABORT on any listener it cannot positively identify.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFile, spawn, type ExecFileOptions } from 'node:child_process';
import fs, { mkdtempSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const ACCEPT_SCRIPTS = [
  'demo/run-accept-phase3.sh',
  'demo/run-accept-plan.sh',
  'demo/run-accept-spawn.sh',
] as const;

interface RunResult {
  code: number | string;
  stdout: string;
  stderr: string;
}

interface PidRecord {
  pid?: number;
  port?: number;
}

function scratch(t: TestContext, prefix: string): string {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

function run(script: string, args: string[], options: ExecFileOptions = {}): Promise<RunResult> {
  return new Promise<RunResult>((resolve, reject) => {
    // Explicit bash: execFile picks /bin/sh for extensionless-ish scripts on
    // some platforms, and the acceptance scripts use bash-only syntax.
    execFile('bash', [script, ...args], { timeout: 30000, ...options }, (error, stdout, stderr) => {
      if (error?.killed) {
        reject(new Error(`${script} timed out`));
        return;
      }
      // execFile defaults to utf8, so stdout/stderr are strings at runtime;
      // the spread-options overload widens them to string | Buffer, so coerce.
      resolve({
        code: error ? (error.code ?? 1) : 0,
        stdout: stdout.toString(),
        stderr: stderr.toString(),
      });
    });
  });
}

// Run ONLY the reset section of an acceptance script: extract every line up to
// the reset's stop-helper + refuse-to-kill guard, then exercise
// stop_identified_daemon against scratch homes. REAL_HOME/SCRATCH_HOME are
// (re)assigned after the cut so no real pidfile is ever touched, and the
// extracted prefix is re-syntax-checked by bash itself when the harness runs.
// REAL_HOME/SCRATCH_HOME assignments INSIDE the extracted reset are rewritten
// to the scratch homes: the real defaults must never be seen by the reset.
function resetHarness(
  t: TestContext,
  scriptRel: string,
  body: string,
  { beforeGuard = '' }: { beforeGuard?: string } = {},
): { dir: string; harnessPath: string } {
  const dir = scratch(t, 'fd-accept-reset-');
  const source = fs.readFileSync(path.join(REPO_ROOT, scriptRel), 'utf8');
  const lines = source.split('\n');
  const cut = lines.findIndex((line) => line.includes('refusing to kill an unidentified listener'));
  assert.notEqual(
    cut,
    -1,
    `${scriptRel}: reset must end by refusing to kill an unidentified listener`,
  );
  const prefix = lines.slice(0, cut + 3).map((line) => {
    // through the guard's closing `fi`
    if (line.startsWith('REAL_HOME=')) return 'REAL_HOME="$HARNESS_HOME/real"';
    if (line.startsWith('SCRATCH_HOME=')) return 'SCRATCH_HOME="$HARNESS_HOME/scratch"';
    // plan/spawn hardcode FLEETDECK_PORT, so the port/BASE lines are rewritten
    // to honor the env override in every script.
    if (line.startsWith('FLEETDECK_PORT=')) return 'FLEETDECK_PORT="${FLEETDECK_PORT:-4711}"';
    if (line.startsWith('BASE=')) return 'BASE="http://127.0.0.1:$FLEETDECK_PORT"';
    return line;
  });
  // beforeGuard runs right before the refuse-to-kill port guard: the guard's
  // own verdict then proves the abort path on a live, unidentified listener.
  const guardStart = prefix.length - 4; // `if curl …; then` … `fi`
  const harness = [
    'mkdir -p "$HARNESS_HOME/real" "$HARNESS_HOME/scratch"',
    ...prefix.slice(0, guardStart),
    beforeGuard,
    ...prefix.slice(guardStart),
    body,
  ].join('\n');
  const harnessPath = path.join(dir, 'harness.sh');
  fs.writeFileSync(harnessPath, harness, { mode: 0o700 });
  if (process.env['FD_TEST_DEBUG']) {
    const debugPath = `${harnessPath}.debug`;
    fs.writeFileSync(debugPath, harness.replace(' >/dev/null 2>&1\n}', '\n}'), { mode: 0o700 });
    return { dir, harnessPath: debugPath };
  }
  return { dir, harnessPath };
}

function envFor(dir: string, extra: NodeJS.ProcessEnv = {}): NodeJS.ProcessEnv {
  return { ...process.env, HARNESS_HOME: dir, FLEETDECK_PORT: '1', ...extra };
}

function freePort(): Promise<number> {
  return new Promise<number>((resolve, reject) => {
    const probe = http.createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address() as AddressInfo;
      probe.close(() => {
        resolve(port);
      });
    });
  });
}

test('acceptance resets never kill by port: no fuser, no substring health grep', () => {
  for (const rel of ACCEPT_SCRIPTS) {
    const source = fs.readFileSync(path.join(REPO_ROOT, rel), 'utf8');
    const executable = source
      .split('\n')
      .filter((line) => !line.trimStart().startsWith('#'))
      .join('\n');
    assert.equal(
      /\bfuser\b/.test(executable),
      false,
      `${rel}: fuser kills every process associated with the port`,
    );
    assert.equal(
      /curl[^|\n]*\/health[^|\n]*\|\s*grep -q '"ok"'/.test(executable),
      false,
      `${rel}: a substring health grep matches any body containing "ok" (including {"ok":false})`,
    );
    assert.match(
      source,
      /stop_identified_daemon\(\)/,
      `${rel}: reset must stop only positively identified daemons`,
    );
  }
});

test('substring health grep accepts {"ok":false} — the old trigger was real', () => {
  // Grounds the regression: this is the exact predicate the old reset used,
  // proving a non-Fleet-Deck body would have unleashed fuser -k.
  const probe = "printf '%s' '{\"ok\":false}' | grep -q '\"ok\"'";
  return new Promise<void>((resolve, reject) => {
    execFile('bash', ['-c', probe], (error) => {
      if (error) {
        reject(new Error('grep -q \'"ok"\' must match {"ok":false}'));
        return;
      }
      resolve();
    });
  });
});

for (const rel of ACCEPT_SCRIPTS) {
  test(`reset harness extraction works: ${rel}`, async (t) => {
    // Guards the harness itself: every acceptance script's reset must carry the
    // identity-bound stop helper and the refuse-to-kill guard the harness cuts
    // on, and the extracted prefix must be runnable bash that leaves a missing
    // pidfile alone.
    const { dir, harnessPath } = resetHarness(
      t,
      rel,
      `
      if ! stop_identified_daemon "$REAL_HOME/fleetd.pid"; then
        echo "RESULT:missing-pidfile-failed"; exit 1
      fi
      if ! stop_identified_daemon "$SCRATCH_HOME/fleetd.pid"; then
        echo "RESULT:missing-pidfile-failed"; exit 1
      fi
      echo "RESULT:ok"
    `,
    );
    const out = await run(harnessPath, [], { env: envFor(dir) });
    assert.equal(out.code, 0, `${rel}: ${out.stdout}${out.stderr}`);
    assert.match(out.stdout, /RESULT:ok/);
  });
}

test('reset stops a positively identified daemon and never touches other port users', async (t) => {
  const { dir, harnessPath } = resetHarness(
    t,
    ACCEPT_SCRIPTS[0],
    `
    if ! stop_identified_daemon "$SCRATCH_HOME/fleetd.pid" 2>&1; then
      echo "RESULT:identified-daemon-refused"; exit 1
    fi
    for i in $(seq 1 50); do kill -0 "$HARNESS_DAEMON_PID" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$HARNESS_DAEMON_PID" 2>/dev/null; then echo "RESULT:daemon-survived"; exit 1; fi
    if ! kill -0 "$HARNESS_CLIENT_PID" 2>/dev/null; then echo "RESULT:client-killed"; exit 1; fi
    echo "RESULT:ok"
  `,
  );

  const fleetdLike = path.join(dir, 'fleetd.mjs');
  fs.writeFileSync(
    fleetdLike,
    `
    import fs from 'node:fs';
    import http from 'node:http';
    const home = process.env.FAKE_HOME;
    const port = Number(process.env.FAKE_PORT);
    fs.mkdirSync(home, { recursive: true });
    fs.writeFileSync(home + '/fleetd.pid', JSON.stringify({ pid: process.pid, port }));
    // /health reports this pid — the identity proof the reset requires.
    http.createServer((req, res) => {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ ok: true, pid: process.pid }));
    }).listen(port, '127.0.0.1');
  `,
  );

  const port = await freePort();
  const daemon = spawn('node', [fleetdLike], {
    env: { ...process.env, FAKE_HOME: path.join(dir, 'scratch'), FAKE_PORT: String(port) },
    stdio: 'ignore',
  });
  t.after(() => {
    try {
      daemon.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  });

  // A connected CLIENT of the port with the connection HELD OPEN: fuser -k
  // $PORT/tcp kills this too (it signals every socket owner on the port).
  const client = spawn(
    'node',
    [
      '-e',
      `
    const net = require('node:net');
    const socket = net.connect(${port}, '127.0.0.1');
    socket.on('error', () => {});
    setTimeout(() => {}, 30000);
  `,
    ],
    { stdio: 'ignore' },
  );
  t.after(() => {
    try {
      client.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  });

  // Wait for the identity-bound pidfile AND health before running the reset.
  const pidfile = path.join(dir, 'scratch', 'fleetd.pid');
  let healthOk = false;
  for (let i = 0; i < 50 && !healthOk; i += 1) {
    await new Promise<void>((resolve) => setTimeout(resolve, 100));
    let record: PidRecord | null = null;
    try {
      record = JSON.parse(fs.readFileSync(pidfile, 'utf8')) as PidRecord;
    } catch {
      /* not written yet */
    }
    if (record === null || record.pid !== daemon.pid || record.port !== port) continue;
    healthOk = await new Promise<boolean>((resolve) => {
      http
        .get({ host: '127.0.0.1', port, path: '/health' }, (res) => {
          let body = '';
          res.on('data', (chunk: Buffer) => {
            body += chunk.toString();
          });
          res.on('end', () => {
            try {
              resolve((JSON.parse(body) as PidRecord).pid === daemon.pid);
            } catch {
              resolve(false);
            }
          });
        })
        .on('error', () => {
          resolve(false);
        });
    });
  }
  assert.ok(healthOk, 'fake fleetd never came up with its identity-bound health');

  const out = await run(harnessPath, [], {
    env: envFor(dir, {
      FLEETDECK_PORT: String(port),
      HARNESS_DAEMON_PID: String(daemon.pid),
      HARNESS_CLIENT_PID: String(client.pid),
    }),
  });
  assert.equal(out.code, 0, out.stdout + out.stderr);
  assert.match(out.stdout, /RESULT:ok/);
  // Old code would have fuser -k'd the port: the client (and listener) died.
  assert.doesNotMatch(out.stdout, /RESULT:client-killed/);
});

test('reset refuses to signal an unidentified listener and aborts', async (t) => {
  const { dir, harnessPath } = resetHarness(
    t,
    ACCEPT_SCRIPTS[0],
    `
    echo "RESULT:port-clear"
  `,
    {
      beforeGuard: `
    printf '{"pid":999999,"port":0}' > "$SCRATCH_HOME/fleetd.pid"
    stop_identified_daemon "$SCRATCH_HOME/fleetd.pid"
    echo "STOP1:$?"
    echo '{"ok":false}' > "$SCRATCH_HOME/fleetd.pid"
    stop_identified_daemon "$SCRATCH_HOME/fleetd.pid"
    echo "STOP2:$?"
    `,
    },
  );

  // An unrelated listener whose body happens to contain "ok" and whose pid
  // does NOT match the pidfile record — no positive identification.
  const server = http.createServer((_req, res) => {
    res.setHeader('content-type', 'application/json');
    res.end('{"ok":false}');
  });
  t.after(() => {
    server.close();
  });
  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve();
    });
  });
  const port = (server.address() as AddressInfo).port;

  const out = await run(harnessPath, [], { env: envFor(dir, { FLEETDECK_PORT: String(port) }) });
  assert.equal(
    out.code,
    1,
    `reset must abort on an unidentified listener: ${out.stdout}${out.stderr}`,
  );
  assert.match(
    out.stdout,
    /STOP1:2/,
    'pidfile exists but names no health-proven daemon → refuse, no signal',
  );
  assert.match(out.stdout, /STOP2:2/, 'a non-JSON pidfile is not a signalable identity');
  assert.match(out.stdout, /refusing to kill an unidentified listener/);
  assert.doesNotMatch(out.stdout, /RESULT:port-clear/);
});
