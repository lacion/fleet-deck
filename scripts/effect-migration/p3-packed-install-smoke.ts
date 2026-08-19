import { randomUUID } from 'node:crypto';
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

import { assertCommand, REPO_ROOT, runCommand } from './metrics.ts';

const COMMAND_TIMEOUT_MS = 120_000;
const HEALTH_TIMEOUT_MS = 10_000;
const CHILD_TIMEOUT_MS = 8_000;
const SHUTDOWN_TIMEOUT_MS = 10_000;
const RESIDUE_TIMEOUT_MS = 3_000;

interface HealthBody {
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
  const result = assertCommand(
    await runCommand(['ps', '-A', '-o', 'pid=', '-o', 'ppid='], {
      timeoutMs: 5_000,
    }),
  );
  const children = new Map<number, number[]>();
  for (const line of result.stdout.split('\n')) {
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

async function stopForCleanup(processHandle: Bun.ReadableSubprocess | undefined): Promise<void> {
  if (!processHandle || processHandle.exitCode !== null) return;
  processHandle.kill('SIGTERM');
  try {
    await awaitExit(processHandle, 2_000);
  } catch {
    if (processHandle.exitCode === null) processHandle.kill('SIGKILL');
    await processHandle.exited.catch(() => -1);
  }
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
  const packDir = path.join(scratch, 'pack');
  const consumerDir = path.join(scratch, 'consumer');
  const home = path.join(scratch, 'home');
  const childFixture = path.join(scratch, 'agents-child.mjs');
  const childPidFile = path.join(scratch, 'agents-child.pid');
  mkdirSync(packDir);
  mkdirSync(consumerDir);
  invariant(
    !/\s/.test(process.execPath) && !/\s/.test(childFixture),
    'FLEETDECK_AGENTS_CMD is whitespace-tokenized; the Bun and fixture paths must not contain whitespace',
  );

  let daemon: Bun.ReadableSubprocess | undefined;
  let daemonStdout: Promise<string> | undefined;
  let daemonStderr: Promise<string> | undefined;
  let childPid: number | null = null;

  try {
    assertCommand(
      await runCommand(
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

    // Resolve the exact local tarball into a lock without installing, then make
    // the real clean install prove that lock is accepted unchanged in frozen
    // mode. `copyfile` prevents an accidental checkout/cache symlink from
    // making the installed artifact look healthier than the tarball itself.
    assertCommand(
      await runCommand(
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
    assertCommand(
      await runCommand(
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
    invariant(
      Buffer.from(lockAfter).equals(Buffer.from(lockBefore)),
      'frozen consumer install changed bun.lock',
    );

    const installedRoot = path.join(consumerDir, 'node_modules', 'fleetdeck');
    const installedPackage: unknown = await Bun.file(
      path.join(installedRoot, 'package.json'),
    ).json();
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

    await Bun.write(childFixture, CHILD_FIXTURE);
    const port = await reservePort();
    const baseUrl = `http://127.0.0.1:${String(port)}`;
    const pidFile = path.join(home, 'fleetd.pid');
    daemon = Bun.spawn([process.execPath, '--no-env-file', installedDaemon], {
      cwd: installedRoot,
      env: cleanDaemonEnvironment(home, port, childFixture, childPidFile),
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    daemonStdout = Bun.readableStreamToText(daemon.stdout).catch(() => '');
    daemonStderr = Bun.readableStreamToText(daemon.stderr).catch(() => '');

    const health = await waitForHealth(daemon, baseUrl);
    invariant(
      health.version === expectedVersion,
      `/health did not report version ${expectedVersion}`,
    );
    childPid = await waitForChild(childPidFile);
    const descendants = await descendantsOf(daemon.pid);
    invariant(
      descendants.includes(childPid),
      `agents child pid ${String(childPid)} is not a descendant of daemon pid ${String(daemon.pid)}`,
    );

    const pidRecord: unknown = JSON.parse(readFileSync(pidFile, 'utf8'));
    invariant(isObject(pidRecord), 'fleetd.pid is not a JSON object');
    const owned = pidRecord as PidRecord;
    invariant(
      owned.pid === daemon.pid && owned.port === port,
      'fleetd.pid does not identify the installed daemon and reserved port',
    );

    daemon.kill('SIGTERM');
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

    const [stdout, stderr] = await Promise.all([daemonStdout, daemonStderr]);
    process.stdout.write(
      `${JSON.stringify({
        ok: true,
        bun: Bun.version,
        package: `fleetdeck@${expectedVersion}`,
        tarball: tarballName,
        daemonPid: daemon.pid,
        childPid,
        descendants,
        health: { ok: health.ok, startup: health.startup, version: health.version },
        exitCode,
        residue,
        outputBytes: {
          stdout: Buffer.byteLength(stdout),
          stderr: Buffer.byteLength(stderr),
        },
      })}\n`,
    );
  } catch (error) {
    await stopForCleanup(daemon);
    const [stdout, stderr] = await Promise.all([
      daemonStdout ?? Promise.resolve(''),
      daemonStderr ?? Promise.resolve(''),
    ]);
    if (stdout || stderr) {
      process.stderr.write(`--- packed daemon stdout ---\n${stdout}`);
      process.stderr.write(`--- packed daemon stderr ---\n${stderr}`);
    }
    throw error;
  } finally {
    await stopForCleanup(daemon);
    if (childPid !== null && isPidAlive(childPid)) {
      try {
        process.kill(childPid, 'SIGKILL');
      } catch {
        // The daemon normally reaps the fixture before this fallback runs.
      }
    }
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

await main();
