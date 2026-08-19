import { randomUUID } from 'node:crypto';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readlinkSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { arch, cpus, platform, release, totalmem } from 'node:os';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { assertCommand, REPO_ROOT, runCommand, summarize, writeJsonReport } from './metrics.ts';

interface BaselineConfig {
  launches: number;
  forcedLaunches: number;
  mdnsLaunches: number;
  commandRuns: number;
  builds: number;
  packs: number;
  idleMs: number;
  idleSampleMs: number;
  healthTimeoutMs: number;
  shutdownTimeoutMs: number;
  residueTimeoutMs: number;
  mdnsWatchdogMs: number;
  hookDeadlineMs: number;
  cliDeadlineMs: number;
  daemonPath: string;
}

interface HealthBody {
  ok?: unknown;
  pid?: unknown;
  startup?: unknown;
  [key: string]: unknown;
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

interface DescriptorInspection {
  available: boolean;
  source: 'procfs' | 'lsof' | 'unavailable';
  fds: number;
  sockets: number;
}

interface DescendantInspection {
  available: boolean;
  pids: number[];
}

interface ProcessSample {
  rssBytes: number;
  reportedCpuPercent: number;
  cumulativeCpuMs: number;
}

const encoder = new TextEncoder();
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

function readConfig(): BaselineConfig {
  const daemonFlag = stringFlag('daemon', 'src/daemon/fleetd.bundle.mjs');
  return {
    launches: integerFlag('launches', 30, 2),
    forcedLaunches: integerFlag('forced-launches', 3, 1),
    mdnsLaunches: integerFlag('mdns-launches', 3, 1),
    commandRuns: integerFlag('command-runs', 30, 1),
    builds: integerFlag('builds', 2, 2),
    packs: integerFlag('packs', 2, 2),
    idleMs: integerFlag('idle-ms', 300_000, 0),
    idleSampleMs: integerFlag('idle-sample-ms', 1_000, 10),
    healthTimeoutMs: integerFlag('health-timeout-ms', 10_000, 100),
    shutdownTimeoutMs: integerFlag('shutdown-timeout-ms', 3_000, 100),
    residueTimeoutMs: integerFlag('residue-timeout-ms', 1_000, 100),
    mdnsWatchdogMs: integerFlag('mdns-watchdog-ms', 1_000, 100),
    hookDeadlineMs: integerFlag('hook-deadline-ms', 2_500, 100),
    cliDeadlineMs: integerFlag('cli-deadline-ms', 1_500, 100),
    daemonPath: path.resolve(REPO_ROOT, daemonFlag),
  };
}

function sha256(bytes: Uint8Array): string {
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
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

function cleanEnvironment(): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (typeof value === 'string' && !key.startsWith('FLEETDECK_')) env[key] = value;
  }
  delete env['TMUX'];
  delete env['TMUX_PANE'];
  return env;
}

function daemonEnvironment(
  home: string,
  port: number,
  nonce: string,
  mdns: boolean,
): Record<string, string> {
  return {
    ...cleanEnvironment(),
    FLEETDECK_PORT: String(port),
    FLEETDECK_HOME: home,
    FLEETDECK_BIND: mdns ? '0.0.0.0' : '127.0.0.1',
    FLEETDECK_MDNS: mdns ? 'on' : 'off',
    FLEETDECK_AGENTS_CMD: 'false',
    FLEETDECK_HOLD_SCOPE: 'all',
    FLEETDECK_TMUX_SOCKET: `fleetdeck-effect-p0-${process.pid}-${nonce}`,
  };
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

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

async function healthAt(baseUrl: string, timeoutMs: number): Promise<HealthBody | null> {
  try {
    const response = await fetch(`${baseUrl}/health?baseline=${randomUUID()}`, {
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
  daemon: Pick<RunningDaemon, 'process' | 'baseUrl'>,
  timeoutMs: number,
  startedAt: number,
): Promise<{ healthMs: number; reconciliationReadyMs: number }> {
  const deadline = performance.now() + timeoutMs;
  let healthMs: number | null = null;
  while (performance.now() < deadline) {
    if (daemon.process.exitCode !== null) {
      throw new Error(`daemon exited ${daemon.process.exitCode} before readiness`);
    }
    const remaining = Math.max(1, Math.floor(deadline - performance.now()));
    const health = await healthAt(daemon.baseUrl, Math.min(remaining, 500));
    if (health?.ok === true && health.pid === daemon.process.pid) {
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
  config: BaselineConfig,
  label: string,
  options: { mdns?: boolean } = {},
): Promise<RunningDaemon> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-baseline-'));
  const port = await reservePort();
  // Include process creation itself in the cold-start clock. Starting this after
  // Bun.spawn would systematically omit one of the costs P0 is meant to freeze.
  const startedAt = performance.now();
  const processHandle = Bun.spawn([process.execPath, '--no-env-file', config.daemonPath], {
    cwd: REPO_ROOT,
    env: daemonEnvironment(home, port, label, options.mdns === true),
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdout = Bun.readableStreamToText(processHandle.stdout).catch(() => '');
  const stderr = Bun.readableStreamToText(processHandle.stderr).catch(() => '');
  try {
    const readiness = await waitForReady(
      { process: processHandle, baseUrl: `http://127.0.0.1:${port}` },
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

function isPidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return !(isObject(error) && typeof error['code'] === 'string' && error['code'] === 'ESRCH');
  }
}

async function inspectDescriptors(pid: number): Promise<DescriptorInspection> {
  const procFd = `/proc/${pid}/fd`;
  if (existsSync('/proc/self/fd')) {
    if (!existsSync(procFd)) return { available: true, source: 'procfs', fds: 0, sockets: 0 };
    const entries = readdirSync(procFd);
    let sockets = 0;
    for (const entry of entries) {
      try {
        if (readlinkSync(path.join(procFd, entry)).startsWith('socket:[')) sockets += 1;
      } catch {
        // A descriptor can close between readdir and readlink.
      }
    }
    return { available: true, source: 'procfs', fds: entries.length, sockets };
  }

  try {
    const result = await runCommand(['lsof', '-nP', '-a', '-p', String(pid), '-Fftn']);
    const fields = result.stdout.split('\n').filter(Boolean);
    const fds = fields.filter((line) => line.startsWith('f')).length;
    const sockets = fields.filter((line) =>
      ['tIPv4', 'tIPv6', 'tunix', 'tsock'].includes(line),
    ).length;
    return { available: true, source: 'lsof', fds, sockets };
  } catch {
    return { available: false, source: 'unavailable', fds: 0, sockets: 0 };
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
      const list = children.get(ppid) ?? [];
      list.push(pid);
      children.set(ppid, list);
    }
    const found: number[] = [];
    const queue = [...(children.get(parentPid) ?? [])];
    while (queue.length > 0) {
      const pid = queue.shift();
      if (pid === undefined || found.includes(pid)) continue;
      found.push(pid);
      queue.push(...(children.get(pid) ?? []));
    }
    return { available: true, pids: found.sort((a, b) => a - b) };
  } catch {
    return { available: false, pids: [] };
  }
}

async function portCanRebind(port: number): Promise<boolean> {
  let probe: ReturnType<typeof Bun.serve> | null = null;
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

async function waitForResidueRelease(
  daemon: RunningDaemon,
  descendants: DescendantInspection,
  mode: 'graceful' | 'sigkill',
  timeoutMs: number,
) {
  const startedAt = performance.now();
  let latest: {
    pidAlive: boolean;
    portRebindSucceeded: boolean;
    pidFilePresent: boolean;
    knownDescendantsAlive: number[];
    descriptors: DescriptorInspection;
  } | null = null;
  for (;;) {
    const descriptors = await inspectDescriptors(daemon.process.pid);
    latest = {
      pidAlive: isPidAlive(daemon.process.pid),
      portRebindSucceeded: await portCanRebind(daemon.port),
      pidFilePresent: existsSync(path.join(daemon.home, 'fleetd.pid')),
      knownDescendantsAlive: descendants.pids.filter(isPidAlive),
      descriptors,
    };
    const released =
      !latest.pidAlive &&
      latest.portRebindSucceeded &&
      (!descendants.available || latest.knownDescendantsAlive.length === 0) &&
      (!descriptors.available || (descriptors.fds === 0 && descriptors.sockets === 0)) &&
      (mode === 'sigkill' || !latest.pidFilePresent);
    if (released || performance.now() - startedAt >= timeoutMs) {
      return {
        ...latest,
        released,
        waitMs: rounded(performance.now() - startedAt),
        timeoutMs,
      };
    }
    await Bun.sleep(25);
  }
}

async function stopAndMeasure(
  daemon: RunningDaemon,
  mode: 'graceful' | 'sigkill',
  config: BaselineConfig,
) {
  const beforeDescriptors = await inspectDescriptors(daemon.process.pid);
  const descendants = await descendantsOf(daemon.process.pid);
  const startedAt = performance.now();
  daemon.process.kill(mode === 'graceful' ? 'SIGTERM' : 'SIGKILL');
  let deadlineExceeded = false;
  let exitCode: number;
  try {
    exitCode = await awaitExit(daemon.process, config.shutdownTimeoutMs);
  } catch {
    deadlineExceeded = true;
    daemon.process.kill('SIGKILL');
    exitCode = await daemon.process.exited;
  }
  const durationMs = rounded(performance.now() - startedAt);
  const [stdout, stderr] = await Promise.all([daemon.stdout, daemon.stderr]);
  const residue = await waitForResidueRelease(daemon, descendants, mode, config.residueTimeoutMs);
  const checks = {
    exitedWithinDeadline: !deadlineExceeded,
    expectedExit: mode === 'graceful' ? exitCode === 0 : daemon.process.signalCode === 'SIGKILL',
    pidReleased: !residue.pidAlive,
    portReleased: residue.portRebindSucceeded,
    descendantsReleased: !descendants.available || residue.knownDescendantsAlive.length === 0,
    descriptorsReleased:
      !residue.descriptors.available ||
      (residue.descriptors.fds === 0 && residue.descriptors.sockets === 0),
    pidFileReleased: mode === 'sigkill' ? null : !residue.pidFilePresent,
    residueReleasedWithinDeadline: residue.released,
  };
  return {
    mode,
    pid: daemon.process.pid,
    port: daemon.port,
    healthMs: daemon.healthMs,
    reconciliationReadyMs: daemon.reconciliationReadyMs,
    shutdownMs: durationMs,
    deadlineExceeded,
    exitCode,
    signalCode: daemon.process.signalCode,
    processUsage: processUsage(daemon.process),
    output: {
      stdoutBytes: encoder.encode(stdout).byteLength,
      stderrBytes: encoder.encode(stderr).byteLength,
      stdoutSha256: sha256(encoder.encode(stdout)),
      stderrSha256: sha256(encoder.encode(stderr)),
    },
    before: {
      descriptors: beforeDescriptors,
      descendants,
    },
    residue,
    checks,
  };
}

async function lifecycleRun(
  config: BaselineConfig,
  index: number,
  mode: 'graceful' | 'sigkill',
  options: { mdns?: boolean } = {},
) {
  const daemon = await startDaemon(config, `${mode}-${index}-${randomUUID()}`, options);
  try {
    return await stopAndMeasure(daemon, mode, config);
  } finally {
    if (daemon.process.exitCode === null) {
      daemon.process.kill('SIGKILL');
      await daemon.process.exited.catch(() => -1);
    }
    rmSync(daemon.home, { recursive: true, force: true });
  }
}

function checksPassed(checks: Record<string, boolean | null>): boolean {
  return Object.values(checks).every((value) => value !== false);
}

async function buildMeasurements(builds: number) {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-build-'));
  const workspace = path.join(scratch, 'repo');
  const scripts = ['bundle', 'bundle:bin', 'bundle:hooks', 'build:board'] as const;
  const generatedFiles = [
    'src/daemon/fleetd.bundle.mjs',
    'bin/fleetdeck.mjs',
    'scripts/fleet-hook.mjs',
    'scripts/fleet-sessionstart.mjs',
    'scripts/fleet-watch.mjs',
  ];

  const artifactManifest = async () => {
    const paths = [...generatedFiles];
    const boardGlob = new Bun.Glob('**/*');
    for await (const name of boardGlob.scan({
      cwd: path.join(workspace, 'src/daemon/board-dist'),
      onlyFiles: true,
    })) {
      paths.push(path.posix.join('src/daemon/board-dist', name.split(path.sep).join('/')));
    }
    paths.sort();
    const artifacts: { path: string; bytes: number; sha256: string }[] = [];
    const aggregate = new Bun.CryptoHasher('sha256');
    for (const relative of paths) {
      const bytes = await Bun.file(path.join(workspace, relative)).bytes();
      const hash = sha256(bytes);
      artifacts.push({ path: relative, bytes: bytes.byteLength, sha256: hash });
      aggregate.update(`${relative}\0${bytes.byteLength}\0${hash}\n`);
    }
    return {
      bytes: artifacts.reduce((sum, artifact) => sum + artifact.bytes, 0),
      sha256: aggregate.digest('hex'),
      artifacts,
    };
  };

  try {
    cpSync(REPO_ROOT, workspace, {
      recursive: true,
      filter: (source) => {
        const relative = path.relative(REPO_ROOT, source);
        return !['.git', '.codegraph', 'node_modules', path.join('board', 'node_modules')].some(
          (excluded) => relative === excluded || relative.startsWith(`${excluded}${path.sep}`),
        );
      },
    });
    symlinkSync(
      path.join(REPO_ROOT, 'node_modules'),
      path.join(workspace, 'node_modules'),
      process.platform === 'win32' ? 'junction' : 'dir',
    );
    cpSync(path.join(REPO_ROOT, 'board/node_modules'), path.join(workspace, 'board/node_modules'), {
      recursive: true,
    });

    const runs: {
      totalDurationMs: number;
      commands: { script: (typeof scripts)[number]; durationMs: number }[];
      manifest: Awaited<ReturnType<typeof artifactManifest>>;
    }[] = [];
    for (let index = 0; index < builds; index += 1) {
      const startedAt = performance.now();
      const commands: { script: (typeof scripts)[number]; durationMs: number }[] = [];
      for (const script of scripts) {
        const result = assertCommand(
          await runCommand([process.execPath, 'run', script], { cwd: workspace }),
        );
        commands.push({ script, durationMs: result.durationMs });
      }
      runs.push({
        totalDurationMs: rounded(performance.now() - startedAt),
        commands,
        manifest: await artifactManifest(),
      });
    }
    const commandDurationMs = Object.fromEntries(
      scripts.map((script) => [
        script,
        summarize(
          runs.map(
            (run) => run.commands.find((command) => command.script === script)?.durationMs ?? 0,
          ),
        ),
      ]),
    );
    return {
      pipeline: scripts.map((script) => `bun run ${script}`),
      workspace: 'disposable repository copy',
      runs,
      totalDurationMs: summarize(runs.map((run) => run.totalDurationMs)),
      commandDurationMs,
      deterministic:
        new Set(runs.map((run) => `${run.manifest.bytes}:${run.manifest.sha256}`)).size === 1,
    };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

async function packageMeasurement(packs: number) {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-pack-'));
  try {
    const runs = [];
    for (let index = 0; index < packs; index += 1) {
      const destination = path.join(scratch, `run-${index}`);
      mkdirSync(destination);
      const pack = assertCommand(
        await runCommand(
          [
            process.execPath,
            'pm',
            'pack',
            '--destination',
            destination,
            '--ignore-scripts',
            '--gzip-level',
            '9',
            '--quiet',
          ],
          { cwd: REPO_ROOT },
        ),
      );
      const tarballs = readdirSync(destination).filter((name) => name.endsWith('.tgz'));
      if (tarballs.length !== 1 || tarballs[0] === undefined) {
        throw new Error(`bun pm pack run ${index} produced ${tarballs.length} tarballs`);
      }
      const tarball = path.join(destination, tarballs[0]);
      const bytes = await Bun.file(tarball).bytes();
      const files = await new Bun.Archive(bytes).files();
      const contents: { path: string; bytes: number; sha256: string }[] = [];
      for (const [name, file] of files) {
        const content = new Uint8Array(await file.arrayBuffer());
        contents.push({ path: name, bytes: file.size, sha256: sha256(content) });
      }
      contents.sort((a, b) => a.path.localeCompare(b.path));
      const normalized = new Bun.CryptoHasher('sha256');
      for (const file of contents) {
        normalized.update(`${file.path}\0${file.bytes}\0${file.sha256}\n`);
      }
      runs.push({
        command: pack.command.map((argument) => {
          if (argument === process.execPath) return '<bun>';
          if (argument.startsWith(scratch)) return argument.replace(scratch, '<scratch>');
          return argument;
        }),
        durationMs: pack.durationMs,
        filename: tarballs[0],
        bytes: bytes.byteLength,
        sha256: sha256(bytes),
        normalizedManifestSha256: normalized.digest('hex'),
        fileCount: contents.length,
        contents,
      });
    }
    const first = runs[0];
    if (!first) throw new Error('package measurement produced no runs');
    return {
      runs,
      durationMs: summarize(runs.map((run) => run.durationMs)),
      normalizedDeterministic: new Set(runs.map((run) => run.normalizedManifestSha256)).size === 1,
      rawTarballDeterministic: new Set(runs.map((run) => run.sha256)).size === 1,
      bytes: first.bytes,
      fileCount: first.fileCount,
      contents: first.contents,
    };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

async function testCount(): Promise<number> {
  let count = 0;
  const glob = new Bun.Glob('**/*.test.ts');
  for await (const _file of glob.scan({ cwd: path.join(REPO_ROOT, 'tests'), onlyFiles: true })) {
    count += 1;
  }
  return count;
}

async function repositoryMetadata() {
  const [commit, dirty] = await Promise.all([
    runCommand(['git', 'rev-parse', 'HEAD'], { cwd: REPO_ROOT }).then(assertCommand),
    runCommand(['git', 'status', '--porcelain=v1', '--untracked-files=all'], {
      cwd: REPO_ROOT,
    }).then(assertCommand),
  ]);
  const cpuList = cpus();
  return {
    commit: commit.stdout.trim(),
    dirty: dirty.stdout.trim() !== '',
    dirtyPaths: dirty.stdout
      .split('\n')
      .filter(Boolean)
      .map((line) => line.slice(3)),
    runtime: { bun: Bun.version, revision: Bun.revision, executable: '<bun>' },
    machine: {
      platform: platform(),
      release: release(),
      arch: arch(),
      cpu: cpuList[0]?.model ?? 'unknown',
      logicalCpuCount: cpuList.length,
      totalMemoryBytes: totalmem(),
    },
    testFiles: await testCount(),
  };
}

async function artifactMeasurement(bundlePath: string) {
  const bytes = await Bun.file(bundlePath).bytes();
  const gzip = Bun.gzipSync(bytes, { level: 9, library: 'zlib' });
  return {
    path: path.relative(REPO_ROOT, bundlePath),
    rawBytes: bytes.byteLength,
    rawSha256: sha256(bytes),
    gzipBytes: gzip.byteLength,
    gzipSha256: sha256(gzip),
    gzip: { api: 'Bun.gzipSync', level: 9, library: 'zlib' },
  };
}

async function seedHookCompatibility(home: string): Promise<Record<string, string>> {
  const compatibility: unknown = await Bun.file(path.join(REPO_ROOT, 'compatibility.json')).json();
  const packageJson: unknown = await Bun.file(path.join(REPO_ROOT, 'package.json')).json();
  if (!isObject(compatibility) || !isObject(compatibility['claudeCode'])) {
    throw new Error('compatibility.json does not have the expected shape');
  }
  if (!isObject(packageJson) || typeof packageJson['version'] !== 'string') {
    throw new Error('package.json does not have a version');
  }
  const schema = compatibility['schema'];
  const minimum = compatibility['claudeCode']['min'];
  if (typeof schema !== 'number' || typeof minimum !== 'string') {
    throw new Error('compatibility.json policy is not numeric-schema/string-minimum');
  }
  const claudePid = String(process.pid);
  const claudeVersion = '2.1.234';
  const now = Date.now();
  writeFileSync(
    path.join(home, `claude-compat-${claudePid}.json`),
    `${JSON.stringify({
      schema: 1,
      identity: { key: Number(claudePid), source: 'CLAUDE_PID' },
      generation: { kind: 'p0-baseline', value: `${claudePid}:${claudeVersion}` },
      fleetdeckVersion: packageJson['version'],
      policy: `${schema}:${minimum}`,
      claudeVersion,
      active: true,
      createdAt: now,
      expiresAt: now + 3_600_000,
    })}\n`,
    { mode: 0o600 },
  );
  return { CLAUDE_PID: claudePid, FLEETDECK_TEST_CLAUDE_VERSION: claudeVersion };
}

async function commandColdStarts(daemon: RunningDaemon, config: BaselineConfig) {
  const compatibilityEnv = await seedHookCompatibility(daemon.home);
  const env = {
    ...cleanEnvironment(),
    ...compatibilityEnv,
    FLEETDECK_PORT: String(daemon.port),
    FLEETDECK_HOME: daemon.home,
  };
  const hookDurations: number[] = [];
  const cliDurations: number[] = [];
  const hook = path.join(REPO_ROOT, 'scripts/fleet-hook.mjs');
  const cli = path.join(REPO_ROOT, 'bin/fleetdeck.mjs');
  for (let index = 0; index < config.commandRuns; index += 1) {
    const result = assertCommand(
      await runCommand([process.execPath, '--no-env-file', hook, 'Notification'], {
        cwd: REPO_ROOT,
        env,
        stdin: encoder.encode(
          JSON.stringify({
            session_id: 'effect-p0-hook-cold-start',
            hook_event_name: 'Notification',
            cwd: REPO_ROOT,
            message: `P0 cold hook ${index}`,
          }),
        ),
      }),
    );
    const output: unknown = JSON.parse(result.stdout || '{}');
    if (!isObject(output)) throw new Error('hook cold start did not emit a JSON object');
    hookDurations.push(result.durationMs);
  }
  for (let index = 0; index < config.commandRuns; index += 1) {
    const result = assertCommand(
      await runCommand([process.execPath, '--no-env-file', cli, 'status'], {
        cwd: REPO_ROOT,
        env,
      }),
    );
    if (!result.stdout.includes(`pid      ${daemon.process.pid}`)) {
      throw new Error(`CLI status run ${index} did not report the measured daemon`);
    }
    cliDurations.push(result.durationMs);
  }
  return {
    hook: {
      artifact: path.relative(REPO_ROOT, hook),
      event: 'Notification',
      hardDeadlineMs: config.hookDeadlineMs,
      durationMs: summarize(hookDurations),
      minimumMarginMs: rounded(config.hookDeadlineMs - Math.max(...hookDurations)),
      withinDeadline: hookDurations.every((duration) => duration < config.hookDeadlineMs),
    },
    cli: {
      artifact: path.relative(REPO_ROOT, cli),
      command: 'status',
      hardDeadlineMs: config.cliDeadlineMs,
      durationMs: summarize(cliDurations),
      minimumMarginMs: rounded(config.cliDeadlineMs - Math.max(...cliDurations)),
      withinDeadline: cliDurations.every((duration) => duration < config.cliDeadlineMs),
    },
  };
}

function parseCpuTime(text: string): number | null {
  const [dayText, clockText] = text.includes('-') ? text.split('-', 2) : [undefined, text];
  const parts = clockText?.split(':').map(Number) ?? [];
  if (parts.some((part) => !Number.isFinite(part))) return null;
  const days = dayText === undefined ? 0 : Number(dayText);
  if (!Number.isFinite(days)) return null;
  let seconds = days * 86_400;
  if (parts.length === 3)
    seconds += (parts[0] ?? 0) * 3_600 + (parts[1] ?? 0) * 60 + (parts[2] ?? 0);
  else if (parts.length === 2) seconds += (parts[0] ?? 0) * 60 + (parts[1] ?? 0);
  else return null;
  return seconds * 1_000;
}

async function processSample(pid: number): Promise<ProcessSample | null> {
  try {
    const result = await runCommand([
      'ps',
      '-o',
      'rss=',
      '-o',
      '%cpu=',
      '-o',
      'time=',
      '-p',
      String(pid),
    ]);
    if (result.exitCode !== 0) return null;
    const line = result.stdout.trim().split('\n').at(-1) ?? '';
    const match = /^(\d+)\s+([\d.,]+)\s+(\S+)$/.exec(line.trim());
    if (!match) return null;
    const rssKiB = Number(match[1]);
    const reportedCpuPercent = Number((match[2] ?? '').replace(',', '.'));
    const cumulativeCpuMs = parseCpuTime(match[3] ?? '');
    if (
      !Number.isFinite(rssKiB) ||
      !Number.isFinite(reportedCpuPercent) ||
      cumulativeCpuMs === null
    ) {
      return null;
    }
    return { rssBytes: rssKiB * 1024, reportedCpuPercent, cumulativeCpuMs };
  } catch {
    return null;
  }
}

async function idleMeasurement(daemon: RunningDaemon, config: BaselineConfig) {
  const startedAt = performance.now();
  const samples: { elapsedMs: number; sample: ProcessSample }[] = [];
  const takeSample = async (): Promise<void> => {
    const sample = await processSample(daemon.process.pid);
    if (sample) samples.push({ elapsedMs: rounded(performance.now() - startedAt), sample });
  };
  await takeSample();
  while (performance.now() - startedAt < config.idleMs) {
    const remaining = config.idleMs - (performance.now() - startedAt);
    await Bun.sleep(Math.max(1, Math.min(config.idleSampleMs, remaining)));
    await takeSample();
  }
  const first = samples[0];
  const last = samples.at(-1);
  const elapsedMs = last && first ? last.elapsedMs - first.elapsedMs : 0;
  const cpuMs = last && first ? last.sample.cumulativeCpuMs - first.sample.cumulativeCpuMs : 0;
  return {
    available: samples.length > 0,
    unavailableReason: samples.length > 0 ? null : 'ps did not return a parseable process sample',
    requestedDurationMs: config.idleMs,
    measuredDurationMs: rounded(performance.now() - startedAt),
    source: 'ps rss/%cpu/time',
    sampleCount: samples.length,
    rssBytes: summarize(samples.map(({ sample }) => sample.rssBytes)),
    reportedCpuPercent: summarize(samples.map(({ sample }) => sample.reportedCpuPercent)),
    intervalCpuPercent: elapsedMs > 0 ? rounded((Math.max(0, cpuMs) / elapsedMs) * 100) : null,
    samples,
  };
}

async function main(): Promise<void> {
  const floor = assertRuntimeFloor();
  const config = readConfig();
  if (!(await Bun.file(config.daemonPath).exists())) {
    throw new Error(`daemon artifact not found: ${config.daemonPath}`);
  }
  const bundle = path.join(REPO_ROOT, 'src/daemon/fleetd.bundle.mjs');
  // Do not overlap timed build and pack work: CPU/disk contention would make the
  // recorded production-pipeline durations impossible to compare between runs.
  const [metadata, artifact] = await Promise.all([
    repositoryMetadata(),
    artifactMeasurement(bundle),
  ]);
  const build = await buildMeasurements(config.builds);
  const packed = await packageMeasurement(config.packs);

  const graceful = [];
  for (let index = 0; index < config.launches; index += 1) {
    graceful.push(await lifecycleRun(config, index, 'graceful'));
  }
  const forced = [];
  for (let index = 0; index < config.forcedLaunches; index += 1) {
    forced.push(await lifecycleRun(config, index, 'sigkill'));
  }
  const mdns = [];
  for (let index = 0; index < config.mdnsLaunches; index += 1) {
    mdns.push(await lifecycleRun(config, index, 'graceful', { mdns: true }));
  }

  const steady = await startDaemon(config, `steady-${randomUUID()}`);
  let commands: Awaited<ReturnType<typeof commandColdStarts>>;
  let idle: Awaited<ReturnType<typeof idleMeasurement>>;
  let steadyShutdown: Awaited<ReturnType<typeof stopAndMeasure>>;
  try {
    commands = await commandColdStarts(steady, config);
    idle = await idleMeasurement(steady, config);
    steadyShutdown = await stopAndMeasure(steady, 'graceful', config);
  } finally {
    if (steady.process.exitCode === null) {
      steady.process.kill('SIGKILL');
      await steady.process.exited.catch(() => -1);
    }
    rmSync(steady.home, { recursive: true, force: true });
  }

  const violations: string[] = [];
  if (!build.deterministic) {
    violations.push('production build-pipeline outputs were not deterministic');
  }
  if (!packed.normalizedDeterministic) {
    violations.push('normalized package contents were not deterministic');
  }
  for (const run of graceful) {
    if (!checksPassed(run.checks)) violations.push(`graceful lifecycle failed for pid ${run.pid}`);
  }
  for (const run of forced) {
    if (!checksPassed(run.checks)) violations.push(`forced lifecycle failed for pid ${run.pid}`);
  }
  for (const run of mdns) {
    if (!checksPassed(run.checks)) violations.push(`mDNS lifecycle failed for pid ${run.pid}`);
    if (run.shutdownMs > config.mdnsWatchdogMs) {
      violations.push(
        `mDNS graceful shutdown exceeded ${config.mdnsWatchdogMs}ms for pid ${run.pid} (${run.shutdownMs}ms)`,
      );
    }
  }
  if (!checksPassed(steadyShutdown.checks)) violations.push('steady daemon shutdown failed');
  if (!idle.available) violations.push(`idle metrics unavailable: ${idle.unavailableReason}`);
  if (!commands.hook.withinDeadline) violations.push('hook cold start exceeded its deadline');
  if (!commands.cli.withinDeadline) violations.push('CLI cold start exceeded its deadline');

  const normalizedConfig = { ...config, daemonPath: path.relative(REPO_ROOT, config.daemonPath) };
  const comparisonKey = sha256(
    encoder.encode(
      JSON.stringify({
        commit: metadata.commit,
        runtime: metadata.runtime,
        machine: metadata.machine,
        config: normalizedConfig,
      }),
    ),
  );
  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p0-baseline',
    ok: violations.length === 0,
    recordedAt: new Date().toISOString(),
    config: normalizedConfig,
    comparison: {
      key: comparisonKey,
      requirement: 'compare only reports with identical key on the same idle machine',
    },
    runtimeFloor: floor,
    metadata,
    artifact,
    build,
    package: packed,
    daemon: {
      coldLaunches: {
        healthMs: summarize(graceful.map((run) => run.healthMs)),
        reconciliationReadyMs: summarize(graceful.map((run) => run.reconciliationReadyMs)),
        gracefulShutdownMs: summarize(graceful.map((run) => run.shutdownMs)),
        runs: graceful,
      },
      forcedShutdowns: {
        mechanism: 'direct SIGKILL (pre-P4 baseline has no force latch)',
        durationMs: summarize(forced.map((run) => run.shutdownMs)),
        runs: forced,
      },
      mdnsStartup: {
        bind: '0.0.0.0',
        enabled: true,
        reconciliationReadyMs: summarize(mdns.map((run) => run.reconciliationReadyMs)),
        gracefulGoodbye: {
          implementationWatchdogMs: config.mdnsWatchdogMs,
          shutdownMs: summarize(mdns.map((run) => run.shutdownMs)),
          withinWatchdog: mdns.every((run) => run.shutdownMs <= config.mdnsWatchdogMs),
        },
        runs: mdns,
      },
      commands,
      idle,
      steadyShutdown,
    },
    violations,
  });
  if (violations.length > 0) process.exitCode = 1;
}

try {
  await main();
} catch (error) {
  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p0-baseline',
    ok: false,
    recordedAt: new Date().toISOString(),
    runtimeFloor: runtimeFloor(),
    error: error instanceof Error ? error.message : String(error),
  });
  process.exitCode = 1;
}
