// tests/port-validation.test.ts
//
// FLEETDECK_PORT must be an integer in 1..65535. Port 0 asks Node for an
// ephemeral port, but the pidfile, hooks, health checks and board URLs would
// all keep advertising the literal 0 — a live daemon no client can reach.
// resolvePort rejects it (and every other non-port value), and the daemon
// refuses to start before claiming FLEETDECK_HOME.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { resolvePort } from '../scripts/fleetd/config.ts';
import { spawnRaw, randomPort } from './helpers/daemon.ts';

function withPort(value: string | undefined, fn: () => void) {
  const saved = process.env['FLEETDECK_PORT'];
  if (value === undefined) delete process.env['FLEETDECK_PORT'];
  else process.env['FLEETDECK_PORT'] = value;
  try {
    fn();
  } finally {
    if (saved === undefined) delete process.env['FLEETDECK_PORT'];
    else process.env['FLEETDECK_PORT'] = saved;
  }
}

test('resolvePort defaults to 4711 and accepts valid ports', () => {
  withPort(undefined, () => {
    assert.equal(resolvePort(), 4711);
  });
  withPort('', () => {
    assert.equal(resolvePort(), 4711);
  });
  withPort('1', () => {
    assert.equal(resolvePort(), 1);
  });
  withPort('65535', () => {
    assert.equal(resolvePort(), 65535);
  });
  withPort(String(randomPort()), () => {
    assert.equal(resolvePort(), Number(process.env['FLEETDECK_PORT']));
  });
});

test('resolvePort rejects port 0, whitespace, and out-of-range or non-numeric values', () => {
  for (const bad of ['0', ' ', '  0  ', '-1', '65536', '1.5', 'abc', 'NaN', 'Infinity']) {
    withPort(bad, () => {
      assert.throws(
        () => resolvePort(),
        /invalid FLEETDECK_PORT/,
        `FLEETDECK_PORT=${JSON.stringify(bad)} should be rejected`,
      );
    });
  }
});

test('fleetd refuses to start on FLEETDECK_PORT=0 before claiming HOME', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-port0-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const refused = spawnRaw({ port: 0, home });
  t.after(async () => {
    await refused.kill();
  });

  const code = await refused.waitForExit(10000);
  assert.equal(code, 1, `expected exit 1 on port 0, got ${code}. stderr: ${refused.stderr}`);
  assert.match(refused.stderr, /refused to start: invalid FLEETDECK_PORT/);
});
