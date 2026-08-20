import { randomUUID } from 'node:crypto';
import { chmodSync, existsSync, mkdtempSync, rmSync, statSync } from 'node:fs';
import { connect as connectTcp } from 'node:net';
import { arch, cpus, platform, release, tmpdir } from 'node:os';
import path from 'node:path';

import { LATEST_USER_VERSION, openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';

import { REPO_ROOT, summarize, writeJsonReport } from './metrics.ts';

interface WorkloadConfig {
  iterations: number;
  concurrency: number[];
  warmup: number;
  wsIterations: number;
  wsClients: number;
  wsHeartbeatMs: number;
  terminalViewers: number;
  pasteIterations: number;
  pasteBytes: number;
  stallTimeoutMs: number;
  sqliteIterations: number;
  sqliteStatements: number;
  requestTimeoutMs: number;
  healthTimeoutMs: number;
  shutdownTimeoutMs: number;
  daemonPath: string;
}

interface RunningDaemon {
  process: Bun.ReadableSubprocess;
  stdout: Promise<string>;
  stderr: Promise<string>;
  home: string;
  port: number;
  baseUrl: string;
  token: string;
  healthMs: number;
  reconciliationReadyMs: number;
}

interface HealthBody {
  ok?: unknown;
  pid?: unknown;
  startup?: unknown;
  [key: string]: unknown;
}

interface OperationMeasurement {
  count: number;
  concurrency: number;
  durationMs: number;
  operationsPerSecond: number;
  responseBytes: number;
  latencyMs: ReturnType<typeof summarize>;
}

const encoder = new TextEncoder();
const SPAWN_FIXTURE = path.join(REPO_ROOT, 'tests/helpers/spawn-cmd-fixture.ts');
const TERM_FIXTURE = path.join(REPO_ROOT, 'tests/helpers/term-cmd-fixture.ts');
const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';

function runtimeFloor() {
  const actual = { bun: Bun.version, revision: Bun.revision };
  return {
    expected: { bun: EXPECTED_BUN_VERSION, revision: EXPECTED_BUN_REVISION },
    actual,
    exactMatch: actual.bun === EXPECTED_BUN_VERSION && actual.revision === EXPECTED_BUN_REVISION,
  };
}

function assertRuntimeFloor(): ReturnType<typeof runtimeFloor> {
  const floor = runtimeFloor();
  if (!floor.exactMatch) {
    throw new Error(
      `P0 requires Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${Bun.version} (${Bun.revision})`,
    );
  }
  return floor;
}

function rounded(value: number): number {
  return Math.round(value * 1000) / 1000;
}

function processUsage(processHandle: Bun.Subprocess) {
  const usage = processHandle.resourceUsage();
  if (!usage) return null;
  return {
    maxRSS: usage.maxRSS,
    cpuTime: {
      user: usage.cpuTime.user.toString(),
      system: usage.cpuTime.system.toString(),
      total: usage.cpuTime.total.toString(),
    },
    contextSwitches: {
      voluntary: usage.contextSwitches.voluntary,
      involuntary: usage.contextSwitches.involuntary,
    },
    messages: { sent: usage.messages.sent, received: usage.messages.received },
    ops: { in: usage.ops.in, out: usage.ops.out },
    shmSize: usage.shmSize,
    signalCount: usage.signalCount,
    swapCount: usage.swapCount,
  };
}

function integerFlag(name: string, fallback: number, minimum: number): number {
  const prefix = `--${name}=`;
  const raw = Bun.argv.find((arg) => arg.startsWith(prefix))?.slice(prefix.length);
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`--${name} must be an integer >= ${minimum}`);
  }
  return value;
}

function stringFlag(name: string, fallback: string): string {
  const prefix = `--${name}=`;
  return Bun.argv.find((arg) => arg.startsWith(prefix))?.slice(prefix.length) ?? fallback;
}

function concurrencyFlag(): number[] {
  const raw = stringFlag('concurrency', '1,10,100');
  const values = raw.split(',').map((part) => Number(part.trim()));
  if (values.length === 0 || values.some((value) => !Number.isSafeInteger(value) || value < 1)) {
    throw new Error('--concurrency must be a comma-separated list of positive integers');
  }
  return [...new Set(values)].sort((a, b) => a - b);
}

function readConfig(): WorkloadConfig {
  const daemonFlag = stringFlag('daemon', 'src/daemon/fleetd.bundle.mjs');
  return {
    iterations: integerFlag('iterations', 1_000, 1),
    concurrency: concurrencyFlag(),
    warmup: integerFlag('warmup', 20, 0),
    wsIterations: integerFlag('ws-iterations', 30, 1),
    wsClients: integerFlag('ws-clients', 10, 1),
    wsHeartbeatMs: integerFlag('ws-heartbeat-ms', 31_000, 0),
    terminalViewers: integerFlag('terminal-viewers', 10, 1),
    pasteIterations: integerFlag('paste-iterations', 10, 1),
    pasteBytes: integerFlag('paste-bytes', 2 * 1024 * 1024, 1_024),
    stallTimeoutMs: integerFlag('stall-timeout-ms', 15_000, 1_000),
    sqliteIterations: integerFlag('sqlite-iterations', 10_000, 10),
    sqliteStatements: integerFlag('sqlite-statements', 1_000, 1),
    requestTimeoutMs: integerFlag('request-timeout-ms', 15_000, 100),
    healthTimeoutMs: integerFlag('health-timeout-ms', 10_000, 100),
    shutdownTimeoutMs: integerFlag('shutdown-timeout-ms', 3_000, 100),
    daemonPath: path.resolve(REPO_ROOT, daemonFlag),
  };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
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
    const response = await fetch(`${baseUrl}/health?workload=${randomUUID()}`, {
      signal: AbortSignal.timeout(timeoutMs),
    });
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
        return {
          healthMs,
          reconciliationReadyMs: rounded(performance.now() - startedAt),
        };
      }
    }
    await Bun.sleep(5);
  }
  throw new Error(`daemon did not become reconciliation-ready within ${timeoutMs}ms`);
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

async function startDaemon(
  config: WorkloadConfig,
  extraEnv: Record<string, string> = {},
): Promise<RunningDaemon> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-workload-'));
  const port = await reservePort();
  const env = {
    ...cleanEnvironment(),
    FLEETDECK_PORT: String(port),
    FLEETDECK_HOME: home,
    FLEETDECK_BIND: '127.0.0.1',
    FLEETDECK_MDNS: 'off',
    FLEETDECK_AGENTS_CMD: 'false',
    FLEETDECK_HOLD_SCOPE: 'all',
    FLEETDECK_TMUX_SOCKET: `fleetdeck-effect-workload-${process.pid}-${randomUUID()}`,
    ...extraEnv,
  };
  // Cold readiness includes Bun.spawn itself, matching the baseline clock.
  const startedAt = performance.now();
  const processHandle = Bun.spawn([process.execPath, '--no-env-file', config.daemonPath], {
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
    const token = (await Bun.file(path.join(home, 'token')).text()).trim();
    if (token.length < 16) throw new Error('daemon did not create a valid bearer token');
    return {
      process: processHandle,
      stdout,
      stderr,
      home,
      port,
      baseUrl: `http://127.0.0.1:${port}`,
      token,
      ...readiness,
    };
  } catch (error) {
    if (processHandle.exitCode === null) processHandle.kill('SIGKILL');
    await processHandle.exited.catch(() => -1);
    await Promise.all([stdout, stderr]);
    rmSync(home, { recursive: true, force: true });
    throw error;
  }
}

async function stopDaemon(daemon: RunningDaemon, timeoutMs: number, requireGraceful = true) {
  const startedAt = performance.now();
  daemon.process.kill('SIGTERM');
  let forced = false;
  let exitCode: number;
  try {
    exitCode = await awaitExit(daemon.process, timeoutMs);
  } catch {
    forced = true;
    daemon.process.kill('SIGKILL');
    exitCode = await daemon.process.exited;
  }
  const [stdout, stderr] = await Promise.all([daemon.stdout, daemon.stderr]);
  if (requireGraceful && (forced || exitCode !== 0)) {
    throw new Error(`daemon workload shutdown failed (forced=${forced}, exit=${exitCode})`);
  }
  return {
    durationMs: rounded(performance.now() - startedAt),
    forced,
    exitCode,
    signalCode: daemon.process.signalCode,
    processUsage: processUsage(daemon.process),
    stdoutBytes: encoder.encode(stdout).byteLength,
    stderrBytes: encoder.encode(stderr).byteLength,
  };
}

async function runConcurrent(
  count: number,
  concurrency: number,
  operation: (index: number) => Promise<number>,
): Promise<OperationMeasurement> {
  let cursor = 0;
  const latencies = new Array<number>(count);
  const responseBytes = new Array<number>(count);
  const startedAt = performance.now();
  const workers = Array.from({ length: Math.min(count, concurrency) }, async () => {
    for (;;) {
      const index = cursor;
      cursor += 1;
      if (index >= count) return;
      const operationStartedAt = performance.now();
      responseBytes[index] = await operation(index);
      latencies[index] = performance.now() - operationStartedAt;
    }
  });
  await Promise.all(workers);
  const durationMs = performance.now() - startedAt;
  return {
    count,
    concurrency,
    durationMs: rounded(durationMs),
    operationsPerSecond: rounded(count / (durationMs / 1_000)),
    responseBytes: responseBytes.reduce((sum, bytes) => sum + (bytes ?? 0), 0),
    latencyMs: summarize(latencies.map((latency) => latency ?? 0)),
  };
}

async function benchmarkMatrix(
  config: WorkloadConfig,
  operation: (index: number) => Promise<number>,
  count = config.iterations,
  warmup = config.warmup,
): Promise<OperationMeasurement[]> {
  if (warmup > 0) {
    await runConcurrent(warmup, Math.min(10, warmup), operation);
  }
  const measurements: OperationMeasurement[] = [];
  for (const concurrency of config.concurrency) {
    // A configured 100-way sample must actually have enough operations to reach
    // 100 in flight, even for the deliberately smaller large-paste sample.
    measurements.push(await runConcurrent(Math.max(count, concurrency), concurrency, operation));
  }
  return measurements;
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

async function responseText(response: Response, context: string): Promise<string> {
  const text = await response.text();
  if (!response.ok) throw new Error(`${context} returned HTTP ${response.status}: ${text}`);
  return text;
}

function requestInit(config: WorkloadConfig, init: RequestInit = {}): RequestInit {
  return { ...init, signal: AbortSignal.timeout(config.requestTimeoutMs) };
}

async function hookPost(
  daemon: RunningDaemon,
  config: WorkloadConfig,
  event: string,
  payload: Record<string, unknown>,
): Promise<number> {
  const response = await fetch(
    `${daemon.baseUrl}/hook/${encodeURIComponent(event)}`,
    requestInit(config, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${daemon.token}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify(payload),
    }),
  );
  const text = await responseText(response, `POST /hook/${event}`);
  parseJsonObject(text || '{}', `POST /hook/${event}`);
  return encoder.encode(text).byteLength;
}

function sha256(bytes: Uint8Array): string {
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
}

async function largePasteWorkload(daemon: RunningDaemon, config: WorkloadConfig) {
  const image = new Uint8Array(config.pasteBytes);
  image.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const requestBody = JSON.stringify({ data: image.toBase64() });
  const expectedHash = sha256(image);
  const paths = new Set<string>();
  const paste = async (): Promise<number> => {
    const response = await fetch(
      `${daemon.baseUrl}/api/paste-image`,
      requestInit(config, {
        method: 'POST',
        headers: {
          authorization: `Bearer ${daemon.token}`,
          'content-type': 'application/json',
        },
        body: requestBody,
      }),
    );
    const text = await response.text();
    if (response.status !== 201) {
      throw new Error(`large paste returned HTTP ${response.status}: ${text}`);
    }
    const body = parseJsonObject(text, 'POST /api/paste-image');
    const storedPath = body['path'];
    if (
      body['ok'] !== true ||
      typeof storedPath !== 'string' ||
      !storedPath.startsWith(`${path.join(daemon.home, 'pastes')}${path.sep}`) ||
      !storedPath.endsWith('.png')
    ) {
      throw new Error('large paste violated its ok/path/extension contract');
    }
    paths.add(storedPath);
    return encoder.encode(text).byteLength;
  };

  // Verify persistence byte-for-byte once outside the concurrent pruning race.
  await paste();
  const correctnessPath = [...paths][0];
  if (!correctnessPath || !existsSync(correctnessPath)) {
    throw new Error('large paste acknowledgement did not leave a file on disk');
  }
  const persisted = await Bun.file(correctnessPath).bytes();
  if (persisted.byteLength !== image.byteLength || sha256(persisted) !== expectedHash) {
    throw new Error('large paste bytes changed between request and disk');
  }

  const matrix = await benchmarkMatrix(config, paste, config.pasteIterations, 0);
  return {
    decodedBytes: image.byteLength,
    requestBytes: encoder.encode(requestBody).byteLength,
    sha256: expectedHash,
    persistedExact: true,
    acknowledgedUniquePaths: paths.size,
    matrix,
  };
}

interface PartialBodyResult {
  mode: 'withheld-client-abort' | 'stalled-server-fin';
  durationMs: number;
  requestAgeMs: number;
  connectedMs: number;
  declaredBytes: number;
  sentBodyBytes: number;
  responseBytes: number;
  heldPastDrainGrace: boolean;
  noPrematureResponse: boolean;
  closeKind: 'close' | 'end';
}

function partialBodyRequest(
  daemon: RunningDaemon,
  config: WorkloadConfig,
  mode: PartialBodyResult['mode'],
): Promise<PartialBodyResult> {
  return new Promise((resolve, reject) => {
    const declaredBytes = 200;
    const body = `{"session_id":"${'a'.repeat(35)}`;
    const raw =
      `POST /hook/Stop HTTP/1.1\r\n` +
      `Host: 127.0.0.1:${daemon.port}\r\n` +
      `Authorization: Bearer ${daemon.token}\r\n` +
      'Content-Type: application/json\r\n' +
      `Content-Length: ${declaredBytes}\r\n` +
      '\r\n';
    const startedAt = performance.now();
    let connectedMs = 0;
    let responseBytes = 0;
    let heldPastDrainGrace = false;
    let settled = false;
    let clientAborted = false;
    let requestSentAt = 0;
    let grace: ReturnType<typeof setTimeout> | undefined;
    const socket = connectTcp({ host: '127.0.0.1', port: daemon.port });
    socket.setNoDelay(true);

    const cleanup = (): void => {
      if (grace) clearTimeout(grace);
      clearTimeout(deadline);
      socket.removeAllListeners();
    };
    const finish = (closeKind: 'close' | 'end'): void => {
      if (settled) return;
      settled = true;
      const requestAgeMs = requestSentAt === 0 ? 0 : performance.now() - requestSentAt;
      heldPastDrainGrace ||= requestAgeMs >= 1_000;
      cleanup();
      if (!heldPastDrainGrace && !clientAborted) {
        reject(
          new Error(
            `partial request ${mode} closed after ${rounded(requestAgeMs)}ms, before the one-second drain grace elapsed`,
          ),
        );
        return;
      }
      resolve({
        mode,
        durationMs: rounded(performance.now() - startedAt),
        requestAgeMs: rounded(requestAgeMs),
        connectedMs,
        declaredBytes,
        sentBodyBytes: encoder.encode(body).byteLength,
        responseBytes,
        heldPastDrainGrace,
        noPrematureResponse: responseBytes === 0,
        closeKind,
      });
    };
    const deadline = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      socket.destroy();
      reject(new Error(`raw partial-body ${mode} exceeded ${config.stallTimeoutMs}ms`));
    }, config.stallTimeoutMs);
    socket.once('connect', () => {
      connectedMs = rounded(performance.now() - startedAt);
      requestSentAt = performance.now();
      socket.write(raw);
      socket.write(body);
      grace = setTimeout(() => {
        heldPastDrainGrace = true;
        if (mode === 'withheld-client-abort') {
          clientAborted = true;
          socket.destroy();
        }
      }, 1_250);
    });
    socket.on('data', (chunk) => {
      responseBytes += chunk.byteLength;
    });
    socket.on('error', () => {
      // ECONNRESET is a valid form of the server's bounded FIN/terminate path.
    });
    socket.once('end', () => finish('end'));
    socket.once('close', () => finish('close'));
  });
}

async function rawBodyWorkloads(daemon: RunningDaemon, config: WorkloadConfig) {
  const withheld = await partialBodyRequest(daemon, config, 'withheld-client-abort');
  if (!withheld.heldPastDrainGrace || !withheld.noPrematureResponse) {
    throw new Error('withheld request was answered before its incomplete body drained');
  }
  const stalledFin = await partialBodyRequest(daemon, config, 'stalled-server-fin');
  if (!stalledFin.heldPastDrainGrace || !stalledFin.noPrematureResponse) {
    throw new Error('stalled raw request violated its bounded no-response FIN contract');
  }
  return { withheld, stalledFin };
}

async function httpWorkloads(daemon: RunningDaemon, config: WorkloadConfig) {
  const sessionIds = Array.from(
    { length: 15 },
    (_, index) => `effect-p0-http-${index}-${randomUUID()}`,
  );
  for (const sessionId of sessionIds) {
    await hookPost(daemon, config, 'SessionStart', {
      session_id: sessionId,
      hook_event_name: 'SessionStart',
      cwd: REPO_ROOT,
      source: 'startup',
    });
  }

  const health = await benchmarkMatrix(config, async () => {
    const response = await fetch(
      `${daemon.baseUrl}/health?bench=${randomUUID()}`,
      requestInit(config),
    );
    const text = await responseText(response, 'GET /health');
    const body = parseJsonObject(text, 'GET /health');
    if (body['ok'] !== true || body['pid'] !== daemon.process.pid) {
      throw new Error('GET /health violated its ok/pid contract');
    }
    return encoder.encode(text).byteLength;
  });

  const state = await benchmarkMatrix(config, async () => {
    const response = await fetch(
      `${daemon.baseUrl}/state`,
      requestInit(config, {
        headers: { authorization: `Bearer ${daemon.token}` },
      }),
    );
    const text = await responseText(response, 'GET /state');
    const body = parseJsonObject(text, 'GET /state');
    if (!Array.isArray(body['sessions'])) throw new Error('GET /state omitted sessions[]');
    return encoder.encode(text).byteLength;
  });

  const staticAsset = await benchmarkMatrix(config, async () => {
    const response = await fetch(`${daemon.baseUrl}/`, requestInit(config));
    const text = await responseText(response, 'GET /');
    if (
      !response.headers.get('content-type')?.includes('text/html') ||
      !text.includes('id="root"')
    ) {
      throw new Error('GET / did not return the Fleet Deck HTML shell');
    }
    return encoder.encode(text).byteLength;
  });

  const hook = await benchmarkMatrix(config, async (index) =>
    hookPost(daemon, config, 'Notification', {
      session_id: sessionIds[index % sessionIds.length],
      hook_event_name: 'Notification',
      cwd: REPO_ROOT,
      message: `P0 HTTP workload ${index}`,
    }),
  );

  const largePaste = await largePasteWorkload(daemon, config);
  const rawBody = await rawBodyWorkloads(daemon, config);

  return {
    seededSessions: sessionIds.length,
    sessionIds,
    health,
    state,
    static: staticAsset,
    hook,
    largePaste,
    rawBody,
  };
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

function waitForOpen(socket: WebSocket, timeoutMs: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error('WebSocket open timed out'));
    }, timeoutMs);
    const opened = () => {
      cleanup();
      resolve();
    };
    const failed = () => {
      cleanup();
      reject(new Error('WebSocket failed before open'));
    };
    const cleanup = () => {
      clearTimeout(timer);
      socket.removeEventListener('open', opened);
      socket.removeEventListener('error', failed);
    };
    socket.addEventListener('open', opened, { once: true });
    socket.addEventListener('error', failed, { once: true });
  });
}

function waitForMessage(socket: WebSocket, timeoutMs: number): Promise<string> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error('WebSocket message timed out'));
    }, timeoutMs);
    const received = (event: MessageEvent) => {
      cleanup();
      messageDataText(event.data).then(resolve, reject);
    };
    const failed = () => {
      cleanup();
      reject(new Error('WebSocket failed while awaiting a frame'));
    };
    const closed = () => {
      cleanup();
      reject(new Error('WebSocket closed while awaiting a frame'));
    };
    const cleanup = () => {
      clearTimeout(timer);
      socket.removeEventListener('message', received);
      socket.removeEventListener('error', failed);
      socket.removeEventListener('close', closed);
    };
    socket.addEventListener('message', received, { once: true });
    socket.addEventListener('error', failed, { once: true });
    socket.addEventListener('close', closed, { once: true });
  });
}

async function closeWebSocket(socket: WebSocket, timeoutMs: number): Promise<void> {
  if (socket.readyState === WebSocket.CLOSED) return;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, timeoutMs);
    socket.addEventListener(
      'close',
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
    socket.close(1000, 'P0 workload complete');
  });
}

function assertSnapshotFrame(text: string): number {
  const frame = parseJsonObject(text, 'WebSocket snapshot');
  if (frame['type'] !== 'snapshot' || !Array.isArray(frame['sessions'])) {
    throw new Error('WebSocket frame violated the snapshot contract');
  }
  return encoder.encode(text).byteLength;
}

function snapshotContainsSession(text: string, sessionId: string): boolean {
  const frame = parseJsonObject(text, 'WebSocket reconnect snapshot');
  const sessions = frame['sessions'];
  return (
    frame['type'] === 'snapshot' &&
    Array.isArray(sessions) &&
    sessions.some((session) => isObject(session) && session['session_id'] === sessionId)
  );
}

function waitForClose(
  socket: WebSocket,
  timeoutMs: number,
): Promise<{ code: number; reason: string }> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error('WebSocket close timed out'));
    }, timeoutMs);
    const closed = (event: CloseEvent) => {
      cleanup();
      resolve({ code: event.code, reason: event.reason });
    };
    const cleanup = () => {
      clearTimeout(timer);
      socket.removeEventListener('close', closed);
    };
    socket.addEventListener('close', closed, { once: true });
  });
}

async function websocketWorkload(daemon: RunningDaemon, config: WorkloadConfig) {
  // The last HTTP mutation can have one coalesced broadcast queued. Drain it before
  // connecting so each client observes exactly one initial frame and one frame per trigger.
  await Bun.sleep(50);
  const sockets: WebSocket[] = [];
  const connectionLatencies: number[] = [];
  let receivedBytes = 0;
  try {
    await Promise.all(
      Array.from({ length: config.wsClients }, async () => {
        const startedAt = performance.now();
        const socket = new WebSocket(
          `${daemon.baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(daemon.token)}`,
        );
        sockets.push(socket);
        const firstMessage = waitForMessage(socket, config.requestTimeoutMs);
        await waitForOpen(socket, config.requestTimeoutMs);
        receivedBytes += assertSnapshotFrame(await firstMessage);
        connectionLatencies.push(performance.now() - startedAt);
      }),
    );

    const broadcastLatencies: number[] = [];
    const sessionId = `effect-p0-ws-${randomUUID()}`;
    for (let index = 0; index < config.wsIterations; index += 1) {
      const frames = sockets.map((socket) => waitForMessage(socket, config.requestTimeoutMs));
      const startedAt = performance.now();
      await hookPost(daemon, config, 'Notification', {
        session_id: sessionId,
        hook_event_name: 'Notification',
        cwd: REPO_ROOT,
        message: `P0 WS broadcast ${index}`,
      });
      const delivered = await Promise.all(frames);
      broadcastLatencies.push(performance.now() - startedAt);
      for (const frame of delivered) receivedBytes += assertSnapshotFrame(frame);
    }

    const reconnectStartedAt = performance.now();
    const replaced = sockets.shift();
    if (!replaced) throw new Error('WebSocket reconnect workload had no original socket');
    await closeWebSocket(replaced, config.requestTimeoutMs);
    const replacement = new WebSocket(
      `${daemon.baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(daemon.token)}`,
    );
    const replacementSnapshot = waitForMessage(replacement, config.requestTimeoutMs);
    await waitForOpen(replacement, config.requestTimeoutMs);
    const reconnectFrame = await replacementSnapshot;
    receivedBytes += assertSnapshotFrame(reconnectFrame);
    if (!snapshotContainsSession(reconnectFrame, sessionId)) {
      throw new Error('WebSocket reconnect snapshot omitted the latest mutation');
    }
    sockets.push(replacement);
    const reconnectMs = rounded(performance.now() - reconnectStartedAt);

    const heartbeatStartedAt = performance.now();
    if (config.wsHeartbeatMs > 0) await Bun.sleep(config.wsHeartbeatMs);
    if (sockets.some((socket) => socket.readyState !== WebSocket.OPEN)) {
      throw new Error('an auto-pong WebSocket did not survive the heartbeat hold');
    }
    const heartbeat = {
      holdMs: rounded(performance.now() - heartbeatStartedAt),
      serverCadenceMs: 30_000,
      expectedPingCadences: Math.floor(config.wsHeartbeatMs / 30_000),
      clientsStillOpen: sockets.length,
    };

    const closeStartedAt = performance.now();
    await Promise.all(sockets.map((socket) => closeWebSocket(socket, config.requestTimeoutMs)));
    return {
      clients: sockets.length,
      broadcasts: config.wsIterations,
      deliveredFrames: sockets.length * (config.wsIterations + 1),
      receivedBytes,
      queue: {
        serverBufferedAmountLimitBytes: 1 << 20,
        droppedFrames: 0,
        resyncs: 0,
      },
      connectLatencyMs: summarize(connectionLatencies),
      broadcastFanoutLatencyMs: summarize(broadcastLatencies),
      reconnectMs,
      heartbeat,
      closeMs: rounded(performance.now() - closeStartedAt),
    };
  } finally {
    await Promise.all(sockets.map((socket) => closeWebSocket(socket, 250)));
  }
}

async function websocketBackpressureWorkload(config: WorkloadConfig) {
  const daemon = await startDaemon(config, { FLEETDECK_WS_BUFFER_MAX: '-1' });
  const sockets: WebSocket[] = [];
  try {
    const sessionId = `effect-p0-backpressure-${randomUUID()}`;
    const first = new WebSocket(
      `${daemon.baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(daemon.token)}`,
    );
    sockets.push(first);
    const initial = waitForMessage(first, config.requestTimeoutMs);
    await waitForOpen(first, config.requestTimeoutMs);
    const initialText = await initial;
    const initialBytes = assertSnapshotFrame(initialText);
    const closed = waitForClose(first, config.requestTimeoutMs);
    const evictionStartedAt = performance.now();
    await hookPost(daemon, config, 'Notification', {
      session_id: sessionId,
      hook_event_name: 'Notification',
      cwd: REPO_ROOT,
      message: 'P0 deterministic slow-reader/backpressure eviction',
    });
    const close = await closed;
    const evictionMs = rounded(performance.now() - evictionStartedAt);

    const reconnectStartedAt = performance.now();
    const replacement = new WebSocket(
      `${daemon.baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(daemon.token)}`,
    );
    sockets.push(replacement);
    const fresh = waitForMessage(replacement, config.requestTimeoutMs);
    await waitForOpen(replacement, config.requestTimeoutMs);
    const freshText = await fresh;
    const freshBytes = assertSnapshotFrame(freshText);
    if (!snapshotContainsSession(freshText, sessionId)) {
      throw new Error('backpressure reconnect did not resync the dropped mutation');
    }
    const reconnectMs = rounded(performance.now() - reconnectStartedAt);
    await closeWebSocket(replacement, config.requestTimeoutMs);
    const shutdown = await stopDaemon(daemon, config.shutdownTimeoutMs);
    return {
      seam: 'FLEETDECK_WS_BUFFER_MAX=-1 treats an otherwise-drained peer as slow',
      configuredQueueLimitBytes: -1,
      mutations: 1,
      sentBeforeEviction: 0,
      droppedOrResyncedFrames: 1,
      initialBytes,
      freshSnapshotBytes: freshBytes,
      evictionMs,
      close,
      reconnectMs,
      resynced: true,
      shutdown,
    };
  } finally {
    await Promise.all(sockets.map((socket) => closeWebSocket(socket, 100)));
    if (daemon.process.exitCode === null) {
      daemon.process.kill('SIGKILL');
      await daemon.process.exited.catch(() => -1);
    }
    rmSync(daemon.home, { recursive: true, force: true });
  }
}

interface CollectedFrame {
  value: Record<string, unknown>;
  bytes: number;
  receivedAt: number;
}

function collectJsonFrames(socket: WebSocket) {
  const frames: CollectedFrame[] = [];
  let decodeError: Error | null = null;
  socket.addEventListener('message', (event) => {
    void messageDataText(event.data)
      .then((text) => {
        frames.push({
          value: parseJsonObject(text, 'terminal WebSocket'),
          bytes: encoder.encode(text).byteLength,
          receivedAt: performance.now(),
        });
      })
      .catch((error: unknown) => {
        decodeError = error instanceof Error ? error : new Error(String(error));
      });
  });
  return {
    frames,
    async waitFor(type: string, timeoutMs: number): Promise<CollectedFrame> {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        if (decodeError) throw decodeError;
        const frame = frames.find(({ value }) => value['t'] === type);
        if (frame) return frame;
        const error = frames.find(({ value }) => value['t'] === 'err');
        if (error)
          throw new Error(`terminal WebSocket returned err: ${String(error.value['reason'])}`);
        await Bun.sleep(5);
      }
      throw new Error(`terminal WebSocket did not receive ${type} within ${timeoutMs}ms`);
    },
  };
}

async function terminalFanoutWorkload(
  daemon: RunningDaemon,
  config: WorkloadConfig,
  scratch: string,
) {
  const response = await fetch(
    `${daemon.baseUrl}/api/spawn`,
    requestInit(config, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${daemon.token}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({ cwd: scratch, prompt: 'P0 terminal output fanout' }),
    }),
  );
  const text = await response.text();
  if (response.status !== 200) {
    throw new Error(`terminal fixture spawn returned HTTP ${response.status}: ${text}`);
  }
  const spawn = parseJsonObject(text, 'POST /api/spawn');
  const spawnId = spawn['spawn_id'];
  if (typeof spawnId !== 'string' || spawnId.length === 0) {
    throw new Error('terminal fixture spawn did not return spawn_id');
  }

  const sockets: WebSocket[] = [];
  const collectors: ReturnType<typeof collectJsonFrames>[] = [];
  const startedAt = performance.now();
  try {
    await Promise.all(
      Array.from({ length: config.terminalViewers }, async () => {
        const socket = new WebSocket(
          `${daemon.baseUrl.replace(/^http/, 'ws')}/ws/term?spawn=${encodeURIComponent(spawnId)}&cols=90&rows=30&t=${encodeURIComponent(daemon.token)}`,
        );
        sockets.push(socket);
        collectors.push(collectJsonFrames(socket));
        await waitForOpen(socket, config.requestTimeoutMs);
      }),
    );
    const initFrames = await Promise.all(
      collectors.map((collector) => collector.waitFor('init', config.requestTimeoutMs)),
    );
    const outputFrames = await Promise.all(
      collectors.map((collector) => collector.waitFor('out', config.requestTimeoutMs)),
    );
    for (const frame of initFrames) {
      if (frame.value['cols'] !== 90 || frame.value['rows'] !== 30) {
        throw new Error('terminal init frame changed the requested dimensions');
      }
    }
    for (const frame of outputFrames) {
      if (typeof frame.value['data'] !== 'string' || !frame.value['data'].includes('live %')) {
        throw new Error('terminal fanout frame omitted fixture output');
      }
    }
    const initLatencies = initFrames.map((frame) => frame.receivedAt - startedAt);
    const outputLatencies = outputFrames.map((frame) => frame.receivedAt - startedAt);
    const receivedBytes = collectors.reduce(
      (total, collector) => total + collector.frames.reduce((sum, frame) => sum + frame.bytes, 0),
      0,
    );
    const closeStartedAt = performance.now();
    await Promise.all(sockets.map((socket) => closeWebSocket(socket, config.requestTimeoutMs)));
    return {
      viewers: config.terminalViewers,
      spawnId,
      initFrames: initFrames.length,
      outputFrames: outputFrames.length,
      deliveredFrames: initFrames.length + outputFrames.length,
      receivedBytes,
      queue: {
        preInitOutputLimitBytes: 256 * 1024,
        socketBufferedAmountLimitBytes: 4 << 20,
        droppedFrames: 0,
      },
      initLatencyMs: summarize(initLatencies),
      outputFanoutLatencyMs: summarize(outputLatencies),
      closeMs: rounded(performance.now() - closeStartedAt),
    };
  } finally {
    await Promise.all(sockets.map((socket) => closeWebSocket(socket, 100)));
  }
}

function startEventLoopMonitor(intervalMs = 10) {
  const delays: number[] = [];
  let expected = performance.now() + intervalMs;
  const timer = setInterval(() => {
    const now = performance.now();
    delays.push(Math.max(0, now - expected));
    expected = now + intervalMs;
  }, intervalMs);
  return {
    stop() {
      clearInterval(timer);
      return {
        observer: 'workload driver process',
        intervalMs,
        samples: delays.length,
        delayMs: summarize(delays),
        available: delays.length > 0,
      };
    },
  };
}

async function busyShutdownWorkload(config: WorkloadConfig) {
  const daemon = await startDaemon(config, { FLEETDECK_STALL_FIN_S: '4' });
  const inFlight = Math.min(15, Math.max(...config.concurrency));
  try {
    const requests = Array.from({ length: inFlight }, () =>
      partialBodyRequest(daemon, config, 'stalled-server-fin'),
    );
    // Keep every request incomplete past the body-drain grace so shutdown lands
    // with real request/socket work in flight.
    await Bun.sleep(1_300);
    const shutdown = await stopDaemon(daemon, config.shutdownTimeoutMs, false);
    const results = await Promise.allSettled(requests);
    const fulfilled = results.flatMap((result) =>
      result.status === 'fulfilled' ? [result.value] : [],
    );
    return {
      state: 'busy-incomplete-request-bodies',
      inFlight,
      settled: results.filter((result) => result.status === 'fulfilled').length,
      rejected: results.filter((result) => result.status === 'rejected').length,
      requestDurationMs: summarize(fulfilled.map((result) => result.durationMs)),
      responseBytes: fulfilled.reduce((sum, result) => sum + result.responseBytes, 0),
      shutdown,
    };
  } finally {
    if (daemon.process.exitCode === null) {
      daemon.process.kill('SIGKILL');
      await daemon.process.exited.catch(() => -1);
    }
    rmSync(daemon.home, { recursive: true, force: true });
  }
}

async function heldHookShutdownWorkload(config: WorkloadConfig) {
  const daemon = await startDaemon(config);
  let socket: WebSocket | null = null;
  try {
    const sessionId = `effect-p0-held-${randomUUID()}`;
    await hookPost(daemon, config, 'SessionStart', {
      session_id: sessionId,
      hook_event_name: 'SessionStart',
      cwd: REPO_ROOT,
      source: 'startup',
    });
    socket = new WebSocket(
      `${daemon.baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(daemon.token)}`,
    );
    const first = waitForMessage(socket, config.requestTimeoutMs);
    await waitForOpen(socket, config.requestTimeoutMs);
    assertSnapshotFrame(await first);

    let settled = false;
    const heldStartedAt = performance.now();
    const heldRequest = fetch(
      `${daemon.baseUrl}/hook/PermissionRequest`,
      requestInit(config, {
        method: 'POST',
        headers: {
          authorization: `Bearer ${daemon.token}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify({
          session_id: sessionId,
          hook_event_name: 'PermissionRequest',
          cwd: REPO_ROOT,
          tool_name: 'Bash',
          tool_input: { command: 'echo held-for-p0' },
        }),
      }),
    )
      .then(async (response) => ({
        kind: 'response' as const,
        status: response.status,
        bytes: encoder.encode(await response.text()).byteLength,
      }))
      .catch((error: unknown) => ({
        kind: 'error' as const,
        error: error instanceof Error ? error.message : String(error),
      }))
      .finally(() => {
        settled = true;
      });
    await Bun.sleep(200);
    if (settled) throw new Error('PermissionRequest did not enter the held-hook state');
    const shutdown = await stopDaemon(daemon, config.shutdownTimeoutMs, false);
    const outcome = await heldRequest;
    return {
      state: 'held-PermissionRequest-with-live-board-consumer',
      confirmedHeldBeforeSignal: true,
      heldBeforeSignalMs: rounded(performance.now() - heldStartedAt - shutdown.durationMs),
      requestOutcome: outcome,
      shutdown,
    };
  } finally {
    if (socket) await closeWebSocket(socket, 100);
    if (daemon.process.exitCode === null) {
      daemon.process.kill('SIGKILL');
      await daemon.process.exited.catch(() => -1);
    }
    rmSync(daemon.home, { recursive: true, force: true });
  }
}

function snapshotSessions(snapshot: unknown): Record<string, unknown>[] {
  if (!isObject(snapshot) || !Array.isArray(snapshot['sessions'])) {
    throw new Error('Fleet Deck core.snapshot() omitted sessions[]');
  }
  return snapshot['sessions'].filter(isObject);
}

async function sqliteWorkload(config: WorkloadConfig) {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-sqlite-'));
  const databasePath = path.join(scratch, 'fleetd.db');
  const rssBefore = process.memoryUsage().rss;
  const cpuBefore = process.cpuUsage();
  let database: ReturnType<typeof openDb> | null = null;
  let contender: ReturnType<typeof openDb> | null = null;
  try {
    const openedAt = performance.now();
    database = openDb(databasePath);
    const openAndMigrateMs = rounded(performance.now() - openedAt);
    const userVersion = Number(
      database.prepare<{ user_version: number | bigint }>('PRAGMA user_version').get()
        ?.user_version ?? -1,
    );
    if (userVersion !== LATEST_USER_VERSION) {
      throw new Error(
        `Fleet Deck migration ended at user_version=${userVersion}, expected ${LATEST_USER_VERSION}`,
      );
    }

    const core = createCore(database, {
      port: 0,
      home: scratch,
      version: 'effect-p0-workload',
    });
    await core.retentionSweep();
    const sessionIds = Array.from(
      { length: 15 },
      (_, index) => `effect-p0-sqlite-${index}-${randomUUID()}`,
    );
    for (const sessionId of sessionIds) {
      core.applyEvent({
        session_id: sessionId,
        hook_event_name: 'SessionStart',
        cwd: scratch,
        source: 'startup',
      });
    }
    const seeded = snapshotSessions(core.snapshot());
    if (!sessionIds.every((id) => seeded.some((session) => session['session_id'] === id))) {
      throw new Error('Fleet Deck snapshot did not contain all 15 seeded sessions');
    }

    // Hold distinct native statements until close to expose statement-cache/
    // prepared-statement growth at the real openDb seam.
    const rssBeforeStatements = process.memoryUsage().rss;
    const distinctStatements = Array.from({ length: config.sqliteStatements }, (_, index) => {
      const statement = database?.prepare<{ value: number }>(
        `SELECT ? AS value /* effect-p0-distinct-${index} */`,
      );
      const row = statement?.get(index);
      if (!statement || row?.value !== index) {
        throw new Error(`distinct Fleet Deck statement ${index} returned the wrong value`);
      }
      return statement;
    });
    const statementRssBytes = process.memoryUsage().rss - rssBeforeStatements;

    const readLatencies: number[] = [];
    const writeLatencies: number[] = [];
    const transactionLatencies: number[] = [];
    let reads = 0;
    let writes = 0;
    let transactions = 0;
    const eventLoopScheduledAt = performance.now();
    let eventLoopFiredAt = 0;
    const eventLoopTurn = new Promise<void>((resolve) => {
      setTimeout(() => {
        eventLoopFiredAt = performance.now();
        resolve();
      }, 0);
    });
    const workloadStartedAt = performance.now();
    for (let index = 0; index < config.sqliteIterations; index += 1) {
      const sessionId = sessionIds[index % sessionIds.length];
      if (!sessionId) throw new Error('SQLite mix lost its seeded session');
      const kind = index % 10;
      const operationStartedAt = performance.now();
      if (kind < 6) {
        const sessions = snapshotSessions(core.snapshot());
        if (!sessions.some((session) => session['session_id'] === sessionId)) {
          throw new Error(`snapshot read lost ${sessionId}`);
        }
        reads += 1;
        readLatencies.push(performance.now() - operationStartedAt);
      } else if (kind < 9) {
        core.applyEvent({
          session_id: sessionId,
          hook_event_name: 'Notification',
          message: `P0 SQLite write ${index}`,
        });
        writes += 1;
        writeLatencies.push(performance.now() - operationStartedAt);
      } else {
        database.exec('BEGIN IMMEDIATE');
        try {
          for (let offset = 0; offset < 2; offset += 1) {
            const transactionSession = sessionIds[(index + offset) % sessionIds.length];
            if (!transactionSession) throw new Error('transaction lost its seeded session');
            core.applyEvent({
              session_id: transactionSession,
              hook_event_name: 'Notification',
              message: `P0 SQLite transaction ${index}/${offset}`,
            });
          }
          database.exec('COMMIT');
        } catch (error) {
          database.exec('ROLLBACK');
          throw error;
        }
        transactions += 1;
        transactionLatencies.push(performance.now() - operationStartedAt);
      }
    }
    const durationMs = performance.now() - workloadStartedAt;
    await eventLoopTurn;
    const blockedEventLoopMs = rounded(eventLoopFiredAt - eventLoopScheduledAt);

    const finalSessions = snapshotSessions(core.snapshot());
    if (
      finalSessions.length < sessionIds.length ||
      !sessionIds.every((id) => finalSessions.some((session) => session['session_id'] === id))
    ) {
      throw new Error('Fleet Deck final snapshot lost a seeded session');
    }
    const counts = database
      .prepare<{ sessions: number; events: number }>(
        `SELECT
           (SELECT COUNT(*) FROM sessions) AS sessions,
           (SELECT COUNT(*) FROM events) AS events`,
      )
      .get();
    if (!counts || Number(counts.sessions) < 15 || Number(counts.events) < 15 + writes) {
      throw new Error('Fleet Deck SQLite row counts did not reflect the mixed workload');
    }

    contender = openDb(databasePath);
    contender.exec('PRAGMA busy_timeout = 50');
    let busyError = '';
    const busyStartedAt = performance.now();
    database.exec('BEGIN IMMEDIATE');
    try {
      try {
        contender
          .prepare('UPDATE sessions SET note = note WHERE session_id = ?')
          .run(sessionIds[0] ?? '');
      } catch (error) {
        busyError = error instanceof Error ? error.message : String(error);
      }
    } finally {
      database.exec('ROLLBACK');
    }
    const busyMs = rounded(performance.now() - busyStartedAt);
    if (!/busy|locked/i.test(busyError)) {
      throw new Error(`second Fleet Deck connection did not surface busy contention: ${busyError}`);
    }
    contender.close();
    contender = null;

    database.exec('PRAGMA wal_checkpoint(TRUNCATE)');
    const databaseBytes = statSync(databasePath).size;
    // Keep the array strongly reachable through the measurement, then release
    // everything at the same close boundary Fleet Deck uses.
    const retainedStatementCount = distinctStatements.length;
    database.close();
    database = null;

    const restartStartedAt = performance.now();
    database = openDb(databasePath);
    const restartedCore = createCore(database, {
      port: 0,
      home: scratch,
      version: 'effect-p0-workload-restart',
    });
    await restartedCore.retentionSweep();
    const restartedSessions = snapshotSessions(restartedCore.snapshot());
    const restartMs = rounded(performance.now() - restartStartedAt);
    if (
      !sessionIds.every((id) => restartedSessions.some((session) => session['session_id'] === id))
    ) {
      throw new Error('Fleet Deck restart snapshot lost durable sessions');
    }
    const restartUserVersion = Number(
      database.prepare<{ user_version: number | bigint }>('PRAGMA user_version').get()
        ?.user_version ?? -1,
    );
    if (restartUserVersion !== LATEST_USER_VERSION) {
      throw new Error('Fleet Deck restart changed the migrated user_version');
    }
    database.close();
    database = null;

    const cpu = process.cpuUsage(cpuBefore);
    return {
      api: 'Fleet Deck openDb/createCore over bun:sqlite',
      strictProductionSchema: true,
      iterations: config.sqliteIterations,
      openAndMigrateMs,
      userVersion,
      expectedUserVersion: LATEST_USER_VERSION,
      durationMs: rounded(durationMs),
      operationsPerSecond: rounded(config.sqliteIterations / (durationMs / 1_000)),
      mix: { reads, writes, transactions },
      latencyMs: {
        snapshotReads: summarize(readLatencies),
        writes: summarize(writeLatencies),
        transactions: summarize(transactionLatencies),
      },
      rows: { sessions: Number(counts.sessions), events: Number(counts.events) },
      statementGrowth: {
        distinctPreparedStatements: retainedStatementCount,
        rssBytes: statementRssBytes,
      },
      busyContention: { surfaced: true, timeoutMs: 50, durationMs: busyMs, error: busyError },
      blockedEventLoopMs,
      restart: { verified: true, durationMs: restartMs, userVersion: restartUserVersion },
      databaseBytes,
      processDelta: {
        rssBytes: process.memoryUsage().rss - rssBefore,
        cpuUserMicros: cpu.user,
        cpuSystemMicros: cpu.system,
      },
    };
  } finally {
    try {
      contender?.close();
    } catch {
      // Best-effort cleanup after a failed contention probe.
    }
    try {
      database?.close();
    } catch {
      // The success path closes before the restart verification.
    }
    rmSync(scratch, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const floor = assertRuntimeFloor();
  const config = readConfig();
  if (!(await Bun.file(config.daemonPath).exists())) {
    throw new Error(`daemon artifact not found: ${config.daemonPath}`);
  }
  const sqlite = await sqliteWorkload(config);
  const backpressure = await websocketBackpressureWorkload(config);
  const terminalScratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-terminal-'));
  const termRecord = path.join(terminalScratch, 'term.jsonl');
  try {
    chmodSync(SPAWN_FIXTURE, 0o755);
    chmodSync(TERM_FIXTURE, 0o755);
  } catch {
    // Git preserves both fixture executable bits; chmod is a portable best effort.
  }
  const daemon = await startDaemon(config, {
    FLEETDECK_STALL_FIN_S: '4',
    FLEETDECK_SPAWN_CMD: SPAWN_FIXTURE,
    FLEETDECK_TERM_CMD: TERM_FIXTURE,
    FLEETDECK_TEST_TERM_RECORD: termRecord,
    FLEETDECK_NUDGE_MS: '60000',
  });
  let http: Awaited<ReturnType<typeof httpWorkloads>>;
  let websocket: Awaited<ReturnType<typeof websocketWorkload>>;
  let terminal: Awaited<ReturnType<typeof terminalFanoutWorkload>>;
  let shutdown: Awaited<ReturnType<typeof stopDaemon>>;
  const loopMonitor = startEventLoopMonitor();
  let eventLoop: ReturnType<typeof loopMonitor.stop>;
  try {
    http = await httpWorkloads(daemon, config);
    websocket = await websocketWorkload(daemon, config);
    terminal = await terminalFanoutWorkload(daemon, config, terminalScratch);
    eventLoop = loopMonitor.stop();
    shutdown = await stopDaemon(daemon, config.shutdownTimeoutMs);
  } finally {
    eventLoop ??= loopMonitor.stop();
    if (daemon.process.exitCode === null) {
      daemon.process.kill('SIGKILL');
      await daemon.process.exited.catch(() => -1);
    }
    rmSync(daemon.home, { recursive: true, force: true });
    rmSync(terminalScratch, { recursive: true, force: true });
  }
  const busyShutdown = await busyShutdownWorkload(config);
  const heldHookShutdown = await heldHookShutdownWorkload(config);

  const machine = {
    platform: platform(),
    release: release(),
    arch: arch(),
    cpu: cpus()[0]?.model ?? 'unknown',
    logicalCpuCount: cpus().length,
  };
  const comparisonKey = sha256(
    encoder.encode(
      JSON.stringify({
        runtime: { bun: Bun.version, revision: Bun.revision },
        machine,
        config: { ...config, daemonPath: path.relative(REPO_ROOT, config.daemonPath) },
      }),
    ),
  );

  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p0-workloads',
    ok: true,
    recordedAt: new Date().toISOString(),
    runtimeFloor: floor,
    runtime: { bun: Bun.version, revision: Bun.revision },
    machine,
    config: { ...config, daemonPath: path.relative(REPO_ROOT, config.daemonPath) },
    comparison: {
      key: comparisonKey,
      requirement: 'compare only reports with identical key on the same idle machine',
    },
    daemon: {
      pid: daemon.process.pid,
      healthMs: daemon.healthMs,
      reconciliationReadyMs: daemon.reconciliationReadyMs,
      shutdown: {
        idleAfterWorkloads: shutdown,
        busy: busyShutdown,
        heldHook: heldHookShutdown,
      },
      eventLoop: {
        driver: eventLoop,
        daemonRoundTripProxy: {
          source: '/health response latency by configured concurrency',
          directInternalCounterAvailable: false,
          unavailableReason: 'the P0 daemon exposes no internal event-loop-delay counter',
          p99Ms: http.health.map((measurement) => ({
            concurrency: measurement.concurrency,
            p99: measurement.latencyMs.p99,
          })),
        },
      },
    },
    http,
    websocket: {
      snapshotBroadcast: websocket,
      slowReaderBackpressure: backpressure,
      terminalOutputFanout: terminal,
    },
    sqlite,
  });
}

try {
  await main();
} catch (error) {
  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p0-workloads',
    ok: false,
    recordedAt: new Date().toISOString(),
    runtimeFloor: runtimeFloor(),
    error: error instanceof Error ? error.message : String(error),
  });
  process.exitCode = 1;
}
