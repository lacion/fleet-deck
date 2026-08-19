import path from 'node:path';

import { REPO_ROOT, writeJsonReport } from './metrics.ts';

type JsonObject = Record<string, unknown>;

type EvidenceKind =
  | 'fleetdeck-effect-p0-baseline'
  | 'fleetdeck-effect-p0-exec-bench'
  | 'fleetdeck-effect-p0-workloads'
  | 'fleetdeck-effect-p0-probe';

interface ValidatedEvidence {
  kind: EvidenceKind;
  runtime: JsonObject;
  machine?: JsonObject;
  config?: JsonObject;
  stableFacts: JsonObject;
  healthChecks: string[];
}

interface NumericDelta {
  absolute: number;
  percent: number | null;
}

const EXPECTED_BUN_VERSION = '1.3.14';
const EXPECTED_BUN_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
const EXPECTED_EFFECT_VERSION = '4.0.0-rc.110';
const SHA256 = /^[0-9a-f]{64}$/;
const KNOWN_KINDS = new Set<EvidenceKind>([
  'fleetdeck-effect-p0-baseline',
  'fleetdeck-effect-p0-exec-bench',
  'fleetdeck-effect-p0-workloads',
  'fleetdeck-effect-p0-probe',
]);

function fail(message: string): never {
  throw new Error(message);
}

function isObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function objectAt(value: unknown, location: string): JsonObject {
  if (!isObject(value)) fail(`${location} must be an object`);
  return value;
}

function arrayAt(value: unknown, location: string): unknown[] {
  if (!Array.isArray(value)) fail(`${location} must be an array`);
  return value;
}

function stringAt(value: unknown, location: string): string {
  if (typeof value !== 'string' || value.length === 0)
    fail(`${location} must be a non-empty string`);
  return value;
}

function numberAt(value: unknown, location: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    fail(`${location} must be a finite number`);
  }
  return value;
}

function integerAt(value: unknown, location: string): number {
  const result = numberAt(value, location);
  if (!Number.isSafeInteger(result)) fail(`${location} must be a safe integer`);
  return result;
}

function booleanAt(value: unknown, location: string): boolean {
  if (typeof value !== 'boolean') fail(`${location} must be a boolean`);
  return value;
}

function trueAt(value: unknown, location: string): void {
  if (value !== true) fail(`${location} must be true`);
}

function falseAt(value: unknown, location: string): void {
  if (value !== false) fail(`${location} must be false`);
}

function shaAt(value: unknown, location: string): string {
  const result = stringAt(value, location);
  if (!SHA256.test(result)) fail(`${location} must be a lowercase SHA-256 digest`);
  return result;
}

function property(root: JsonObject, dottedPath: string): unknown {
  let current: unknown = root;
  for (const segment of dottedPath.split('.')) {
    if (!isObject(current) || !Object.hasOwn(current, segment)) {
      fail(`${dottedPath} is required`);
    }
    current = current[segment];
  }
  return current;
}

function requireTruePath(root: JsonObject, dottedPath: string): void {
  trueAt(property(root, dottedPath), dottedPath);
}

function optionValue(name: '--left' | '--right'): string {
  const args = Bun.argv.slice(2);
  const values: string[] = [];
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === name) {
      const value = args[index + 1];
      if (!value || value.startsWith('--')) fail(`${name} requires a path`);
      values.push(value);
      index += 1;
    } else if (argument?.startsWith(`${name}=`)) {
      const value = argument.slice(name.length + 1);
      if (!value) fail(`${name} requires a path`);
      values.push(value);
    }
  }
  if (values.length === 0) fail(`${name} is required`);
  if (values.length !== 1) fail(`${name} must be passed exactly once`);
  return values[0] as string;
}

async function readEvidence(inputPath: string, label: 'left' | 'right'): Promise<JsonObject> {
  const absolute = path.resolve(inputPath);
  if (!(await Bun.file(absolute).exists())) fail(`${label} evidence does not exist: ${inputPath}`);
  let parsed: unknown;
  try {
    parsed = await Bun.file(absolute).json();
  } catch (error) {
    fail(
      `${label} evidence is not valid JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  return objectAt(parsed, `${label} evidence`);
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (isObject(value)) {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

function sha256(value: unknown): string {
  return new Bun.CryptoHasher('sha256').update(canonical(value)).digest('hex');
}

function firstDifference(left: unknown, right: unknown, location = '$'): string | null {
  if (Object.is(left, right)) return null;
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right)) return `${location} changed type`;
    if (left.length !== right.length) {
      return `${location}.length changed from ${left.length} to ${right.length}`;
    }
    for (let index = 0; index < left.length; index += 1) {
      const difference = firstDifference(left[index], right[index], `${location}[${index}]`);
      if (difference) return difference;
    }
    return null;
  }
  if (isObject(left) || isObject(right)) {
    if (!isObject(left) || !isObject(right)) return `${location} changed type`;
    const leftKeys = Object.keys(left).sort();
    const rightKeys = Object.keys(right).sort();
    if (canonical(leftKeys) !== canonical(rightKeys)) return `${location} changed keys`;
    for (const key of leftKeys) {
      const difference = firstDifference(left[key], right[key], `${location}.${key}`);
      if (difference) return difference;
    }
    return null;
  }
  return `${location} changed from ${JSON.stringify(left)} to ${JSON.stringify(right)}`;
}

function assertEqual(left: unknown, right: unknown, label: string): void {
  const difference = firstDifference(left, right);
  if (difference) fail(`${label} differs: ${difference}`);
}

function validateRuntime(runtimeValue: unknown, label: string): JsonObject {
  const runtime = objectAt(runtimeValue, `${label}.runtime`);
  const bun = stringAt(runtime['bun'], `${label}.runtime.bun`);
  const revision = stringAt(runtime['revision'], `${label}.runtime.revision`);
  if (bun !== EXPECTED_BUN_VERSION || revision !== EXPECTED_BUN_REVISION) {
    fail(
      `${label} must record Bun ${EXPECTED_BUN_VERSION} (${EXPECTED_BUN_REVISION}); got ${bun} (${revision})`,
    );
  }
  return runtime;
}

function validateLifecycleRuntimeFloor(
  value: unknown,
  runtime: JsonObject,
  location: string,
): JsonObject {
  const floor = objectAt(value, location);
  trueAt(floor['exactMatch'], `${location}.exactMatch`);
  const expected = objectAt(floor['expected'], `${location}.expected`);
  const actual = objectAt(floor['actual'], `${location}.actual`);
  for (const [name, candidate] of [
    ['expected', expected],
    ['actual', actual],
  ] as const) {
    if (
      candidate['bun'] !== EXPECTED_BUN_VERSION ||
      candidate['revision'] !== EXPECTED_BUN_REVISION
    ) {
      fail(`${location}.${name} must record the exact Bun floor`);
    }
  }
  if (actual['bun'] !== runtime['bun'] || actual['revision'] !== runtime['revision']) {
    fail(`${location}.actual does not match the report runtime`);
  }
  return expected;
}

function validateChecks(value: unknown, location: string, allowNull: boolean): string[] {
  const checks = objectAt(value, location);
  const entries = Object.entries(checks);
  if (entries.length === 0) fail(`${location} must not be empty`);
  for (const [name, result] of entries) {
    if (result === true || (allowNull && result === null)) continue;
    fail(`${location}.${name} is not healthy`);
  }
  return entries.map(([name]) => `${location}.${name}`);
}

function validateManifest(value: unknown, location: string): JsonObject {
  const manifest = objectAt(value, location);
  numberAt(manifest['bytes'], `${location}.bytes`);
  shaAt(manifest['sha256'], `${location}.sha256`);
  const artifacts = arrayAt(manifest['artifacts'], `${location}.artifacts`);
  if (artifacts.length === 0) fail(`${location}.artifacts must not be empty`);
  for (const [index, entry] of artifacts.entries()) {
    const artifact = objectAt(entry, `${location}.artifacts[${index}]`);
    stringAt(artifact['path'], `${location}.artifacts[${index}].path`);
    numberAt(artifact['bytes'], `${location}.artifacts[${index}].bytes`);
    shaAt(artifact['sha256'], `${location}.artifacts[${index}].sha256`);
  }
  return manifest;
}

function validatePackageContents(value: unknown, fileCount: number, location: string): unknown[] {
  const contents = arrayAt(value, location);
  if (fileCount !== contents.length || contents.length === 0) {
    fail(`${location} must match its non-zero fileCount`);
  }
  for (const [index, entry] of contents.entries()) {
    const file = objectAt(entry, `${location}[${index}]`);
    stringAt(file['path'], `${location}[${index}].path`);
    integerAt(file['bytes'], `${location}[${index}].bytes`);
    shaAt(file['sha256'], `${location}[${index}].sha256`);
  }
  return contents;
}

function normalizedPackageManifest(contents: readonly unknown[], location: string): string {
  const hasher = new Bun.CryptoHasher('sha256');
  for (const [index, entry] of contents.entries()) {
    const file = objectAt(entry, `${location}[${index}]`);
    hasher.update(`${file['path']}\0${file['bytes']}\0${file['sha256']}\n`);
  }
  return hasher.digest('hex');
}

function validateBaseline(document: JsonObject, label: string): ValidatedEvidence {
  trueAt(document['ok'], `${label}.ok`);
  const violations = arrayAt(document['violations'], `${label}.violations`);
  if (violations.length !== 0) fail(`${label}.violations must be empty`);

  const config = objectAt(document['config'], `${label}.config`);
  const metadata = objectAt(document['metadata'], `${label}.metadata`);
  const runtime = validateRuntime(metadata['runtime'], `${label}.metadata`);
  const runtimeFloor = validateLifecycleRuntimeFloor(
    document['runtimeFloor'],
    runtime,
    `${label}.runtimeFloor`,
  );
  const machine = objectAt(metadata['machine'], `${label}.metadata.machine`);
  const commit = stringAt(metadata['commit'], `${label}.metadata.commit`);
  const testFiles = integerAt(metadata['testFiles'], `${label}.metadata.testFiles`);
  const comparison = objectAt(document['comparison'], `${label}.comparison`);
  const comparisonKey = shaAt(comparison['key'], `${label}.comparison.key`);
  const computedComparisonKey = new Bun.CryptoHasher('sha256')
    .update(JSON.stringify({ commit, runtime, machine, config }))
    .digest('hex');
  if (comparisonKey !== computedComparisonKey) {
    fail(`${label}.comparison.key does not match its commit, runtime, machine, and config`);
  }

  const artifact = objectAt(document['artifact'], `${label}.artifact`);
  stringAt(artifact['path'], `${label}.artifact.path`);
  numberAt(artifact['rawBytes'], `${label}.artifact.rawBytes`);
  shaAt(artifact['rawSha256'], `${label}.artifact.rawSha256`);
  numberAt(artifact['gzipBytes'], `${label}.artifact.gzipBytes`);
  shaAt(artifact['gzipSha256'], `${label}.artifact.gzipSha256`);
  const gzip = objectAt(artifact['gzip'], `${label}.artifact.gzip`);
  if (gzip['api'] !== 'Bun.gzipSync' || gzip['level'] !== 9 || gzip['library'] !== 'zlib') {
    fail(`${label}.artifact.gzip must record Bun.gzipSync at zlib level 9`);
  }

  const build = objectAt(document['build'], `${label}.build`);
  trueAt(build['deterministic'], `${label}.build.deterministic`);
  const pipeline = arrayAt(build['pipeline'], `${label}.build.pipeline`);
  if (pipeline.length === 0 || pipeline.some((entry) => typeof entry !== 'string')) {
    fail(`${label}.build.pipeline must be a non-empty string array`);
  }
  const buildRuns = arrayAt(build['runs'], `${label}.build.runs`);
  const configuredBuilds = integerAt(config['builds'], `${label}.config.builds`);
  if (buildRuns.length !== configuredBuilds || buildRuns.length < 2) {
    fail(`${label}.build.runs must contain the configured repeatable builds`);
  }
  const manifests = buildRuns.map((run, index) =>
    validateManifest(
      objectAt(run, `${label}.build.runs[${index}]`)['manifest'],
      `${label}.build.runs[${index}].manifest`,
    ),
  );
  for (const manifest of manifests.slice(1)) {
    assertEqual(manifests[0], manifest, `${label} build manifests`);
  }

  const packed = objectAt(document['package'], `${label}.package`);
  trueAt(packed['normalizedDeterministic'], `${label}.package.normalizedDeterministic`);
  booleanAt(packed['rawTarballDeterministic'], `${label}.package.rawTarballDeterministic`);
  const packRuns = arrayAt(packed['runs'], `${label}.package.runs`);
  const configuredPacks = integerAt(config['packs'], `${label}.config.packs`);
  if (packRuns.length !== configuredPacks || packRuns.length < 2) {
    fail(`${label}.package.runs must contain the configured repeatable packs`);
  }
  const normalizedManifests: string[] = [];
  for (const [index, entry] of packRuns.entries()) {
    const run = objectAt(entry, `${label}.package.runs[${index}]`);
    stringAt(run['filename'], `${label}.package.runs[${index}].filename`);
    integerAt(run['bytes'], `${label}.package.runs[${index}].bytes`);
    shaAt(run['sha256'], `${label}.package.runs[${index}].sha256`);
    const normalizedManifestSha256 = shaAt(
      run['normalizedManifestSha256'],
      `${label}.package.runs[${index}].normalizedManifestSha256`,
    );
    const runFileCount = integerAt(run['fileCount'], `${label}.package.runs[${index}].fileCount`);
    const runContents = validatePackageContents(
      run['contents'],
      runFileCount,
      `${label}.package.runs[${index}].contents`,
    );
    if (
      normalizedManifestSha256 !==
      normalizedPackageManifest(runContents, `${label}.package.runs[${index}].contents`)
    ) {
      fail(`${label}.package.runs[${index}] normalized manifest digest is incorrect`);
    }
    normalizedManifests.push(normalizedManifestSha256);
  }
  if (new Set(normalizedManifests).size !== 1) {
    fail(`${label}.package.runs have different normalized manifests`);
  }
  const firstPack = objectAt(packRuns[0], `${label}.package.runs[0]`);
  if (packed['bytes'] !== firstPack['bytes']) {
    fail(`${label}.package.bytes does not match the first pack run`);
  }
  const fileCount = integerAt(packed['fileCount'], `${label}.package.fileCount`);
  const contents = validatePackageContents(
    packed['contents'],
    fileCount,
    `${label}.package.contents`,
  );
  if (fileCount !== firstPack['fileCount']) {
    fail(`${label}.package.fileCount does not match the first pack run`);
  }
  assertEqual(contents, firstPack['contents'], `${label} top-level package contents`);

  const daemon = objectAt(document['daemon'], `${label}.daemon`);
  const coldLaunches = objectAt(daemon['coldLaunches'], `${label}.daemon.coldLaunches`);
  const forcedShutdowns = objectAt(daemon['forcedShutdowns'], `${label}.daemon.forcedShutdowns`);
  const mdnsStartup = objectAt(daemon['mdnsStartup'], `${label}.daemon.mdnsStartup`);
  const coldRuns = arrayAt(coldLaunches['runs'], `${label}.daemon.coldLaunches.runs`);
  const forcedRuns = arrayAt(forcedShutdowns['runs'], `${label}.daemon.forcedShutdowns.runs`);
  const mdnsRuns = arrayAt(mdnsStartup['runs'], `${label}.daemon.mdnsStartup.runs`);
  if (coldRuns.length !== integerAt(config['launches'], `${label}.config.launches`)) {
    fail(`${label}.daemon.coldLaunches.runs does not match config.launches`);
  }
  if (forcedRuns.length !== integerAt(config['forcedLaunches'], `${label}.config.forcedLaunches`)) {
    fail(`${label}.daemon.forcedShutdowns.runs does not match config.forcedLaunches`);
  }
  if (mdnsRuns.length !== integerAt(config['mdnsLaunches'], `${label}.config.mdnsLaunches`)) {
    fail(`${label}.daemon.mdnsStartup.runs does not match config.mdnsLaunches`);
  }
  if (mdnsStartup['bind'] !== '0.0.0.0') fail(`${label}.daemon.mdnsStartup.bind is incorrect`);
  trueAt(mdnsStartup['enabled'], `${label}.daemon.mdnsStartup.enabled`);
  const mdnsGoodbye = objectAt(
    mdnsStartup['gracefulGoodbye'],
    `${label}.daemon.mdnsStartup.gracefulGoodbye`,
  );
  trueAt(
    mdnsGoodbye['withinWatchdog'],
    `${label}.daemon.mdnsStartup.gracefulGoodbye.withinWatchdog`,
  );
  const mdnsWatchdogMs = integerAt(config['mdnsWatchdogMs'], `${label}.config.mdnsWatchdogMs`);
  if (mdnsGoodbye['implementationWatchdogMs'] !== mdnsWatchdogMs) {
    fail(`${label}.daemon.mdnsStartup watchdog does not match config.mdnsWatchdogMs`);
  }

  const healthChecks: string[] = [
    `${label}.ok`,
    `${label}.runtimeFloor.exactMatch`,
    `${label}.build.deterministic`,
    `${label}.package.normalizedDeterministic`,
    `${label}.daemon.mdnsStartup.gracefulGoodbye.withinWatchdog`,
  ];
  for (const [index, run] of coldRuns.entries()) {
    const lifecycle = objectAt(run, `${label}.daemon.coldLaunches.runs[${index}]`);
    if (lifecycle['mode'] !== 'graceful') {
      fail(`${label}.daemon.coldLaunches.runs[${index}].mode must be graceful`);
    }
    healthChecks.push(
      ...validateChecks(
        lifecycle['checks'],
        `${label}.daemon.coldLaunches.runs[${index}].checks`,
        false,
      ),
    );
  }
  for (const [index, run] of forcedRuns.entries()) {
    const lifecycle = objectAt(run, `${label}.daemon.forcedShutdowns.runs[${index}]`);
    if (lifecycle['mode'] !== 'sigkill') {
      fail(`${label}.daemon.forcedShutdowns.runs[${index}].mode must be sigkill`);
    }
    healthChecks.push(
      ...validateChecks(
        lifecycle['checks'],
        `${label}.daemon.forcedShutdowns.runs[${index}].checks`,
        true,
      ),
    );
  }
  for (const [index, run] of mdnsRuns.entries()) {
    const lifecycle = objectAt(run, `${label}.daemon.mdnsStartup.runs[${index}]`);
    if (lifecycle['mode'] !== 'graceful') {
      fail(`${label}.daemon.mdnsStartup.runs[${index}].mode must be graceful`);
    }
    if (
      numberAt(lifecycle['shutdownMs'], `${label}.daemon.mdnsStartup.runs[${index}].shutdownMs`) >
      mdnsWatchdogMs
    ) {
      fail(`${label}.daemon.mdnsStartup.runs[${index}] exceeded the mDNS watchdog`);
    }
    healthChecks.push(
      ...validateChecks(
        lifecycle['checks'],
        `${label}.daemon.mdnsStartup.runs[${index}].checks`,
        false,
      ),
    );
  }
  healthChecks.push(
    ...validateChecks(
      objectAt(daemon['steadyShutdown'], `${label}.daemon.steadyShutdown`)['checks'],
      `${label}.daemon.steadyShutdown.checks`,
      false,
    ),
  );
  requireTruePath(document, 'daemon.commands.hook.withinDeadline');
  requireTruePath(document, 'daemon.commands.cli.withinDeadline');
  const commandRuns = integerAt(config['commandRuns'], `${label}.config.commandRuns`);
  for (const name of ['hook', 'cli']) {
    const command = objectAt(
      property(document, `daemon.commands.${name}`),
      `${label}.daemon.commands.${name}`,
    );
    const duration = objectAt(command['durationMs'], `${label}.daemon.commands.${name}.durationMs`);
    if (
      integerAt(duration['count'], `${label}.daemon.commands.${name}.durationMs.count`) !==
      commandRuns
    ) {
      fail(`${label}.daemon.commands.${name} did not record config.commandRuns samples`);
    }
  }
  const idle = objectAt(daemon['idle'], `${label}.daemon.idle`);
  trueAt(idle['available'], `${label}.daemon.idle.available`);
  const idleSampleCount = integerAt(idle['sampleCount'], `${label}.daemon.idle.sampleCount`);
  if (idleSampleCount <= 0) fail(`${label}.daemon.idle.sampleCount must be positive`);
  if (arrayAt(idle['samples'], `${label}.daemon.idle.samples`).length !== idleSampleCount) {
    fail(`${label}.daemon.idle.samples does not match sampleCount`);
  }
  for (const name of ['rssBytes', 'reportedCpuPercent']) {
    const distribution = objectAt(idle[name], `${label}.daemon.idle.${name}`);
    if (distribution['count'] !== idleSampleCount) {
      fail(`${label}.daemon.idle.${name}.count does not match sampleCount`);
    }
  }
  healthChecks.push(
    `${label}.daemon.commands.hook.withinDeadline`,
    `${label}.daemon.commands.cli.withinDeadline`,
    `${label}.daemon.idle.available`,
  );

  return {
    kind: 'fleetdeck-effect-p0-baseline',
    runtime,
    machine,
    config,
    stableFacts: {
      comparisonKey,
      runtimeFloor,
      commit,
      testFiles,
      artifact,
      build: { pipeline, manifest: manifests[0] },
      package: {
        normalizedManifestSha256: normalizedManifests[0],
        fileCount,
        contents,
      },
    },
    healthChecks,
  };
}

function validateExecBench(document: JsonObject, label: string): ValidatedEvidence {
  if (document['refused'] === true) fail(`${label} is a refused benchmark report`);
  const runtime = validateRuntime(document['runtime'], label);
  const runtimeFloor = objectAt(document['runtimeFloor'], `${label}.runtimeFloor`);
  trueAt(runtimeFloor['exact'], `${label}.runtimeFloor.exact`);
  const expected = objectAt(runtimeFloor['expected'], `${label}.runtimeFloor.expected`);
  const observed = objectAt(runtimeFloor['observed'], `${label}.runtimeFloor.observed`);
  if (
    expected['bun'] !== EXPECTED_BUN_VERSION ||
    expected['revision'] !== EXPECTED_BUN_REVISION ||
    observed['bun'] !== EXPECTED_BUN_VERSION ||
    observed['revision'] !== EXPECTED_BUN_REVISION
  ) {
    fail(`${label}.runtimeFloor must contain the exact expected and observed Bun floor`);
  }

  const config = objectAt(document['config'], `${label}.config`);
  const baseline = objectAt(document['baseline'], `${label}.baseline`);
  if (
    baseline['module'] !== 'src/daemon/exec.ts' ||
    baseline['export'] !== 'execFileP' ||
    baseline['directArgv'] !== true ||
    baseline['shell'] !== false
  ) {
    fail(`${label}.baseline does not identify the direct-argv execFileP contract`);
  }
  stringAt(baseline['implementation'], `${label}.baseline.implementation`);
  numberAt(baseline['combinedOutputLimitBytes'], `${label}.baseline.combinedOutputLimitBytes`);
  numberAt(baseline['termToKillGraceMs'], `${label}.baseline.termToKillGraceMs`);
  const resourceCapabilities = objectAt(
    document['resourceCapabilities'],
    `${label}.resourceCapabilities`,
  );
  objectAt(document['workloads'], `${label}.workloads`);
  objectAt(document['resources'], `${label}.resources`);
  const verification = objectAt(document['verification'], `${label}.verification`);
  trueAt(verification['passed'], `${label}.verification.passed`);
  const healthChecks = validateChecks(
    verification['checks'],
    `${label}.verification.checks`,
    false,
  );
  healthChecks.push(`${label}.runtimeFloor.exact`, `${label}.verification.passed`);

  const platform = stringAt(runtime['platform'], `${label}.runtime.platform`);
  const architecture = stringAt(runtime['arch'], `${label}.runtime.arch`);
  return {
    kind: 'fleetdeck-effect-p0-exec-bench',
    runtime,
    machine: { platform, arch: architecture },
    config,
    stableFacts: { runtimeFloor: expected, baseline, resourceCapabilities },
    healthChecks,
  };
}

function positiveIntegerAt(value: unknown, location: string): number {
  const result = integerAt(value, location);
  if (result <= 0) fail(`${location} must be positive`);
  return result;
}

function nonNegativeNumberAt(value: unknown, location: string): number {
  const result = numberAt(value, location);
  if (result < 0) fail(`${location} must be non-negative`);
  return result;
}

function validateDistributionCount(value: unknown, expected: number, location: string): JsonObject {
  const distribution = objectAt(value, location);
  if (integerAt(distribution['count'], `${location}.count`) !== expected) {
    fail(`${location}.count must be ${expected}`);
  }
  for (const name of ['min', 'p50', 'p95', 'p99', 'max', 'mean']) {
    nonNegativeNumberAt(distribution[name], `${location}.${name}`);
  }
  return distribution;
}

function validateOperationMatrix(
  value: unknown,
  concurrency: readonly number[],
  baseCount: number,
  location: string,
): JsonObject[] {
  const matrix = arrayAt(value, location).map((entry, index) =>
    objectAt(entry, `${location}[${index}]`),
  );
  if (matrix.length !== concurrency.length) {
    fail(`${location} must contain one measurement per configured concurrency`);
  }
  for (const [index, measurement] of matrix.entries()) {
    const expectedConcurrency = concurrency[index];
    if (expectedConcurrency === undefined)
      fail(`${location}[${index}] has no configured concurrency`);
    if (measurement['concurrency'] !== expectedConcurrency) {
      fail(`${location}[${index}].concurrency must be ${expectedConcurrency}`);
    }
    const expectedCount = Math.max(baseCount, expectedConcurrency);
    if (measurement['count'] !== expectedCount) {
      fail(`${location}[${index}].count must be ${expectedCount}`);
    }
    nonNegativeNumberAt(measurement['durationMs'], `${location}[${index}].durationMs`);
    if (
      numberAt(measurement['operationsPerSecond'], `${location}[${index}].operationsPerSecond`) <= 0
    ) {
      fail(`${location}[${index}].operationsPerSecond must be positive`);
    }
    if (integerAt(measurement['responseBytes'], `${location}[${index}].responseBytes`) <= 0) {
      fail(`${location}[${index}].responseBytes must be positive`);
    }
    validateDistributionCount(
      measurement['latencyMs'],
      expectedCount,
      `${location}[${index}].latencyMs`,
    );
  }
  return matrix;
}

function validateGracefulShutdown(value: unknown, location: string): JsonObject {
  const shutdown = objectAt(value, location);
  falseAt(shutdown['forced'], `${location}.forced`);
  if (shutdown['exitCode'] !== 0) fail(`${location}.exitCode must be 0`);
  nonNegativeNumberAt(shutdown['durationMs'], `${location}.durationMs`);
  integerAt(shutdown['stdoutBytes'], `${location}.stdoutBytes`);
  integerAt(shutdown['stderrBytes'], `${location}.stderrBytes`);
  return shutdown;
}

function validateWorkloads(document: JsonObject, label: string): ValidatedEvidence {
  trueAt(document['ok'], `${label}.ok`);
  const runtime = validateRuntime(document['runtime'], label);
  const runtimeFloor = validateLifecycleRuntimeFloor(
    document['runtimeFloor'],
    runtime,
    `${label}.runtimeFloor`,
  );
  const machine = objectAt(document['machine'], `${label}.machine`);
  const config = objectAt(document['config'], `${label}.config`);
  const comparison = objectAt(document['comparison'], `${label}.comparison`);
  const comparisonKey = shaAt(comparison['key'], `${label}.comparison.key`);
  const computedComparisonKey = new Bun.CryptoHasher('sha256')
    .update(
      JSON.stringify({
        runtime: { bun: runtime['bun'], revision: runtime['revision'] },
        machine,
        config,
      }),
    )
    .digest('hex');
  if (comparisonKey !== computedComparisonKey) {
    fail(`${label}.comparison.key does not match its runtime, machine, and config`);
  }
  const concurrency = arrayAt(config['concurrency'], `${label}.config.concurrency`).map(
    (entry, index) => positiveIntegerAt(entry, `${label}.config.concurrency[${index}]`),
  );
  if (
    concurrency.length === 0 ||
    concurrency.some((entry, index) => index > 0 && entry <= (concurrency[index - 1] ?? 0))
  ) {
    fail(`${label}.config.concurrency must be a non-empty strictly increasing list`);
  }
  const iterations = positiveIntegerAt(config['iterations'], `${label}.config.iterations`);
  const pasteIterations = positiveIntegerAt(
    config['pasteIterations'],
    `${label}.config.pasteIterations`,
  );
  const pasteBytes = positiveIntegerAt(config['pasteBytes'], `${label}.config.pasteBytes`);
  const wsIterations = positiveIntegerAt(config['wsIterations'], `${label}.config.wsIterations`);
  const wsClients = positiveIntegerAt(config['wsClients'], `${label}.config.wsClients`);
  const wsHeartbeatMs = integerAt(config['wsHeartbeatMs'], `${label}.config.wsHeartbeatMs`);
  const terminalViewers = positiveIntegerAt(
    config['terminalViewers'],
    `${label}.config.terminalViewers`,
  );
  const sqliteIterations = positiveIntegerAt(
    config['sqliteIterations'],
    `${label}.config.sqliteIterations`,
  );
  const sqliteStatements = positiveIntegerAt(
    config['sqliteStatements'],
    `${label}.config.sqliteStatements`,
  );

  const daemon = objectAt(document['daemon'], `${label}.daemon`);
  const http = objectAt(document['http'], `${label}.http`);
  const websocket = objectAt(document['websocket'], `${label}.websocket`);
  const sqlite = objectAt(document['sqlite'], `${label}.sqlite`);

  const sessionIds = arrayAt(http['sessionIds'], `${label}.http.sessionIds`);
  const seededSessions = positiveIntegerAt(http['seededSessions'], `${label}.http.seededSessions`);
  if (
    sessionIds.length !== seededSessions ||
    sessionIds.some((entry) => typeof entry !== 'string' || entry.length === 0)
  ) {
    fail(`${label}.http.sessionIds must match seededSessions`);
  }
  for (const endpoint of ['health', 'state', 'static', 'hook']) {
    validateOperationMatrix(http[endpoint], concurrency, iterations, `${label}.http.${endpoint}`);
  }

  const largePaste = objectAt(http['largePaste'], `${label}.http.largePaste`);
  if (largePaste['decodedBytes'] !== pasteBytes) {
    fail(`${label}.http.largePaste.decodedBytes must match config.pasteBytes`);
  }
  if (
    positiveIntegerAt(largePaste['requestBytes'], `${label}.http.largePaste.requestBytes`) <=
    pasteBytes
  ) {
    fail(`${label}.http.largePaste.requestBytes must include encoded request overhead`);
  }
  const pasteSha256 = shaAt(largePaste['sha256'], `${label}.http.largePaste.sha256`);
  trueAt(largePaste['persistedExact'], `${label}.http.largePaste.persistedExact`);
  const pasteMatrix = validateOperationMatrix(
    largePaste['matrix'],
    concurrency,
    pasteIterations,
    `${label}.http.largePaste.matrix`,
  );
  const expectedPastePaths =
    1 + pasteMatrix.reduce((total, measurement) => total + Number(measurement['count']), 0);
  if (largePaste['acknowledgedUniquePaths'] !== expectedPastePaths) {
    fail(`${label}.http.largePaste.acknowledgedUniquePaths is inconsistent`);
  }

  const rawBody = objectAt(http['rawBody'], `${label}.http.rawBody`);
  for (const [name, expectedMode] of [
    ['withheld', 'withheld-client-abort'],
    ['stalledFin', 'stalled-server-fin'],
  ] as const) {
    const result = objectAt(rawBody[name], `${label}.http.rawBody.${name}`);
    if (result['mode'] !== expectedMode) fail(`${label}.http.rawBody.${name}.mode is incorrect`);
    trueAt(result['heldPastDrainGrace'], `${label}.http.rawBody.${name}.heldPastDrainGrace`);
    trueAt(result['noPrematureResponse'], `${label}.http.rawBody.${name}.noPrematureResponse`);
    if (result['responseBytes'] !== 0) {
      fail(`${label}.http.rawBody.${name}.responseBytes must be 0`);
    }
    if (
      positiveIntegerAt(result['declaredBytes'], `${label}.http.rawBody.${name}.declaredBytes`) <=
      positiveIntegerAt(result['sentBodyBytes'], `${label}.http.rawBody.${name}.sentBodyBytes`)
    ) {
      fail(`${label}.http.rawBody.${name} must remain an incomplete request body`);
    }
    if (result['closeKind'] !== 'close' && result['closeKind'] !== 'end') {
      fail(`${label}.http.rawBody.${name}.closeKind is invalid`);
    }
  }

  const snapshot = objectAt(websocket['snapshotBroadcast'], `${label}.websocket.snapshotBroadcast`);
  if (snapshot['clients'] !== wsClients || snapshot['broadcasts'] !== wsIterations) {
    fail(`${label}.websocket.snapshotBroadcast does not match configured clients/broadcasts`);
  }
  if (snapshot['deliveredFrames'] !== wsClients * (wsIterations + 1)) {
    fail(`${label}.websocket.snapshotBroadcast.deliveredFrames is inconsistent`);
  }
  positiveIntegerAt(
    snapshot['receivedBytes'],
    `${label}.websocket.snapshotBroadcast.receivedBytes`,
  );
  validateDistributionCount(
    snapshot['connectLatencyMs'],
    wsClients,
    `${label}.websocket.snapshotBroadcast.connectLatencyMs`,
  );
  validateDistributionCount(
    snapshot['broadcastFanoutLatencyMs'],
    wsIterations,
    `${label}.websocket.snapshotBroadcast.broadcastFanoutLatencyMs`,
  );
  const snapshotQueue = objectAt(snapshot['queue'], `${label}.websocket.snapshotBroadcast.queue`);
  if (
    snapshotQueue['serverBufferedAmountLimitBytes'] !== 1 << 20 ||
    snapshotQueue['droppedFrames'] !== 0 ||
    snapshotQueue['resyncs'] !== 0
  ) {
    fail(`${label}.websocket.snapshotBroadcast.queue violated its no-drop contract`);
  }
  const heartbeat = objectAt(
    snapshot['heartbeat'],
    `${label}.websocket.snapshotBroadcast.heartbeat`,
  );
  if (
    heartbeat['clientsStillOpen'] !== wsClients ||
    heartbeat['expectedPingCadences'] !== Math.floor(wsHeartbeatMs / 30_000)
  ) {
    fail(`${label}.websocket.snapshotBroadcast.heartbeat is inconsistent`);
  }

  const backpressure = objectAt(
    websocket['slowReaderBackpressure'],
    `${label}.websocket.slowReaderBackpressure`,
  );
  if (
    backpressure['configuredQueueLimitBytes'] !== -1 ||
    backpressure['mutations'] !== 1 ||
    backpressure['sentBeforeEviction'] !== 0 ||
    backpressure['droppedOrResyncedFrames'] !== 1
  ) {
    fail(`${label}.websocket.slowReaderBackpressure did not exercise the forced eviction seam`);
  }
  trueAt(backpressure['resynced'], `${label}.websocket.slowReaderBackpressure.resynced`);
  positiveIntegerAt(
    backpressure['initialBytes'],
    `${label}.websocket.slowReaderBackpressure.initialBytes`,
  );
  positiveIntegerAt(
    backpressure['freshSnapshotBytes'],
    `${label}.websocket.slowReaderBackpressure.freshSnapshotBytes`,
  );
  validateGracefulShutdown(
    backpressure['shutdown'],
    `${label}.websocket.slowReaderBackpressure.shutdown`,
  );

  const terminal = objectAt(
    websocket['terminalOutputFanout'],
    `${label}.websocket.terminalOutputFanout`,
  );
  stringAt(terminal['spawnId'], `${label}.websocket.terminalOutputFanout.spawnId`);
  if (
    terminal['viewers'] !== terminalViewers ||
    terminal['initFrames'] !== terminalViewers ||
    terminal['outputFrames'] !== terminalViewers ||
    terminal['deliveredFrames'] !== terminalViewers * 2
  ) {
    fail(`${label}.websocket.terminalOutputFanout frame counts are inconsistent`);
  }
  positiveIntegerAt(
    terminal['receivedBytes'],
    `${label}.websocket.terminalOutputFanout.receivedBytes`,
  );
  const terminalQueue = objectAt(
    terminal['queue'],
    `${label}.websocket.terminalOutputFanout.queue`,
  );
  if (
    terminalQueue['preInitOutputLimitBytes'] !== 256 * 1024 ||
    terminalQueue['socketBufferedAmountLimitBytes'] !== 4 << 20 ||
    terminalQueue['droppedFrames'] !== 0
  ) {
    fail(`${label}.websocket.terminalOutputFanout.queue violated its no-drop contract`);
  }
  validateDistributionCount(
    terminal['initLatencyMs'],
    terminalViewers,
    `${label}.websocket.terminalOutputFanout.initLatencyMs`,
  );
  validateDistributionCount(
    terminal['outputFanoutLatencyMs'],
    terminalViewers,
    `${label}.websocket.terminalOutputFanout.outputFanoutLatencyMs`,
  );

  const shutdown = objectAt(daemon['shutdown'], `${label}.daemon.shutdown`);
  validateGracefulShutdown(
    shutdown['idleAfterWorkloads'],
    `${label}.daemon.shutdown.idleAfterWorkloads`,
  );
  const busy = objectAt(shutdown['busy'], `${label}.daemon.shutdown.busy`);
  if (busy['state'] !== 'busy-incomplete-request-bodies') {
    fail(`${label}.daemon.shutdown.busy.state is incorrect`);
  }
  const expectedInFlight = Math.min(15, Math.max(...concurrency));
  if (
    busy['inFlight'] !== expectedInFlight ||
    Number(busy['settled']) + Number(busy['rejected']) !== expectedInFlight
  ) {
    fail(`${label}.daemon.shutdown.busy request counts are inconsistent`);
  }
  validateGracefulShutdown(busy['shutdown'], `${label}.daemon.shutdown.busy.shutdown`);
  const heldHook = objectAt(shutdown['heldHook'], `${label}.daemon.shutdown.heldHook`);
  if (heldHook['state'] !== 'held-PermissionRequest-with-live-board-consumer') {
    fail(`${label}.daemon.shutdown.heldHook.state is incorrect`);
  }
  trueAt(
    heldHook['confirmedHeldBeforeSignal'],
    `${label}.daemon.shutdown.heldHook.confirmedHeldBeforeSignal`,
  );
  const heldOutcome = objectAt(
    heldHook['requestOutcome'],
    `${label}.daemon.shutdown.heldHook.requestOutcome`,
  );
  if (heldOutcome['kind'] === 'response') {
    integerAt(heldOutcome['status'], `${label}.daemon.shutdown.heldHook.requestOutcome.status`);
  } else if (heldOutcome['kind'] === 'error') {
    stringAt(heldOutcome['error'], `${label}.daemon.shutdown.heldHook.requestOutcome.error`);
  } else {
    fail(`${label}.daemon.shutdown.heldHook.requestOutcome.kind is invalid`);
  }
  validateGracefulShutdown(heldHook['shutdown'], `${label}.daemon.shutdown.heldHook.shutdown`);

  const eventLoop = objectAt(daemon['eventLoop'], `${label}.daemon.eventLoop`);
  const driver = objectAt(eventLoop['driver'], `${label}.daemon.eventLoop.driver`);
  trueAt(driver['available'], `${label}.daemon.eventLoop.driver.available`);
  const eventLoopSamples = positiveIntegerAt(
    driver['samples'],
    `${label}.daemon.eventLoop.driver.samples`,
  );
  validateDistributionCount(
    driver['delayMs'],
    eventLoopSamples,
    `${label}.daemon.eventLoop.driver.delayMs`,
  );
  const roundTripProxy = objectAt(
    eventLoop['daemonRoundTripProxy'],
    `${label}.daemon.eventLoop.daemonRoundTripProxy`,
  );
  falseAt(
    roundTripProxy['directInternalCounterAvailable'],
    `${label}.daemon.eventLoop.daemonRoundTripProxy.directInternalCounterAvailable`,
  );
  stringAt(
    roundTripProxy['unavailableReason'],
    `${label}.daemon.eventLoop.daemonRoundTripProxy.unavailableReason`,
  );
  const p99Proxy = arrayAt(
    roundTripProxy['p99Ms'],
    `${label}.daemon.eventLoop.daemonRoundTripProxy.p99Ms`,
  );
  if (p99Proxy.length !== concurrency.length) {
    fail(`${label}.daemon.eventLoop.daemonRoundTripProxy.p99Ms has the wrong shape`);
  }
  p99Proxy.forEach((entry, index) => {
    const sample = objectAt(
      entry,
      `${label}.daemon.eventLoop.daemonRoundTripProxy.p99Ms[${index}]`,
    );
    if (sample['concurrency'] !== concurrency[index]) {
      fail(`${label}.daemon.eventLoop.daemonRoundTripProxy.p99Ms[${index}].concurrency is wrong`);
    }
    nonNegativeNumberAt(
      sample['p99'],
      `${label}.daemon.eventLoop.daemonRoundTripProxy.p99Ms[${index}].p99`,
    );
  });

  const userVersion = integerAt(sqlite['userVersion'], `${label}.sqlite.userVersion`);
  const expectedUserVersion = integerAt(
    sqlite['expectedUserVersion'],
    `${label}.sqlite.expectedUserVersion`,
  );
  const restart = objectAt(sqlite['restart'], `${label}.sqlite.restart`);
  const restartUserVersion = integerAt(
    restart['userVersion'],
    `${label}.sqlite.restart.userVersion`,
  );
  if (userVersion !== expectedUserVersion || restartUserVersion !== expectedUserVersion) {
    fail(`${label}.sqlite user_version evidence is inconsistent`);
  }
  trueAt(sqlite['strictProductionSchema'], `${label}.sqlite.strictProductionSchema`);
  if (sqlite['iterations'] !== sqliteIterations) {
    fail(`${label}.sqlite.iterations must match config.sqliteIterations`);
  }
  if (numberAt(sqlite['operationsPerSecond'], `${label}.sqlite.operationsPerSecond`) <= 0) {
    fail(`${label}.sqlite.operationsPerSecond must be positive`);
  }
  const mix = objectAt(sqlite['mix'], `${label}.sqlite.mix`);
  const reads = integerAt(mix['reads'], `${label}.sqlite.mix.reads`);
  const writes = integerAt(mix['writes'], `${label}.sqlite.mix.writes`);
  const transactions = integerAt(mix['transactions'], `${label}.sqlite.mix.transactions`);
  if (reads + writes + transactions !== sqliteIterations) {
    fail(`${label}.sqlite.mix does not sum to config.sqliteIterations`);
  }
  const sqliteLatency = objectAt(sqlite['latencyMs'], `${label}.sqlite.latencyMs`);
  validateDistributionCount(
    sqliteLatency['snapshotReads'],
    reads,
    `${label}.sqlite.latencyMs.snapshotReads`,
  );
  validateDistributionCount(sqliteLatency['writes'], writes, `${label}.sqlite.latencyMs.writes`);
  validateDistributionCount(
    sqliteLatency['transactions'],
    transactions,
    `${label}.sqlite.latencyMs.transactions`,
  );
  const rows = objectAt(sqlite['rows'], `${label}.sqlite.rows`);
  if (
    integerAt(rows['sessions'], `${label}.sqlite.rows.sessions`) < 15 ||
    integerAt(rows['events'], `${label}.sqlite.rows.events`) < 15 + writes + transactions * 2
  ) {
    fail(`${label}.sqlite.rows do not reflect the seeded mixed workload`);
  }
  const statementGrowth = objectAt(sqlite['statementGrowth'], `${label}.sqlite.statementGrowth`);
  if (statementGrowth['distinctPreparedStatements'] !== sqliteStatements) {
    fail(`${label}.sqlite.statementGrowth does not match config.sqliteStatements`);
  }
  const contention = objectAt(sqlite['busyContention'], `${label}.sqlite.busyContention`);
  trueAt(contention['surfaced'], `${label}.sqlite.busyContention.surfaced`);
  if (!/busy|locked/i.test(stringAt(contention['error'], `${label}.sqlite.busyContention.error`))) {
    fail(`${label}.sqlite.busyContention.error does not record lock contention`);
  }
  trueAt(restart['verified'], `${label}.sqlite.restart.verified`);
  positiveIntegerAt(sqlite['databaseBytes'], `${label}.sqlite.databaseBytes`);

  const positivePaths = [
    'http.largePaste.persistedExact',
    'http.rawBody.withheld.heldPastDrainGrace',
    'http.rawBody.withheld.noPrematureResponse',
    'http.rawBody.stalledFin.heldPastDrainGrace',
    'http.rawBody.stalledFin.noPrematureResponse',
    'websocket.slowReaderBackpressure.resynced',
    'daemon.eventLoop.driver.available',
    'daemon.shutdown.heldHook.confirmedHeldBeforeSignal',
    'sqlite.strictProductionSchema',
    'sqlite.busyContention.surfaced',
    'sqlite.restart.verified',
  ];

  return {
    kind: 'fleetdeck-effect-p0-workloads',
    runtime,
    machine,
    config,
    stableFacts: {
      comparisonKey,
      runtimeFloor,
      http: { pasteBytes, pasteSha256 },
      websocket: {
        snapshotQueue,
        backpressureQueueLimitBytes: backpressure['configuredQueueLimitBytes'],
        terminalQueue,
      },
      sqlite: {
        api: stringAt(sqlite['api'], `${label}.sqlite.api`),
        userVersion,
        expectedUserVersion,
        restartUserVersion,
      },
    },
    healthChecks: [
      `${label}.ok`,
      `${label}.runtimeFloor.exactMatch`,
      ...positivePaths.map((entry) => `${label}.${entry}`),
      `${label}.daemon.shutdown.idleAfterWorkloads`,
      `${label}.daemon.shutdown.busy.shutdown`,
      `${label}.daemon.shutdown.heldHook.shutdown`,
      `${label}.websocket.slowReaderBackpressure.shutdown`,
    ],
  };
}

function validateNaturalEvents(value: unknown, location: string): void {
  const events = arrayAt(value, location).map((entry, index) =>
    objectAt(entry, `${location}[${index}]`),
  );
  for (const expected of ['acquire', 'complete', 'release']) {
    if (!events.some((event) => event['event'] === expected)) {
      fail(`${location} is missing the ${expected} event`);
    }
  }
  const release = events.find((event) => event['event'] === 'release');
  if (release?.['exit'] !== 'Success') fail(`${location} did not finalize with Success`);
}

function validateProbe(document: JsonObject, label: string): ValidatedEvidence {
  const runtime = validateRuntime(document['runtime'], label);
  const commands = objectAt(document['commands'], `${label}.commands`);
  for (const name of ['install', 'source', 'build', 'bundled', 'signals', 'dependencyTree']) {
    const command = objectAt(commands[name], `${label}.commands.${name}`);
    if (integerAt(command['exitCode'], `${label}.commands.${name}.exitCode`) !== 0) {
      fail(`${label}.commands.${name} did not exit successfully`);
    }
  }

  const verification = objectAt(document['verification'], `${label}.verification`);
  const exactVersions = objectAt(
    verification['exactVersions'],
    `${label}.verification.exactVersions`,
  );
  if (
    exactVersions['effect'] !== EXPECTED_EFFECT_VERSION ||
    exactVersions['platformBun'] !== EXPECTED_EFFECT_VERSION
  ) {
    fail(`${label}.verification.exactVersions must record the exact RC cohort`);
  }
  validateNaturalEvents(verification['sourceEvents'], `${label}.verification.sourceEvents`);
  validateNaturalEvents(verification['bundleEvents'], `${label}.verification.bundleEvents`);
  trueAt(verification['rootKeepAliveObserved'], `${label}.verification.rootKeepAliveObserved`);
  const signalEvents = arrayAt(
    verification['signalEvents'],
    `${label}.verification.signalEvents`,
  ).map((entry, index) => objectAt(entry, `${label}.verification.signalEvents[${index}]`));
  for (const signal of ['SIGINT', 'SIGTERM']) {
    const event = signalEvents.find((entry) => entry['signal'] === signal);
    if (!event) fail(`${label}.verification.signalEvents is missing ${signal}`);
    if (event['exitCode'] !== 130 || event['stderr'] !== '') {
      fail(`${label}.verification.signalEvents.${signal} violated the interrupt contract`);
    }
    falseAt(event['forcedCleanup'], `${label}.verification.signalEvents.${signal}.forcedCleanup`);
  }
  const interruptedEvents = objectAt(
    verification['interruptedEvents'],
    `${label}.verification.interruptedEvents`,
  );
  for (const signal of ['SIGINT', 'SIGTERM']) {
    const events = arrayAt(
      interruptedEvents[signal],
      `${label}.verification.interruptedEvents.${signal}`,
    ).map((entry, index) =>
      objectAt(entry, `${label}.verification.interruptedEvents.${signal}[${index}]`),
    );
    if (!events.some((event) => event['event'] === 'ready')) {
      fail(`${label}.verification.interruptedEvents.${signal} is missing ready`);
    }
    const release = events.find((event) => event['event'] === 'release');
    if (release?.['exit'] !== 'Failure') {
      fail(`${label}.verification.interruptedEvents.${signal} did not run its interrupt finalizer`);
    }
  }

  const fixtureLockSha256 = shaAt(
    verification['fixtureLockSha256'],
    `${label}.verification.fixtureLockSha256`,
  );
  const resolvedPackageCount = integerAt(
    verification['resolvedPackageCount'],
    `${label}.verification.resolvedPackageCount`,
  );
  if (resolvedPackageCount <= 0)
    fail(`${label}.verification.resolvedPackageCount must be positive`);
  const installedTree = objectAt(
    verification['installedTree'],
    `${label}.verification.installedTree`,
  );
  if (
    integerAt(installedTree['bytes'], `${label}.verification.installedTree.bytes`) <= 0 ||
    integerAt(installedTree['files'], `${label}.verification.installedTree.files`) <= 0
  ) {
    fail(`${label}.verification.installedTree must contain positive byte and file counts`);
  }
  const probeBundleBytes = integerAt(
    verification['probeBundleBytes'],
    `${label}.verification.probeBundleBytes`,
  );
  if (probeBundleBytes <= 0) fail(`${label}.verification.probeBundleBytes must be positive`);

  return {
    kind: 'fleetdeck-effect-p0-probe',
    runtime,
    stableFacts: {
      exactVersions,
      fixtureLockSha256,
      resolvedPackageCount,
      installedTree,
      probeBundleBytes,
    },
    healthChecks: [
      `${label}.verification.rootKeepAliveObserved`,
      `${label}.verification.sourceEvents`,
      `${label}.verification.bundleEvents`,
      `${label}.verification.signalEvents`,
      `${label}.verification.interruptedEvents`,
    ],
  };
}

function validateEvidence(document: JsonObject, label: string): ValidatedEvidence {
  const kind = stringAt(document['kind'], `${label}.kind`);
  if (!KNOWN_KINDS.has(kind as EvidenceKind)) fail(`${label}.kind is unsupported: ${kind}`);
  switch (kind as EvidenceKind) {
    case 'fleetdeck-effect-p0-baseline':
      return validateBaseline(document, label);
    case 'fleetdeck-effect-p0-exec-bench':
      return validateExecBench(document, label);
    case 'fleetdeck-effect-p0-workloads':
      return validateWorkloads(document, label);
    case 'fleetdeck-effect-p0-probe':
      return validateProbe(document, label);
  }
}

function rounded(value: number): number {
  return Math.round(value * 1_000) / 1_000;
}

function delta(left: number, right: number): NumericDelta {
  const absolute = rounded(right - left);
  return {
    absolute,
    percent: left === 0 ? (right === 0 ? 0 : null) : rounded((absolute / Math.abs(left)) * 100),
  };
}

function metricCategory(location: string): 'timing' | 'size' | 'other' {
  if (/bytes|rss|size/i.test(location)) return 'size';
  if (/ms|micros|duration|latency|time|cpu|percent/i.test(location)) return 'timing';
  return 'other';
}

function distributionAt(
  value: unknown,
): { count?: number; p50: number; p95: number; p99: number } | null {
  if (!isObject(value)) return null;
  const p50 = value['p50'];
  const p95 = value['p95'];
  const p99 = value['p99'];
  if (
    typeof p50 !== 'number' ||
    !Number.isFinite(p50) ||
    typeof p95 !== 'number' ||
    !Number.isFinite(p95) ||
    typeof p99 !== 'number' ||
    !Number.isFinite(p99)
  ) {
    return null;
  }
  const count = value['count'];
  return {
    ...(typeof count === 'number' && Number.isFinite(count) ? { count } : {}),
    p50,
    p95,
    p99,
  };
}

function collectDistributions(root: unknown): Map<string, ReturnType<typeof distributionAt>> {
  const found = new Map<string, ReturnType<typeof distributionAt>>();
  const visit = (value: unknown, location: string): void => {
    const distribution = distributionAt(value);
    if (distribution) {
      found.set(location, distribution);
      return;
    }
    if (Array.isArray(value)) {
      value.forEach((entry, index) => visit(entry, `${location}[${index}]`));
    } else if (isObject(value)) {
      for (const [key, entry] of Object.entries(value)) visit(entry, `${location}.${key}`);
    }
  };
  visit(root, '$');
  return found;
}

function collectScalarMetrics(root: unknown, category: 'timing' | 'size'): Map<string, number> {
  const found = new Map<string, number>();
  const visit = (value: unknown, location: string, arrayDepth: number): void => {
    if (distributionAt(value)) return;
    if (Array.isArray(value)) {
      value.forEach((entry, index) => visit(entry, `${location}[${index}]`, arrayDepth + 1));
      return;
    }
    if (isObject(value)) {
      for (const [key, entry] of Object.entries(value)) {
        const childLocation = `${location}.${key}`;
        if (
          arrayDepth === 0 &&
          typeof entry === 'number' &&
          Number.isFinite(entry) &&
          metricCategory(childLocation) === category
        ) {
          found.set(childLocation, entry);
        } else {
          visit(entry, childLocation, arrayDepth);
        }
      }
    }
  };
  visit(root, '$', 0);
  return found;
}

function distributionDeltas(left: JsonObject, right: JsonObject) {
  const leftMetrics = collectDistributions(left);
  const rightMetrics = collectDistributions(right);
  assertEqual(
    [...leftMetrics.keys()].sort(),
    [...rightMetrics.keys()].sort(),
    'distribution evidence',
  );
  return [...leftMetrics.entries()].map(([location, leftDistribution]) => {
    const rightDistribution = rightMetrics.get(location);
    if (!leftDistribution || !rightDistribution)
      fail(`distribution evidence is missing ${location}`);
    return {
      path: location,
      category: metricCategory(location),
      left: leftDistribution,
      right: rightDistribution,
      delta: {
        count:
          leftDistribution.count === undefined || rightDistribution.count === undefined
            ? null
            : rightDistribution.count - leftDistribution.count,
        p50: delta(leftDistribution.p50, rightDistribution.p50),
        p95: delta(leftDistribution.p95, rightDistribution.p95),
        p99: delta(leftDistribution.p99, rightDistribution.p99),
      },
    };
  });
}

function scalarDeltas(left: JsonObject, right: JsonObject, category: 'timing' | 'size') {
  const leftMetrics = collectScalarMetrics(left, category);
  const rightMetrics = collectScalarMetrics(right, category);
  return [...leftMetrics.entries()]
    .filter(([location]) => rightMetrics.has(location))
    .map(([location, leftValue]) => {
      const rightValue = rightMetrics.get(location) as number;
      return {
        path: location,
        left: leftValue,
        right: rightValue,
        delta: delta(leftValue, rightValue),
      };
    });
}

function displayPath(inputPath: string): string {
  const absolute = path.resolve(inputPath);
  const relative = path.relative(REPO_ROOT, absolute);
  return relative.startsWith('..') ? absolute : relative || '.';
}

async function compare() {
  const leftPath = optionValue('--left');
  const rightPath = optionValue('--right');
  const [leftDocument, rightDocument] = await Promise.all([
    readEvidence(leftPath, 'left'),
    readEvidence(rightPath, 'right'),
  ]);

  const leftSchema = integerAt(leftDocument['schema'], 'left.schema');
  const rightSchema = integerAt(rightDocument['schema'], 'right.schema');
  if (leftSchema !== rightSchema) fail(`schema differs: left=${leftSchema}, right=${rightSchema}`);
  if (leftSchema !== 1) fail(`unsupported evidence schema: ${leftSchema}`);
  const leftKind = stringAt(leftDocument['kind'], 'left.kind');
  const rightKind = stringAt(rightDocument['kind'], 'right.kind');
  if (leftKind !== rightKind) fail(`kind differs: left=${leftKind}, right=${rightKind}`);

  const left = validateEvidence(leftDocument, 'left');
  const right = validateEvidence(rightDocument, 'right');
  assertEqual(left.machine, right.machine, 'machine evidence');
  assertEqual(left.config, right.config, 'configuration evidence');
  const stableDifference = firstDifference(left.stableFacts, right.stableFacts);
  if (stableDifference) fail(`stable evidence differs: ${stableDifference}`);

  const distributions = distributionDeltas(leftDocument, rightDocument);
  return {
    schema: 1,
    kind: 'fleetdeck-effect-p0-comparison',
    ok: true,
    recordedAt: new Date().toISOString(),
    source: { schema: leftSchema, kind: left.kind },
    inputs: { left: displayPath(leftPath), right: displayPath(rightPath) },
    requiredRuntime: { bun: EXPECTED_BUN_VERSION, revision: EXPECTED_BUN_REVISION },
    checks: {
      sameSchema: true,
      sameKind: true,
      exactBunFloor: true,
      sameMachineWhenRecorded: true,
      sameConfigWhenRecorded: true,
      correctnessAndVerificationHealthy: true,
      stableEvidenceEqual: true,
      matchingMetricShape: true,
    },
    coverage: {
      machineRecorded: left.machine !== undefined,
      configRecorded: left.config !== undefined,
      correctnessChecks: left.healthChecks.length + right.healthChecks.length,
      distributions: distributions.length,
    },
    stableEvidence: {
      sha256: sha256(left.stableFacts),
      equal: true,
    },
    metrics: {
      distributions,
      scalarTimings: scalarDeltas(leftDocument, rightDocument, 'timing'),
      scalarSizes: scalarDeltas(leftDocument, rightDocument, 'size'),
      convention: 'delta is right minus left; percent is relative to abs(left)',
    },
  };
}

let report: unknown;
try {
  report = await compare();
} catch (error) {
  report = {
    schema: 1,
    kind: 'fleetdeck-effect-p0-comparison',
    ok: false,
    recordedAt: new Date().toISOString(),
    error: error instanceof Error ? error.message : String(error),
  };
  process.exitCode = 1;
}
await writeJsonReport(report);
