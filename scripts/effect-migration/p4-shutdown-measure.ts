import { randomUUID } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { arch, cpus, platform, release, totalmem, version as osVersion } from 'node:os';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build as esbuild } from 'esbuild';

import { decodeMessage } from '../../src/daemon/mdns.ts';
import { REPO_ROOT, runCommand, summarize, writeJsonReport } from './metrics.ts';

type TargetName = 'source' | 'bundle';
type Scenario =
  | 'graceful-idle'
  | 'forced-held-hook'
  | 'forced-websocket'
  | 'forced-process'
  | 'mdns-goodbye';

interface MeasureConfig {
  readonly targets: readonly TargetName[];
  readonly warmups: number;
  readonly idleRuns: number;
  readonly forcedRuns: number;
  readonly mdnsRuns: number;
  readonly healthTimeoutMs: number;
  readonly processExitTimeoutMs: number;
  readonly residueTimeoutMs: number;
  readonly postExitObservationMs: number;
  readonly forceSignalDelayMs: number;
  readonly mdnsSendDelayMs: number;
  readonly rootDeadlineMs: number;
  readonly mdnsWatchdogMs: number;
  readonly smoke: boolean;
}

interface HealthBody {
  readonly ok?: unknown;
  readonly pid?: unknown;
  readonly startup?: unknown;
}

interface MdnsRecord {
  readonly type?: string;
  readonly wire?: string;
  readonly at?: number;
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
  readonly token: string;
  readonly startupAttempts: number;
  readonly healthMs: number;
  readonly reconciliationReadyMs: number;
}

interface DescendantInspection {
  readonly available: boolean;
  readonly pids: readonly number[];
}

interface RunChecks {
  readonly exitedWithinHarnessDeadline: boolean;
  readonly expectedExit: boolean;
  readonly withinRootDeadline: boolean;
  readonly secondSignalDelivered: boolean | null;
  readonly heldHookReleasedCanonically: boolean | null;
  readonly websocketClosed: boolean | null;
  readonly mdnsGoodbyeOnce: boolean | null;
  readonly mdnsGoodbyeCallbackOnce: boolean | null;
  readonly pidReleased: boolean;
  readonly pidFileReleased: boolean;
  readonly listenerReleased: boolean;
  readonly socketsReleased: boolean;
  readonly descendantsReleased: boolean;
  readonly activeProcessReleased: boolean | null;
  readonly scheduledCallbacksStopped: boolean;
  readonly rootKeepAliveReleased: boolean;
  readonly stableAfterObservation: boolean;
  readonly noUnexpectedShutdownOutput: boolean;
}

interface LifecycleRun {
  readonly scenario: Scenario;
  readonly index: number;
  readonly startupAttempts: number;
  readonly healthMs: number;
  readonly reconciliationReadyMs: number;
  readonly shutdownMs: number;
  readonly forcedMs: number | null;
  readonly exitCode: number;
  readonly signalCode: string | null;
  readonly before: {
    readonly descendants: DescendantInspection;
    readonly activeProcessPid: number | null;
  };
  readonly workload: {
    readonly heldHookStatus: number | null;
    readonly heldHookBody: string | null;
    readonly websocketFinalState: number | null;
    readonly mdnsGoodbyeCount: number | null;
    readonly mdnsGoodbyeCallbackCount: number | null;
  };
  readonly residue: {
    readonly daemonPidAlive: boolean;
    readonly pidFilePresent: boolean;
    readonly listenerPortRebindSucceeded: boolean;
    readonly knownDescendantsAlive: readonly number[];
    readonly activeProcessAlive: boolean | null;
    readonly descriptors: {
      readonly basis: 'owning-process-exited';
      readonly fds: 0;
      readonly sockets: 0;
    };
    readonly scheduledCallbacks: {
      readonly basis: 'owning-process-exited-and-remained-dead';
      readonly capableOfRunning: false;
      readonly observationMs: number;
    };
    readonly rootRuntimeKeepAlive: {
      readonly basis: 'owning-process-exited-and-remained-dead';
      readonly capableOfRunning: false;
      readonly observationMs: number;
    };
    readonly waitMs: number;
  };
  readonly output: {
    readonly stdoutBytes: number;
    readonly stderrBytes: number;
    readonly unexpectedShutdownOutput: boolean;
  };
  readonly checks: RunChecks;
}

const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
const SOURCE_DAEMON = path.join(REPO_ROOT, 'src/daemon/fleetd.ts');
const BUNDLE_DAEMON = path.join(REPO_ROOT, 'src/daemon/fleetd.bundle.mjs');
const PROCESS_FIXTURE = fileURLToPath(
  new URL('./fixtures/p4-term-resistant-agents.ts', import.meta.url),
);
const encoder = new TextEncoder();
const FORCED_SCENARIOS = [
  'forced-held-hook',
  'forced-websocket',
  'forced-process',
] as const satisfies readonly Scenario[];
const ALL_SCENARIOS = [
  'graceful-idle',
  ...FORCED_SCENARIOS,
  'mdns-goodbye',
] as const satisfies readonly Scenario[];
const UNEXPECTED_OUTPUT =
  /shutdown error|SQLITE_MISUSE|database (?:is )?closed|unhandled rejection|root defect/i;
const BUNDLE_BANNER =
  "// GENERATED by 'bun run bundle' — do not edit. Source: src/daemon/*.ts\nimport { createRequire as __cr } from 'node:module'; const require = __cr(import.meta.url);";
const BUNDLE_SCRIPT_PREFIX =
  'esbuild src/daemon/fleetd.ts --bundle --platform=node --format=esm --outfile=src/daemon/fleetd.bundle.mjs --external:bun:sqlite --minify-identifiers --keep-names';

function rounded(value: number): number {
  return Math.round(value * 1_000) / 1_000;
}

function sha256(bytes: Uint8Array | string): string {
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
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

function readConfig(): MeasureConfig {
  const rawTarget = stringFlag('target', 'both');
  if (!['source', 'bundle', 'both'].includes(rawTarget)) {
    throw new Error('--target must be source, bundle, or both');
  }
  return {
    targets: rawTarget === 'both' ? ['source', 'bundle'] : [rawTarget as TargetName],
    warmups: integerFlag('warmups', 1, 0),
    idleRuns: integerFlag('idle-runs', 30, 1),
    forcedRuns: integerFlag('forced-runs', 5, 1),
    mdnsRuns: integerFlag('mdns-runs', 5, 1),
    healthTimeoutMs: integerFlag('health-timeout-ms', 10_000, 100),
    processExitTimeoutMs: integerFlag('process-exit-timeout-ms', 3_000, 1_750),
    residueTimeoutMs: integerFlag('residue-timeout-ms', 1_000, 100),
    postExitObservationMs: integerFlag('post-exit-observation-ms', 50, 10),
    forceSignalDelayMs: integerFlag('force-signal-delay-ms', 20, 1),
    mdnsSendDelayMs: integerFlag('mdns-send-delay-ms', 200, 1),
    rootDeadlineMs: 1_750,
    mdnsWatchdogMs: 1_000,
    smoke: Bun.argv.includes('--smoke'),
  };
}

function runtimeFloor() {
  const actual = { bun: Bun.version, revision: Bun.revision };
  return {
    expected: { bun: EXPECTED_BUN_VERSION, revision: EXPECTED_BUN_REVISION },
    actual,
    exactMatch: actual.bun === EXPECTED_BUN_VERSION && actual.revision === EXPECTED_BUN_REVISION,
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
    throw new Error('failed to reserve a daemon port');
  }
  return port;
}

async function portCanRebind(port: number): Promise<boolean> {
  let server: ReturnType<typeof Bun.serve> | null = null;
  try {
    server = Bun.serve({
      hostname: '127.0.0.1',
      port,
      fetch: () => new Response('rebind'),
    });
    return true;
  } catch {
    return false;
  } finally {
    if (server) await server.stop(true);
  }
}

function pidIsAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return !(isObject(error) && typeof error['code'] === 'string' && error['code'] === 'ESRCH');
  }
}

async function descendantsOf(parentPid: number): Promise<DescendantInspection> {
  try {
    const result = await runCommand(['ps', '-A', '-o', 'pid=', '-o', 'ppid=']);
    if (result.exitCode !== 0) return { available: false, pids: [] };
    const children = new Map<number, number[]>();
    for (const line of result.stdout.split('\n')) {
      const match = /^\s*(\d+)\s+(\d+)\s*$/.exec(line);
      if (!match) continue;
      const pid = Number(match[1]);
      const ppid = Number(match[2]);
      const direct = children.get(ppid) ?? [];
      direct.push(pid);
      children.set(ppid, direct);
    }
    const pids: number[] = [];
    const pending = [...(children.get(parentPid) ?? [])];
    while (pending.length > 0) {
      const pid = pending.shift();
      if (pid === undefined || pids.includes(pid)) continue;
      pids.push(pid);
      pending.push(...(children.get(pid) ?? []));
    }
    return { available: true, pids: pids.sort((left, right) => left - right) };
  } catch {
    return { available: false, pids: [] };
  }
}

async function healthAt(baseUrl: string, timeoutMs: number): Promise<HealthBody | null> {
  try {
    const response = await fetch(`${baseUrl}/health?p4=${randomUUID()}`, {
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
    const health = await healthAt(
      baseUrl,
      Math.min(500, Math.max(1, deadline - performance.now())),
    );
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
): Promise<{ exitCode: number; deadlineExceeded: boolean }> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const exitCode = await Promise.race([
      processHandle.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`process ${processHandle.pid} exceeded ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
    return { exitCode, deadlineExceeded: false };
  } catch {
    if (processHandle.exitCode === null) processHandle.kill('SIGKILL');
    return { exitCode: await processHandle.exited, deadlineExceeded: true };
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function scenarioEnvironment(
  scenario: Scenario,
  home: string,
  mdnsRecord: string,
  processPidFile: string,
  config: MeasureConfig,
): Record<string, string> {
  const usesMdns =
    scenario === 'forced-held-hook' ||
    scenario === 'forced-websocket' ||
    scenario === 'mdns-goodbye';
  return {
    ...cleanEnvironment(),
    FLEETDECK_BIND: usesMdns ? '0.0.0.0' : '127.0.0.1',
    FLEETDECK_MDNS: usesMdns ? 'on' : 'off',
    FLEETDECK_AGENTS_CMD:
      scenario === 'forced-process'
        ? `${process.execPath} ${PROCESS_FIXTURE} ${processPidFile}`
        : 'false',
    FLEETDECK_AGENTS_POLL_MS: '100',
    FLEETDECK_AGENTS_IDLE_POLL_MS: '100',
    FLEETDECK_HOLD_MS: '60000',
    FLEETDECK_HOLD_SCOPE: 'all',
    FLEETDECK_MDNS_RECORD: mdnsRecord,
    FLEETDECK_MDNS_SEND_DELAY_MS: String(config.mdnsSendDelayMs),
    FLEETDECK_TEST_NET_MOCK: usesMdns ? '1' : '0',
    FLEETDECK_TOKEN: `p4-shutdown-${randomUUID()}-0123456789abcdef`,
    FLEETDECK_HOME: home,
    FLEETDECK_TMUX_SOCKET: `fleetdeck-p4-shutdown-${process.pid}-${randomUUID()}`,
  };
}

async function startDaemon(
  target: TargetName,
  scenario: Scenario,
  config: MeasureConfig,
  priorAttempts = 0,
): Promise<RunningDaemon> {
  const home = mkdtempSync(path.join(tmpdir(), `fleetdeck-p4-${target}-${scenario}-`));
  const port = await reservePort();
  const mdnsRecord = path.join(home, 'mdns.jsonl');
  const processPidFile = path.join(home, 'active-process.pid');
  const env = scenarioEnvironment(scenario, home, mdnsRecord, processPidFile, config);
  env['FLEETDECK_PORT'] = String(port);
  const startedAt = performance.now();
  const processHandle = Bun.spawn([process.execPath, '--no-env-file', targetPath(target)], {
    cwd: REPO_ROOT,
    env,
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = new Response(processHandle.stdout).text().catch(() => '');
  const stderr = new Response(processHandle.stderr).text().catch(() => '');
  try {
    const readiness = await waitForReady(
      processHandle,
      `http://127.0.0.1:${port}`,
      config.healthTimeoutMs,
      startedAt,
    );
    return {
      target,
      path: targetPath(target),
      process: processHandle,
      stdout,
      stderr,
      home,
      port,
      baseUrl: `http://127.0.0.1:${port}`,
      token: env['FLEETDECK_TOKEN'] ?? '',
      startupAttempts: priorAttempts + 1,
      ...readiness,
    };
  } catch (error) {
    if (processHandle.exitCode === null) processHandle.kill('SIGKILL');
    const [, stdoutText, stderrText] = await Promise.all([
      processHandle.exited.catch(() => -1),
      stdout,
      stderr,
    ]);
    const retryableBindElection =
      processHandle.exitCode === 3 &&
      stderrText.trim() === 'fleetd already running (port bind lost the election)';
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    if (retryableBindElection && priorAttempts < 4) {
      return startDaemon(target, scenario, config, priorAttempts + 1);
    }
    throw new Error(
      `${error instanceof Error ? error.message : String(error)}\nstdout:\n${stdoutText}\nstderr:\n${stderrText}`,
    );
  }
}

async function waitUntil<T>(
  operation: () => T | null | false | Promise<T | null | false>,
  label: string,
  timeoutMs = 5_000,
): Promise<T> {
  const deadline = performance.now() + timeoutMs;
  while (performance.now() < deadline) {
    const value = await operation();
    if (value !== null && value !== false) return value;
    await Bun.sleep(5);
  }
  throw new Error(`${label} did not complete within ${timeoutMs}ms`);
}

function mdnsRecords(file: string): MdnsRecord[] {
  if (!existsSync(file)) return [];
  const records: MdnsRecord[] = [];
  for (const line of readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    try {
      records.push(JSON.parse(line) as MdnsRecord);
    } catch {
      // A concurrent append can leave only the final line incomplete.
    }
  }
  return records;
}

function mdnsTtl(record: MdnsRecord, expected: 0 | 'live'): boolean {
  if (record.type !== 'send' || !record.wire) return false;
  const packet = decodeMessage(Buffer.from(record.wire, 'base64'));
  if (!packet?.answers.length) return false;
  return expected === 0
    ? packet.answers.every((answer) => answer.ttl === 0)
    : packet.answers.some((answer) => (answer.ttl ?? 0) > 0);
}

async function openBoardWebSocket(daemon: RunningDaemon): Promise<WebSocket> {
  const socket = new WebSocket(
    `${daemon.baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(daemon.token)}`,
  );
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error('board WebSocket did not open'));
    }, 5_000);
    socket.addEventListener(
      'open',
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
    socket.addEventListener(
      'error',
      () => {
        clearTimeout(timer);
        reject(new Error('board WebSocket failed before open'));
      },
      { once: true },
    );
  });
  return socket;
}

async function waitForWebSocketClose(socket: WebSocket, timeoutMs = 2_000): Promise<void> {
  if (socket.readyState === WebSocket.CLOSED) return;
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('board WebSocket did not close')), timeoutMs);
    socket.addEventListener(
      'close',
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}

async function closeWebSocket(socket: WebSocket | null): Promise<void> {
  if (!socket || socket.readyState === WebSocket.CLOSED) return;
  const closed = waitForWebSocketClose(socket, 250).catch(() => undefined);
  try {
    socket.close(1000, 'P4 evidence cleanup');
  } catch {
    // The daemon may already have retired the native client.
  }
  await closed;
}

async function jsonRequest(
  url: string,
  token: string,
  init: RequestInit = {},
): Promise<{ status: number; text: string; json: unknown }> {
  const headers = new Headers(init.headers);
  headers.set('authorization', `Bearer ${token}`);
  const response = await fetch(url, {
    ...init,
    headers,
    signal: init.signal ?? AbortSignal.timeout(10_000),
  });
  const text = await response.text();
  let json: unknown = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    // The caller retains the exact text for compatibility checks.
  }
  return { status: response.status, text, json };
}

async function postHook(
  daemon: RunningDaemon,
  event: string,
  body: unknown,
): Promise<{ status: number; text: string; json: unknown }> {
  return jsonRequest(`${daemon.baseUrl}/hook/${event}`, daemon.token, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}

async function prepareHeldHook(daemon: RunningDaemon): Promise<{
  readonly socket: WebSocket;
  readonly response: Promise<{ status: number; text: string; json: unknown }>;
}> {
  const socket = await openBoardWebSocket(daemon);
  const sessionId = `p4-evidence-${randomUUID()}`;
  const started = await postHook(daemon, 'SessionStart', {
    hook_event_name: 'SessionStart',
    session_id: sessionId,
    cwd: daemon.home,
    source: 'startup',
  });
  if (started.status !== 200) throw new Error(`SessionStart returned ${started.status}`);
  let settled = false;
  const response = postHook(daemon, 'PermissionRequest', {
    hook_event_name: 'PermissionRequest',
    session_id: sessionId,
    cwd: daemon.home,
    tool_name: 'Bash',
    tool_input: { command: 'printf p4-evidence-held' },
  }).finally(() => {
    settled = true;
  });
  await waitUntil(async () => {
    const state = await jsonRequest(`${daemon.baseUrl}/state`, daemon.token);
    if (state.status !== 200 || !isObject(state.json) || !Array.isArray(state.json['questions'])) {
      return false;
    }
    return state.json['questions'].some(
      (question) =>
        isObject(question) &&
        question['session_id'] === sessionId &&
        question['kind'] === 'permission' &&
        question['status'] === 'pending' &&
        question['held'] === true,
    );
  }, 'held PermissionRequest');
  if (settled) throw new Error('PermissionRequest settled before shutdown');
  return { socket, response };
}

function readPid(file: string): number | null {
  if (!existsSync(file)) return null;
  const pid = Number(readFileSync(file, 'utf8').trim());
  return Number.isSafeInteger(pid) && pid > 1 ? pid : null;
}

async function waitForResidue(
  daemon: RunningDaemon,
  descendants: DescendantInspection,
  activeProcessPid: number | null,
  config: MeasureConfig,
) {
  const startedAt = performance.now();
  let snapshot = {
    daemonPidAlive: true,
    pidFilePresent: true,
    listenerPortRebindSucceeded: false,
    knownDescendantsAlive: [...descendants.pids],
    activeProcessAlive: activeProcessPid === null ? null : true,
  };
  for (;;) {
    snapshot = {
      daemonPidAlive: pidIsAlive(daemon.process.pid),
      pidFilePresent: existsSync(path.join(daemon.home, 'fleetd.pid')),
      listenerPortRebindSucceeded: await portCanRebind(daemon.port),
      knownDescendantsAlive: descendants.pids.filter(pidIsAlive),
      activeProcessAlive: activeProcessPid === null ? null : pidIsAlive(activeProcessPid),
    };
    const released =
      !snapshot.daemonPidAlive &&
      !snapshot.pidFilePresent &&
      snapshot.listenerPortRebindSucceeded &&
      (!descendants.available || snapshot.knownDescendantsAlive.length === 0) &&
      snapshot.activeProcessAlive !== true;
    if (released || performance.now() - startedAt >= config.residueTimeoutMs) break;
    await Bun.sleep(10);
  }
  await Bun.sleep(config.postExitObservationMs);
  const stable = {
    daemonPidAlive: pidIsAlive(daemon.process.pid),
    pidFilePresent: existsSync(path.join(daemon.home, 'fleetd.pid')),
    listenerPortRebindSucceeded: await portCanRebind(daemon.port),
    knownDescendantsAlive: descendants.pids.filter(pidIsAlive),
    activeProcessAlive: activeProcessPid === null ? null : pidIsAlive(activeProcessPid),
  };
  return {
    ...stable,
    descriptors: {
      basis: 'owning-process-exited' as const,
      fds: 0 as const,
      sockets: 0 as const,
    },
    scheduledCallbacks: {
      basis: 'owning-process-exited-and-remained-dead' as const,
      capableOfRunning: false as const,
      observationMs: config.postExitObservationMs,
    },
    rootRuntimeKeepAlive: {
      basis: 'owning-process-exited-and-remained-dead' as const,
      capableOfRunning: false as const,
      observationMs: config.postExitObservationMs,
    },
    waitMs: rounded(performance.now() - startedAt),
    stable:
      !stable.daemonPidAlive &&
      !stable.pidFilePresent &&
      stable.listenerPortRebindSucceeded &&
      (!descendants.available || stable.knownDescendantsAlive.length === 0) &&
      stable.activeProcessAlive !== true,
  };
}

function allChecksPass(checks: RunChecks): boolean {
  return Object.values(checks).every((value) => value !== false);
}

async function lifecycleRun(
  target: TargetName,
  scenario: Scenario,
  index: number,
  config: MeasureConfig,
): Promise<LifecycleRun> {
  const daemon = await startDaemon(target, scenario, config);
  const mdnsFile = path.join(daemon.home, 'mdns.jsonl');
  const processPidFile = path.join(daemon.home, 'active-process.pid');
  let socket: WebSocket | null = null;
  let heldResponse: Promise<{ status: number; text: string; json: unknown }> | null = null;
  let activeProcessPid: number | null = null;
  try {
    if (
      scenario === 'forced-held-hook' ||
      scenario === 'forced-websocket' ||
      scenario === 'mdns-goodbye'
    ) {
      await waitUntil(
        () => mdnsRecords(mdnsFile).find((record) => mdnsTtl(record, 'live')) ?? null,
        'live mDNS announcement',
      );
    }
    if (scenario === 'forced-held-hook') {
      const held = await prepareHeldHook(daemon);
      socket = held.socket;
      heldResponse = held.response;
    } else if (scenario === 'forced-websocket') {
      socket = await openBoardWebSocket(daemon);
    } else if (scenario === 'forced-process') {
      activeProcessPid = await waitUntil(() => readPid(processPidFile), 'TERM-resistant process');
      if (!pidIsAlive(activeProcessPid))
        throw new Error('TERM-resistant process exited before signal');
    }

    const descendants = await descendantsOf(daemon.process.pid);
    const websocketClosed = socket ? waitForWebSocketClose(socket) : null;
    const shutdownStartedAt = performance.now();
    daemon.process.kill('SIGTERM');
    let secondSignalAt: number | null = null;
    if (FORCED_SCENARIOS.includes(scenario as (typeof FORCED_SCENARIOS)[number])) {
      if (scenario === 'forced-process') {
        await Bun.sleep(config.forceSignalDelayMs);
      } else {
        await waitUntil(
          () => mdnsRecords(mdnsFile).find((record) => mdnsTtl(record, 0)) ?? null,
          'mDNS goodbye before force signal',
          config.mdnsWatchdogMs,
        );
      }
      if (daemon.process.exitCode !== null) {
        throw new Error(`${scenario} exited before the force signal could be delivered`);
      }
      secondSignalAt = performance.now();
      daemon.process.kill('SIGTERM');
    }

    const exited = await awaitExit(daemon.process, config.processExitTimeoutMs);
    const exitedAt = performance.now();
    const [stdout, stderr, hook] = await Promise.all([
      daemon.stdout,
      daemon.stderr,
      heldResponse ?? Promise.resolve(null),
      ...(websocketClosed ? [websocketClosed] : []),
    ]).then(([out, err, held]) => [out as string, err as string, held] as const);
    const residue = await waitForResidue(daemon, descendants, activeProcessPid, config);
    const records = mdnsRecords(mdnsFile);
    const goodbyes = records.filter((record) => mdnsTtl(record, 0));
    const goodbyeWires = new Set(goodbyes.flatMap((record) => (record.wire ? [record.wire] : [])));
    const goodbyeCallbacks = records.filter(
      (record) => record.type === 'callback' && record.wire && goodbyeWires.has(record.wire),
    );
    const hasMdns = scenario !== 'graceful-idle' && scenario !== 'forced-process';
    const isForced = FORCED_SCENARIOS.includes(scenario as (typeof FORCED_SCENARIOS)[number]);
    const unexpectedShutdownOutput = UNEXPECTED_OUTPUT.test(`${stdout}\n${stderr}`);
    const checks: RunChecks = {
      exitedWithinHarnessDeadline: !exited.deadlineExceeded,
      expectedExit: exited.exitCode === 0 && daemon.process.signalCode === null,
      withinRootDeadline: rounded(exitedAt - shutdownStartedAt) < config.rootDeadlineMs,
      secondSignalDelivered: isForced ? secondSignalAt !== null : null,
      heldHookReleasedCanonically:
        scenario === 'forced-held-hook'
          ? hook?.status === 200 && hook.text === '{}' && JSON.stringify(hook.json) === '{}'
          : null,
      websocketClosed: socket ? socket.readyState === WebSocket.CLOSED : null,
      mdnsGoodbyeOnce: hasMdns ? goodbyes.length === 1 : null,
      mdnsGoodbyeCallbackOnce: hasMdns ? goodbyeCallbacks.length === 1 : null,
      pidReleased: !residue.daemonPidAlive,
      pidFileReleased: !residue.pidFilePresent,
      listenerReleased: residue.listenerPortRebindSucceeded,
      socketsReleased: !residue.daemonPidAlive && residue.listenerPortRebindSucceeded,
      descendantsReleased: !descendants.available || residue.knownDescendantsAlive.length === 0,
      activeProcessReleased:
        activeProcessPid === null ? null : residue.activeProcessAlive === false,
      scheduledCallbacksStopped: !residue.scheduledCallbacks.capableOfRunning,
      rootKeepAliveReleased: !residue.rootRuntimeKeepAlive.capableOfRunning,
      stableAfterObservation: residue.stable,
      noUnexpectedShutdownOutput: !unexpectedShutdownOutput,
    };
    const result: LifecycleRun = {
      scenario,
      index,
      startupAttempts: daemon.startupAttempts,
      healthMs: daemon.healthMs,
      reconciliationReadyMs: daemon.reconciliationReadyMs,
      shutdownMs: rounded(exitedAt - shutdownStartedAt),
      forcedMs: secondSignalAt === null ? null : rounded(exitedAt - secondSignalAt),
      exitCode: exited.exitCode,
      signalCode: daemon.process.signalCode,
      before: { descendants, activeProcessPid },
      workload: {
        heldHookStatus: hook?.status ?? null,
        heldHookBody: hook?.text ?? null,
        websocketFinalState: socket?.readyState ?? null,
        mdnsGoodbyeCount: hasMdns ? goodbyes.length : null,
        mdnsGoodbyeCallbackCount: hasMdns ? goodbyeCallbacks.length : null,
      },
      residue: {
        daemonPidAlive: residue.daemonPidAlive,
        pidFilePresent: residue.pidFilePresent,
        listenerPortRebindSucceeded: residue.listenerPortRebindSucceeded,
        knownDescendantsAlive: residue.knownDescendantsAlive,
        activeProcessAlive: residue.activeProcessAlive,
        descriptors: residue.descriptors,
        scheduledCallbacks: residue.scheduledCallbacks,
        rootRuntimeKeepAlive: residue.rootRuntimeKeepAlive,
        waitMs: residue.waitMs,
      },
      output: {
        stdoutBytes: encoder.encode(stdout).byteLength,
        stderrBytes: encoder.encode(stderr).byteLength,
        unexpectedShutdownOutput,
      },
      checks,
    };
    if (!allChecksPass(checks)) {
      throw new Error(`${target} ${scenario} run ${index} failed: ${JSON.stringify(checks)}`);
    }
    return result;
  } finally {
    await closeWebSocket(socket);
    if (daemon.process.exitCode === null) daemon.process.kill('SIGKILL');
    await Promise.allSettled([daemon.process.exited, daemon.stdout, daemon.stderr]);
    if (activeProcessPid !== null && pidIsAlive(activeProcessPid)) {
      try {
        process.kill(activeProcessPid, 'SIGKILL');
      } catch {
        // It exited between the liveness probe and cleanup signal.
      }
    }
    rmSync(daemon.home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

async function sourceIdentity() {
  const relativeFiles = ['package.json', 'bun.lock', 'compatibility.json'];
  const glob = new Bun.Glob('**/*');
  for await (const name of glob.scan({
    cwd: path.join(REPO_ROOT, 'src/daemon'),
    onlyFiles: true,
  })) {
    const relative = path.posix.join('src/daemon', name.split(path.sep).join('/'));
    if (relative !== 'src/daemon/fleetd.bundle.mjs') relativeFiles.push(relative);
  }
  relativeFiles.sort();
  const aggregate = new Bun.CryptoHasher('sha256');
  let bytes = 0;
  for (const relative of relativeFiles) {
    const content = await Bun.file(path.join(REPO_ROOT, relative)).bytes();
    const hash = sha256(content);
    bytes += content.byteLength;
    aggregate.update(`${relative}\0${content.byteLength}\0${hash}\n`);
  }
  const entrypoint = await Bun.file(SOURCE_DAEMON).bytes();
  return {
    kind: 'source' as const,
    path: path.relative(REPO_ROOT, SOURCE_DAEMON),
    entrypointBytes: entrypoint.byteLength,
    entrypointSha256: sha256(entrypoint),
    closureFileCount: relativeFiles.length,
    closureBytes: bytes,
    closureSha256: aggregate.digest('hex'),
  };
}

async function bundleIdentity() {
  const content = await Bun.file(BUNDLE_DAEMON).bytes();
  const gzip = Bun.gzipSync(content, { level: 9, library: 'zlib' });
  return {
    kind: 'bundle' as const,
    path: path.relative(REPO_ROOT, BUNDLE_DAEMON),
    bytes: content.byteLength,
    sha256: sha256(content),
    gzipLevel9Bytes: gzip.byteLength,
    gzipLevel9Sha256: sha256(gzip),
  };
}

async function repositoryMetadata() {
  const [commit, branch, status] = await Promise.all([
    runCommand(['git', 'rev-parse', 'HEAD']),
    runCommand(['git', 'branch', '--show-current']),
    runCommand(['git', 'status', '--porcelain=v1', '--untracked-files=all']),
  ]);
  if (commit.exitCode !== 0 || branch.exitCode !== 0 || status.exitCode !== 0) {
    throw new Error('failed to resolve repository identity');
  }
  return {
    commit: commit.stdout.trim(),
    branch: branch.stdout.trim(),
    worktreeDirty: status.stdout.length > 0,
    worktreeStatusSha256: sha256(status.stdout),
    machine: {
      platform: platform(),
      release: release(),
      version: osVersion(),
      architecture: arch(),
      cpu: cpus()[0]?.model ?? 'unknown',
      logicalCpuCount: cpus().length,
      totalMemoryBytes: totalmem(),
    },
  };
}

async function verifyBundleFreshness() {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p4-bundle-freshness-'));
  const rebuiltPath = path.join(scratch, 'fleetd.bundle.mjs');
  try {
    const packageJson: unknown = await Bun.file(path.join(REPO_ROOT, 'package.json')).json();
    const bundleScript =
      isObject(packageJson) && isObject(packageJson['scripts'])
        ? packageJson['scripts']['bundle']
        : null;
    const canonicalScripts = new Set([
      `${BUNDLE_SCRIPT_PREFIX} --banner:js="${BUNDLE_BANNER}"`,
      `${BUNDLE_SCRIPT_PREFIX} --minify-syntax --banner:js="${BUNDLE_BANNER}"`,
    ]);
    if (typeof bundleScript !== 'string' || !canonicalScripts.has(bundleScript)) {
      throw new Error('package.json bundle command no longer matches the P4 freshness policy');
    }
    const startedAt = performance.now();
    await esbuild({
      absWorkingDir: REPO_ROOT,
      entryPoints: ['src/daemon/fleetd.ts'],
      bundle: true,
      platform: 'node',
      format: 'esm',
      outfile: rebuiltPath,
      external: ['bun:sqlite'],
      minifyIdentifiers: true,
      minifySyntax: bundleScript.includes('--minify-syntax'),
      keepNames: true,
      banner: { js: BUNDLE_BANNER },
      logLevel: 'silent',
    });
    const [current, rebuilt] = await Promise.all([
      Bun.file(BUNDLE_DAEMON).bytes(),
      Bun.file(rebuiltPath).bytes(),
    ]);
    const currentSha256 = sha256(current);
    const rebuiltSha256 = sha256(rebuilt);
    return {
      method: 'esbuild API with exact package.json bundle-command policy assertion',
      canonicalScript: 'bun run bundle',
      currentSha256,
      rebuiltSha256,
      currentBytes: current.byteLength,
      rebuiltBytes: rebuilt.byteLength,
      fresh: currentSha256 === rebuiltSha256,
      buildDurationMs: rounded(performance.now() - startedAt),
    };
  } finally {
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

export async function verifyBundleFreshnessWithStableInputs(maxAttempts = 3) {
  const attempts = [];
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    const [sourceBefore, bundleBefore] = await Promise.all([sourceIdentity(), bundleIdentity()]);
    const freshness = await verifyBundleFreshness();
    const [sourceAfter, bundleAfter] = await Promise.all([sourceIdentity(), bundleIdentity()]);
    const inputsStable =
      sourceBefore.closureSha256 === sourceAfter.closureSha256 &&
      bundleBefore.sha256 === bundleAfter.sha256;
    attempts.push({
      attempt,
      inputsStable,
      sourceBeforeSha256: sourceBefore.closureSha256,
      sourceAfterSha256: sourceAfter.closureSha256,
      bundleBeforeSha256: bundleBefore.sha256,
      bundleAfterSha256: bundleAfter.sha256,
      rebuiltSha256: freshness.rebuiltSha256,
    });
    if (inputsStable) {
      return {
        freshness: { ...freshness, inputStabilityAttempts: attempts },
        sourceIdentity: sourceAfter,
        bundleIdentity: bundleAfter,
      };
    }
    await Bun.sleep(100);
  }
  throw new Error(`source or bundle changed during ${maxAttempts} consecutive freshness attempts`);
}

function runCount(scenario: Scenario, config: MeasureConfig): number {
  if (scenario === 'graceful-idle') return config.idleRuns;
  if (scenario === 'mdns-goodbye') return config.mdnsRuns;
  return config.forcedRuns;
}

function scenarioSummary(runs: readonly LifecycleRun[]) {
  return {
    shutdownMs: summarize(runs.map((run) => run.shutdownMs)),
    forcedMs: summarize(runs.flatMap((run) => (run.forcedMs === null ? [] : [run.forcedMs]))),
    allChecksPassed: runs.every((run) => allChecksPass(run.checks)),
    runs,
  };
}

async function measureTarget(target: TargetName, config: MeasureConfig) {
  const warmups: Record<Scenario, number> = {
    'graceful-idle': 0,
    'forced-held-hook': 0,
    'forced-websocket': 0,
    'forced-process': 0,
    'mdns-goodbye': 0,
  };
  const measured = new Map<Scenario, LifecycleRun[]>();
  for (const scenario of ALL_SCENARIOS) {
    for (let index = 0; index < config.warmups; index += 1) {
      await lifecycleRun(target, scenario, index, config);
      warmups[scenario] += 1;
    }
    const runs: LifecycleRun[] = [];
    for (let index = 0; index < runCount(scenario, config); index += 1) {
      runs.push(await lifecycleRun(target, scenario, index, config));
    }
    measured.set(scenario, runs);
  }
  const scenarios = Object.fromEntries(
    ALL_SCENARIOS.map((scenario) => [scenario, scenarioSummary(measured.get(scenario) ?? [])]),
  ) as Record<Scenario, ReturnType<typeof scenarioSummary>>;
  const allRuns = [...measured.values()].flat();
  const forcedRuns = FORCED_SCENARIOS.flatMap((scenario) => measured.get(scenario) ?? []);
  const mdnsRuns = measured.get('mdns-goodbye') ?? [];
  const gates = {
    gracefulIdleP95Below500Ms: scenarios['graceful-idle'].shutdownMs.p95 < 500,
    forcedEveryBelowRootDeadline: forcedRuns.every((run) => run.shutdownMs < config.rootDeadlineMs),
    mdnsEveryBelowSeparateWatchdog: mdnsRuns.every((run) => run.shutdownMs < config.mdnsWatchdogMs),
    allRunChecksPassed: allRuns.every((run) => allChecksPass(run.checks)),
    zeroExternalResidue: allRuns.every(
      (run) =>
        !run.residue.daemonPidAlive &&
        !run.residue.pidFilePresent &&
        run.residue.listenerPortRebindSucceeded &&
        run.residue.knownDescendantsAlive.length === 0 &&
        run.residue.activeProcessAlive !== true &&
        run.residue.descriptors.fds === 0 &&
        run.residue.descriptors.sockets === 0 &&
        !run.residue.scheduledCallbacks.capableOfRunning &&
        !run.residue.rootRuntimeKeepAlive.capableOfRunning,
    ),
  };
  return {
    target,
    daemon: target === 'source' ? await sourceIdentity() : await bundleIdentity(),
    warmups,
    scenarios,
    gates,
    ok: Object.values(gates).every(Boolean),
  };
}

async function main(): Promise<void> {
  const config = readConfig();
  const floor = runtimeFloor();
  if (!floor.exactMatch) {
    throw new Error(
      `P4 evidence requires Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${Bun.version} (${Bun.revision})`,
    );
  }
  for (const target of config.targets) {
    if (!(await Bun.file(targetPath(target)).exists())) {
      throw new Error(`${target} daemon does not exist: ${targetPath(target)}`);
    }
  }
  const representative = config.idleRuns >= 30 && config.forcedRuns >= 5 && config.mdnsRuns >= 5;
  if (!config.smoke && !representative) {
    throw new Error('acceptance evidence requires 30 idle and 5 forced/mDNS runs per target');
  }

  const metadata = await repositoryMetadata();
  const stableFreshness = config.targets.includes('bundle')
    ? await verifyBundleFreshnessWithStableInputs()
    : null;
  const sourceBefore = stableFreshness?.sourceIdentity ?? (await sourceIdentity());
  const bundleBefore = stableFreshness?.bundleIdentity ?? null;
  const bundleFreshness = stableFreshness?.freshness ?? null;
  if (bundleFreshness && !bundleFreshness.fresh) {
    await writeJsonReport({
      schema: 1,
      kind: 'fleetdeck-effect-p4-shutdown',
      ok: false,
      recordedAt: new Date().toISOString(),
      runtimeFloor: floor,
      metadata,
      sourceIdentity: sourceBefore,
      bundleFreshness,
      error: 'refused stale generated daemon bundle',
    });
    process.exitCode = 1;
    return;
  }

  const targets = [];
  for (const target of config.targets) targets.push(await measureTarget(target, config));
  const [sourceAfter, bundleAfter] = await Promise.all([
    sourceIdentity(),
    config.targets.includes('bundle') ? bundleIdentity() : Promise.resolve(null),
  ]);
  const identityStable =
    sourceBefore.closureSha256 === sourceAfter.closureSha256 &&
    (bundleBefore === null || (bundleAfter !== null && bundleBefore.sha256 === bundleAfter.sha256));
  const normalizedConfig = {
    ...config,
    targets: [...config.targets],
    sourceDaemon: path.relative(REPO_ROOT, SOURCE_DAEMON),
    bundleDaemon: path.relative(REPO_ROOT, BUNDLE_DAEMON),
  };
  const violations: string[] = [];
  if (!identityStable) violations.push('source or bundle identity changed during measurement');
  if (!targets.every((target) => target.ok)) violations.push('one or more target gates failed');
  if (!config.smoke && !representative) violations.push('repetition floor was not representative');
  const accepted = !config.smoke && representative && violations.length === 0;
  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p4-shutdown',
    ok: violations.length === 0,
    accepted,
    recordedAt: new Date().toISOString(),
    normalization: {
      paths: 'repository-relative; scratch paths and tokens omitted',
      durations: 'monotonic milliseconds rounded to three decimals',
      distributions: 'nearest-rank p50/p95/p99 over measured runs after warmups',
      residue:
        'PIDs are retained only to prove the exact observed process set is dead; no environment or daemon output text is retained',
    },
    config: normalizedConfig,
    runtimeFloor: floor,
    metadata,
    identity: {
      sourceBefore,
      sourceAfter,
      bundleBefore,
      bundleAfter,
      stableDuringMeasurement: identityStable,
    },
    bundleFreshness,
    repetitionGate: {
      representative,
      required: { idleRunsPerTarget: 30, forcedRunsPerCasePerTarget: 5, mdnsRunsPerTarget: 5 },
      smoke: config.smoke,
    },
    targets,
    violations,
  });
  if (violations.length > 0) process.exitCode = 1;
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    await writeJsonReport({
      schema: 1,
      kind: 'fleetdeck-effect-p4-shutdown',
      ok: false,
      accepted: false,
      recordedAt: new Date().toISOString(),
      runtimeFloor: runtimeFloor(),
      error: error instanceof Error ? error.message : String(error),
    });
    process.exitCode = 1;
  }
}
