import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  installSignalCleanup,
  PackedSmokeOwnership,
} from '../../../scripts/effect-migration/p3-packed-install-smoke.ts';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function ignoreTermination(): void {
  process.on('SIGINT', () => {});
  process.on('SIGTERM', () => {});
}

const fixture = fileURLToPath(import.meta.url);
const role = process.argv[2];

if (role === 'child') {
  ignoreTermination();
  await Bun.sleep(500);
  const pid = `${String(process.pid)}\n`;
  writeFileSync(required('PACKED_SMOKE_PRIVATE_CHILD_PID'), pid, { mode: 0o600 });
  await new Promise<never>(() => {});
} else if (role === 'daemon') {
  ignoreTermination();
  const child = Bun.spawn([process.execPath, '--no-env-file', fixture, 'child'], {
    env: process.env,
    stdin: 'ignore',
    stdout: 'ignore',
    stderr: 'ignore',
  });
  writeFileSync(required('PACKED_SMOKE_OBSERVED_CHILD_PID'), `${String(child.pid)}\n`, {
    mode: 0o600,
  });
  await new Promise<never>(() => {});
} else if (role === 'host') {
  const scratch = required('PACKED_SMOKE_SCRATCH');
  const privateChildPid = required('PACKED_SMOKE_PRIVATE_CHILD_PID');
  const ready = required('PACKED_SMOKE_READY');
  const forceMarker = process.env['PACKED_SMOKE_FORCE_MARKER'];
  const processTermMs = Number(process.env['PACKED_SMOKE_PROCESS_TERM_MS'] ?? '100');
  const signalTimeoutMs = Number(process.env['PACKED_SMOKE_SIGNAL_TIMEOUT_MS'] ?? '2500');
  if (!Number.isSafeInteger(processTermMs) || processTermMs < 1) {
    throw new Error('PACKED_SMOKE_PROCESS_TERM_MS must be a positive integer');
  }
  if (!Number.isSafeInteger(signalTimeoutMs) || signalTimeoutMs < 1) {
    throw new Error('PACKED_SMOKE_SIGNAL_TIMEOUT_MS must be a positive integer');
  }
  mkdirSync(scratch, { recursive: true });
  const ownership = new (class extends PackedSmokeOwnership {
    override forceCleanupSync(): void {
      if (forceMarker) writeFileSync(forceMarker, 'forced\n', { mode: 0o600 });
      super.forceCleanupSync();
    }
  })(scratch, {
    processTermMs,
    processKillMs: 500,
    pidTermMs: 100,
    pidKillMs: 500,
  });
  ownership.configureDaemonCleanup(privateChildPid, fixture);
  installSignalCleanup(ownership, signalTimeoutMs);
  const daemon = Bun.spawn([process.execPath, '--no-env-file', fixture, 'daemon'], {
    env: process.env,
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  ownership.registerDaemon(daemon);
  void Bun.readableStreamToText(daemon.stdout).catch(() => '');
  void Bun.readableStreamToText(daemon.stderr).catch(() => '');
  // This is deliberately published before the child marker exists. The test
  // interrupts the host without ever calling rememberOwnedPid, reproducing a
  // failure during the daemon's pre-readiness interval.
  writeFileSync(ready, `${JSON.stringify({ daemonPid: daemon.pid })}\n`, { mode: 0o600 });
  await new Promise<never>(() => {});
} else {
  throw new Error(`unknown fixture role: ${String(role)}`);
}
