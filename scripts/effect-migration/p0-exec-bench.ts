import { mkdtempSync, readFileSync, rmSync, writeFileSync, writeSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { bindExecFileDelegate, execFileP } from '../../src/daemon/exec.ts';
import type { ExecResult } from '../../src/daemon/exec.ts';
import type { RootIngressSupervisorService } from '../../src/daemon/app/services/ingress-supervisor.ts';
import { summarize, writeJsonReport } from './metrics.ts';

const MAX_OUTPUT_BYTES = 1024 * 1024;
const KILL_GRACE_MS = 1_000;
const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
const INTERNAL_CHILD_FLAG = '--p0-exec-bench-child';
const SCRIPT_PATH = fileURLToPath(import.meta.url);

interface BenchConfig {
  shortIterations: number;
  warmupIterations: number;
  scenarioIterations: number;
  timeoutMs: number;
  readyTimeoutMs: number;
  cleanupTimeoutMs: number;
  settlementToleranceMs: number;
}

interface ResourceSnapshot {
  cpu: NodeJS.CpuUsage;
  maxRssRuntimeUnits: number;
  rssBytes: number;
}

interface ChildState {
  descendantPid?: number;
  pid: number;
  ready: true;
  resources: ChildResourceSample;
  termSignals: number;
}

interface ChildResourceSample {
  averageCpuPercentOfOneCore: number;
  cpuIntervalSampleCount: number;
  cpuSystemMicroseconds: number;
  cpuTotalMicroseconds: number;
  cpuUserMicroseconds: number;
  elapsedMs: number;
  maxRssRuntimeUnits: number;
  peakSampledCpuPercentOfOneCore: number | null;
  peakSampledRssBytes: number;
  rssBytes: number;
  sampleCount: number;
}

interface SettlementObservation {
  promiseConstructions: number;
  rejectAttempts: number;
  resolveAttempts: number;
}

interface FixtureIdentity {
  descendantPids: number[];
  pid: number;
}

interface TimedExec {
  durationMs: number;
  observeSettlement: () => SettlementObservation;
  result: ExecResult;
}

function rounded(value: number): number {
  return Math.round(value * 1_000) / 1_000;
}

function optionValue(name: string): string | undefined {
  const equalsPrefix = `${name}=`;
  const equalsOption = Bun.argv.find((argument) => argument.startsWith(equalsPrefix));
  if (equalsOption) return equalsOption.slice(equalsPrefix.length);
  const index = Bun.argv.indexOf(name);
  if (index === -1) return undefined;
  const value = Bun.argv[index + 1];
  if (!value || value.startsWith('--')) throw new Error(`${name} requires an integer`);
  return value;
}

function integerOption(name: string, fallback: number, minimum: number): number {
  const raw = optionValue(name);
  if (raw === undefined) return fallback;
  const parsed = Number(raw);
  if (!Number.isSafeInteger(parsed) || parsed < minimum) {
    throw new Error(`${name} must be an integer >= ${minimum}`);
  }
  return parsed;
}

function readConfig(): BenchConfig {
  return {
    shortIterations: integerOption('--iterations', 1_000, 1),
    warmupIterations: integerOption('--warmup', 100, 0),
    scenarioIterations: integerOption('--scenario-iterations', 5, 1),
    timeoutMs: integerOption('--timeout-ms', 250, 1),
    readyTimeoutMs: integerOption('--ready-timeout-ms', 5_000, 1),
    cleanupTimeoutMs: integerOption('--cleanup-timeout-ms', 5_000, 1),
    settlementToleranceMs: integerOption('--settlement-tolerance-ms', 100, 0),
  };
}

function resourceSnapshot(): ResourceSnapshot {
  return {
    cpu: process.cpuUsage(),
    maxRssRuntimeUnits: process.resourceUsage().maxRSS,
    rssBytes: process.memoryUsage().rss,
  };
}

function resourceDelta(before: ResourceSnapshot, wallMs: number, peakSampledRssBytes: number) {
  const cpu = process.cpuUsage(before.cpu);
  const after = resourceSnapshot();
  const totalCpuMicroseconds = cpu.user + cpu.system;
  return {
    scope: 'benchmark-parent-process',
    wallMs: rounded(wallMs),
    cpuDelta: {
      userMicroseconds: cpu.user,
      systemMicroseconds: cpu.system,
      totalMicroseconds: totalCpuMicroseconds,
      percentOfOneCore: wallMs === 0 ? 0 : rounded(totalCpuMicroseconds / (wallMs * 10)),
    },
    rss: {
      beforeBytes: before.rssBytes,
      afterBytes: after.rssBytes,
      deltaBytes: after.rssBytes - before.rssBytes,
      peakSampledBytes: peakSampledRssBytes,
      maxRssRuntimeUnits: {
        note: 'process.resourceUsage().maxRSS units are runtime-defined; use rss byte fields for comparisons',
        before: before.maxRssRuntimeUnits,
        after: after.maxRssRuntimeUnits,
        delta: after.maxRssRuntimeUnits - before.maxRssRuntimeUnits,
      },
    },
  };
}

async function withResources<T>(
  run: (sampleRss: () => void) => Promise<T>,
): Promise<{ resources: ReturnType<typeof resourceDelta>; value: T }> {
  const before = resourceSnapshot();
  let peakSampledRssBytes = before.rssBytes;
  const sampleRss = (): void => {
    peakSampledRssBytes = Math.max(peakSampledRssBytes, process.memoryUsage().rss);
  };
  const startedAt = performance.now();
  const value = await run(sampleRss);
  sampleRss();
  const wallMs = performance.now() - startedAt;
  return { value, resources: resourceDelta(before, wallMs, peakSampledRssBytes) };
}

function sameResult(left: ExecResult, right: ExecResult): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function resultContract(results: readonly ExecResult[], expected: ExecResult) {
  const observedByJson = new Map<string, ExecResult>();
  for (const result of results) observedByJson.set(JSON.stringify(result), result);
  return {
    expected,
    observedDistinct: [...observedByJson.values()],
    observedKeySets: [
      ...new Set(results.map((result) => JSON.stringify(Object.keys(result).sort()))),
    ].map((keys) => JSON.parse(keys) as string[]),
    allExact: results.every((result) => sameResult(result, expected)),
  };
}

function settlementAccuracy(observedMs: readonly number[], targetMs: number, toleranceMs: number) {
  const signedErrorMs = observedMs.map((value) => value - targetMs);
  const absoluteErrorMs = signedErrorMs.map(Math.abs);
  return {
    targetMs,
    toleranceMs,
    observedMs: summarize(observedMs),
    signedErrorMs: summarize(signedErrorMs),
    absoluteErrorMs: summarize(absoluteErrorMs),
    withinTolerance: absoluteErrorMs.every((value) => value <= toleranceMs),
  };
}

function settlementContract(observations: readonly SettlementObservation[]) {
  const observedByJson = new Map<string, SettlementObservation>();
  for (const observation of observations) {
    observedByJson.set(JSON.stringify(observation), observation);
  }
  return {
    method:
      'temporarily wraps the Promise constructor while execFileP constructs its outer promise, counting executor resolve/reject calls independently of then/await delivery',
    observedDistinct: [...observedByJson.values()],
    allExactlyOnce: observations.every(
      (observation) =>
        observation.promiseConstructions === 1 &&
        observation.resolveAttempts === 1 &&
        observation.rejectAttempts === 0,
    ),
  };
}

function startObservedExec(
  command: string,
  args: readonly string[],
  options: Parameters<typeof execFileP>[2] = {},
): { observe: () => SettlementObservation; promise: Promise<ExecResult> } {
  const NativePromise = globalThis.Promise;
  let promiseConstructions = 0;
  let rejectAttempts = 0;
  let resolveAttempts = 0;
  class InstrumentedPromise<T> extends NativePromise<T> {
    static override get [Symbol.species](): PromiseConstructor {
      return NativePromise;
    }

    constructor(
      executor: (
        resolve: (value: T | PromiseLike<T>) => void,
        reject: (reason?: unknown) => void,
      ) => void,
    ) {
      promiseConstructions += 1;
      super((resolve, reject) => {
        executor(
          (value) => {
            resolveAttempts += 1;
            resolve(value);
          },
          (reason) => {
            rejectAttempts += 1;
            reject(reason);
          },
        );
      });
    }
  }

  let promise: Promise<ExecResult>;
  globalThis.Promise = InstrumentedPromise as PromiseConstructor;
  try {
    // execFileP constructs its one public Promise synchronously. Restore the
    // global before yielding so unrelated asynchronous work is never wrapped.
    promise = execFileP(command, args, options);
  } finally {
    globalThis.Promise = NativePromise;
  }
  return {
    promise,
    observe: () => ({ promiseConstructions, rejectAttempts, resolveAttempts }),
  };
}

async function timedExec(
  command: string,
  args: readonly string[],
  options: Parameters<typeof execFileP>[2] = {},
): Promise<TimedExec> {
  const startedAt = performance.now();
  const execution = startObservedExec(command, args, options);
  const result = await execution.promise;
  return {
    durationMs: performance.now() - startedAt,
    observeSettlement: execution.observe,
    result,
  };
}

function createChildResourceSampler(): () => ChildResourceSample {
  let cpuIntervalSampleCount = 0;
  let lastAt: number | null = null;
  let lastCpuMicroseconds = 0;
  let peakSampledCpuPercentOfOneCore = 0;
  let peakSampledRssBytes = 0;
  let sampleCount = 0;
  return () => {
    const sampledAt = performance.now();
    const cpu = process.cpuUsage();
    const cpuTotalMicroseconds = cpu.user + cpu.system;
    const elapsedMs = process.uptime() * 1_000;
    const intervalMs = lastAt === null ? 0 : sampledAt - lastAt;
    if (lastAt !== null && intervalMs > 0) {
      cpuIntervalSampleCount += 1;
      peakSampledCpuPercentOfOneCore = Math.max(
        peakSampledCpuPercentOfOneCore,
        (cpuTotalMicroseconds - lastCpuMicroseconds) / (intervalMs * 10),
      );
    }
    const rssBytes = process.memoryUsage().rss;
    peakSampledRssBytes = Math.max(peakSampledRssBytes, rssBytes);
    sampleCount += 1;
    lastAt = sampledAt;
    lastCpuMicroseconds = cpuTotalMicroseconds;
    return {
      averageCpuPercentOfOneCore:
        elapsedMs === 0 ? 0 : rounded(cpuTotalMicroseconds / (elapsedMs * 10)),
      cpuIntervalSampleCount,
      cpuSystemMicroseconds: cpu.system,
      cpuTotalMicroseconds,
      cpuUserMicroseconds: cpu.user,
      elapsedMs: rounded(elapsedMs),
      maxRssRuntimeUnits: process.resourceUsage().maxRSS,
      peakSampledCpuPercentOfOneCore:
        cpuIntervalSampleCount === 0 ? null : rounded(peakSampledCpuPercentOfOneCore),
      peakSampledRssBytes,
      rssBytes,
      sampleCount,
    };
  };
}

function childResourceSummary(states: readonly ChildState[]) {
  const cpuPeakSamples = states.flatMap((state) => {
    const peak = state.resources.peakSampledCpuPercentOfOneCore;
    return peak === null ? [] : [peak];
  });
  return {
    available: true,
    platform: process.platform,
    source:
      'each fixture process self-samples process.cpuUsage(), process.memoryUsage().rss, and process.resourceUsage().maxRSS',
    samplingNote:
      'CPU percentages include lightweight fixture state publication; maxRSS units are runtime-defined',
    processCount: states.length,
    peakSampledRssBytes: summarize(states.map((state) => state.resources.peakSampledRssBytes)),
    peakSampledCpuPercentOfOneCore: {
      available: cpuPeakSamples.length > 0,
      distribution: summarize(cpuPeakSamples),
      unavailableProcessPids: states
        .filter((state) => state.resources.peakSampledCpuPercentOfOneCore === null)
        .map((state) => state.pid),
      unavailableReason:
        cpuPeakSamples.length === states.length
          ? null
          : 'process exited before a second self-sample could form a CPU interval',
    },
    cpuTotalMicroseconds: {
      distribution: summarize(states.map((state) => state.resources.cpuTotalMicroseconds)),
      sum: states.reduce((sum, state) => sum + state.resources.cpuTotalMicroseconds, 0),
    },
    processes: states.map((state) => ({ pid: state.pid, resources: state.resources })),
  };
}

function childResourcesUnavailable(reason: string) {
  return {
    available: false,
    platform: process.platform,
    reason,
  };
}

function parseFixtureIdentity(text: string): FixtureIdentity {
  const value = JSON.parse(text) as Partial<FixtureIdentity>;
  if (!Number.isSafeInteger(value.pid) || (value.pid ?? 0) < 1) {
    throw new Error('invalid fixture identity pid');
  }
  if (
    !Array.isArray(value.descendantPids) ||
    !value.descendantPids.every((pid) => Number.isSafeInteger(pid) && pid > 0)
  ) {
    throw new Error('invalid fixture descendant identities');
  }
  return { pid: value.pid as number, descendantPids: [...value.descendantPids] };
}

function publishFixtureIdentity(
  identityPath: string,
  descendantPids: readonly number[] = [],
): void {
  writeFileSync(
    identityPath,
    JSON.stringify({
      pid: process.pid,
      descendantPids: [...descendantPids],
    } satisfies FixtureIdentity),
  );
}

class FixtureOwnership {
  readonly groups = new Set<number>();
  readonly pids = new Set<number>();
  readonly #expected = new Map<string, { groupLeader: boolean }>();

  expect(identityPath: string, groupLeader = false): void {
    this.#expected.set(identityPath, { groupLeader });
  }

  capture(identityPath: string): FixtureIdentity | null {
    const expected = this.#expected.get(identityPath);
    if (!expected) return null;
    try {
      const identity = parseFixtureIdentity(readFileSync(identityPath, 'utf8'));
      this.pids.add(identity.pid);
      for (const descendantPid of identity.descendantPids) this.pids.add(descendantPid);
      if (expected.groupLeader) this.groups.add(identity.pid);
      return identity;
    } catch {
      return null;
    }
  }

  async wait(identityPath: string, timeoutMs: number): Promise<FixtureIdentity> {
    const deadline = performance.now() + timeoutMs;
    while (performance.now() < deadline) {
      const identity = this.capture(identityPath);
      if (identity) return identity;
      await Bun.sleep(5);
    }
    throw new Error(
      `fixture did not publish ownership identity ${path.basename(identityPath)} within ${timeoutMs}ms`,
    );
  }

  retire(
    identityPaths: readonly string[],
    pids: readonly number[],
    groupLeaderPids: readonly number[] = [],
  ): void {
    for (const identityPath of identityPaths) this.#expected.delete(identityPath);
    for (const pid of pids) this.pids.delete(pid);
    for (const pid of groupLeaderPids) this.groups.delete(pid);
  }

  async cleanup(timeoutMs: number) {
    const startedAt = performance.now();
    const deadline = startedAt + timeoutMs;
    let signalPasses = 0;
    while (true) {
      for (const identityPath of this.#expected.keys()) this.capture(identityPath);
      signalPasses += 1;
      if (process.platform !== 'win32') {
        for (const leaderPid of this.groups) {
          if (!processGroupAlive(leaderPid)) continue;
          try {
            process.kill(-leaderPid, 'SIGKILL');
          } catch {
            // The group exited between the liveness probe and signal.
          }
        }
      }
      for (const pid of this.pids) {
        if (!processAlive(pid)) continue;
        try {
          process.kill(pid, 'SIGKILL');
        } catch {
          // The process exited between the liveness probe and signal.
        }
      }

      for (const identityPath of this.#expected.keys()) this.capture(identityPath);
      const alivePids = [...this.pids].filter(processAlive);
      const aliveGroups =
        process.platform === 'win32' ? [] : [...this.groups].filter(processGroupAlive);
      const unresolvedIdentityPaths = [...this.#expected.keys()]
        .filter((identityPath) => this.capture(identityPath) === null)
        .map((identityPath) => path.basename(identityPath));
      if (
        alivePids.length === 0 &&
        aliveGroups.length === 0 &&
        unresolvedIdentityPaths.length === 0
      ) {
        return {
          bounded: true,
          timeoutMs,
          waitMs: rounded(performance.now() - startedAt),
          signalPasses,
          discoveredPids: [...this.pids],
          discoveredGroupLeaders: [...this.groups],
          residue: { alivePids, aliveGroups, unresolvedIdentityPaths },
          assertionPassed: true,
        };
      }
      if (performance.now() >= deadline) {
        return {
          bounded: true,
          timeoutMs,
          waitMs: rounded(performance.now() - startedAt),
          signalPasses,
          discoveredPids: [...this.pids],
          discoveredGroupLeaders: [...this.groups],
          residue: { alivePids, aliveGroups, unresolvedIdentityPaths },
          assertionPassed: false,
        };
      }
      await Bun.sleep(10);
    }
  }
}

function writeRepeated(fd: number, totalBytes: number, byte: number): void {
  const chunk = Buffer.alloc(Math.min(64 * 1024, totalBytes), byte);
  let remaining = totalBytes;
  while (remaining > 0) {
    const length = Math.min(chunk.byteLength, remaining);
    try {
      writeSync(fd, chunk, 0, length);
    } catch {
      return;
    }
    remaining -= length;
  }
}

function publishState(statePath: string, state: ChildState): void {
  writeFileSync(statePath, JSON.stringify(state));
}

async function holdTermResistant(
  statePath: string,
  extra: Pick<ChildState, 'descendantPid'> | Record<string, never>,
): Promise<never> {
  let termSignals = 0;
  const sampleResources = createChildResourceSampler();
  const state = (): ChildState => ({
    ...extra,
    pid: process.pid,
    ready: true,
    resources: sampleResources(),
    termSignals,
  });
  process.on('SIGTERM', () => {
    termSignals += 1;
    publishState(statePath, state());
  });
  publishState(statePath, state());
  setInterval(() => publishState(statePath, state()), 50);
  return await new Promise<never>(() => undefined);
}

function publishOneShotState(
  statePath: string,
  sampleResources: () => ChildResourceSample,
  descendantPid?: number,
): void {
  const state: ChildState = {
    pid: process.pid,
    ready: true,
    resources: sampleResources(),
    termSignals: 0,
  };
  if (descendantPid !== undefined) state.descendantPid = descendantPid;
  publishState(statePath, state);
}

async function runInternalChild(args: readonly string[]): Promise<void> {
  const identityPath = args[0];
  const mode = args[1];
  if (!identityPath) throw new Error('internal fixture requires an ownership identity path');
  // Ownership is intentionally published before richer readiness/resource state
  // or any descendant spawn, so the harness can clean a partially initialized
  // fixture even when later JSON parsing or setup fails.
  publishFixtureIdentity(identityPath);
  if (mode === 'output') {
    const stdoutBytes = Number(args[2]);
    const stderrBytes = Number(args[3]);
    const statePath = args[4];
    if (!Number.isSafeInteger(stdoutBytes) || !Number.isSafeInteger(stderrBytes) || !statePath) {
      throw new Error('output child requires byte counts and a state path');
    }
    const sampleResources = createChildResourceSampler();
    publishOneShotState(statePath, sampleResources);
    writeRepeated(1, stdoutBytes, 'o'.charCodeAt(0));
    writeRepeated(2, stderrBytes, 'e'.charCodeAt(0));
    publishOneShotState(statePath, sampleResources);
    return;
  }
  if (mode === 'linger') {
    const statePath = args[2];
    if (!statePath) throw new Error('linger child requires a state path');
    await holdTermResistant(statePath, {});
  }
  if (mode === 'tree-leaf') {
    const statePath = args[2];
    if (!statePath) throw new Error('tree leaf requires a state path');
    await holdTermResistant(statePath, {});
  }
  if (mode === 'tree-root') {
    const rootStatePath = args[2];
    const leafStatePath = args[3];
    const leafIdentityPath = args[4];
    if (!rootStatePath || !leafStatePath || !leafIdentityPath) {
      throw new Error('tree root requires root/leaf state and leaf identity paths');
    }
    const descendant = Bun.spawn(
      [
        process.execPath,
        SCRIPT_PATH,
        INTERNAL_CHILD_FLAG,
        leafIdentityPath,
        'tree-leaf',
        leafStatePath,
      ],
      { stdin: 'ignore', stdout: 'ignore', stderr: 'ignore' },
    );
    publishFixtureIdentity(identityPath, [descendant.pid]);
    await holdTermResistant(rootStatePath, { descendantPid: descendant.pid });
  }
  if (mode === 'open-pipe-root') {
    const rootStatePath = args[2];
    const leafStatePath = args[3];
    const leafIdentityPath = args[4];
    if (!rootStatePath || !leafStatePath || !leafIdentityPath) {
      throw new Error('open-pipe root requires root/leaf state and leaf identity paths');
    }
    const sampleResources = createChildResourceSampler();
    const descendant = Bun.spawn(
      [
        process.execPath,
        SCRIPT_PATH,
        INTERNAL_CHILD_FLAG,
        leafIdentityPath,
        'tree-leaf',
        leafStatePath,
      ],
      { stdin: 'ignore', stdout: 'inherit', stderr: 'inherit' },
    );
    publishFixtureIdentity(identityPath, [descendant.pid]);
    descendant.unref();
    publishOneShotState(rootStatePath, sampleResources, descendant.pid);
    return;
  }
  throw new Error(`unknown internal child mode: ${mode ?? '(missing)'}`);
}

function parseChildState(text: string): ChildState {
  const value = JSON.parse(text) as Partial<ChildState>;
  if (
    value.ready !== true ||
    !Number.isSafeInteger(value.pid) ||
    value.resources === null ||
    typeof value.resources !== 'object' ||
    typeof value.termSignals !== 'number'
  ) {
    throw new Error('invalid child state');
  }
  const state: ChildState = {
    pid: value.pid as number,
    ready: true,
    resources: value.resources as ChildResourceSample,
    termSignals: value.termSignals,
  };
  if (Number.isSafeInteger(value.descendantPid)) {
    state.descendantPid = value.descendantPid as number;
  }
  return state;
}

function readChildState(statePath: string): ChildState {
  return parseChildState(readFileSync(statePath, 'utf8'));
}

async function waitForChildState(statePath: string, timeoutMs: number): Promise<ChildState> {
  const deadline = performance.now() + timeoutMs;
  let lastError: unknown;
  while (performance.now() < deadline) {
    try {
      return readChildState(statePath);
    } catch (error) {
      lastError = error;
    }
    await Bun.sleep(5);
  }
  throw new Error(
    `child did not publish readiness within ${timeoutMs}ms${
      lastError instanceof Error ? `: ${lastError.message}` : ''
    }`,
  );
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function processGroupAlive(leaderPid: number): boolean {
  if (process.platform === 'win32') return false;
  try {
    process.kill(-leaderPid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitForProcessExit(pid: number, timeoutMs: number): Promise<boolean> {
  const deadline = performance.now() + timeoutMs;
  while (performance.now() < deadline) {
    if (!processAlive(pid)) return true;
    await Bun.sleep(5);
  }
  return !processAlive(pid);
}

async function waitForCleanup(pids: readonly number[], timeoutMs: number, groupLeaderPid?: number) {
  const startedAt = performance.now();
  const deadline = startedAt + timeoutMs;
  while (performance.now() < deadline) {
    const anyProcessAlive = pids.some(processAlive);
    const groupAlive = groupLeaderPid === undefined ? false : processGroupAlive(groupLeaderPid);
    if (!anyProcessAlive && !groupAlive) break;
    await Bun.sleep(10);
  }
  const processes = pids.map((pid) => ({ pid, alive: processAlive(pid) }));
  const groupAlive = groupLeaderPid === undefined ? false : processGroupAlive(groupLeaderPid);
  return {
    waitMs: rounded(performance.now() - startedAt),
    timeoutMs,
    processes,
    groupLeaderPid: groupLeaderPid ?? null,
    groupAlive,
    allGone: processes.every((processState) => !processState.alive) && !groupAlive,
  };
}

async function runShortArgv(config: BenchConfig) {
  const expected: ExecResult = { ok: true, out: '' };
  for (let index = 0; index < config.warmupIterations; index += 1) {
    const result = await execFileP(process.execPath, ['-e', '0'], { timeout: 5_000 });
    if (!sameResult(result, expected)) {
      throw new Error(`short-command warmup failed: ${JSON.stringify(result)}`);
    }
  }
  const measured = await withResources(async (sampleRss) => {
    const durations: number[] = [];
    const results: ExecResult[] = [];
    const settlements: SettlementObservation[] = [];
    for (let index = 0; index < config.shortIterations; index += 1) {
      const execution = await timedExec(process.execPath, ['-e', '0'], { timeout: 5_000 });
      durations.push(execution.durationMs);
      results.push(execution.result);
      settlements.push(execution.observeSettlement());
      sampleRss();
    }
    return { durations, results, settlements };
  });
  const settlement = settlementContract(measured.value.settlements);
  return {
    command: ['<bun>', '-e', '0'],
    directArgv: true,
    shell: false,
    warmupIterations: config.warmupIterations,
    measuredIterations: config.shortIterations,
    latencyMs: summarize(measured.value.durations),
    resultContract: resultContract(measured.value.results, expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    childResources: childResourcesUnavailable(
      'the compatibility facade does not expose a pid or exited-child rusage, and changing the short command to a reporting fixture would invalidate this startup workload',
    ),
    resources: measured.resources,
  };
}

async function runOutputCase(
  config: BenchConfig,
  scratch: string,
  label: string,
  ownership: FixtureOwnership,
  stderrBytes: number,
  expected: ExecResult,
) {
  const measured = await withResources(async (sampleRss) => {
    const durations: number[] = [];
    const results: ExecResult[] = [];
    const settlementObservers: (() => SettlementObservation)[] = [];
    const states: { identityPath: string; path: string; ready: ChildState }[] = [];
    for (let index = 0; index < config.scenarioIterations; index += 1) {
      const statePath = path.join(scratch, `${label}-${index}.json`);
      const identityPath = path.join(scratch, `${label}-${index}.identity.json`);
      ownership.expect(identityPath);
      const execution = await timedExec(
        process.execPath,
        [
          SCRIPT_PATH,
          INTERNAL_CHILD_FLAG,
          identityPath,
          'output',
          '1',
          String(stderrBytes),
          statePath,
        ],
        { timeout: 5_000 },
      );
      await ownership.wait(identityPath, config.readyTimeoutMs);
      const ready = await waitForChildState(statePath, config.readyTimeoutMs);
      durations.push(execution.durationMs);
      results.push(execution.result);
      settlementObservers.push(execution.observeSettlement);
      states.push({ identityPath, path: statePath, ready });
      sampleRss();
    }
    const cleanup = await waitForCleanup(
      states.map(({ ready }) => ready.pid),
      config.cleanupTimeoutMs + KILL_GRACE_MS,
    );
    if (cleanup.allGone) {
      ownership.retire(
        states.map(({ identityPath }) => identityPath),
        states.map(({ ready }) => ready.pid),
      );
    }
    const finalStates = states.map(({ path: statePath, ready }) => {
      try {
        return readChildState(statePath);
      } catch {
        return ready;
      }
    });
    return {
      cleanup,
      durations,
      finalStates,
      results,
      settlements: settlementObservers.map((observe) => observe()),
    };
  });
  const settlement = settlementContract(measured.value.settlements);
  return {
    measuredIterations: config.scenarioIterations,
    stdoutBytes: 1,
    stderrBytes,
    combinedBytes: stderrBytes + 1,
    latencyMs: summarize(measured.value.durations),
    resultContract: resultContract(measured.value.results, expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    childResources: childResourceSummary(measured.value.finalStates),
    cleanup: measured.value.cleanup,
    resources: measured.resources,
  };
}

async function runTimeoutCase(config: BenchConfig, scratch: string, ownership: FixtureOwnership) {
  const expected: ExecResult = {
    ok: false,
    code: 'ETIMEDOUT',
    err: `timed out after ${config.timeoutMs}ms`,
  };
  const measured = await withResources(async (sampleRss) => {
    const durations: number[] = [];
    const results: ExecResult[] = [];
    const states: {
      observeSettlement: () => SettlementObservation;
      identityPath: string;
      path: string;
      ready: ChildState;
    }[] = [];
    for (let index = 0; index < config.scenarioIterations; index += 1) {
      const statePath = path.join(scratch, `timeout-${index}.json`);
      const identityPath = path.join(scratch, `timeout-${index}.identity.json`);
      ownership.expect(identityPath);
      const startedAt = performance.now();
      const observed = startObservedExec(
        process.execPath,
        [SCRIPT_PATH, INTERNAL_CHILD_FLAG, identityPath, 'linger', statePath],
        { timeout: config.timeoutMs },
      );
      const execution = observed.promise.then((result) => {
        return { durationMs: performance.now() - startedAt, result };
      });
      const [, ready] = await Promise.all([
        ownership.wait(identityPath, config.readyTimeoutMs),
        waitForChildState(statePath, config.readyTimeoutMs),
      ]);
      const settled = await execution;
      durations.push(settled.durationMs);
      results.push(settled.result);
      states.push({
        observeSettlement: observed.observe,
        identityPath,
        path: statePath,
        ready,
      });
      sampleRss();
    }
    const cleanup = await waitForCleanup(
      states.map(({ ready }) => ready.pid),
      config.cleanupTimeoutMs + KILL_GRACE_MS,
    );
    if (cleanup.allGone) {
      ownership.retire(
        states.map(({ identityPath }) => identityPath),
        states.map(({ ready }) => ready.pid),
      );
    }
    const finalStates = states.map(({ path: statePath, ready }) => {
      try {
        return readChildState(statePath);
      } catch {
        return ready;
      }
    });
    sampleRss();
    return {
      cleanup,
      durations,
      finalStates,
      results,
      settlements: states.map(({ observeSettlement }) => observeSettlement()),
    };
  });
  const settlement = settlementContract(measured.value.settlements);
  return {
    measuredIterations: config.scenarioIterations,
    timeoutMs: config.timeoutMs,
    latencyMs: summarize(measured.value.durations),
    settlementAccuracy: settlementAccuracy(
      measured.value.durations,
      config.timeoutMs,
      config.settlementToleranceMs,
    ),
    resultContract: resultContract(measured.value.results, expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    termResistanceObserved: measured.value.finalStates.every((state) => state.termSignals >= 1),
    childStates: measured.value.finalStates,
    childResources: childResourceSummary(measured.value.finalStates),
    cleanup: measured.value.cleanup,
    resources: measured.resources,
  };
}

async function runPreAbortCase(config: BenchConfig) {
  const expected: ExecResult = { ok: false, code: 'ECANCELED', err: 'cancelled' };
  const measured = await withResources(async (sampleRss) => {
    const durations: number[] = [];
    const results: ExecResult[] = [];
    const settlementObservers: (() => SettlementObservation)[] = [];
    for (let index = 0; index < config.scenarioIterations; index += 1) {
      const controller = new AbortController();
      controller.abort();
      const execution = await timedExec(process.execPath, ['-e', '0'], {
        signal: controller.signal,
        timeout: 5_000,
      });
      durations.push(execution.durationMs);
      results.push(execution.result);
      settlementObservers.push(execution.observeSettlement);
      sampleRss();
    }
    await Bun.sleep(25);
    return {
      durations,
      results,
      settlements: settlementObservers.map((observe) => observe()),
    };
  });
  const settlement = settlementContract(measured.value.settlements);
  return {
    measuredIterations: config.scenarioIterations,
    signalStateBeforeCall: 'aborted',
    latencyMs: summarize(measured.value.durations),
    settlementAccuracy: settlementAccuracy(
      measured.value.durations,
      0,
      config.settlementToleranceMs,
    ),
    resultContract: resultContract(measured.value.results, expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    childResources: childResourcesUnavailable(
      'the already-aborted signal may terminate the subprocess before fixture code can publish a pid or self-resource sample',
    ),
    resources: measured.resources,
  };
}

async function runCancellationCase(
  config: BenchConfig,
  scratch: string,
  ownership: FixtureOwnership,
) {
  const expected: ExecResult = { ok: false, code: 'ECANCELED', err: 'cancelled' };
  const measured = await withResources(async (sampleRss) => {
    const abortToSettlementMs: number[] = [];
    const startToReadyMs: number[] = [];
    const results: ExecResult[] = [];
    const states: {
      observeSettlement: () => SettlementObservation;
      identityPath: string;
      path: string;
      ready: ChildState;
    }[] = [];
    for (let index = 0; index < config.scenarioIterations; index += 1) {
      const statePath = path.join(scratch, `cancel-${index}.json`);
      const identityPath = path.join(scratch, `cancel-${index}.identity.json`);
      ownership.expect(identityPath);
      const controller = new AbortController();
      const startedAt = performance.now();
      const observed = startObservedExec(
        process.execPath,
        [SCRIPT_PATH, INTERNAL_CHILD_FLAG, identityPath, 'linger', statePath],
        {
          signal: controller.signal,
          timeout: config.readyTimeoutMs + config.cleanupTimeoutMs + KILL_GRACE_MS,
        },
      );
      const execution = observed.promise.then((result) => {
        return { result, settledAt: performance.now() };
      });
      const [, ready] = await Promise.all([
        ownership.wait(identityPath, config.readyTimeoutMs),
        waitForChildState(statePath, config.readyTimeoutMs),
      ]);
      startToReadyMs.push(performance.now() - startedAt);
      const abortedAt = performance.now();
      controller.abort();
      const settled = await execution;
      abortToSettlementMs.push(settled.settledAt - abortedAt);
      results.push(settled.result);
      states.push({
        observeSettlement: observed.observe,
        identityPath,
        path: statePath,
        ready,
      });
      sampleRss();
    }
    const cleanup = await waitForCleanup(
      states.map(({ ready }) => ready.pid),
      config.cleanupTimeoutMs + KILL_GRACE_MS,
    );
    if (cleanup.allGone) {
      ownership.retire(
        states.map(({ identityPath }) => identityPath),
        states.map(({ ready }) => ready.pid),
      );
    }
    const finalStates = states.map(({ path: statePath, ready }) => {
      try {
        return readChildState(statePath);
      } catch {
        return ready;
      }
    });
    sampleRss();
    return {
      abortToSettlementMs,
      cleanup,
      finalStates,
      results,
      settlements: states.map(({ observeSettlement }) => observeSettlement()),
      startToReadyMs,
    };
  });
  const settlement = settlementContract(measured.value.settlements);
  return {
    measuredIterations: config.scenarioIterations,
    startToReadyMs: summarize(measured.value.startToReadyMs),
    abortToSettlementMs: summarize(measured.value.abortToSettlementMs),
    settlementAccuracy: settlementAccuracy(
      measured.value.abortToSettlementMs,
      0,
      config.settlementToleranceMs,
    ),
    resultContract: resultContract(measured.value.results, expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    termResistanceObserved: measured.value.finalStates.every((state) => state.termSignals >= 1),
    childStates: measured.value.finalStates,
    childResources: childResourceSummary(measured.value.finalStates),
    cleanup: measured.value.cleanup,
    resources: measured.resources,
  };
}

async function runDescendantCleanupCase(
  config: BenchConfig,
  scratch: string,
  ownership: FixtureOwnership,
) {
  if (process.platform === 'win32') {
    return {
      skipped: true as const,
      reason: 'POSIX process groups are unavailable on Windows',
      childResources: childResourcesUnavailable(
        'POSIX process-group descendant fixture is unavailable on Windows',
      ),
    };
  }
  const expected: ExecResult = { ok: false, code: 'ECANCELED', err: 'cancelled' };
  const measured = await withResources(async (sampleRss) => {
    const rootStatePath = path.join(scratch, 'tree-root.json');
    const leafStatePath = path.join(scratch, 'tree-leaf.json');
    const rootIdentityPath = path.join(scratch, 'tree-root.identity.json');
    const leafIdentityPath = path.join(scratch, 'tree-leaf.identity.json');
    ownership.expect(rootIdentityPath, true);
    ownership.expect(leafIdentityPath);
    const controller = new AbortController();
    const startedAt = performance.now();
    const observed = startObservedExec(
      process.execPath,
      [
        SCRIPT_PATH,
        INTERNAL_CHILD_FLAG,
        rootIdentityPath,
        'tree-root',
        rootStatePath,
        leafStatePath,
        leafIdentityPath,
      ],
      {
        killTree: true,
        signal: controller.signal,
        timeout: config.readyTimeoutMs + config.cleanupTimeoutMs + KILL_GRACE_MS,
      },
    );
    const execution = observed.promise.then((result) => {
      return { result, settledAt: performance.now() };
    });
    const [, , rootReady, leafReady] = await Promise.all([
      ownership.wait(rootIdentityPath, config.readyTimeoutMs),
      ownership.wait(leafIdentityPath, config.readyTimeoutMs),
      waitForChildState(rootStatePath, config.readyTimeoutMs),
      waitForChildState(leafStatePath, config.readyTimeoutMs),
    ]);
    const readyAt = performance.now();
    const abortedAt = performance.now();
    controller.abort();
    const settled = await execution;
    const cleanup = await waitForCleanup(
      [rootReady.pid, leafReady.pid],
      config.cleanupTimeoutMs + KILL_GRACE_MS,
      rootReady.pid,
    );
    if (cleanup.allGone) {
      ownership.retire(
        [rootIdentityPath, leafIdentityPath],
        [rootReady.pid, leafReady.pid],
        [rootReady.pid],
      );
    }
    let rootFinal = rootReady;
    let leafFinal = leafReady;
    try {
      rootFinal = readChildState(rootStatePath);
    } catch {
      // The ready state is still enough to identify and clean the owned process.
    }
    try {
      leafFinal = readChildState(leafStatePath);
    } catch {
      // The ready state is still enough to identify and clean the owned process.
    }
    sampleRss();
    return {
      abortToSettlementMs: settled.settledAt - abortedAt,
      cleanup,
      leafFinal,
      result: settled.result,
      rootFinal,
      settlement: observed.observe(),
      startToReadyMs: readyAt - startedAt,
      rootRecordedDescendant: rootReady.descendantPid,
    };
  });
  const settlement = settlementContract([measured.value.settlement]);
  return {
    skipped: false as const,
    killTree: true,
    directArgv: true,
    shell: false,
    startToReadyMs: rounded(measured.value.startToReadyMs),
    abortToSettlementMs: rounded(measured.value.abortToSettlementMs),
    settlementAccuracy: settlementAccuracy(
      [measured.value.abortToSettlementMs],
      0,
      config.settlementToleranceMs,
    ),
    resultContract: resultContract([measured.value.result], expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    rootState: measured.value.rootFinal,
    descendantState: measured.value.leafFinal,
    rootRecordedDescendantMatches:
      measured.value.rootRecordedDescendant === measured.value.leafFinal.pid,
    termResistanceObserved:
      measured.value.rootFinal.termSignals >= 1 && measured.value.leafFinal.termSignals >= 1,
    childResources: childResourceSummary([measured.value.rootFinal, measured.value.leafFinal]),
    cleanup: measured.value.cleanup,
    noDescendantsRemain: measured.value.cleanup.allGone,
    resources: measured.resources,
  };
}

async function runOpenInheritedPipeCase(
  config: BenchConfig,
  scratch: string,
  ownership: FixtureOwnership,
) {
  if (process.platform === 'win32') {
    return {
      skipped: true as const,
      reason:
        'the baseline can only target a root-exited descendant through its POSIX process group',
      childResources: childResourcesUnavailable(
        'POSIX inherited-pipe process-group fixture is unavailable on Windows',
      ),
    };
  }
  const expected: ExecResult = {
    ok: false,
    code: 'ETIMEDOUT',
    err: `timed out after ${config.timeoutMs}ms`,
  };
  const measured = await withResources(async (sampleRss) => {
    const rootStatePath = path.join(scratch, 'open-pipe-root.json');
    const leafStatePath = path.join(scratch, 'open-pipe-leaf.json');
    const rootIdentityPath = path.join(scratch, 'open-pipe-root.identity.json');
    const leafIdentityPath = path.join(scratch, 'open-pipe-leaf.identity.json');
    ownership.expect(rootIdentityPath, true);
    ownership.expect(leafIdentityPath);
    const startedAt = performance.now();
    const observed = startObservedExec(
      process.execPath,
      [
        SCRIPT_PATH,
        INTERNAL_CHILD_FLAG,
        rootIdentityPath,
        'open-pipe-root',
        rootStatePath,
        leafStatePath,
        leafIdentityPath,
      ],
      { killTree: true, timeout: config.timeoutMs },
    );
    let publicSettled = false;
    const execution = observed.promise.then((result) => {
      publicSettled = true;
      return { durationMs: performance.now() - startedAt, result };
    });
    const [, , rootReady, leafReady] = await Promise.all([
      ownership.wait(rootIdentityPath, config.readyTimeoutMs),
      ownership.wait(leafIdentityPath, config.readyTimeoutMs),
      waitForChildState(rootStatePath, config.readyTimeoutMs),
      waitForChildState(leafStatePath, config.readyTimeoutMs),
    ]);
    const rootExitedBeforeSettlement = await waitForProcessExit(
      rootReady.pid,
      config.readyTimeoutMs,
    );
    const promisePendingAfterRootExit = !publicSettled;
    const descendantAliveWhilePromisePending = processAlive(leafReady.pid);
    const groupAliveAfterRootExit = processGroupAlive(rootReady.pid);
    const settled = await execution;
    const cleanup = await waitForCleanup(
      [rootReady.pid, leafReady.pid],
      config.cleanupTimeoutMs + KILL_GRACE_MS,
      rootReady.pid,
    );
    if (cleanup.allGone) {
      ownership.retire(
        [rootIdentityPath, leafIdentityPath],
        [rootReady.pid, leafReady.pid],
        [rootReady.pid],
      );
    }
    let rootFinal = rootReady;
    let leafFinal = leafReady;
    try {
      rootFinal = readChildState(rootStatePath);
    } catch {
      // The initial state remains valid resource evidence for the exited root.
    }
    try {
      leafFinal = readChildState(leafStatePath);
    } catch {
      // The ready state remains valid identity/resource evidence for cleanup.
    }
    sampleRss();
    return {
      cleanup,
      descendantAliveWhilePromisePending,
      durationMs: settled.durationMs,
      groupAliveAfterRootExit,
      leafFinal,
      promisePendingAfterRootExit,
      result: settled.result,
      rootExitedBeforeSettlement,
      rootFinal,
      rootRecordedDescendant: rootReady.descendantPid,
      settlement: observed.observe(),
    };
  });
  const settlement = settlementContract([measured.value.settlement]);
  return {
    skipped: false as const,
    killTree: true,
    directArgv: true,
    shell: false,
    inheritedPipeOwners: ['stdout', 'stderr'],
    fixtureSequence:
      'root spawns an unrefed TERM-resistant descendant inheriting both captured pipes, publishes its pid, then exits before the descendant',
    latencyMs: summarize([measured.value.durationMs]),
    settlementAccuracy: settlementAccuracy(
      [measured.value.durationMs],
      config.timeoutMs,
      config.settlementToleranceMs,
    ),
    resultContract: resultContract([measured.value.result], expected),
    settlement,
    settlementExactlyOnce: settlement.allExactlyOnce,
    rootExitedBeforeSettlement: measured.value.rootExitedBeforeSettlement,
    promisePendingAfterRootExit: measured.value.promisePendingAfterRootExit,
    descendantAliveWhilePromisePending: measured.value.descendantAliveWhilePromisePending,
    groupAliveAfterRootExit: measured.value.groupAliveAfterRootExit,
    rootState: measured.value.rootFinal,
    descendantState: measured.value.leafFinal,
    rootRecordedDescendantMatches:
      measured.value.rootRecordedDescendant === measured.value.leafFinal.pid,
    termResistanceObserved: measured.value.leafFinal.termSignals >= 1,
    childResources: childResourceSummary([measured.value.rootFinal, measured.value.leafFinal]),
    cleanup: measured.value.cleanup,
    noDescendantsRemain: measured.value.cleanup.allGone,
    resources: measured.resources,
  };
}

function runtimeFloorEvidence() {
  return {
    expected: {
      bun: EXPECTED_BUN_VERSION,
      revision: EXPECTED_BUN_REVISION,
    },
    observed: {
      bun: Bun.version,
      revision: Bun.revision,
    },
    exact: Bun.version === EXPECTED_BUN_VERSION && Bun.revision === EXPECTED_BUN_REVISION,
  };
}

async function main(): Promise<void> {
  const config = readConfig();
  const runtimeFloor = runtimeFloorEvidence();
  if (!runtimeFloor.exact) {
    await writeJsonReport({
      schema: 1,
      kind: 'fleetdeck-effect-p0-exec-bench',
      recordedAt: new Date().toISOString(),
      refused: true,
      reason: `P0 exec benchmark requires Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${Bun.version} (${Bun.revision})`,
      runtimeFloor,
      config,
      verification: { checks: { runtimeFloorExact: false }, passed: false },
    });
    process.exitCode = 1;
    return;
  }
  // Internal fixture children recursively execute this file. Load Effect and
  // the candidate root Context only in the parent benchmark so child readiness
  // remains comparable to P0 instead of paying kernel initialization first.
  const [Context, Effect, Exit, Layer, Scope, facadeModule, ingressModule, processModule] =
    await Promise.all([
      import('effect/Context'),
      import('effect/Effect'),
      import('effect/Exit'),
      import('effect/Layer'),
      import('effect/Scope'),
      import('../../src/daemon/app/legacy-process-facade.ts'),
      import('../../src/daemon/platform/bun/ingress-supervisor-live.ts'),
      import('../../src/daemon/platform/bun/process-runner-live.ts'),
    ]);
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p0-exec-bench-'));
  const rootScope = Scope.makeUnsafe('sequential');
  const runBenchmarkPromise = Effect.runPromiseWith(Context.empty());
  let ingress: RootIngressSupervisorService | null = null;
  let unbindExecFile: (() => void) | null = null;
  const ownership = new FixtureOwnership();
  const overallBefore = resourceSnapshot();
  const overallStartedAt = performance.now();
  let peakOverallRssBytes = overallBefore.rssBytes;
  try {
    const rootContext = await runBenchmarkPromise(
      Layer.buildWithScope(processModule.ProcessRunnerLive, rootScope),
    );
    ingress = (await runBenchmarkPromise(
      ingressModule.makeIngressSupervisor(rootContext, rootScope),
    )) as RootIngressSupervisorService;
    unbindExecFile = bindExecFileDelegate(facadeModule.makeIngressExecFileDelegate(ingress));

    const shortArgv = await runShortArgv(config);
    peakOverallRssBytes = Math.max(peakOverallRssBytes, shortArgv.resources.rss.peakSampledBytes);

    const boundedOutputExact = await runOutputCase(
      config,
      scratch,
      'output-exact',
      ownership,
      MAX_OUTPUT_BYTES - 1,
      {
        ok: true,
        out: 'o',
      },
    );
    peakOverallRssBytes = Math.max(
      peakOverallRssBytes,
      boundedOutputExact.resources.rss.peakSampledBytes,
    );
    const boundedOutputOver = await runOutputCase(
      config,
      scratch,
      'output-over',
      ownership,
      MAX_OUTPUT_BYTES,
      {
        ok: false,
        code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
        err: `subprocess output exceeded ${MAX_OUTPUT_BYTES} bytes`,
      },
    );
    peakOverallRssBytes = Math.max(
      peakOverallRssBytes,
      boundedOutputOver.resources.rss.peakSampledBytes,
    );

    const timeoutTermResistant = await runTimeoutCase(config, scratch, ownership);
    peakOverallRssBytes = Math.max(
      peakOverallRssBytes,
      timeoutTermResistant.resources.rss.peakSampledBytes,
    );
    const preAborted = await runPreAbortCase(config);
    peakOverallRssBytes = Math.max(peakOverallRssBytes, preAborted.resources.rss.peakSampledBytes);
    const inFlightCancellation = await runCancellationCase(config, scratch, ownership);
    peakOverallRssBytes = Math.max(
      peakOverallRssBytes,
      inFlightCancellation.resources.rss.peakSampledBytes,
    );
    const posixDescendantCleanup = await runDescendantCleanupCase(config, scratch, ownership);
    if (!posixDescendantCleanup.skipped) {
      peakOverallRssBytes = Math.max(
        peakOverallRssBytes,
        posixDescendantCleanup.resources.rss.peakSampledBytes,
      );
    }
    const openInheritedPipeDescendant = await runOpenInheritedPipeCase(config, scratch, ownership);
    if (!openInheritedPipeDescendant.skipped) {
      peakOverallRssBytes = Math.max(
        peakOverallRssBytes,
        openInheritedPipeDescendant.resources.rss.peakSampledBytes,
      );
    }

    const ownershipCleanup = await ownership.cleanup(config.cleanupTimeoutMs + KILL_GRACE_MS);
    const checks = {
      runtimeFloorExact: runtimeFloor.exact,
      finalCleanupNoResidue: ownershipCleanup.assertionPassed,
      shortArgvExact: shortArgv.resultContract.allExact && shortArgv.settlementExactlyOnce,
      exactCombinedLimit:
        boundedOutputExact.combinedBytes === MAX_OUTPUT_BYTES &&
        boundedOutputExact.resultContract.allExact &&
        boundedOutputExact.settlementExactlyOnce &&
        boundedOutputExact.childResources.available &&
        boundedOutputExact.cleanup.allGone,
      oneByteOverCombinedLimit:
        boundedOutputOver.combinedBytes === MAX_OUTPUT_BYTES + 1 &&
        boundedOutputOver.resultContract.allExact &&
        boundedOutputOver.settlementExactlyOnce &&
        boundedOutputOver.childResources.available &&
        boundedOutputOver.cleanup.allGone,
      timeoutExact:
        timeoutTermResistant.resultContract.allExact &&
        timeoutTermResistant.settlementExactlyOnce &&
        timeoutTermResistant.termResistanceObserved &&
        timeoutTermResistant.childResources.peakSampledCpuPercentOfOneCore.available &&
        timeoutTermResistant.cleanup.allGone,
      timeoutSettlementWithinTolerance: timeoutTermResistant.settlementAccuracy.withinTolerance,
      preAbortExact: preAborted.resultContract.allExact && preAborted.settlementExactlyOnce,
      preAbortSettlementWithinTolerance: preAborted.settlementAccuracy.withinTolerance,
      cancellationExact:
        inFlightCancellation.resultContract.allExact &&
        inFlightCancellation.settlementExactlyOnce &&
        inFlightCancellation.termResistanceObserved &&
        inFlightCancellation.childResources.peakSampledCpuPercentOfOneCore.available &&
        inFlightCancellation.cleanup.allGone,
      cancellationSettlementWithinTolerance:
        inFlightCancellation.settlementAccuracy.withinTolerance,
      descendantCleanup:
        posixDescendantCleanup.skipped ||
        (posixDescendantCleanup.resultContract.allExact &&
          posixDescendantCleanup.settlementExactlyOnce &&
          posixDescendantCleanup.rootRecordedDescendantMatches &&
          posixDescendantCleanup.termResistanceObserved &&
          posixDescendantCleanup.childResources.peakSampledCpuPercentOfOneCore.available &&
          posixDescendantCleanup.noDescendantsRemain &&
          posixDescendantCleanup.settlementAccuracy.withinTolerance),
      inheritedPipeDescendant:
        openInheritedPipeDescendant.skipped ||
        (openInheritedPipeDescendant.resultContract.allExact &&
          openInheritedPipeDescendant.settlementExactlyOnce &&
          openInheritedPipeDescendant.rootExitedBeforeSettlement &&
          openInheritedPipeDescendant.promisePendingAfterRootExit &&
          openInheritedPipeDescendant.descendantAliveWhilePromisePending &&
          openInheritedPipeDescendant.groupAliveAfterRootExit &&
          openInheritedPipeDescendant.rootRecordedDescendantMatches &&
          openInheritedPipeDescendant.termResistanceObserved &&
          openInheritedPipeDescendant.childResources.peakSampledCpuPercentOfOneCore.available &&
          openInheritedPipeDescendant.noDescendantsRemain &&
          openInheritedPipeDescendant.settlementAccuracy.withinTolerance),
    };
    const passed = Object.values(checks).every(Boolean);
    const overallResources = resourceDelta(
      overallBefore,
      performance.now() - overallStartedAt,
      peakOverallRssBytes,
    );
    await writeJsonReport({
      schema: 1,
      kind: 'fleetdeck-effect-p0-exec-bench',
      recordedAt: new Date().toISOString(),
      runtime: {
        bun: Bun.version,
        revision: Bun.revision,
        platform: process.platform,
        arch: process.arch,
      },
      runtimeFloor,
      baseline: {
        module: 'src/daemon/exec.ts',
        export: 'execFileP',
        implementation:
          'Bun.spawn through one root ProcessRunnerLive Context and IngressSupervisor',
        directArgv: true,
        shell: false,
        combinedOutputLimitBytes: MAX_OUTPUT_BYTES,
        termToKillGraceMs: KILL_GRACE_MS,
      },
      resourceCapabilities: {
        childSelfSampling: {
          available: true,
          platform: process.platform,
          rss: 'process.memoryUsage().rss bytes with sampled peak',
          cpu: 'process.cpuUsage() cumulative microseconds with interval peak and lifetime average',
        },
        exitedChildAggregateRusage: {
          available: false,
          platform: process.platform,
          reason:
            'execFileP intentionally exposes only ExecResult, not the ChildProcess handle or exited-child rusage',
        },
      },
      config,
      workloads: {
        shortArgv,
        boundedOutput: {
          exactLimit: boundedOutputExact,
          oneByteOver: boundedOutputOver,
        },
        timeoutTermResistant,
        preAborted,
        inFlightCancellation,
        posixDescendantCleanup,
        openInheritedPipeDescendant,
      },
      resources: overallResources,
      ownershipCleanup,
      verification: { checks, passed },
    });
    if (!passed) process.exitCode = 1;
  } catch (error) {
    const ownershipCleanup = await ownership.cleanup(config.cleanupTimeoutMs + KILL_GRACE_MS);
    await writeJsonReport({
      schema: 1,
      kind: 'fleetdeck-effect-p0-exec-bench',
      recordedAt: new Date().toISOString(),
      runtime: {
        bun: Bun.version,
        revision: Bun.revision,
        platform: process.platform,
        arch: process.arch,
      },
      runtimeFloor,
      config,
      error: error instanceof Error ? error.message : String(error),
      ownershipCleanup,
      verification: {
        checks: {
          runtimeFloorExact: runtimeFloor.exact,
          finalCleanupNoResidue: ownershipCleanup.assertionPassed,
          workloadCompleted: false,
        },
        passed: false,
      },
    });
    process.exitCode = 1;
  } finally {
    ingress?.quiesce();
    try {
      if (ingress) await ingress.close();
    } finally {
      try {
        unbindExecFile?.();
        await runBenchmarkPromise(Scope.close(rootScope, Exit.void));
      } finally {
        rmSync(scratch, { recursive: true, force: true });
      }
    }
  }
}

const args = Bun.argv.slice(2);
if (args[0] === INTERNAL_CHILD_FLAG) {
  await runInternalChild(args.slice(1));
} else {
  await main();
}
