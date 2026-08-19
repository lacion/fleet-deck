import { cpSync, mkdtempSync, readdirSync, realpathSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { assertCommand, REPO_ROOT, runCommand, writeJsonReport } from './metrics.ts';

interface TreeSize {
  bytes: number;
  files: number;
}

interface ProbeEvent {
  event?: unknown;
  mode?: unknown;
  exit?: unknown;
  [key: string]: unknown;
}

const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
const EXPECTED_EFFECT_VERSION = '4.0.0-rc.110';
const MAX_INSTALLED_BYTES = 75 * 1024 * 1024;
const MAX_RESOLVED_PACKAGES = 24;

function treeSize(root: string): TreeSize {
  let bytes = 0;
  let files = 0;
  const visit = (dir: string): void => {
    for (const name of readdirSync(dir)) {
      const target = path.join(dir, name);
      const stat = statSync(target, { throwIfNoEntry: false });
      if (!stat) continue;
      if (stat.isDirectory()) visit(target);
      else if (stat.isFile()) {
        bytes += stat.size;
        files += 1;
      }
    }
  };
  visit(root);
  return { bytes, files };
}

function parseJsonLines(text: string): unknown[] {
  return text
    .trim()
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line) as unknown);
}

function sha256(bytes: Uint8Array): string {
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
}

function probeEvents(value: unknown[]): ProbeEvent[] {
  return value.map((entry, index) => {
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
      throw new Error(`probe event ${index} was not an object`);
    }
    return entry as ProbeEvent;
  });
}

function assertNaturalEvents(label: string, value: unknown[]): ProbeEvent[] {
  const events = probeEvents(value);
  const names = events.map((entry) => entry.event);
  if (JSON.stringify(names) !== JSON.stringify(['acquire', 'complete', 'release'])) {
    throw new Error(`${label} natural event order changed: ${JSON.stringify(names)}`);
  }
  if (events.some((entry) => entry.mode !== 'natural') || events.at(-1)?.exit !== 'Success') {
    throw new Error(`${label} natural execution did not complete and release successfully`);
  }
  return events;
}

function assertInterruptedEvents(signal: string, stdout: unknown): ProbeEvent[] {
  if (!Array.isArray(stdout) || stdout.some((line) => typeof line !== 'string')) {
    throw new Error(`${signal} did not report line-oriented probe output`);
  }
  const events = probeEvents((stdout as string[]).map((line) => JSON.parse(line) as unknown));
  const names = events.map((entry) => entry.event);
  if (JSON.stringify(names) !== JSON.stringify(['acquire', 'ready', 'release'])) {
    throw new Error(`${signal} interruption event order changed: ${JSON.stringify(names)}`);
  }
  if (events.some((entry) => entry.mode !== 'hold') || events.at(-1)?.exit !== 'Failure') {
    throw new Error(`${signal} did not interrupt and release the held root`);
  }
  return events;
}

function normalizedCommand(
  result: Awaited<ReturnType<typeof runCommand>>,
  scratch: string,
): Awaited<ReturnType<typeof runCommand>> {
  const replacements = [
    [realpathSync(scratch), '<scratch>'],
    [scratch, '<scratch>'],
    [process.execPath, '<bun>'],
  ] as const;
  const normalize = (value: string): string => {
    let normalized = value;
    for (const [from, to] of replacements) normalized = normalized.replaceAll(from, to);
    return normalized;
  };
  return {
    ...result,
    command: result.command.map(normalize),
    cwd: normalize(result.cwd),
    stdout: normalize(result.stdout),
    stderr: normalize(result.stderr),
  };
}

const keep = Bun.argv.includes('--keep');
const fixture = path.join(REPO_ROOT, 'scripts/effect-migration/fixtures/p0-probe');
const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-p0-'));
cpSync(fixture, scratch, { recursive: true });
cpSync(path.join(scratch, 'probe.ts.fixture'), path.join(scratch, 'probe.ts'));
cpSync(path.join(scratch, 'signal-probe.ts.fixture'), path.join(scratch, 'signal-probe.ts'));

try {
  if (Bun.version !== EXPECTED_BUN_VERSION || Bun.revision !== EXPECTED_BUN_REVISION) {
    throw new Error(
      `P0 floor probe requires Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${Bun.version} (${Bun.revision})`,
    );
  }
  const install = assertCommand(
    await runCommand([process.execPath, 'install', '--frozen-lockfile', '--ignore-scripts'], {
      cwd: scratch,
      timeoutMs: 30_000,
    }),
  );
  const source = assertCommand(
    await runCommand([process.execPath, 'probe.ts', 'natural'], {
      cwd: scratch,
      timeoutMs: 5_000,
    }),
  );
  const build = assertCommand(
    await runCommand(
      [
        process.execPath,
        'build',
        'probe.ts',
        '--target=bun',
        '--format=esm',
        '--outfile=probe.bundle.mjs',
      ],
      { cwd: scratch, timeoutMs: 30_000 },
    ),
  );
  const bundled = assertCommand(
    await runCommand([process.execPath, 'probe.bundle.mjs', 'natural'], {
      cwd: scratch,
      timeoutMs: 5_000,
    }),
  );
  const signals = assertCommand(
    await runCommand([process.execPath, 'signal-probe.ts'], {
      cwd: scratch,
      timeoutMs: 15_000,
    }),
  );
  const dependencyTree = assertCommand(
    await runCommand([process.execPath, 'pm', 'ls', '--all'], {
      cwd: scratch,
      timeoutMs: 5_000,
    }),
  );

  const sourceEvents = assertNaturalEvents('source', parseJsonLines(source.stdout));
  const bundleEvents = assertNaturalEvents('bundle', parseJsonLines(bundled.stdout));
  const signalEvents = parseJsonLines(signals.stdout) as {
    signal?: unknown;
    exitCode?: unknown;
    stdout?: unknown[];
    stderr?: unknown;
    forcedCleanup?: unknown;
    keepAliveObservationMs?: unknown;
    aliveBeforeSignal?: unknown;
    exitCodeBeforeSignal?: unknown;
  }[];
  const interruptedEvents: Record<string, ProbeEvent[]> = {};
  for (const signal of ['SIGINT', 'SIGTERM']) {
    const observed = signalEvents.find((entry) => entry.signal === signal);
    if (
      observed?.exitCode !== 130 ||
      observed.forcedCleanup !== false ||
      observed.stderr !== '' ||
      observed.aliveBeforeSignal !== true ||
      observed.exitCodeBeforeSignal !== null ||
      typeof observed.keepAliveObservationMs !== 'number' ||
      observed.keepAliveObservationMs < 250
    ) {
      throw new Error(`${signal} did not interrupt the root and run the scope finalizer`);
    }
    interruptedEvents[signal] = assertInterruptedEvents(signal, observed.stdout);
  }
  if (signalEvents.length !== 2) throw new Error('signal probe reported an unexpected extra run');

  const effectPackage = await Bun.file(
    path.join(scratch, 'node_modules/effect/package.json'),
  ).json();
  const bunPackage = await Bun.file(
    path.join(scratch, 'node_modules/@effect/platform-bun/package.json'),
  ).json();
  if (
    effectPackage.version !== EXPECTED_EFFECT_VERSION ||
    bunPackage.version !== EXPECTED_EFFECT_VERSION
  ) {
    throw new Error(
      `exact Effect cohort drifted: effect=${effectPackage.version}, platform-bun=${bunPackage.version}`,
    );
  }
  const installedTree = treeSize(path.join(scratch, 'node_modules'));
  const resolvedPackageCount = (dependencyTree.stdout.match(/^[├└]── /gm) ?? []).length;
  if (installedTree.bytes > MAX_INSTALLED_BYTES) {
    throw new Error(`Effect probe install exceeded ${MAX_INSTALLED_BYTES} bytes`);
  }
  if (resolvedPackageCount > MAX_RESOLVED_PACKAGES) {
    throw new Error(`Effect probe resolved ${resolvedPackageCount} packages`);
  }
  const lockBytes = await Bun.file(path.join(scratch, 'bun.lock')).bytes();
  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p0-probe',
    recordedAt: new Date().toISOString(),
    runtime: { bun: Bun.version, revision: Bun.revision },
    scratch: keep ? '<scratch>' : '(removed after probe)',
    commands: {
      install: normalizedCommand(install, scratch),
      source: normalizedCommand(source, scratch),
      build: normalizedCommand(build, scratch),
      bundled: normalizedCommand(bundled, scratch),
      signals: normalizedCommand(signals, scratch),
      dependencyTree: normalizedCommand(dependencyTree, scratch),
    },
    verification: {
      exactVersions: {
        effect: effectPackage.version,
        platformBun: bunPackage.version,
      },
      sourceEvents,
      bundleEvents,
      signalEvents,
      interruptedEvents,
      rootKeepAliveObserved: Object.values(interruptedEvents).every((events) =>
        events.some((entry) => entry.event === 'ready'),
      ),
      fixtureLockSha256: sha256(lockBytes),
      resolvedPackageCount,
      installedTree,
      probeBundleBytes: statSync(path.join(scratch, 'probe.bundle.mjs')).size,
    },
  });
} finally {
  if (!keep) rmSync(scratch, { recursive: true, force: true });
}
