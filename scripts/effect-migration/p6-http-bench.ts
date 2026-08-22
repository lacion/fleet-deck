// P6.8 HTTP/WS load harness.
//
// Duration-based, pure Bun, no external load tools. Starts a real daemon
// (source `src/daemon/fleetd.ts` or generated `src/daemon/fleetd.bundle.mjs`)
// the same way P0/P4 do: scratch FLEETDECK_HOME, loopback bind, pinned token,
// isolated tmux socket, mDNS off. Drives the P6.8 workload set against the
// frozen P6.1 matrix shapes. Warmup is discarded. JSON goes to --out; a human
// table always prints on stdout.
//
// This file records numbers. It does not decide whether a delta is acceptable.
// Quiet-host baselines belong under docs/v1/evidence/effect/ and MUST be
// labeled `baseline`. `--smoke` numbers are harness validation only.
import { randomUUID } from 'node:crypto';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { connect as connectTcp, type Socket } from 'node:net';
import { arch, cpus, platform, release, tmpdir } from 'node:os';
import path from 'node:path';

import { REPO_ROOT, summarize, type Distribution } from './metrics.ts';

type TargetName = 'source' | 'bundle';
type LabelName = 'smoke' | 'baseline';
type WorkloadName =
  | 'health'
  | 'state'
  | 'hook'
  | 'hook-fail-open'
  | 'paste'
  | 'withheld'
  | 'ws'
  | 'static-shell'
  | 'static-asset';

interface BenchConfig {
  readonly targets: readonly TargetName[];
  readonly workloads: readonly WorkloadName[];
  readonly concurrency: readonly number[];
  readonly durationMs: number;
  readonly warmupMs: number;
  readonly label: LabelName;
  readonly out: string | null;
  readonly pasteBytes: number;
  readonly probeConcurrency: number;
  readonly requestTimeoutMs: number;
  readonly healthTimeoutMs: number;
  readonly shutdownTimeoutMs: number;
  readonly requireFloor: boolean;
  readonly smoke: boolean;
}

interface RunningDaemon {
  readonly target: TargetName;
  readonly path: string;
  readonly process: Bun.ReadableSubprocess;
  readonly stdout: Promise<string>;
  readonly stderr: Promise<string>;
  readonly home: string;
  readonly port: number;
  readonly baseUrl: string;
  readonly wsUrl: string;
  readonly token: string;
  readonly healthMs: number;
  readonly reconciliationReadyMs: number;
  readonly assetPath: string;
}

interface HealthBody {
  readonly ok?: unknown;
  readonly pid?: unknown;
  readonly startup?: unknown;
}

interface ErrorBucket {
  readonly error: string;
  readonly count: number;
}

interface WorkloadRow {
  readonly workload: WorkloadName;
  readonly target: TargetName;
  readonly concurrency: number;
  readonly durationMs: number;
  readonly warmupMs: number;
  readonly count: number;
  readonly ok: number;
  readonly errors: number;
  readonly operationsPerSecond: number;
  readonly responseBytes: number;
  readonly latencyMs: Distribution;
  readonly errorLatencyMs: Distribution;
  readonly errorBuckets: readonly ErrorBucket[];
  readonly notes: string;
  readonly extras: Record<string, unknown>;
}

interface SnapshotClient {
  readonly socket: WebSocket;
  take: (timeoutMs: number) => Promise<string>;
  drain: () => number;
  close: () => Promise<void>;
}

const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
const SOURCE_DAEMON = path.join(REPO_ROOT, 'src/daemon/fleetd.ts');
const BUNDLE_DAEMON = path.join(REPO_ROOT, 'src/daemon/fleetd.bundle.mjs');
const PNG_MAGIC = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] as const;
const BODY_DRAIN_GRACE_MS = 1_000;
const BROADCAST_COALESCE_MS = 60;
const SEEDED_SESSIONS = 8;
const encoder = new TextEncoder();
const ALL_WORKLOADS = [
  'health',
  'state',
  'hook',
  'hook-fail-open',
  'paste',
  'withheld',
  'ws',
  'static-shell',
  'static-asset',
] as const satisfies readonly WorkloadName[];
const liveDaemons = new Set<RunningDaemon>();

function runtimeFloor() {
  const actual = { bun: Bun.version, revision: Bun.revision };
  return {
    expected: { bun: EXPECTED_BUN_VERSION, revision: EXPECTED_BUN_REVISION },
    actual,
    exactMatch: actual.bun === EXPECTED_BUN_VERSION && actual.revision === EXPECTED_BUN_REVISION,
  };
}

function rounded(value: number): number {
  return Math.round(value * 1_000) / 1_000;
}

function hrtimeMsSince(started: bigint): number {
  return Number(process.hrtime.bigint() - started) / 1e6;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function integerFlag(name: string, fallback: number, minimum: number): number {
  const prefix = `--${name}=`;
  const raw = Bun.argv.find((argument) => argument.startsWith(prefix))?.slice(prefix.length);
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`--${name} must be an integer >= ${minimum}`);
  }
  return value;
}

function stringFlag(name: string, fallback: string): string {
  const prefix = `--${name}=`;
  return Bun.argv.find((argument) => argument.startsWith(prefix))?.slice(prefix.length) ?? fallback;
}

function printHelp(): void {
  process.stdout.write(`P6.8 HTTP/WS load harness — duration-based, pure Bun, real daemon.

Usage:
  bun scripts/effect-migration/p6-http-bench.ts [flags]

Flags:
  --target=source|bundle|both   daemon artifact (default: both)
  --workload=name[,name]|all    ${ALL_WORKLOADS.join(', ')} (default: all)
  --concurrency=1,8,32          in-flight HTTP / withheld sockets / WS clients
  --duration=15                 measured seconds; warmup is excluded
  --warmup=2                    discarded warmup seconds (0 allowed)
  --label=smoke|baseline        JSON label (default: smoke; --smoke forces smoke)
  --out=path.json               machine-readable report (table always on stdout)
  --paste-bytes=N               decoded /api/paste-image size (default: 2097152)
  --probe-concurrency=N         GET /health probes during withheld (default: 8)
  --request-timeout-ms=15000
  --health-timeout-ms=10000
  --shutdown-timeout-ms=3000
  --smoke                       preset: label=smoke duration=2 warmup=1 concurrency=1,8
  --require-floor               fail unless Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION})
  --help

Quiet-host baseline (idle machine, twice; do not use a busy host):
  bun scripts/effect-migration/p6-http-bench.ts --target=both --label=baseline --require-floor
    --workload=all --concurrency=1,8,32 --duration=15 --warmup=2 --paste-bytes=2097152
    --probe-concurrency=8 --out=docs/v1/evidence/effect/p6-http-bench-run-1.json

See scripts/effect-migration/p6-http-bench.md.
`);
}

function parseWorkloads(raw: string): WorkloadName[] {
  if (raw === 'all') return [...ALL_WORKLOADS];
  const names: WorkloadName[] = [];
  for (const part of raw
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean)) {
    if (part === 'static') {
      names.push('static-shell', 'static-asset');
      continue;
    }
    if (!(ALL_WORKLOADS as readonly string[]).includes(part)) {
      throw new Error(`unknown --workload ${part}; expected ${ALL_WORKLOADS.join('|')}|static|all`);
    }
    names.push(part as WorkloadName);
  }
  if (names.length === 0) throw new Error('--workload must name at least one workload');
  return [...new Set(names)];
}

function parseConcurrency(raw: string): number[] {
  const values = raw.split(',').map((part) => Number(part.trim()));
  if (values.length === 0 || values.some((value) => !Number.isSafeInteger(value) || value < 1)) {
    throw new Error('--concurrency must be a comma-separated list of positive integers');
  }
  return [...new Set(values)].sort((left, right) => left - right);
}

function readConfig(): BenchConfig {
  if (Bun.argv.includes('--help') || Bun.argv.includes('-h')) {
    printHelp();
    process.exit(0);
  }
  const smoke = Bun.argv.includes('--smoke');
  const rawTarget = stringFlag('target', 'both');
  if (!['source', 'bundle', 'both'].includes(rawTarget)) {
    throw new Error('--target must be source, bundle, or both');
  }
  const rawLabel = stringFlag('label', 'smoke');
  if (rawLabel !== 'smoke' && rawLabel !== 'baseline') {
    throw new Error('--label must be smoke or baseline');
  }
  const label: LabelName = smoke ? 'smoke' : rawLabel;
  const outRaw = stringFlag('out', '');
  return {
    targets: rawTarget === 'both' ? ['source', 'bundle'] : [rawTarget as TargetName],
    workloads: parseWorkloads(stringFlag('workload', 'all')),
    concurrency: parseConcurrency(stringFlag('concurrency', smoke ? '1,8' : '1,8,32')),
    durationMs: integerFlag('duration', smoke ? 2 : 15, 1) * 1_000,
    warmupMs: integerFlag('warmup', smoke ? 1 : 2, 0) * 1_000,
    label,
    out: outRaw === '' ? null : path.resolve(outRaw),
    pasteBytes: integerFlag('paste-bytes', 2 * 1024 * 1024, 1_024),
    probeConcurrency: integerFlag('probe-concurrency', 8, 1),
    requestTimeoutMs: integerFlag('request-timeout-ms', 15_000, 100),
    healthTimeoutMs: integerFlag('health-timeout-ms', 10_000, 100),
    shutdownTimeoutMs: integerFlag('shutdown-timeout-ms', 3_000, 100),
    requireFloor: Bun.argv.includes('--require-floor') || label === 'baseline',
    smoke,
  };
}

function targetPath(target: TargetName): string {
  return target === 'source' ? SOURCE_DAEMON : BUNDLE_DAEMON;
}

function cleanEnvironment(): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (typeof value === 'string' && !key.startsWith('FLEETDECK_')) env[key] = value;
  }
  delete env['TMUX'];
  delete env['TMUX_PANE'];
  return env;
}

async function reservePort(): Promise<number> {
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    fetch: () => new Response('reserved'),
  });
  const port = server.port;
  await server.stop(true);
  if (typeof port !== 'number' || !Number.isSafeInteger(port) || port <= 0) {
    throw new Error('Bun.serve did not allocate a port');
  }
  return port;
}

async function fetchHealth(baseUrl: string, timeoutMs: number): Promise<HealthBody | null> {
  try {
    const response = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(timeoutMs) });
    if (!response.ok) return null;
    const body: unknown = await response.json();
    return isObject(body) ? body : null;
  } catch {
    return null;
  }
}

async function waitForReady(
  processHandle: Bun.ReadableSubprocess,
  baseUrl: string,
  timeoutMs: number,
  startedAt: number,
): Promise<{ healthMs: number; reconciliationReadyMs: number }> {
  const deadline = performance.now() + timeoutMs;
  let healthMs: number | null = null;
  while (performance.now() < deadline) {
    if (processHandle.exitCode !== null) {
      throw new Error(`daemon exited ${processHandle.exitCode} before readiness`);
    }
    const remaining = Math.max(1, Math.floor(deadline - performance.now()));
    const health = await fetchHealth(baseUrl, Math.min(remaining, 500));
    if (health?.ok === true && health.pid === processHandle.pid) {
      healthMs ??= rounded(performance.now() - startedAt);
      if (health.startup === 'settled') {
        return { healthMs, reconciliationReadyMs: rounded(performance.now() - startedAt) };
      }
    }
    await Bun.sleep(5);
  }
  throw new Error(`daemon did not become reconciliation-ready within ${timeoutMs}ms`);
}

function parseJsonObject(text: string, context: string): Record<string, unknown> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error(`${context} returned malformed JSON`);
  }
  if (!isObject(parsed)) throw new Error(`${context} did not return a JSON object`);
  return parsed;
}

function discoverAssetPath(html: string): string {
  const match = /src="\.\/assets\/([^"]+\.js)"/.exec(html);
  const file = match?.[1];
  if (!file) throw new Error('GET / did not reference a hashed /assets/*.js module');
  return `/assets/${file}`;
}

async function startDaemon(target: TargetName, config: BenchConfig): Promise<RunningDaemon> {
  const daemonFile = targetPath(target);
  if (!(await Bun.file(daemonFile).exists())) {
    throw new Error(`daemon artifact not found: ${daemonFile}`);
  }
  const home = mkdtempSync(path.join(tmpdir(), `fleetdeck-p6-http-bench-${target}-`));
  const port = await reservePort();
  const token = `p6-http-bench-${randomUUID()}`;
  const env = {
    ...cleanEnvironment(),
    FLEETDECK_PORT: String(port),
    FLEETDECK_HOME: home,
    FLEETDECK_BIND: '127.0.0.1',
    FLEETDECK_MDNS: 'off',
    FLEETDECK_AGENTS_CMD: 'false',
    FLEETDECK_HOLD_SCOPE: 'all',
    FLEETDECK_TOKEN: token,
    FLEETDECK_TMUX_SOCKET: `fleetdeck-p6-http-bench-${process.pid}-${randomUUID()}`,
  };
  const startedAt = performance.now();
  const processHandle = Bun.spawn([process.execPath, '--no-env-file', daemonFile], {
    cwd: REPO_ROOT,
    env,
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = Bun.readableStreamToText(processHandle.stdout).catch(() => '');
  const stderr = Bun.readableStreamToText(processHandle.stderr).catch(() => '');
  try {
    const readiness = await waitForReady(
      processHandle,
      `http://127.0.0.1:${port}`,
      config.healthTimeoutMs,
      startedAt,
    );
    const baseUrl = `http://127.0.0.1:${port}`;
    const shell = await fetch(`${baseUrl}/`, {
      signal: AbortSignal.timeout(config.requestTimeoutMs),
    });
    const html = await shell.text();
    if (!shell.ok || !html.includes('id="root"')) {
      throw new Error(`GET / during setup returned HTTP ${shell.status}`);
    }
    const daemon: RunningDaemon = {
      target,
      path: daemonFile,
      process: processHandle,
      stdout,
      stderr,
      home,
      port,
      baseUrl,
      wsUrl: `ws://127.0.0.1:${port}/ws?t=${encodeURIComponent(token)}`,
      token,
      assetPath: discoverAssetPath(html),
      ...readiness,
    };
    liveDaemons.add(daemon);
    return daemon;
  } catch (error) {
    if (processHandle.exitCode === null) processHandle.kill('SIGKILL');
    const [, stdoutText, stderrText] = await Promise.all([
      processHandle.exited.catch(() => -1),
      stdout,
      stderr,
    ]);
    rmSync(home, { recursive: true, force: true });
    throw new Error(
      `${error instanceof Error ? error.message : String(error)}\nstdout:\n${stdoutText}\nstderr:\n${stderrText}`,
    );
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
            reject(new Error(`process ${processHandle.pid} did not exit within ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function stopDaemon(daemon: RunningDaemon, timeoutMs: number): Promise<void> {
  liveDaemons.delete(daemon);
  if (daemon.process.exitCode === null) daemon.process.kill('SIGTERM');
  try {
    await awaitExit(daemon.process, timeoutMs);
  } catch {
    if (daemon.process.exitCode === null) daemon.process.kill('SIGKILL');
    await daemon.process.exited.catch(() => -1);
  }
  await Promise.all([daemon.stdout, daemon.stderr]);
  rmSync(daemon.home, { recursive: true, force: true });
}

function requestInit(config: BenchConfig, init: RequestInit = {}): RequestInit {
  return { ...init, signal: AbortSignal.timeout(config.requestTimeoutMs) };
}

function authHeaders(
  daemon: RunningDaemon,
  extra: Record<string, string> = {},
): Record<string, string> {
  return { authorization: `Bearer ${daemon.token}`, ...extra };
}

function clipError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.length <= 160 ? text : `${text.slice(0, 157)}...`;
}

function bucketErrors(samples: readonly { ok: boolean; error: string | null }[]): ErrorBucket[] {
  const counts = new Map<string, number>();
  for (const sample of samples) {
    if (sample.ok || sample.error === null) continue;
    counts.set(sample.error, (counts.get(sample.error) ?? 0) + 1);
  }
  return [...counts.entries()]
    .sort((left, right) => right[1] - left[1])
    .slice(0, 8)
    .map(([error, count]) => ({ error, count }));
}

interface TimedSample {
  readonly ok: boolean;
  readonly latencyMs: number;
  readonly responseBytes: number;
  readonly error: string | null;
}

async function runTimed(
  durationMs: number,
  warmupMs: number,
  concurrency: number,
  operation: () => Promise<number>,
): Promise<{ samples: TimedSample[]; wallMs: number }> {
  const runPhase = async (
    phaseMs: number,
    record: boolean,
  ): Promise<{ samples: TimedSample[]; wallMs: number }> => {
    const samples: TimedSample[] = [];
    const stopAt = process.hrtime.bigint() + BigInt(Math.max(0, Math.round(phaseMs))) * 1_000_000n;
    const workers = Array.from({ length: Math.max(1, concurrency) }, async () => {
      while (process.hrtime.bigint() < stopAt) {
        const started = process.hrtime.bigint();
        try {
          const responseBytes = await operation();
          if (record) {
            samples.push({
              ok: true,
              latencyMs: hrtimeMsSince(started),
              responseBytes,
              error: null,
            });
          }
        } catch (error) {
          if (record) {
            samples.push({
              ok: false,
              latencyMs: hrtimeMsSince(started),
              responseBytes: 0,
              error: clipError(error),
            });
          }
        }
      }
    });
    const wallStarted = process.hrtime.bigint();
    await Promise.all(workers);
    return { samples, wallMs: hrtimeMsSince(wallStarted) };
  };
  if (warmupMs > 0) await runPhase(warmupMs, false);
  return runPhase(durationMs, true);
}

function rowFromSamples(
  workload: WorkloadName,
  target: TargetName,
  concurrency: number,
  durationMs: number,
  warmupMs: number,
  samples: readonly TimedSample[],
  wallMs: number,
  notes: string,
  extras: Record<string, unknown> = {},
): WorkloadRow {
  const okSamples = samples.filter((sample) => sample.ok);
  const errorSamples = samples.filter((sample) => !sample.ok);
  const measuredMs = wallMs > 0 ? wallMs : durationMs;
  return {
    workload,
    target,
    concurrency,
    durationMs: rounded(measuredMs),
    warmupMs,
    count: samples.length,
    ok: okSamples.length,
    errors: errorSamples.length,
    operationsPerSecond: rounded(okSamples.length / (measuredMs / 1_000)),
    responseBytes: okSamples.reduce((sum, sample) => sum + sample.responseBytes, 0),
    latencyMs: summarize(okSamples.map((sample) => sample.latencyMs)),
    errorLatencyMs: summarize(errorSamples.map((sample) => sample.latencyMs)),
    errorBuckets: bucketErrors(samples),
    notes,
    extras,
  };
}

async function hookPost(
  daemon: RunningDaemon,
  config: BenchConfig,
  event: string,
  payload: Record<string, unknown>,
  options: { emptyBody?: boolean } = {},
): Promise<number> {
  const response = await fetch(
    `${daemon.baseUrl}/hook/${encodeURIComponent(event)}`,
    requestInit(config, {
      method: 'POST',
      headers: authHeaders(daemon, { 'content-type': 'application/json' }),
      body: JSON.stringify(payload),
    }),
  );
  const text = await response.text();
  if (response.status !== 200) {
    throw new Error(`POST /hook/${event} returned HTTP ${response.status}: ${text}`);
  }
  const body = parseJsonObject(text || '{}', `POST /hook/${event}`);
  if (options.emptyBody === true && Object.keys(body).length !== 0) {
    throw new Error(`POST /hook/${event} violated the canonical {} contract: ${text}`);
  }
  return encoder.encode(text).byteLength;
}

async function seedSessions(daemon: RunningDaemon, config: BenchConfig): Promise<string[]> {
  const sessionIds = Array.from(
    { length: SEEDED_SESSIONS },
    (_, index) => `p6-http-bench-${index}-${randomUUID()}`,
  );
  for (const sessionId of sessionIds) {
    await hookPost(daemon, config, 'SessionStart', {
      session_id: sessionId,
      hook_event_name: 'SessionStart',
      cwd: REPO_ROOT,
      source: 'startup',
    });
  }
  return sessionIds;
}

function messageDataText(data: unknown): Promise<string> {
  if (typeof data === 'string') return Promise.resolve(data);
  if (data instanceof Blob) return data.text();
  if (data instanceof ArrayBuffer) return Promise.resolve(new TextDecoder().decode(data));
  if (ArrayBuffer.isView(data)) {
    return Promise.resolve(
      new TextDecoder().decode(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)),
    );
  }
  return Promise.resolve(String(data));
}

function openSnapshotClient(url: string, timeoutMs: number): Promise<SnapshotClient> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const queue: string[] = [];
    const waiters: Array<(text: string) => void> = [];
    let opened = false;
    const timer = setTimeout(() => {
      if (!opened) {
        socket.close();
        reject(new Error('WebSocket open timed out'));
      }
    }, timeoutMs);
    socket.addEventListener('message', (event) => {
      const deliver = (text: string): void => {
        const waiter = waiters.shift();
        if (waiter) waiter(text);
        else queue.push(text);
      };
      if (typeof event.data === 'string') {
        deliver(event.data);
        return;
      }
      void messageDataText(event.data).then(deliver);
    });
    socket.addEventListener('error', () => {
      if (!opened) {
        clearTimeout(timer);
        reject(new Error('WebSocket failed before open'));
      }
    });
    socket.addEventListener('open', () => {
      opened = true;
      clearTimeout(timer);
      resolve({
        socket,
        take(waitMs: number): Promise<string> {
          const queued = queue.shift();
          if (queued !== undefined) return Promise.resolve(queued);
          return new Promise((takeResolve, takeReject) => {
            const waitTimer = setTimeout(() => {
              const index = waiters.indexOf(onText);
              if (index >= 0) waiters.splice(index, 1);
              takeReject(new Error('WebSocket message timed out'));
            }, waitMs);
            const onText = (text: string): void => {
              clearTimeout(waitTimer);
              takeResolve(text);
            };
            waiters.push(onText);
          });
        },
        drain(): number {
          const dropped = queue.length;
          queue.length = 0;
          return dropped;
        },
        close(): Promise<void> {
          if (socket.readyState === WebSocket.CLOSED) return Promise.resolve();
          return new Promise((closeResolve) => {
            const closeTimer = setTimeout(closeResolve, 250);
            socket.addEventListener(
              'close',
              () => {
                clearTimeout(closeTimer);
                closeResolve();
              },
              { once: true },
            );
            socket.close(1000, 'p6-http-bench');
          });
        },
      });
    });
  });
}

function assertSnapshotFrame(text: string): number {
  const frame = parseJsonObject(text, 'WebSocket snapshot');
  if (frame['type'] !== 'snapshot' || !Array.isArray(frame['sessions'])) {
    throw new Error('WebSocket frame violated the snapshot contract');
  }
  return encoder.encode(text).byteLength;
}

interface WithheldSocket {
  readonly socket: Socket;
  responseBytes: number;
  destroy: () => void;
}

function openWithheld(daemon: RunningDaemon): Promise<WithheldSocket> {
  return new Promise((resolve, reject) => {
    const declaredBytes = 400;
    const body = `{"session_id":"${'a'.repeat(24)}"`;
    const raw =
      `POST /hook/Stop HTTP/1.1\r\n` +
      `Host: 127.0.0.1:${daemon.port}\r\n` +
      `Authorization: Bearer ${daemon.token}\r\n` +
      'Content-Type: application/json\r\n' +
      `Content-Length: ${declaredBytes}\r\n` +
      '\r\n';
    const socket = connectTcp({ host: '127.0.0.1', port: daemon.port });
    socket.setNoDelay(true);
    const held: WithheldSocket = {
      socket,
      responseBytes: 0,
      destroy() {
        socket.destroy();
      },
    };
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error('withheld socket did not connect'));
    }, 2_000);
    socket.once('connect', () => {
      clearTimeout(timer);
      socket.write(raw);
      socket.write(body);
      resolve(held);
    });
    socket.on('data', (chunk: Buffer) => {
      held.responseBytes += chunk.byteLength;
    });
    socket.on('error', () => {
      // ECONNRESET is a valid form of the server's bounded FIN path.
    });
  });
}

async function measureHealth(
  daemon: RunningDaemon,
  config: BenchConfig,
  concurrency: number,
): Promise<WorkloadRow> {
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    const response = await fetch(`${daemon.baseUrl}/health`, requestInit(config));
    const text = await response.text();
    if (!response.ok) throw new Error(`GET /health returned HTTP ${response.status}: ${text}`);
    const body = parseJsonObject(text, 'GET /health');
    if (body['ok'] !== true || body['pid'] !== daemon.process.pid) {
      throw new Error('GET /health violated its ok/pid contract');
    }
    return encoder.encode(text).byteLength;
  });
  return rowFromSamples(
    'health',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    'G1 loopback-open GET /health',
  );
}

async function measureState(
  daemon: RunningDaemon,
  config: BenchConfig,
  concurrency: number,
): Promise<WorkloadRow> {
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    const response = await fetch(
      `${daemon.baseUrl}/state`,
      requestInit(config, { headers: authHeaders(daemon) }),
    );
    const text = await response.text();
    if (!response.ok) throw new Error(`GET /state returned HTTP ${response.status}: ${text}`);
    const body = parseJsonObject(text, 'GET /state');
    if (!Array.isArray(body['sessions'])) throw new Error('GET /state omitted sessions[]');
    return encoder.encode(text).byteLength;
  });
  return rowFromSamples(
    'state',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    'G2 GET /state snapshotWithLan() — heavier than the /ws snapshot (matrix §9.2)',
  );
}

async function measureHook(
  daemon: RunningDaemon,
  config: BenchConfig,
  sessionIds: readonly string[],
  concurrency: number,
): Promise<WorkloadRow> {
  let cursor = 0;
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    const index = cursor;
    cursor += 1;
    const sessionId = sessionIds[index % sessionIds.length];
    if (!sessionId) throw new Error('hook workload lost its seeded session');
    return hookPost(
      daemon,
      config,
      'Notification',
      {
        session_id: sessionId,
        hook_event_name: 'Notification',
        cwd: REPO_ROOT,
        message: `p6-http-bench ${index}`,
      },
      { emptyBody: true },
    );
  });
  return rowFromSamples(
    'hook',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    'P1 valid Notification — known handler, still 200 {}',
  );
}

async function measureHookFailOpen(
  daemon: RunningDaemon,
  config: BenchConfig,
  concurrency: number,
): Promise<WorkloadRow> {
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    return hookPost(
      daemon,
      config,
      'Stop',
      {
        hook_event_name: 'Stop',
        cwd: REPO_ROOT,
      },
      { emptyBody: true },
    );
  });
  return rowFromSamples(
    'hook-fail-open',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    'P1 fail-open: authenticated /hook/Stop missing session_id → validateHookEvent 200 {}',
  );
}

async function measurePaste(
  daemon: RunningDaemon,
  config: BenchConfig,
  concurrency: number,
): Promise<WorkloadRow> {
  const image = new Uint8Array(config.pasteBytes);
  image.set(PNG_MAGIC);
  const requestBody = JSON.stringify({ data: image.toBase64() });
  const expectedHash = sha256(image);
  const pasteDir = `${path.join(daemon.home, 'pastes')}${path.sep}`;
  const pasteOnce = async (): Promise<{ bytes: number; storedPath: string }> => {
    const response = await fetch(
      `${daemon.baseUrl}/api/paste-image`,
      requestInit(config, {
        method: 'POST',
        headers: authHeaders(daemon, { 'content-type': 'application/json' }),
        body: requestBody,
      }),
    );
    const text = await response.text();
    if (response.status !== 201) {
      throw new Error(`POST /api/paste-image returned HTTP ${response.status}: ${text}`);
    }
    const body = parseJsonObject(text, 'POST /api/paste-image');
    const storedPath = body['path'];
    if (
      body['ok'] !== true ||
      typeof storedPath !== 'string' ||
      !storedPath.startsWith(pasteDir) ||
      !storedPath.endsWith('.png')
    ) {
      throw new Error('large paste violated its ok/path/extension contract');
    }
    return { bytes: encoder.encode(text).byteLength, storedPath };
  };
  const first = await pasteOnce();
  if (!existsSync(first.storedPath)) {
    throw new Error('large paste acknowledgement did not leave a file on disk');
  }
  const persisted = await Bun.file(first.storedPath).bytes();
  if (persisted.byteLength !== image.byteLength || sha256(persisted) !== expectedHash) {
    throw new Error('large paste bytes changed between request and disk');
  }
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    const result = await pasteOnce();
    return result.bytes;
  });
  return rowFromSamples(
    'paste',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    `P8 /api/paste-image ${config.pasteBytes} decoded bytes (under 10 MiB image / 14e6 transport)`,
    {
      decodedBytes: config.pasteBytes,
      requestBytes: encoder.encode(requestBody).byteLength,
      sha256: expectedHash,
      persistedExact: true,
    },
  );
}

async function measureStaticShell(
  daemon: RunningDaemon,
  config: BenchConfig,
  concurrency: number,
): Promise<WorkloadRow> {
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    const response = await fetch(`${daemon.baseUrl}/`, requestInit(config));
    const text = await response.text();
    if (
      !response.ok ||
      !response.headers.get('content-type')?.includes('text/html') ||
      !text.includes('id="root"')
    ) {
      throw new Error(`GET / did not return the Fleet Deck HTML shell (HTTP ${response.status})`);
    }
    return encoder.encode(text).byteLength;
  });
  return rowFromSamples(
    'static-shell',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    'G10 public shell GET / (no-store HTML)',
  );
}

async function measureStaticAsset(
  daemon: RunningDaemon,
  config: BenchConfig,
  concurrency: number,
): Promise<WorkloadRow> {
  const timed = await runTimed(config.durationMs, config.warmupMs, concurrency, async () => {
    const response = await fetch(`${daemon.baseUrl}${daemon.assetPath}`, requestInit(config));
    const buffer = await response.arrayBuffer();
    const cacheControl = response.headers.get('cache-control') ?? '';
    if (
      !response.ok ||
      !response.headers.get('content-type')?.includes('javascript') ||
      !cacheControl.includes('immutable') ||
      buffer.byteLength === 0
    ) {
      throw new Error(
        `GET ${daemon.assetPath} violated the hashed-asset contract (HTTP ${response.status})`,
      );
    }
    return buffer.byteLength;
  });
  return rowFromSamples(
    'static-asset',
    daemon.target,
    concurrency,
    config.durationMs,
    config.warmupMs,
    timed.samples,
    timed.wallMs,
    `G10 public hashed asset GET ${daemon.assetPath}`,
    { assetPath: daemon.assetPath },
  );
}

async function measureWithheld(
  daemon: RunningDaemon,
  config: BenchConfig,
  withheldCount: number,
): Promise<WorkloadRow> {
  const sockets: WithheldSocket[] = [];
  try {
    for (let index = 0; index < withheldCount; index += 1) {
      sockets.push(await openWithheld(daemon));
    }
    await Bun.sleep(BODY_DRAIN_GRACE_MS + 250);
    const prematureBefore = sockets.reduce((sum, held) => sum + held.responseBytes, 0);
    if (prematureBefore > 0) {
      throw new Error(
        `withheld sockets received ${prematureBefore} response bytes before the probe`,
      );
    }
    const probeConcurrency = config.probeConcurrency;
    const timed = await runTimed(config.durationMs, 0, probeConcurrency, async () => {
      const response = await fetch(`${daemon.baseUrl}/health`, requestInit(config));
      const text = await response.text();
      if (!response.ok)
        throw new Error(`probe GET /health returned HTTP ${response.status}: ${text}`);
      const body = parseJsonObject(text, 'probe GET /health');
      if (body['ok'] !== true) throw new Error('probe GET /health violated ok=true');
      return encoder.encode(text).byteLength;
    });
    const prematureAfter = sockets.reduce((sum, held) => sum + held.responseBytes, 0);
    if (prematureAfter > 0) {
      throw new Error(
        `withheld sockets received ${prematureAfter} response bytes during the probe`,
      );
    }
    const notes =
      `N=${withheldCount} withheld POST /hook/Stop partial bodies; ` +
      `probe GET /health conc=${probeConcurrency}; prematureBytes=${prematureAfter}`;
    return rowFromSamples(
      'withheld',
      daemon.target,
      withheldCount,
      config.durationMs,
      0,
      timed.samples,
      timed.wallMs,
      notes,
      {
        withheldCount,
        probeConcurrency,
        drainGraceMs: BODY_DRAIN_GRACE_MS,
        prematureResponseBytes: prematureAfter,
        heldPastDrainGrace: true,
      },
    );
  } finally {
    for (const held of sockets) held.destroy();
  }
}

async function measureWs(
  daemon: RunningDaemon,
  config: BenchConfig,
  clientCount: number,
): Promise<WorkloadRow> {
  const clients: SnapshotClient[] = [];
  try {
    for (let index = 0; index < clientCount; index += 1) {
      const client = await openSnapshotClient(daemon.wsUrl, config.requestTimeoutMs);
      assertSnapshotFrame(await client.take(config.requestTimeoutMs));
      clients.push(client);
    }
    await Bun.sleep(BROADCAST_COALESCE_MS + 40);
    for (const client of clients) client.drain();

    const mutate = async (label: string): Promise<number> => {
      const sessionId = `p6-http-bench-ws-${randomUUID()}`;
      const frames = clients.map((client) => client.take(config.requestTimeoutMs));
      await hookPost(
        daemon,
        config,
        'Notification',
        {
          session_id: sessionId,
          hook_event_name: 'Notification',
          cwd: REPO_ROOT,
          message: label,
        },
        { emptyBody: true },
      );
      const delivered = await Promise.all(frames);
      let bytes = 0;
      for (const frame of delivered) bytes += assertSnapshotFrame(frame);
      return bytes;
    };

    const warmupCount = config.warmupMs > 0 ? 2 : 0;
    for (let index = 0; index < warmupCount; index += 1) {
      await mutate(`warmup-${index}`);
    }

    const samples: TimedSample[] = [];
    const stopAt = process.hrtime.bigint() + BigInt(config.durationMs) * 1_000_000n;
    const wallStarted = process.hrtime.bigint();
    let broadcasts = 0;
    while (process.hrtime.bigint() < stopAt) {
      const started = process.hrtime.bigint();
      try {
        const responseBytes = await mutate(`broadcast-${broadcasts}`);
        samples.push({
          ok: true,
          latencyMs: hrtimeMsSince(started),
          responseBytes,
          error: null,
        });
      } catch (error) {
        samples.push({
          ok: false,
          latencyMs: hrtimeMsSince(started),
          responseBytes: 0,
          error: clipError(error),
        });
      }
      broadcasts += 1;
    }
    return rowFromSamples(
      'ws',
      daemon.target,
      clientCount,
      config.durationMs,
      config.warmupMs,
      samples,
      hrtimeMsSince(wallStarted),
      `N=${clientCount} /ws snapshot clients; serial Notification mutations; coalesce ${BROADCAST_COALESCE_MS}ms`,
      { clients: clientCount, broadcasts, coalesceMs: BROADCAST_COALESCE_MS },
    );
  } finally {
    await Promise.all(clients.map((client) => client.close()));
  }
}

async function runWorkloads(
  daemon: RunningDaemon,
  config: BenchConfig,
  sessionIds: readonly string[],
): Promise<WorkloadRow[]> {
  const rows: WorkloadRow[] = [];
  const wanted = new Set(config.workloads);
  for (const concurrency of config.concurrency) {
    if (wanted.has('health')) rows.push(await measureHealth(daemon, config, concurrency));
    if (wanted.has('state')) rows.push(await measureState(daemon, config, concurrency));
    if (wanted.has('hook')) rows.push(await measureHook(daemon, config, sessionIds, concurrency));
    if (wanted.has('hook-fail-open'))
      rows.push(await measureHookFailOpen(daemon, config, concurrency));
    if (wanted.has('paste')) rows.push(await measurePaste(daemon, config, concurrency));
    if (wanted.has('static-shell'))
      rows.push(await measureStaticShell(daemon, config, concurrency));
    if (wanted.has('static-asset'))
      rows.push(await measureStaticAsset(daemon, config, concurrency));
    if (wanted.has('withheld')) rows.push(await measureWithheld(daemon, config, concurrency));
    if (wanted.has('ws')) rows.push(await measureWs(daemon, config, concurrency));
  }
  return rows;
}

function pad(value: string, width: number, right = false): string {
  return right ? value.padStart(width) : value.padEnd(width);
}

function formatMs(value: number): string {
  return value.toFixed(3);
}

function printTable(config: BenchConfig, rows: readonly WorkloadRow[]): void {
  const label = config.label === 'smoke' ? 'SMOKE (not a baseline)' : 'BASELINE';
  process.stdout.write(
    `P6.8 HTTP/WS bench  label=${label}  bun=${Bun.version}  duration=${config.durationMs / 1000}s  warmup=${config.warmupMs / 1000}s  conc=${config.concurrency.join(',')}\n`,
  );
  const header = [
    pad('target', 8),
    pad('workload', 16),
    pad('conc', 6, true),
    pad('n', 8, true),
    pad('ok', 8, true),
    pad('err', 6, true),
    pad('rps', 10, true),
    pad('p50', 10, true),
    pad('p95', 10, true),
    pad('p99', 10, true),
  ].join(' ');
  process.stdout.write(`${header}\n`);
  for (const row of rows) {
    process.stdout.write(
      [
        pad(row.target, 8),
        pad(row.workload, 16),
        pad(String(row.concurrency), 6, true),
        pad(String(row.count), 8, true),
        pad(String(row.ok), 8, true),
        pad(String(row.errors), 6, true),
        pad(row.operationsPerSecond.toFixed(1), 10, true),
        pad(formatMs(row.latencyMs.p50), 10, true),
        pad(formatMs(row.latencyMs.p95), 10, true),
        pad(formatMs(row.latencyMs.p99), 10, true),
      ].join(' '),
    );
    process.stdout.write('\n');
    if (row.notes) process.stdout.write(`         ${row.notes}\n`);
  }
}

function sha256(bytes: Uint8Array | string): string {
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
}

function killLiveDaemons(): void {
  for (const daemon of liveDaemons) {
    try {
      if (daemon.process.exitCode === null) daemon.process.kill('SIGKILL');
    } catch {
      // Best-effort teardown on signal/exit.
    }
    try {
      rmSync(daemon.home, { recursive: true, force: true });
    } catch {
      // Home removal is best-effort during emergency teardown.
    }
  }
  liveDaemons.clear();
}

async function main(): Promise<void> {
  const floor = runtimeFloor();
  const config = readConfig();
  if (config.requireFloor && !floor.exactMatch) {
    throw new Error(
      `P6.8 baseline requires Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${Bun.version} (${Bun.revision})`,
    );
  }
  if (config.pasteBytes > 8 * 1024 * 1024) {
    throw new Error('--paste-bytes must stay under the 10 MiB decoded-image cap; use a few MB');
  }

  process.on('exit', killLiveDaemons);
  process.on('SIGINT', () => {
    killLiveDaemons();
    process.exit(130);
  });
  process.on('SIGTERM', () => {
    killLiveDaemons();
    process.exit(143);
  });

  const machine = {
    platform: platform(),
    release: release(),
    arch: arch(),
    cpu: cpus()[0]?.model ?? 'unknown',
    logicalCpuCount: cpus().length,
  };
  const rows: WorkloadRow[] = [];
  const daemons: Array<{
    target: TargetName;
    path: string;
    pid: number | undefined;
    healthMs: number;
    reconciliationReadyMs: number;
    assetPath: string;
  }> = [];

  for (const target of config.targets) {
    const daemon = await startDaemon(target, config);
    try {
      const sessionIds = await seedSessions(daemon, config);
      rows.push(...(await runWorkloads(daemon, config, sessionIds)));
      daemons.push({
        target,
        path: path.relative(REPO_ROOT, daemon.path),
        pid: daemon.process.pid,
        healthMs: daemon.healthMs,
        reconciliationReadyMs: daemon.reconciliationReadyMs,
        assetPath: daemon.assetPath,
      });
    } finally {
      await stopDaemon(daemon, config.shutdownTimeoutMs);
    }
  }

  printTable(config, rows);

  const failed = rows.filter((row) => row.errors > 0 || row.ok === 0);
  const report = {
    schema: 1,
    kind: 'fleetdeck-effect-p6-http-bench',
    ok: failed.length === 0,
    label: config.label,
    recordedAt: new Date().toISOString(),
    runtimeFloor: floor,
    runtime: { bun: Bun.version, revision: Bun.revision },
    machine,
    config: {
      targets: config.targets,
      workloads: config.workloads,
      concurrency: config.concurrency,
      durationMs: config.durationMs,
      warmupMs: config.warmupMs,
      label: config.label,
      pasteBytes: config.pasteBytes,
      probeConcurrency: config.probeConcurrency,
      requestTimeoutMs: config.requestTimeoutMs,
      smoke: config.smoke,
    },
    comparison: {
      key: sha256(
        encoder.encode(
          JSON.stringify({
            runtime: { bun: Bun.version, revision: Bun.revision },
            machine,
            config: {
              workloads: config.workloads,
              concurrency: config.concurrency,
              durationMs: config.durationMs,
              warmupMs: config.warmupMs,
              pasteBytes: config.pasteBytes,
              probeConcurrency: config.probeConcurrency,
            },
          }),
        ),
      ),
      requirement:
        'compare only reports with identical key on the same idle machine; never treat label=smoke as a P6.8 baseline',
    },
    designChoices: [
      'Paste uses a few MB decoded (default 2 MiB, same as P0), under the 10 MiB image cap and 14e6 transport cap — not near 14e6.',
      'Valid hook is POST /hook/Notification with session_id (known handler still returns 200 {}).',
      'Fail-open is authenticated POST /hook/Stop with no session_id (validateHookEvent), not the tokenless silentHookRefusal site.',
      'Withheld body is a raw POST /hook/Stop with Content-Length and a partial JSON body; the measured signal is GET /health on other connections after BODY_DRAIN_GRACE_MS.',
      'WS clients subscribe to /ws snapshots (core.snapshot, no lan tokens). Mutations are serial because BROADCAST_COALESCE_MS=60 would merge overlapped POSTs.',
      'Hashed /assets/* path is discovered from the served GET / HTML so the harness does not hard-code Vite fingerprints.',
      'Representative concurrency is 1/8/32 (serial floor, typical board+hooks, burst). P0 characterization used 1/10/100 iteration-based samples; this harness is duration-based.',
    ],
    daemons,
    rows,
  };

  if (config.out) {
    await Bun.write(config.out, `${JSON.stringify(report, null, 2)}\n`);
    process.stdout.write(`wrote ${config.out}\n`);
  }

  if (failed.length > 0) {
    const summary = failed
      .map(
        (row) => `${row.target}/${row.workload}@${row.concurrency} ok=${row.ok} err=${row.errors}`,
      )
      .join('; ');
    throw new Error(`workload errors: ${summary}`);
  }
}

try {
  await main();
} catch (error) {
  killLiveDaemons();
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
