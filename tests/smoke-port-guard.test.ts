// tests/smoke-port-guard.test.ts
//
// BUG-188 regression: demo/run-smoke.sh used to infer port occupancy from a
// successful GET /health, so a process that owns the smoke port but stalls,
// closes, or speaks a non-HTTP protocol was treated as "port free" — the
// script proceeded, SessionStart could not bind fleetd, and the paid workers
// ran into a delayed false failure. The guard must probe TCP occupancy
// independently of HTTP (exclusive bind attempt) and abort on ANY bound
// listener — without ever killing it.
//
// Running the full smoke would spend real Claude usage, so the test stubs
// `claude` on PATH; a guard that misses the collision lets the script run to
// completion (exit 0) instead of aborting.

import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import path from 'node:path';
import { spawn, type ChildProcessByStdio } from 'node:child_process';
import type { Readable } from 'node:stream';
import { fileURLToPath } from 'node:url';
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';

import { scaleMs } from './helpers/wait.ts';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const RUN_SMOKE = path.join(REPO_ROOT, 'demo', 'run-smoke.sh');

type SmokeChild = ChildProcessByStdio<null, Readable, Readable>;

interface RunResult {
  code: number | null;
  output: string;
}

function freePort(): Promise<number> {
  return new Promise<number>((resolve, reject) => {
    const probe = net.createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address() as net.AddressInfo;
      probe.close(() => {
        resolve(port);
      });
    });
  });
}

// A `claude` stub that stands in for the paid worker: it never runs when the
// port guard does its job, but lets a broken guard drive the script to a
// successful exit (0) — distinguishable from the guard's abort (exit 1).
function claudeStubBin(): string {
  const bin = mkdtempSync(path.join(tmpdir(), 'fleetdeck-smoke-guard-bin-'));
  const stub = path.join(bin, 'claude');
  writeFileSync(
    stub,
    '#!/bin/sh\n# BUG-188 test stub: the paid worker must never be reached.\nexit 0\n',
    { mode: 0o755 },
  );
  chmodSync(stub, 0o755);
  return bin;
}

function runSmoke(port: number, claudeBin: string) {
  return spawn('bash', [RUN_SMOKE], {
    env: {
      ...process.env,
      PATH: `${claudeBin}:${String(process.env['PATH'])}`,
      FLEETDECK_SMOKE_PORT: String(port),
      TMPDIR: mkdtempSync(path.join(tmpdir(), 'fleetdeck-smoke-guard-tmp-')),
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function collectOutput(
  child: SmokeChild,
  timeoutMs: number,
  { onOutput }: { onOutput?: (text: string) => void } = {},
): Promise<RunResult> {
  return new Promise<RunResult>((resolve, reject) => {
    let output = '';
    const onChunk = (chunk: Buffer) => {
      output += chunk.toString();
      onOutput?.(output);
    };
    child.stdout.on('data', onChunk);
    child.stderr.on('data', onChunk);
    child.once('error', reject);
    const killTimer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(
        new Error(`run-smoke.sh did not exit within ${timeoutMs}ms; output so far:\n${output}`),
      );
    }, timeoutMs);
    child.once('close', (code) => {
      clearTimeout(killTimer);
      resolve({ code, output });
    });
  });
}

// Occupy `port` with a listener that ACCEPTS connections but never speaks:
// an HTTP health probe against it times out, so the old guard concluded the
// port was free. `release()` fires the passed callback with anything the
// script sent — proof the unknown listener was probed but never harmed.
function holdSilentListener(
  port: number,
  onData: (chunk: Buffer) => void,
): Promise<() => Promise<void>> {
  const server = net.createServer((socket) => {
    socket.on('data', onData);
    socket.on('error', () => {
      /* peer resets during teardown are expected */
    });
  });
  return new Promise<() => Promise<void>>((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => {
      resolve(
        () =>
          new Promise<void>((done) => {
            server.close(() => {
              done();
            });
          }),
      );
    });
  });
}

test(
  'BUG-188: smoke aborts on a bound non-HTTP listener that an HTTP health probe would miss',
  { timeout: scaleMs(120_000) },
  async (t) => {
    const port = await freePort();
    const abortMarker = `ABORT: something is already listening on isolated port :${port}.`;
    // Record how many bytes the script had sent the listener at the moment the
    // guard's ABORT line first appeared in its output.
    let bytesAtAbort: number | null = null;
    let received = Buffer.alloc(0);
    const release = await holdSilentListener(port, (chunk) => {
      received = Buffer.concat([received, chunk]);
    });
    const claudeBin = claudeStubBin();
    t.after(async () => {
      await release();
      rmSync(claudeBin, { recursive: true, force: true });
    });

    const { code, output } = await collectOutput(runSmoke(port, claudeBin), scaleMs(120_000), {
      onOutput: (text) => {
        if (bytesAtAbort === null && text.includes(abortMarker)) bytesAtAbort = received.length;
      },
    });

    assert.equal(code, 1, `smoke must abort on the occupied port; output:\n${output}`);
    assert.match(output, new RegExp(abortMarker.replace(/[.]/g, '\\$&')));

    // The guard must never KILL the unknown listener: it is still serving.
    await new Promise<void>((resolve, reject) => {
      const socket = net.connect(port, '127.0.0.1');
      socket.once('connect', () => socket.end(resolve));
      socket.once('error', reject);
    });

    // And the guard itself must not have spoken HTTP to the listener: the abort
    // happened BEFORE any bytes were sent. (Bytes may still arrive AFTER the
    // abort — the EXIT trap's stop_smoke_daemon health poll is pre-existing
    // behavior, unchanged by this fix, and harmless.)
    assert.equal(
      bytesAtAbort,
      0,
      'guard must probe occupancy without sending any bytes to the unknown listener',
    );
  },
);

test(
  'BUG-188: smoke port guard passes a genuinely free port (no false positive)',
  { timeout: scaleMs(120_000) },
  async (t) => {
    const port = await freePort();
    const claudeBin = claudeStubBin();
    t.after(() => {
      rmSync(claudeBin, { recursive: true, force: true });
    });

    const { code, output } = await collectOutput(runSmoke(port, claudeBin), scaleMs(120_000));

    // The guard must NOT abort: the script gets past it and launches the
    // (stubbed) workers. It then exits nonzero on the never-minted daemon
    // token — the claude stub starts no SessionStart daemon — which is the
    // expected outcome for this harness, not a guard false positive.
    assert.equal(
      code,
      1,
      `smoke should fail later on the missing daemon, not the guard; output:\n${output}`,
    );
    assert.doesNotMatch(output, /ABORT: something is already listening on isolated port/);
    assert.match(
      output,
      /T\+0 session A launched/,
      `guard must let a free port through; output:\n${output}`,
    );
  },
);
