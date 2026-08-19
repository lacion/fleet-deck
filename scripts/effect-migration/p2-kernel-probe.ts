import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { arch, cpus, platform, release, tmpdir, totalmem } from 'node:os';
import path from 'node:path';

import {
  assertCommand,
  type CommandResult,
  REPO_ROOT,
  runCommand,
  summarize,
  writeJsonReport,
} from './metrics.ts';

const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
const EXPECTED_EFFECT_VERSION = '4.0.0-rc.110';
const BUILD_RUNS = 2;
const EXECUTION_RUNS = 10;
const GZIP_LEVEL = 9;
const GZIP_LIBRARY = 'zlib';
// Recorded by both P0 effect-probe evidence runs under the same Bun/Effect cohort.
const P0_PROBE_BUNDLE_BYTES = 83_456;
const FIXTURE = path.join('scripts', 'effect-migration', 'fixtures', 'p2-kernel-probe.ts');

interface ProbeResult {
  version: string;
  port: number;
  process: { ok: true; out: string };
}

function sha256(bytes: Uint8Array | string): string {
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
}

function rounded(value: number): number {
  return Math.round(value * 1_000) / 1_000;
}

function parseProbe(result: CommandResult, label: string): ProbeResult {
  assertCommand(result);
  if (result.stderr !== '') throw new Error(`${label} wrote unexpected stderr: ${result.stderr}`);
  let parsed: unknown;
  try {
    parsed = JSON.parse(result.stdout.trim()) as unknown;
  } catch (error) {
    throw new Error(
      `${label} did not emit one JSON result: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  const expected: ProbeResult = {
    version: 'p2-kernel-probe',
    port: 4711,
    process: {
      ok: true,
      out: 'p2-kernel-probe:fleetdeck-probe\u0000--direct-argv\u0000value with spaces',
    },
  };
  if (JSON.stringify(parsed) !== JSON.stringify(expected)) {
    throw new Error(`${label} changed kernel semantics: ${JSON.stringify(parsed)}`);
  }
  return expected;
}

function normalizeCommand(result: CommandResult, scratch: string): CommandResult {
  const replacements = [
    [scratch, '<scratch>'],
    [process.execPath, '<bun>'],
    [REPO_ROOT, '<repo>'],
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

function relativeDelta(value: number, baseline: number): { bytes: number; percent: number } {
  return { bytes: value - baseline, percent: rounded(((value - baseline) / baseline) * 100) };
}

if (Bun.version !== EXPECTED_BUN_VERSION || Bun.revision !== EXPECTED_BUN_REVISION) {
  throw new Error(
    `P2 kernel probe requires Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${Bun.version} (${Bun.revision})`,
  );
}

const effectPackage = await Bun.file(
  path.join(REPO_ROOT, 'node_modules', 'effect', 'package.json'),
).json();
const platformBunPackage = await Bun.file(
  path.join(REPO_ROOT, 'node_modules', '@effect', 'platform-bun', 'package.json'),
).json();
if (
  effectPackage.version !== EXPECTED_EFFECT_VERSION ||
  platformBunPackage.version !== EXPECTED_EFFECT_VERSION
) {
  throw new Error(
    `P2 kernel probe requires exact RC.110 packages; got effect=${String(effectPackage.version)} platform-bun=${String(platformBunPackage.version)}`,
  );
}

const runtime = {
  bun: Bun.version,
  revision: Bun.revision,
  effect: effectPackage.version as string,
  platformBun: platformBunPackage.version as string,
};
const cpuList = cpus();
const machine = {
  platform: platform(),
  release: release(),
  arch: arch(),
  cpu: cpuList[0]?.model ?? 'unknown',
  logicalCpuCount: cpuList.length,
  totalMemoryBytes: totalmem(),
};
const config = {
  buildRuns: BUILD_RUNS,
  executionRuns: EXECUTION_RUNS,
  fixture: FIXTURE,
  gzip: { api: 'Bun.gzipSync', level: GZIP_LEVEL, library: GZIP_LIBRARY },
};
const comparisonKey = sha256(JSON.stringify({ runtime, machine, config }));

const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-p2-kernel-'));
try {
  const sourceRuns: CommandResult[] = [];
  for (let index = 0; index < EXECUTION_RUNS; index++) {
    const run = await runCommand([process.execPath, FIXTURE], {
      cwd: REPO_ROOT,
      timeoutMs: 5_000,
    });
    parseProbe(run, `source run ${index + 1}`);
    sourceRuns.push(normalizeCommand(run, scratch));
  }

  const buildRuns: Array<{
    command: CommandResult;
    bytes: number;
    gzipBytes: number;
    sha256: string;
    gzipSha256: string;
  }> = [];
  for (let index = 0; index < BUILD_RUNS; index++) {
    const output = path.join(scratch, `kernel-probe-${index + 1}.mjs`);
    const build = assertCommand(
      await runCommand(
        [process.execPath, 'build', FIXTURE, '--target=bun', '--format=esm', `--outfile=${output}`],
        { cwd: REPO_ROOT, timeoutMs: 30_000 },
      ),
    );
    const bytes = readFileSync(output);
    const gzip = Bun.gzipSync(bytes, { level: GZIP_LEVEL, library: GZIP_LIBRARY });
    buildRuns.push({
      command: normalizeCommand(build, scratch),
      bytes: statSync(output).size,
      gzipBytes: gzip.byteLength,
      sha256: sha256(bytes),
      gzipSha256: sha256(gzip),
    });
  }
  const firstBuild = buildRuns[0];
  if (!firstBuild) throw new Error('P2 kernel probe produced no build');
  if (
    buildRuns.some(
      (run) =>
        run.bytes !== firstBuild.bytes ||
        run.gzipBytes !== firstBuild.gzipBytes ||
        run.sha256 !== firstBuild.sha256 ||
        run.gzipSha256 !== firstBuild.gzipSha256,
    )
  ) {
    throw new Error('P2 kernel probe builds were not byte-for-byte deterministic');
  }

  const bundledRuns: CommandResult[] = [];
  const bundlePath = path.join(scratch, 'kernel-probe-1.mjs');
  for (let index = 0; index < EXECUTION_RUNS; index++) {
    const run = await runCommand([process.execPath, bundlePath], {
      cwd: REPO_ROOT,
      timeoutMs: 5_000,
    });
    parseProbe(run, `bundle run ${index + 1}`);
    bundledRuns.push(normalizeCommand(run, scratch));
  }

  const stableFacts = {
    runtime,
    machine,
    config,
    result: parseProbe(sourceRuns[0] as CommandResult, 'stable source result'),
    artifact: {
      bytes: firstBuild.bytes,
      gzipBytes: firstBuild.gzipBytes,
      sha256: firstBuild.sha256,
      gzipSha256: firstBuild.gzipSha256,
    },
    deterministic: true,
    naturalSourceExit: sourceRuns.every((run) => run.exitCode === 0),
    naturalBundleExit: bundledRuns.every((run) => run.exitCode === 0),
  };
  await writeJsonReport({
    schema: 1,
    kind: 'fleetdeck-effect-p2-kernel-probe',
    recordedAt: new Date().toISOString(),
    ok: true,
    stableEvidenceSha256: sha256(JSON.stringify(stableFacts)),
    ...stableFacts,
    comparison: {
      key: comparisonKey,
      requirement: 'compare timings only with identical key on the same idle machine',
      p0StandaloneProbeBytes: P0_PROBE_BUNDLE_BYTES,
      artifactDelta: relativeDelta(firstBuild.bytes, P0_PROBE_BUNDLE_BYTES),
    },
    timing: {
      buildMs: summarize(buildRuns.map((run) => run.command.durationMs)),
      sourceProcessMs: summarize(sourceRuns.map((run) => run.durationMs)),
      bundleProcessMs: summarize(bundledRuns.map((run) => run.durationMs)),
    },
    commands: { builds: buildRuns.map((run) => run.command), sourceRuns, bundledRuns },
  });
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
