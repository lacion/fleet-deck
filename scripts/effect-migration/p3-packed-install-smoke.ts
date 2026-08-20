import { createHash, randomUUID } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertCommand, type CommandResult, REPO_ROOT } from './metrics.ts';

const COMMAND_TIMEOUT_MS = 120_000;
const HEALTH_TIMEOUT_MS = 10_000;
const CHILD_TIMEOUT_MS = 8_000;
const SHUTDOWN_TIMEOUT_MS = 10_000;
const RESIDUE_TIMEOUT_MS = 3_000;
const PROCESS_TERM_TIMEOUT_MS = 2_000;
const PROCESS_KILL_TIMEOUT_MS = 1_000;
const PID_TERM_TIMEOUT_MS = 500;
const PID_KILL_TIMEOUT_MS = 1_000;
const OUTPUT_TIMEOUT_MS = 2_000;
const SIGNAL_CLEANUP_TIMEOUT_MS = 6_000;

interface HealthBody {
  managed?: unknown;
  ok?: unknown;
  pid?: unknown;
  startup?: unknown;
  version?: unknown;
}

interface PidRecord {
  pid?: unknown;
  port?: unknown;
}

interface ResidueState {
  daemonAlive: boolean;
  pidFilePresent: boolean;
  portAvailable: boolean;
  descendantsAlive: number[];
}

interface CleanupTimeouts {
  processTermMs: number;
  processKillMs: number;
  pidTermMs: number;
  pidKillMs: number;
}

interface StreamCapture {
  cancel: () => Promise<void>;
  promise: Promise<string>;
  snapshot: () => string;
}

type SettledWithin<T> = { settled: true; value: T } | { settled: false };

const DEFAULT_CLEANUP_TIMEOUTS: CleanupTimeouts = {
  processTermMs: PROCESS_TERM_TIMEOUT_MS,
  processKillMs: PROCESS_KILL_TIMEOUT_MS,
  pidTermMs: PID_TERM_TIMEOUT_MS,
  pidKillMs: PID_KILL_TIMEOUT_MS,
};

const CHILD_FIXTURE = `import { writeFileSync } from 'node:fs';

const marker = process.env['FLEETDECK_PACKED_SMOKE_CHILD_PID'];
if (!marker) throw new Error('FLEETDECK_PACKED_SMOKE_CHILD_PID is required');

writeFileSync(marker, \`${'${process.pid}'}\\n\`, { mode: 0o600 });
await new Promise((resolve) => setTimeout(resolve, 60_000));
process.stdout.write('[]');
`;

function invariant(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isPidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return !(isObject(error) && typeof error['code'] === 'string' && error['code'] === 'ESRCH');
  }
}

async function settleWithin<T>(promise: Promise<T>, timeoutMs: number): Promise<SettledWithin<T>> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise.then((value) => ({ settled: true as const, value })),
      new Promise<SettledWithin<T>>((resolve) => {
        timer = setTimeout(() => resolve({ settled: false }), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

function captureStream(stream: ReadableStream<Uint8Array>): StreamCapture {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let output = '';
  const promise = (async (): Promise<string> => {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        output += decoder.decode();
        return output;
      }
      output += decoder.decode(value, { stream: true });
    }
  })();
  // A forced process teardown may reject the pending pipe read. Consumers use
  // the bounded snapshot below, so suppress an otherwise-unhandled rejection.
  void promise.catch(() => '');
  return {
    promise,
    snapshot: () => output,
    cancel: async () => {
      try {
        await reader.cancel();
      } catch {
        // The process can close the pipe while cleanup cancels the reader.
      }
    },
  };
}

async function settleCapture(
  capture: StreamCapture | undefined,
  timeoutMs = OUTPUT_TIMEOUT_MS,
): Promise<string> {
  if (!capture) return '';
  const settled = await settleWithin(
    capture.promise.catch(() => capture.snapshot()),
    timeoutMs,
  );
  if (settled.settled) return settled.value;
  await settleWithin(capture.cancel(), 250);
  const afterCancel = await settleWithin(
    capture.promise.catch(() => capture.snapshot()),
    250,
  );
  return afterCancel.settled ? afterCancel.value : capture.snapshot();
}

function signalProcess(processHandle: Bun.ReadableSubprocess, signal: NodeJS.Signals): void {
  if (processHandle.exitCode !== null) return;
  try {
    processHandle.kill(signal);
  } catch {
    // Exit can win the observation-to-signal race.
  }
}

async function stopSubprocess(
  processHandle: Bun.ReadableSubprocess,
  timeouts: Pick<CleanupTimeouts, 'processTermMs' | 'processKillMs'> = DEFAULT_CLEANUP_TIMEOUTS,
): Promise<number | null> {
  if (processHandle.exitCode !== null) return processHandle.exitCode;
  signalProcess(processHandle, 'SIGTERM');
  const afterTerm = await settleWithin(processHandle.exited, timeouts.processTermMs);
  if (afterTerm.settled) return afterTerm.value;
  signalProcess(processHandle, 'SIGKILL');
  const afterKill = await settleWithin(processHandle.exited, timeouts.processKillMs);
  return afterKill.settled ? afterKill.value : null;
}

async function readPs(args: readonly string[]): Promise<string | null> {
  const processHandle = Bun.spawn(['ps', ...args], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const capture = captureStream(processHandle.stdout);
  const stderrCapture = captureStream(processHandle.stderr);
  const exit = await settleWithin(processHandle.exited, 2_000);
  if (!exit.settled) {
    signalProcess(processHandle, 'SIGKILL');
    await settleWithin(processHandle.exited, 500);
  }
  const stdout = await settleCapture(capture, 500);
  await settleCapture(stderrCapture, 500);
  return exit.settled && exit.value === 0 ? stdout : null;
}

async function reservePort(): Promise<number> {
  const reservation = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    fetch: () => new Response('reserved'),
  });
  const port = reservation.port;
  await reservation.stop(true);
  invariant(
    typeof port === 'number' && Number.isSafeInteger(port) && port > 0,
    'Bun.serve did not allocate a valid port',
  );
  return port;
}

async function portCanRebind(port: number): Promise<boolean> {
  let probe: ReturnType<typeof Bun.serve> | undefined;
  try {
    probe = Bun.serve({
      hostname: '127.0.0.1',
      port,
      fetch: () => new Response('rebind probe'),
    });
    return true;
  } catch {
    return false;
  } finally {
    if (probe) await probe.stop(true);
  }
}

async function awaitExit(
  processHandle: Bun.ReadableSubprocess,
  timeoutMs: number,
): Promise<number> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      processHandle.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(
                `daemon pid ${processHandle.pid} did not exit within ${String(timeoutMs)}ms`,
              ),
            ),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function waitForHealth(
  processHandle: Bun.ReadableSubprocess,
  baseUrl: string,
): Promise<HealthBody> {
  const deadline = Date.now() + HEALTH_TIMEOUT_MS;
  let lastError = 'no response';
  while (Date.now() < deadline) {
    if (processHandle.exitCode !== null) {
      throw new Error(`installed daemon exited ${String(processHandle.exitCode)} before /health`);
    }
    try {
      const response = await fetch(`${baseUrl}/health?packed=${randomUUID()}`, {
        signal: AbortSignal.timeout(500),
      });
      const body: unknown = await response.json();
      if (response.ok && isObject(body)) {
        if (body['pid'] !== processHandle.pid) {
          throw new Error(
            `/health belongs to pid ${String(body['pid'])}, not packed daemon pid ${String(processHandle.pid)}`,
          );
        }
        if (body['ok'] === true && body['startup'] === 'settled') return body;
      }
      lastError = `HTTP ${String(response.status)} with incomplete readiness body`;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await Bun.sleep(25);
  }
  throw new Error(`installed daemon did not become healthy: ${lastError}`);
}

function readPositivePid(file: string): number | null {
  if (!existsSync(file)) return null;
  const pid = Number(readFileSync(file, 'utf8').trim());
  return Number.isSafeInteger(pid) && pid > 0 ? pid : null;
}

async function waitForChild(file: string): Promise<number> {
  const deadline = Date.now() + CHILD_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const pid = readPositivePid(file);
    if (pid !== null && isPidAlive(pid)) return pid;
    await Bun.sleep(25);
  }
  throw new Error('installed daemon did not start the packed-smoke agents child');
}

async function descendantsOf(parentPid: number): Promise<number[]> {
  const stdout = await readPs(['-A', '-o', 'pid=', '-o', 'ppid=']);
  if (stdout === null) return [];
  return parseDescendants(stdout, parentPid);
}

function parseDescendants(stdout: string, parentPid: number): number[] {
  const children = new Map<number, number[]>();
  for (const line of stdout.split('\n')) {
    const match = /^\s*(\d+)\s+(\d+)\s*$/.exec(line);
    if (!match) continue;
    const pid = Number(match[1]);
    const ppid = Number(match[2]);
    const owned = children.get(ppid) ?? [];
    owned.push(pid);
    children.set(ppid, owned);
  }

  const found: number[] = [];
  const queued = [...(children.get(parentPid) ?? [])];
  while (queued.length > 0) {
    const pid = queued.shift();
    if (pid === undefined || found.includes(pid)) continue;
    found.push(pid);
    queued.push(...(children.get(pid) ?? []));
  }
  return found.sort((left, right) => left - right);
}

function readPsSync(args: readonly string[]): string | null {
  const result = Bun.spawnSync(['ps', ...args], {
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'ignore',
    timeout: 250,
    killSignal: 'SIGKILL',
    maxBuffer: 1024 * 1024,
  });
  return result.exitCode === 0 ? result.stdout.toString() : null;
}

function descendantsOfSync(parentPid: number): number[] {
  const stdout = readPsSync(['-A', '-o', 'pid=', '-o', 'ppid=']);
  return stdout === null ? [] : parseDescendants(stdout, parentPid);
}

async function processIdentity(pid: number): Promise<string | null> {
  const stdout = await readPs(['-p', String(pid), '-o', 'lstart=', '-o', 'command=']);
  const identity = stdout?.trim();
  return identity ? identity : null;
}

function processIdentitySync(pid: number): string | null {
  const identity = readPsSync(['-p', String(pid), '-o', 'lstart=', '-o', 'command='])?.trim();
  return identity ? identity : null;
}

async function liveOwnedPids(
  identities: ReadonlyMap<number, string>,
): Promise<Map<number, string>> {
  const live = new Map<number, string>();
  await Promise.all(
    [...identities].map(async ([pid, identity]) => {
      if (pid !== process.pid && (await processIdentity(pid)) === identity) live.set(pid, identity);
    }),
  );
  return live;
}

async function waitForOwnedPidsGone(
  identities: ReadonlyMap<number, string>,
  timeoutMs: number,
): Promise<Map<number, string>> {
  const deadline = Date.now() + timeoutMs;
  let alive = await liveOwnedPids(identities);
  while (alive.size > 0 && Date.now() < deadline) {
    await Bun.sleep(20);
    alive = await liveOwnedPids(alive);
  }
  return alive;
}

async function signalOwnedPids(
  identities: ReadonlyMap<number, string>,
  signal: NodeJS.Signals,
): Promise<Map<number, string>> {
  const signaled = new Map<number, string>();
  for (const [pid, identity] of identities) {
    if (pid === process.pid || (await processIdentity(pid)) !== identity) continue;
    // Keep identity-matching failures in the wait/escalation set. EPERM (or any
    // non-ESRCH race) must become a cleanup failure, never false success.
    signaled.set(pid, identity);
    try {
      process.kill(pid, signal);
    } catch {
      // The bounded identity re-check below distinguishes exit from failure.
    }
  }
  return signaled;
}

class PackedSmokeOwnership {
  readonly scratch: string;
  private readonly timeouts: CleanupTimeouts;
  private readonly processes = new Map<Bun.ReadableSubprocess, string>();
  private readonly knownOwnedPids = new Map<number, string>();
  private daemon: Bun.ReadableSubprocess | undefined;
  private childPidFile: string | undefined;
  private childFixture: string | undefined;
  private closing = false;
  private cleanupPromise: Promise<void> | undefined;

  constructor(scratch: string, timeouts: Partial<CleanupTimeouts> = {}) {
    this.scratch = scratch;
    this.timeouts = { ...DEFAULT_CLEANUP_TIMEOUTS, ...timeouts };
  }

  registerProcess(processHandle: Bun.ReadableSubprocess, label: string): void {
    if (this.closing) {
      signalProcess(processHandle, 'SIGKILL');
      throw new Error(`packed-smoke cleanup already started before ${label} registration`);
    }
    this.processes.set(processHandle, label);
  }

  releaseProcess(processHandle: Bun.ReadableSubprocess): void {
    this.processes.delete(processHandle);
  }

  configureDaemonCleanup(childPidFile: string, childFixture: string): void {
    invariant(!this.closing, 'packed-smoke cleanup started before daemon cleanup configuration');
    this.childPidFile = childPidFile;
    this.childFixture = childFixture;
  }

  registerDaemon(processHandle: Bun.ReadableSubprocess): void {
    invariant(this.daemon === undefined, 'packed-smoke daemon was registered twice');
    invariant(
      this.childPidFile !== undefined && this.childFixture !== undefined,
      'daemon cleanup metadata must be configured before spawn',
    );
    this.daemon = processHandle;
    this.registerProcess(processHandle, 'installed daemon');
  }

  async rememberOwnedPid(pid: number): Promise<void> {
    if (pid === process.pid || pid <= 0) return;
    const identity = await processIdentity(pid);
    invariant(identity !== null, `owned pid ${String(pid)} exited before identity capture`);
    invariant(
      this.childFixture === undefined || identity.includes(this.childFixture),
      `pid ${String(pid)} does not identify the packed-smoke child fixture`,
    );
    this.knownOwnedPids.set(pid, identity);
  }

  assertAcceptingWork(): void {
    invariant(!this.closing, 'packed-smoke cleanup has started');
  }

  private async recordOwnedPid(
    owned: Map<number, string>,
    pid: number,
    requiredCommand?: string,
  ): Promise<void> {
    if (pid === process.pid || pid <= 0) return;
    const identity = await processIdentity(pid);
    if (
      identity === null ||
      (requiredCommand !== undefined && !identity.includes(requiredCommand))
    ) {
      return;
    }
    owned.set(pid, identity);
    this.knownOwnedPids.set(pid, identity);
  }

  private async collectOwnedPids(): Promise<Map<number, string>> {
    const owned = new Map(this.knownOwnedPids);
    for (const processHandle of this.processes.keys()) {
      if (!isPidAlive(processHandle.pid)) continue;
      await Promise.all(
        (await descendantsOf(processHandle.pid)).map((pid) => this.recordOwnedPid(owned, pid)),
      );
    }

    const markerPid = this.childPidFile ? readPositivePid(this.childPidFile) : null;
    if (markerPid !== null && markerPid !== process.pid) {
      await this.recordOwnedPid(owned, markerPid, this.childFixture);
    }
    return owned;
  }

  private async cleanupOnce(): Promise<void> {
    const ownedPids = await this.collectOwnedPids();
    const activeProcesses = [...this.processes.keys()];
    await Promise.all(
      activeProcesses.map((processHandle) => stopSubprocess(processHandle, this.timeouts)),
    );
    for (const processHandle of activeProcesses) {
      if (processHandle.exitCode === null && isPidAlive(processHandle.pid)) {
        await this.recordOwnedPid(ownedPids, processHandle.pid);
      }
    }

    // The fixture can publish its pid after the first scan but before its daemon
    // settles. Re-read the private marker after stopping every registered owner.
    const markerPid = this.childPidFile ? readPositivePid(this.childPidFile) : null;
    if (markerPid !== null && markerPid !== process.pid) {
      await this.recordOwnedPid(ownedPids, markerPid, this.childFixture);
    }

    const termSignaled = await signalOwnedPids(ownedPids, 'SIGTERM');
    const afterTerm = await waitForOwnedPidsGone(termSignaled, this.timeouts.pidTermMs);
    const killSignaled = await signalOwnedPids(afterTerm, 'SIGKILL');
    const afterKill = await waitForOwnedPidsGone(killSignaled, this.timeouts.pidKillMs);
    invariant(
      afterKill.size === 0,
      `packed-smoke cleanup could not reap owned pids: ${[...afterKill.keys()].join(', ')}`,
    );
    for (const pid of ownedPids.keys()) this.knownOwnedPids.delete(pid);
    rmSync(this.scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }

  cleanup(): Promise<void> {
    this.closing = true;
    this.cleanupPromise ??= this.cleanupOnce();
    return this.cleanupPromise;
  }

  forceCleanupSync(): void {
    this.closing = true;
    for (const processHandle of this.processes.keys()) {
      if (processHandle.exitCode === null) {
        const ownerIdentity = processIdentitySync(processHandle.pid);
        if (ownerIdentity !== null) {
          this.knownOwnedPids.set(processHandle.pid, ownerIdentity);
        }
        for (const pid of descendantsOfSync(processHandle.pid)) {
          const identity = processIdentitySync(pid);
          if (identity !== null) this.knownOwnedPids.set(pid, identity);
        }
      }
      signalProcess(processHandle, 'SIGKILL');
    }
    const markerPid = this.childPidFile ? readPositivePid(this.childPidFile) : null;
    if (markerPid !== null && markerPid !== process.pid) {
      const identity = processIdentitySync(markerPid);
      if (
        identity !== null &&
        this.childFixture !== undefined &&
        identity.includes(this.childFixture)
      ) {
        this.knownOwnedPids.set(markerPid, identity);
      }
    }
    for (const [pid, identity] of this.knownOwnedPids) {
      if (pid === process.pid || processIdentitySync(pid) !== identity) continue;
      try {
        process.kill(pid, 'SIGKILL');
      } catch {
        // Exit can win the identity-to-signal race.
      }
    }
    const deadline = Date.now() + 250;
    let survivors = [...this.knownOwnedPids].filter(
      ([pid, identity]) => processIdentitySync(pid) === identity,
    );
    while (survivors.length > 0 && Date.now() < deadline) {
      Bun.sleepSync(10);
      survivors = survivors.filter(([pid, identity]) => processIdentitySync(pid) === identity);
    }
    if (survivors.length === 0) {
      try {
        rmSync(this.scratch, { recursive: true, force: true, maxRetries: 1, retryDelay: 10 });
      } catch {
        // The process exits immediately after this last-resort cleanup path.
      }
    }
  }
}

async function runOwnedCommand(
  ownership: PackedSmokeOwnership,
  command: readonly string[],
  {
    cwd = REPO_ROOT,
    env = process.env,
    timeoutMs = COMMAND_TIMEOUT_MS,
  }: {
    cwd?: string;
    env?: Record<string, string | undefined>;
    timeoutMs?: number;
  } = {},
): Promise<CommandResult> {
  ownership.assertAcceptingWork();
  const startedAt = performance.now();
  const processHandle = Bun.spawn([...command], {
    cwd,
    env,
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  ownership.registerProcess(processHandle, command.join(' '));
  const stdoutCapture = captureStream(processHandle.stdout);
  const stderrCapture = captureStream(processHandle.stderr);
  try {
    const exit = await settleWithin(processHandle.exited, timeoutMs);
    if (!exit.settled) {
      await stopSubprocess(processHandle, DEFAULT_CLEANUP_TIMEOUTS);
      throw new Error(`${command.join(' ')} did not complete within ${String(timeoutMs)}ms`);
    }
    const [stdout, stderr] = await Promise.all([
      settleCapture(stdoutCapture),
      settleCapture(stderrCapture),
    ]);
    return {
      command: [...command],
      cwd,
      durationMs: Math.round((performance.now() - startedAt) * 1_000) / 1_000,
      exitCode: exit.value,
      stdout,
      stderr,
    };
  } finally {
    if (processHandle.exitCode !== null) ownership.releaseProcess(processHandle);
    await Promise.all([settleCapture(stdoutCapture, 250), settleCapture(stderrCapture, 250)]);
  }
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

async function verifyCanonicalArtifact(
  ownership: PackedSmokeOwnership,
  scriptName: 'bundle' | 'bundle:bin',
  trackedOutput: string,
): Promise<void> {
  const manifest: unknown = JSON.parse(readFileSync(path.join(REPO_ROOT, 'package.json'), 'utf8'));
  invariant(isObject(manifest) && isObject(manifest['scripts']), 'package scripts are unavailable');
  const recipe = manifest['scripts'][scriptName];
  invariant(typeof recipe === 'string', `package.json has no ${scriptName} build recipe`);
  const outputFlag = `--outfile=${trackedOutput}`;
  const parts = recipe.split(outputFlag);
  invariant(
    parts.length === 2,
    `${scriptName} must contain exactly one canonical ${outputFlag} flag`,
  );
  const rebuiltOutput = path.join(ownership.scratch, `fresh-${path.basename(trackedOutput)}`);
  const scratchRecipe = `${parts[0]}--outfile=${shellQuote(rebuiltOutput)}${parts[1]}`;
  const pathValue = process.env['PATH'];
  const env = {
    ...process.env,
    PATH: `${path.join(REPO_ROOT, 'node_modules', '.bin')}${pathValue ? `${path.delimiter}${pathValue}` : ''}`,
  };
  assertCommand(
    await runOwnedCommand(ownership, ['sh', '-c', scratchRecipe], {
      cwd: REPO_ROOT,
      env,
    }),
  );
  const committedOutput = path.join(REPO_ROOT, trackedOutput);
  const [committed, rebuilt] = await Promise.all([
    Bun.file(committedOutput).bytes(),
    Bun.file(rebuiltOutput).bytes(),
  ]);
  invariant(
    Buffer.from(committed).equals(Buffer.from(rebuilt)),
    `committed ${trackedOutput} is stale (committed ${sha256(committed)}, rebuilt ${sha256(rebuilt)}); run bun run ${scriptName}`,
  );
}

async function verifyBundleFreshness(ownership: PackedSmokeOwnership): Promise<void> {
  await verifyCanonicalArtifact(ownership, 'bundle', 'src/daemon/fleetd.bundle.mjs');
  await verifyCanonicalArtifact(ownership, 'bundle:bin', 'bin/fleetdeck.mjs');
}

interface SignalCleanupHandle {
  dispose: () => void;
  received: () => NodeJS.Signals | null;
}

function installSignalCleanup(
  ownership: PackedSmokeOwnership,
  timeoutMs = SIGNAL_CLEANUP_TIMEOUT_MS,
): SignalCleanupHandle {
  let received: NodeJS.Signals | null = null;
  let watchdog: ReturnType<typeof setTimeout> | undefined;
  const exitCode = (signal: NodeJS.Signals): number => (signal === 'SIGINT' ? 130 : 143);
  const onSignal = (signal: NodeJS.Signals): void => {
    if (received !== null) {
      ownership.forceCleanupSync();
      process.exit(exitCode(signal));
    }
    received = signal;
    watchdog = setTimeout(() => {
      ownership.forceCleanupSync();
      process.exit(exitCode(signal));
    }, timeoutMs);
    void ownership.cleanup().then(
      () => {
        if (watchdog !== undefined) clearTimeout(watchdog);
        process.exit(exitCode(signal));
      },
      (error: unknown) => {
        process.stderr.write(
          `packed-smoke signal cleanup failed: ${error instanceof Error ? error.message : String(error)}\n`,
        );
        ownership.forceCleanupSync();
        process.exit(exitCode(signal));
      },
    );
  };
  process.on('SIGINT', onSignal);
  process.on('SIGTERM', onSignal);
  return {
    received: () => received,
    dispose: () => {
      process.off('SIGINT', onSignal);
      process.off('SIGTERM', onSignal);
      if (watchdog !== undefined) clearTimeout(watchdog);
    },
  };
}

async function waitForResidueRelease(
  processHandle: Bun.ReadableSubprocess,
  port: number,
  pidFile: string,
  descendants: readonly number[],
): Promise<ResidueState> {
  const deadline = Date.now() + RESIDUE_TIMEOUT_MS;
  let latest: ResidueState | undefined;
  do {
    latest = {
      daemonAlive: isPidAlive(processHandle.pid),
      pidFilePresent: existsSync(pidFile),
      portAvailable: await portCanRebind(port),
      descendantsAlive: descendants.filter(isPidAlive),
    };
    if (
      !latest.daemonAlive &&
      !latest.pidFilePresent &&
      latest.portAvailable &&
      latest.descendantsAlive.length === 0
    ) {
      return latest;
    }
    await Bun.sleep(25);
  } while (Date.now() < deadline);
  invariant(latest, 'residue inspection did not run');
  return latest;
}

function cleanDaemonEnvironment(
  home: string,
  port: number,
  childFixture: string,
  childPidFile: string,
): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (typeof value === 'string' && !key.startsWith('FLEETDECK_')) env[key] = value;
  }
  delete env['TMUX'];
  delete env['TMUX_PANE'];
  return {
    ...env,
    FLEETDECK_PORT: String(port),
    FLEETDECK_HOME: home,
    FLEETDECK_BIND: '127.0.0.1',
    FLEETDECK_MDNS: 'off',
    FLEETDECK_HOLD_SCOPE: 'all',
    FLEETDECK_TMUX_SOCKET: `fleetdeck-packed-smoke-${String(process.pid)}-${randomUUID()}`,
    FLEETDECK_AGENTS_CMD: `${process.execPath} --no-env-file ${childFixture}`,
    FLEETDECK_AGENTS_POLL_MS: '100',
    FLEETDECK_AGENTS_IDLE_POLL_MS: '100',
    FLEETDECK_PACKED_SMOKE_CHILD_PID: childPidFile,
  };
}

async function main(): Promise<void> {
  invariant(process.platform !== 'win32', 'packed daemon smoke requires POSIX signals and ps');

  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-packed-install-'));
  const ownership = new PackedSmokeOwnership(scratch);
  const packDir = path.join(scratch, 'pack');
  const consumerDir = path.join(scratch, 'consumer');
  const home = path.join(scratch, 'home');
  const childFixture = path.join(scratch, 'agents-child.mjs');
  const childPidFile = path.join(scratch, 'agents-child.pid');
  ownership.configureDaemonCleanup(childPidFile, childFixture);
  const signals = installSignalCleanup(ownership);

  let daemon: Bun.ReadableSubprocess | undefined;
  let daemonStdout: StreamCapture | undefined;
  let daemonStderr: StreamCapture | undefined;
  let childPid: number | null = null;
  let cleanupAttempted = false;

  try {
    mkdirSync(packDir);
    mkdirSync(consumerDir);
    invariant(
      !/\s/.test(process.execPath) && !/\s/.test(childFixture),
      'FLEETDECK_AGENTS_CMD is whitespace-tokenized; the Bun and fixture paths must not contain whitespace',
    );

    // Rebuild into scratch with the package script's exact esbuild inputs. The
    // smoke must never bless a stale checked-in artifact merely because that
    // same stale artifact was packed and happened to launch.
    await verifyBundleFreshness(ownership);
    assertCommand(
      await runOwnedCommand(
        ownership,
        [
          process.execPath,
          'pm',
          'pack',
          '--destination',
          packDir,
          '--ignore-scripts',
          '--gzip-level',
          '9',
          '--quiet',
        ],
        { cwd: REPO_ROOT, timeoutMs: COMMAND_TIMEOUT_MS },
      ),
    );
    const tarballs = readdirSync(packDir).filter((entry) => entry.endsWith('.tgz'));
    invariant(tarballs.length === 1, `bun pm pack produced ${String(tarballs.length)} tarballs`);
    const tarballName = tarballs[0];
    invariant(tarballName, 'bun pm pack did not name its tarball');

    const rootPackage: unknown = await Bun.file(path.join(REPO_ROOT, 'package.json')).json();
    ownership.assertAcceptingWork();
    invariant(isObject(rootPackage), 'repository package.json is not an object');
    const expectedVersion = rootPackage['version'];
    invariant(typeof expectedVersion === 'string', 'repository package.json has no version');

    const tarballSpec = `file:${path.posix.join('..', 'pack', tarballName)}`;
    await Bun.write(
      path.join(consumerDir, 'package.json'),
      `${JSON.stringify(
        {
          name: 'fleetdeck-packed-install-smoke',
          private: true,
          packageManager: `bun@${Bun.version}`,
          dependencies: { fleetdeck: tarballSpec },
        },
        null,
        2,
      )}\n`,
    );
    ownership.assertAcceptingWork();

    // Resolve the exact local tarball into a lock without installing, then make
    // the real clean install prove that lock is accepted unchanged in frozen
    // mode. `copyfile` prevents an accidental checkout/cache symlink from
    // making the installed artifact look healthier than the tarball itself.
    assertCommand(
      await runOwnedCommand(
        ownership,
        [
          process.execPath,
          'install',
          '--lockfile-only',
          '--exact',
          '--ignore-scripts',
          '--no-progress',
          '--no-summary',
        ],
        { cwd: consumerDir, timeoutMs: COMMAND_TIMEOUT_MS },
      ),
    );
    const lockfile = path.join(consumerDir, 'bun.lock');
    invariant(existsSync(lockfile), 'Bun did not create the consumer lockfile');
    const lockBefore = await Bun.file(lockfile).bytes();
    ownership.assertAcceptingWork();
    assertCommand(
      await runOwnedCommand(
        ownership,
        [
          process.execPath,
          'install',
          '--frozen-lockfile',
          '--exact',
          '--ignore-scripts',
          '--backend=copyfile',
          '--no-progress',
          '--no-summary',
        ],
        { cwd: consumerDir, timeoutMs: COMMAND_TIMEOUT_MS },
      ),
    );
    const lockAfter = await Bun.file(lockfile).bytes();
    ownership.assertAcceptingWork();
    invariant(
      Buffer.from(lockAfter).equals(Buffer.from(lockBefore)),
      'frozen consumer install changed bun.lock',
    );

    const installedRoot = path.join(consumerDir, 'node_modules', 'fleetdeck');
    const installedPackage: unknown = await Bun.file(
      path.join(installedRoot, 'package.json'),
    ).json();
    ownership.assertAcceptingWork();
    invariant(isObject(installedPackage), 'installed fleetdeck package.json is not an object');
    invariant(
      installedPackage['name'] === 'fleetdeck' && installedPackage['version'] === expectedVersion,
      `installed package identity is not fleetdeck@${expectedVersion}`,
    );
    const installedDaemon = path.join(installedRoot, 'src', 'daemon', 'fleetd.bundle.mjs');
    invariant(existsSync(installedDaemon), 'packed install omitted fleetd.bundle.mjs');
    invariant(
      !existsSync(path.join(installedRoot, 'src', 'daemon', 'fleetd.ts')),
      'packed install unexpectedly contains the source daemon fallback',
    );
    const relativeToRepo = path.relative(REPO_ROOT, realpathSync(installedDaemon));
    invariant(
      relativeToRepo.startsWith(`..${path.sep}`) || relativeToRepo === '..',
      'installed daemon resolves back into the repository checkout',
    );
    const installedBin = path.join(consumerDir, 'node_modules', '.bin', 'fleetdeck');
    const expectedBinTarget = path.join(installedRoot, 'bin', 'fleetdeck.mjs');
    invariant(
      isObject(installedPackage['bin']) &&
        installedPackage['bin']['fleetdeck'] === 'bin/fleetdeck.mjs',
      'installed package bin mapping is not fleetdeck -> bin/fleetdeck.mjs',
    );
    invariant(existsSync(installedBin), 'frozen install omitted node_modules/.bin/fleetdeck');
    invariant(
      realpathSync(installedBin) === realpathSync(expectedBinTarget),
      'installed fleetdeck bin does not resolve to the packed CLI entrypoint',
    );
    const [repositoryBundleBytes, installedBundleBytes, repositoryBinBytes, installedBinBytes] =
      await Promise.all([
        Bun.file(path.join(REPO_ROOT, 'src', 'daemon', 'fleetd.bundle.mjs')).bytes(),
        Bun.file(installedDaemon).bytes(),
        Bun.file(path.join(REPO_ROOT, 'bin', 'fleetdeck.mjs')).bytes(),
        Bun.file(expectedBinTarget).bytes(),
      ]);
    ownership.assertAcceptingWork();
    invariant(
      Buffer.from(repositoryBundleBytes).equals(Buffer.from(installedBundleBytes)),
      'packed daemon bytes differ from the freshness-checked repository bundle',
    );
    invariant(
      Buffer.from(repositoryBinBytes).equals(Buffer.from(installedBinBytes)),
      'packed CLI bytes differ from the freshness-checked repository bin bundle',
    );

    await Bun.write(childFixture, CHILD_FIXTURE);
    ownership.assertAcceptingWork();
    const port = await reservePort();
    ownership.assertAcceptingWork();
    const baseUrl = `http://127.0.0.1:${String(port)}`;
    const pidFile = path.join(home, 'fleetd.pid');
    const spawnedDaemon = Bun.spawn([process.execPath, '--no-env-file', installedBin, 'serve'], {
      cwd: consumerDir,
      env: cleanDaemonEnvironment(home, port, childFixture, childPidFile),
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    // Spawn and ownership publication are synchronous and adjacent: a signal
    // can never observe an unregistered daemon, even before the first /health.
    ownership.registerDaemon(spawnedDaemon);
    daemon = spawnedDaemon;
    daemonStdout = captureStream(daemon.stdout);
    daemonStderr = captureStream(daemon.stderr);

    const health = await waitForHealth(daemon, baseUrl);
    invariant(
      health.version === expectedVersion,
      `/health did not report version ${expectedVersion}`,
    );
    invariant(
      health.managed === true,
      'installed CLI serve entrypoint did not mark daemon managed',
    );
    childPid = await waitForChild(childPidFile);
    const descendants = await descendantsOf(daemon.pid);
    invariant(
      descendants.includes(childPid),
      `agents child pid ${String(childPid)} is not a descendant of daemon pid ${String(daemon.pid)}`,
    );
    await ownership.rememberOwnedPid(childPid);

    const pidRecord: unknown = JSON.parse(readFileSync(pidFile, 'utf8'));
    invariant(isObject(pidRecord), 'fleetd.pid is not a JSON object');
    const owned = pidRecord as PidRecord;
    invariant(
      owned.pid === daemon.pid && owned.port === port,
      'fleetd.pid does not identify the installed daemon and reserved port',
    );

    signalProcess(daemon, 'SIGTERM');
    const exitCode = await awaitExit(daemon, SHUTDOWN_TIMEOUT_MS);
    invariant(exitCode === 0, `installed daemon SIGTERM exit was ${String(exitCode)}, expected 0`);
    const residue = await waitForResidueRelease(daemon, port, pidFile, descendants);
    invariant(
      !residue.daemonAlive &&
        !residue.pidFilePresent &&
        residue.portAvailable &&
        residue.descendantsAlive.length === 0,
      `installed daemon left residue: ${JSON.stringify(residue)}`,
    );

    const [stdout, stderr] = await Promise.all([
      settleCapture(daemonStdout),
      settleCapture(daemonStderr),
    ]);
    process.stdout.write(
      `${JSON.stringify({
        ok: true,
        bun: Bun.version,
        package: `fleetdeck@${expectedVersion}`,
        tarball: tarballName,
        daemonPid: daemon.pid,
        childPid,
        descendants,
        health: {
          ok: health.ok,
          managed: health.managed,
          startup: health.startup,
          version: health.version,
        },
        exitCode,
        residue,
        outputBytes: {
          stdout: Buffer.byteLength(stdout),
          stderr: Buffer.byteLength(stderr),
        },
      })}\n`,
    );
  } catch (error) {
    cleanupAttempted = true;
    let cleanupError: unknown;
    try {
      await ownership.cleanup();
    } catch (caught) {
      cleanupError = caught;
    }
    const [stdout, stderr] = await Promise.all([
      settleCapture(daemonStdout),
      settleCapture(daemonStderr),
    ]);
    if (stdout || stderr) {
      process.stderr.write(`--- packed daemon stdout ---\n${stdout}`);
      process.stderr.write(`--- packed daemon stderr ---\n${stderr}`);
    }
    if (signals.received() !== null) return;
    if (cleanupError !== undefined) {
      throw new AggregateError([error, cleanupError], 'packed smoke and cleanup both failed');
    }
    throw error;
  } finally {
    if (!cleanupAttempted) await ownership.cleanup();
    if (signals.received() === null) signals.dispose();
  }
}

const IS_ENTRYPOINT = (() => {
  try {
    return (
      process.argv[1] !== undefined &&
      realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)
    );
  } catch {
    return false;
  }
})();

if (IS_ENTRYPOINT) await main();

export {
  captureStream,
  installSignalCleanup,
  PackedSmokeOwnership,
  settleCapture,
  stopSubprocess,
  verifyBundleFreshness,
};
