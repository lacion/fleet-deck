import {
  copyFileSync,
  existsSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const EFFECT_COHORT = '4.0.0-rc.110';
export const BUN_FLOOR = '1.3.14';
export const BUN_FLOOR_REVISION = '0d9b296af33f2b851fcbf4df3e9ec89751734ba4';
export const MAX_DIRECT_PRODUCTION_PACKAGES = 2;
export const MAX_RESOLVED_PRODUCTION_PACKAGES = 24;
export const MAX_INSTALLED_PRODUCTION_BYTES = 75 * 1024 * 1024;

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../..');
const REQUIRED_DIRECT = ['@effect/platform-bun', 'effect'] as const;
const REQUIRED_COHORT = ['@effect/platform-bun', '@effect/platform-node-shared', 'effect'] as const;
const PRE_EFFECT_WS = { ws: '8.21.2', '@types/ws': '8.18.1' } as const;
const EXACT_VERSION = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const ALLOWED_LICENSES = new Set(['Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'MIT']);
const REVIEWED_INSTALL_SCRIPTS = new Set(['msgpackr-extract@3.0.4']);
const REVIEWED_NATIVE_PREFIX = '@msgpackr-extract/msgpackr-extract-';

type JsonObject = Record<string, unknown>;

export interface PolicyViolation {
  code: string;
  detail: string;
}

export interface ClosurePackage {
  key: string;
  name: string;
  version: string;
  integrity: string | null;
  optional: boolean;
}

export interface CohortReport {
  ok: boolean;
  policy: {
    effectCohort: string;
    bunFloor: string;
    bunFloorRevision: string;
    maxDirectProductionPackages: number;
    maxResolvedProductionPackages: number;
    maxInstalledProductionBytes: number;
  };
  direct: {
    count: number;
    specs: Record<string, string>;
    typesBun: string | null;
    lockAligned: boolean;
  };
  cohort: Array<{ key: string; name: string; version: string }>;
  closure: {
    count: number;
    packages: ClosurePackage[];
  };
  wsMovement: {
    before: typeof PRE_EFFECT_WS;
    after: { ws: string[]; '@types/ws': string[] };
    unchanged: boolean;
    deduped: boolean;
    platformNodeSharedSpecs: { ws: string | null; '@types/ws': string | null };
  };
  violations: PolicyViolation[];
}

interface LockEntry {
  key: string;
  name: string;
  version: string;
  metadata: JsonObject;
  integrity: string | null;
}

interface CommandResult {
  argv: string[];
  cwd: string;
  elapsedMs: number;
  timeoutMs: number;
  timedOut: boolean;
  exitCode: number;
  stdout: string;
  stderr: string;
}

interface TreeMeasurement {
  bytes: number;
  files: number;
}

interface RegistryPackageReport {
  name: string;
  version: string;
  license: string | null;
  repository: string | null;
  integrityMatchesLock: boolean;
  provenance: boolean;
  signatures: number;
}

interface InstalledPackageReport {
  key: string;
  name: string;
  version: string;
  installPath: string;
  license: string | null;
  repository: string | null;
  installScripts: string[];
}

export interface CohortHardGates {
  exactBunFloor: boolean;
  exactDirectSpecs: boolean;
  directPackageCeiling: boolean;
  exactResolvedCohort: boolean;
  platformNodeSharedPresent: boolean;
  exactBunTypesFloor: boolean;
  packageLockAligned: boolean;
  resolvedPackageCeiling: boolean;
  lockIntegrityPresent: boolean;
  wsVersionsUnchanged: boolean;
  wsVersionsDeduped: boolean;
  frozenProductionInstall: boolean | null;
  installedByteCeiling: boolean | null;
  installedClosureComplete: boolean | null;
  licensesApproved: boolean | null;
  installScriptsReviewed: boolean | null;
  nativeBinariesReviewed: boolean | null;
  registryMetadataAvailable: boolean | null;
  registryIntegrityMatchesLock: boolean | null;
  registrySignatureOrProvenancePresent: boolean | null;
  cohortProvenancePresent: boolean | null;
  highCriticalAdvisoriesAbsent: boolean | null;
  all: boolean;
}

export interface CohortMeasurement {
  cleanInstall: CommandResult & {
    frozenLockfile: true;
    ignoredScripts: true;
    productionOnly: true;
  };
  installedTree: TreeMeasurement;
  installedPackages: InstalledPackageReport[];
  nativeBinaries: string[];
  registry: {
    packages: RegistryPackageReport[];
    provenancePresent: number;
    provenanceMissing: string[];
  };
  advisories: {
    command: CommandResult;
    report: unknown;
    highOrCriticalFound: boolean;
  };
  violations: PolicyViolation[];
}

interface CohortEvidenceFacts {
  directProductionPackageCount: number;
  resolvedProductionPackageCount: number;
  installedProductionPackageCount: number | null;
  nodeModulesBytes: number | null;
  nodeModulesFiles: number | null;
  cleanInstallMs: number | null;
  cleanInstallExitCode: number | null;
  cleanInstallTimedOut: boolean | null;
  licenses: Array<{ name: string; version: string; license: string | null }>;
  nativeBinaries: string[];
  installScripts: Array<{ name: string; version: string; scripts: string[] }>;
  provenance: {
    checked: number;
    present: number;
    missing: string[];
    packages: Array<{
      name: string;
      version: string;
      provenance: boolean;
      signatures: number;
      integrityMatchesLock: boolean;
    }>;
  } | null;
  advisories: {
    auditExitCode: number;
    auditTimedOut: boolean;
    highOrCriticalFound: boolean;
    report: unknown;
  } | null;
  wsMovement: CohortReport['wsMovement'];
}

function isObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function objectAt(value: unknown, location: string): JsonObject {
  if (!isObject(value)) throw new Error(`${location} must be an object`);
  return value;
}

function stringMap(value: unknown): Record<string, string> {
  if (!isObject(value)) return {};
  const result: Record<string, string> = {};
  for (const [name, spec] of Object.entries(value)) {
    if (typeof spec === 'string') result[name] = spec;
  }
  return result;
}

function violation(violations: PolicyViolation[], code: string, detail: string): void {
  violations.push({ code, detail });
}

export function parseBunLock(text: string): JsonObject {
  return objectAt(Bun.YAML.parse(text), 'bun.lock');
}

function resolutionParts(resolution: string, key: string): { name: string; version: string } {
  const separator = resolution.lastIndexOf('@');
  if (separator <= 0 || separator === resolution.length - 1) {
    throw new Error(`bun.lock package ${key} has an unsupported resolution ${resolution}`);
  }
  return { name: resolution.slice(0, separator), version: resolution.slice(separator + 1) };
}

function lockEntries(lock: JsonObject): Map<string, LockEntry> {
  const packages = objectAt(lock['packages'], 'bun.lock.packages');
  const entries = new Map<string, LockEntry>();
  for (const [key, raw] of Object.entries(packages)) {
    if (!Array.isArray(raw) || typeof raw[0] !== 'string') {
      throw new Error(`bun.lock package ${key} must be an array with a resolution`);
    }
    const { name, version } = resolutionParts(raw[0], key);
    entries.set(key, {
      key,
      name,
      version,
      metadata: isObject(raw[2]) ? raw[2] : {},
      integrity: typeof raw[3] === 'string' ? raw[3] : null,
    });
  }
  return entries;
}

function packageNamesFromKey(key: string): string[] {
  const segments = key.split('/');
  const names: string[] = [];
  for (let index = 0; index < segments.length; index++) {
    const segment = segments[index];
    if (!segment) continue;
    if (segment.startsWith('@')) {
      const leaf = segments[index + 1];
      if (!leaf) throw new Error(`invalid scoped package key ${key}`);
      names.push(`${segment}/${leaf}`);
      index++;
    } else {
      names.push(segment);
    }
  }
  return names;
}

function resolveDependency(
  entries: Map<string, LockEntry>,
  parentKey: string,
  dependency: string,
): string | null {
  const ancestry = packageNamesFromKey(parentKey);
  for (let depth = ancestry.length; depth >= 0; depth--) {
    const candidate = [...ancestry.slice(0, depth), dependency].join('/');
    if (entries.has(candidate)) return candidate;
  }
  return null;
}

function dependenciesOf(metadata: JsonObject): Array<{ name: string; optional: boolean }> {
  const optionalPeers = new Set(
    Array.isArray(metadata['optionalPeers'])
      ? metadata['optionalPeers'].filter((value): value is string => typeof value === 'string')
      : [],
  );
  const result: Array<{ name: string; optional: boolean }> = [];
  for (const name of Object.keys(stringMap(metadata['dependencies']))) {
    result.push({ name, optional: false });
  }
  for (const name of Object.keys(stringMap(metadata['optionalDependencies']))) {
    result.push({ name, optional: true });
  }
  for (const name of Object.keys(stringMap(metadata['peerDependencies']))) {
    if (!optionalPeers.has(name)) result.push({ name, optional: false });
  }
  return result;
}

function collectClosure(
  directNames: string[],
  entries: Map<string, LockEntry>,
  violations: PolicyViolation[],
): ClosurePackage[] {
  const state = new Map<string, { optional: boolean }>();
  const queue = directNames.map((key) => ({ key, optional: false, from: '<workspace>' }));
  while (queue.length > 0) {
    const current = queue.shift();
    if (!current) continue;
    const entry = entries.get(current.key);
    if (!entry) {
      if (!current.optional) {
        violation(
          violations,
          'lock-missing-package',
          `${current.from} resolves ${current.key}, which is absent from bun.lock`,
        );
      }
      continue;
    }
    const previous = state.get(current.key);
    if (previous && (!previous.optional || current.optional)) continue;
    state.set(current.key, { optional: current.optional });
    for (const dependency of dependenciesOf(entry.metadata)) {
      const resolved = resolveDependency(entries, current.key, dependency.name);
      if (!resolved) {
        if (!dependency.optional) {
          violation(
            violations,
            'lock-missing-transitive',
            `${entry.key} dependency ${dependency.name} is absent from bun.lock`,
          );
        }
        continue;
      }
      queue.push({
        key: resolved,
        optional: current.optional || dependency.optional,
        from: entry.key,
      });
    }
  }
  return [...state.entries()]
    .map(([key, { optional }]) => {
      const entry = entries.get(key);
      if (!entry) throw new Error(`internal closure error for ${key}`);
      return {
        key,
        name: entry.name,
        version: entry.version,
        integrity: entry.integrity,
        optional,
      };
    })
    .sort((left, right) => left.key.localeCompare(right.key));
}

function sameStringMap(left: Record<string, string>, right: Record<string, string>): boolean {
  const leftEntries = Object.entries(left).sort(([a], [b]) => a.localeCompare(b));
  const rightEntries = Object.entries(right).sort(([a], [b]) => a.localeCompare(b));
  return JSON.stringify(leftEntries) === JSON.stringify(rightEntries);
}

export function analyzeEffectCohort(manifestValue: unknown, lockValue: unknown): CohortReport {
  const manifest = objectAt(manifestValue, 'package.json');
  const lock = objectAt(lockValue, 'bun.lock');
  const violations: PolicyViolation[] = [];
  const directSpecs = stringMap(manifest['dependencies']);
  const directNames = Object.keys(directSpecs).sort();
  if (directNames.length > MAX_DIRECT_PRODUCTION_PACKAGES) {
    violation(
      violations,
      'direct-package-ceiling',
      `${directNames.length} direct production packages exceed ${MAX_DIRECT_PRODUCTION_PACKAGES}`,
    );
  }
  for (const [name, spec] of Object.entries(directSpecs)) {
    if (!EXACT_VERSION.test(spec)) {
      violation(violations, 'mutable-direct-spec', `${name} uses mutable spec ${spec}`);
    }
  }
  for (const name of REQUIRED_DIRECT) {
    if (directSpecs[name] !== EFFECT_COHORT) {
      violation(
        violations,
        'direct-cohort-spec',
        `${name} must be exactly ${EFFECT_COHORT}; got ${directSpecs[name] ?? '<missing>'}`,
      );
    }
  }

  const devSpecs = stringMap(manifest['devDependencies']);
  const typesBun = devSpecs['@types/bun'] ?? null;
  if (typesBun !== BUN_FLOOR) {
    violation(
      violations,
      'bun-types-floor',
      `@types/bun must be exactly ${BUN_FLOOR}; got ${typesBun ?? '<missing>'}`,
    );
  }

  const workspaces = objectAt(lock['workspaces'], 'bun.lock.workspaces');
  const rootWorkspace = objectAt(workspaces[''], 'bun.lock.workspaces[""]');
  const lockDirect = stringMap(rootWorkspace['dependencies']);
  const lockDev = stringMap(rootWorkspace['devDependencies']);
  const lockAligned = sameStringMap(directSpecs, lockDirect) && lockDev['@types/bun'] === typesBun;
  if (!sameStringMap(directSpecs, lockDirect)) {
    violation(
      violations,
      'lock-workspace-drift',
      'package.json dependencies and bun.lock root workspace dependencies differ',
    );
  }
  if (lockDev['@types/bun'] !== typesBun) {
    violation(
      violations,
      'lock-bun-types-drift',
      `bun.lock records @types/bun=${lockDev['@types/bun'] ?? '<missing>'}`,
    );
  }

  const entries = lockEntries(lock);
  for (const name of ['@types/bun', 'bun-types']) {
    const entry = entries.get(name);
    if (entry?.version !== BUN_FLOOR) {
      violation(
        violations,
        'resolved-bun-types-floor',
        `${name} must resolve to ${BUN_FLOOR}; got ${entry?.version ?? '<missing>'}`,
      );
    }
  }

  const cohort = [...entries.values()]
    .filter((entry) => entry.name === 'effect' || entry.name.startsWith('@effect/'))
    .map((entry) => ({ key: entry.key, name: entry.name, version: entry.version }))
    .sort((left, right) => left.key.localeCompare(right.key));
  for (const entry of cohort) {
    if (entry.version !== EFFECT_COHORT) {
      violation(
        violations,
        'resolved-cohort-mismatch',
        `${entry.key} resolved ${entry.version}, expected ${EFFECT_COHORT}`,
      );
    }
  }
  for (const name of REQUIRED_COHORT) {
    if (!cohort.some((entry) => entry.name === name && entry.version === EFFECT_COHORT)) {
      violation(violations, 'resolved-cohort-missing', `${name}@${EFFECT_COHORT} is not locked`);
    }
  }

  const closure = collectClosure(directNames, entries, violations);
  if (closure.length > MAX_RESOLVED_PRODUCTION_PACKAGES) {
    violation(
      violations,
      'production-closure-ceiling',
      `${closure.length} resolved production packages exceed ${MAX_RESOLVED_PRODUCTION_PACKAGES}`,
    );
  }
  for (const entry of closure) {
    if (!entry.integrity?.startsWith('sha512-')) {
      violation(
        violations,
        'missing-registry-integrity',
        `${entry.key} has no sha512 registry integrity in bun.lock`,
      );
    }
  }

  const platformBun = [...entries.values()].find((entry) => entry.name === '@effect/platform-bun');
  const platformShared = [...entries.values()].find(
    (entry) => entry.name === '@effect/platform-node-shared',
  );
  const platformBunDeps = stringMap(platformBun?.metadata['dependencies']);
  if (platformBunDeps['@effect/platform-node-shared'] !== `^${EFFECT_COHORT}`) {
    violation(
      violations,
      'platform-shared-edge',
      `platform-bun must request platform-node-shared ^${EFFECT_COHORT}`,
    );
  }

  const versionsOf = (name: string): string[] =>
    [
      ...new Set(closure.filter((entry) => entry.name === name).map((entry) => entry.version)),
    ].sort();
  const after = { ws: versionsOf('ws'), '@types/ws': versionsOf('@types/ws') };
  const sharedDeps = stringMap(platformShared?.metadata['dependencies']);
  const wsMovement = {
    before: PRE_EFFECT_WS,
    after,
    unchanged:
      JSON.stringify(after.ws) === JSON.stringify([PRE_EFFECT_WS.ws]) &&
      JSON.stringify(after['@types/ws']) === JSON.stringify([PRE_EFFECT_WS['@types/ws']]),
    deduped:
      closure.filter((entry) => entry.name === 'ws').length === 1 &&
      closure.filter((entry) => entry.name === '@types/ws').length === 1,
    platformNodeSharedSpecs: {
      ws: sharedDeps['ws'] ?? null,
      '@types/ws': sharedDeps['@types/ws'] ?? null,
    },
  };

  return {
    ok: violations.length === 0,
    policy: {
      effectCohort: EFFECT_COHORT,
      bunFloor: BUN_FLOOR,
      bunFloorRevision: BUN_FLOOR_REVISION,
      maxDirectProductionPackages: MAX_DIRECT_PRODUCTION_PACKAGES,
      maxResolvedProductionPackages: MAX_RESOLVED_PRODUCTION_PACKAGES,
      maxInstalledProductionBytes: MAX_INSTALLED_PRODUCTION_BYTES,
    },
    direct: { count: directNames.length, specs: directSpecs, typesBun, lockAligned },
    cohort,
    closure: { count: closure.length, packages: closure },
    wsMovement,
    violations,
  };
}

function treeMeasurement(root: string): TreeMeasurement {
  let bytes = 0;
  let files = 0;
  const visit = (directory: string): void => {
    for (const name of readdirSync(directory)) {
      const target = path.join(directory, name);
      const stat = statSync(target, { throwIfNoEntry: false });
      if (!stat) continue;
      if (stat.isDirectory()) visit(target);
      else if (stat.isFile()) {
        bytes += stat.size;
        files++;
      }
    }
  };
  visit(root);
  return { bytes, files };
}

function nativeBinaries(root: string): string[] {
  const found: string[] = [];
  const visit = (directory: string): void => {
    for (const name of readdirSync(directory)) {
      const target = path.join(directory, name);
      const stat = statSync(target, { throwIfNoEntry: false });
      if (!stat) continue;
      if (stat.isDirectory()) visit(target);
      else if (stat.isFile() && /\.(?:dll|dylib|node|so)$/.test(name)) {
        found.push(path.relative(root, target));
      }
    }
  };
  visit(root);
  return found.sort();
}

interface StreamCapture {
  promise: Promise<string>;
  cancel: () => Promise<void>;
}

type SettledWithin<T> = { settled: true; value: T } | { settled: false };

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
  const promise = (async (): Promise<string> => {
    const decoder = new TextDecoder();
    let output = '';
    while (true) {
      const { done, value } = await reader.read();
      if (done) return output + decoder.decode();
      output += decoder.decode(value, { stream: true });
    }
  })();
  return {
    promise,
    cancel: async () => {
      try {
        await reader.cancel();
      } catch {
        // The process may already have closed the pipe.
      }
    },
  };
}

async function settleCapture(capture: StreamCapture, timeoutMs: number): Promise<string> {
  const settled = await settleWithin(capture.promise, timeoutMs);
  if (settled.settled) return settled.value;
  await capture.cancel();
  return await capture.promise.catch(() => '');
}

export async function runBoundedCommand(
  argv: string[],
  cwd: string,
  { timeoutMs = 120_000 }: { timeoutMs?: number } = {},
): Promise<CommandResult> {
  const started = performance.now();
  const child = Bun.spawn(argv, {
    cwd,
    env: { ...process.env, FORCE_COLOR: '0', NO_COLOR: '1' },
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stdoutCapture = captureStream(child.stdout);
  const stderrCapture = captureStream(child.stderr);
  const normalExit = await settleWithin(child.exited, timeoutMs);
  let exitCode = normalExit.settled ? normalExit.value : -1;
  if (!normalExit.settled) {
    try {
      child.kill('SIGTERM');
    } catch {
      // The child can win the race between the timeout and signal delivery.
    }
    const termExit = await settleWithin(child.exited, 1_000);
    if (!termExit.settled) {
      try {
        child.kill('SIGKILL');
      } catch {
        // The child can win the race between the grace period and SIGKILL.
      }
      const killExit = await settleWithin(child.exited, 1_000);
      exitCode = killExit.settled ? killExit.value : -1;
    } else {
      exitCode = termExit.value;
    }
  }
  const [stdout, stderr] = await Promise.all([
    settleCapture(stdoutCapture, 2_000),
    settleCapture(stderrCapture, 2_000),
  ]);
  const normalize = (value: string): string => value.split(cwd).join('<scratch>');
  return {
    argv: argv.map((value, index) => (index === 0 ? 'bun' : normalize(value))),
    cwd: '<scratch>',
    elapsedMs: Math.round((performance.now() - started) * 1_000) / 1_000,
    timeoutMs,
    timedOut: !normalExit.settled,
    exitCode,
    stdout: normalize(stdout),
    stderr: normalize(stderr),
  };
}

function repositoryOf(value: unknown): string | null {
  if (typeof value === 'string') return value;
  if (isObject(value) && typeof value['url'] === 'string') return value['url'];
  return null;
}

interface InstalledPackage {
  installPath: string;
  manifest: JsonObject;
}

function installedPackageIndex(nodeModules: string): Map<string, InstalledPackage[]> {
  const index = new Map<string, InstalledPackage[]>();
  const visited = new Set<string>();
  const visitPackage = (packageRoot: string): void => {
    const packageFile = path.join(packageRoot, 'package.json');
    if (!existsSync(packageFile)) return;
    const manifest = objectAt(JSON.parse(readFileSync(packageFile, 'utf8')), packageFile);
    const name = manifest['name'];
    const version = manifest['version'];
    if (typeof name !== 'string' || typeof version !== 'string') return;
    const installPath = path.relative(nodeModules, packageRoot);
    const identity = `${name}@${version}`;
    const packages = index.get(identity) ?? [];
    packages.push({ installPath, manifest });
    index.set(identity, packages);
    visitModules(path.join(packageRoot, 'node_modules'));
  };
  const visitModules = (modulesRoot: string): void => {
    if (!existsSync(modulesRoot) || visited.has(modulesRoot)) return;
    visited.add(modulesRoot);
    for (const name of readdirSync(modulesRoot)) {
      if (name.startsWith('.')) continue;
      const target = path.join(modulesRoot, name);
      if (name.startsWith('@')) {
        if (!statSync(target, { throwIfNoEntry: false })?.isDirectory()) continue;
        for (const scopedName of readdirSync(target)) {
          visitPackage(path.join(target, scopedName));
        }
      } else {
        visitPackage(target);
      }
    }
  };
  visitModules(nodeModules);
  for (const packages of index.values()) {
    packages.sort((left, right) => left.installPath.localeCompare(right.installPath));
  }
  return index;
}

function installedPackages(
  nodeModules: string,
  closure: ClosurePackage[],
  violations: PolicyViolation[],
): InstalledPackageReport[] {
  const reports: InstalledPackageReport[] = [];
  const installed = installedPackageIndex(nodeModules);
  for (const entry of closure) {
    const identity = `${entry.name}@${entry.version}`;
    const installation = installed.get(identity)?.[0];
    if (!installation) {
      if (!entry.optional) {
        violation(
          violations,
          'clean-install-missing-package',
          `${entry.key} (${identity}) was absent from the production install`,
        );
      }
      continue;
    }
    const parsed = installation.manifest;
    const license = typeof parsed['license'] === 'string' ? parsed['license'] : null;
    if (!license || !ALLOWED_LICENSES.has(license)) {
      violation(
        violations,
        'incompatible-license',
        `${entry.name}@${entry.version} declares ${license ?? '<no license>'}`,
      );
    }
    const scripts = stringMap(parsed['scripts']);
    const installScripts = ['preinstall', 'install', 'postinstall'].filter((name) => scripts[name]);
    if (installScripts.length > 0 && !REVIEWED_INSTALL_SCRIPTS.has(identity)) {
      violation(
        violations,
        'unreviewed-install-script',
        `${identity} declares ${installScripts.join(', ')}`,
      );
    }
    reports.push({
      key: entry.key,
      name: entry.name,
      version: entry.version,
      installPath: installation.installPath,
      license,
      repository: repositoryOf(parsed['repository']),
      installScripts,
    });
  }
  return reports.sort((left, right) => left.key.localeCompare(right.key));
}

async function registryPackages(
  closure: ClosurePackage[],
  violations: PolicyViolation[],
): Promise<RegistryPackageReport[]> {
  const reports = await Promise.all(
    closure.map(async (entry): Promise<RegistryPackageReport | null> => {
      const url = `https://registry.npmjs.org/${encodeURIComponent(entry.name)}/${encodeURIComponent(entry.version)}`;
      let response: Response;
      try {
        response = await fetch(url, { signal: AbortSignal.timeout(15_000) });
      } catch (error) {
        violation(
          violations,
          'registry-metadata-unavailable',
          `${entry.name}@${entry.version}: ${error instanceof Error ? error.message : String(error)}`,
        );
        return null;
      }
      if (!response.ok) {
        violation(
          violations,
          'registry-metadata-unavailable',
          `${entry.name}@${entry.version}: HTTP ${response.status}`,
        );
        return null;
      }
      const metadata = objectAt(await response.json(), `${entry.name}@${entry.version} metadata`);
      const dist = isObject(metadata['dist']) ? metadata['dist'] : {};
      const attestations = isObject(dist['attestations']) ? dist['attestations'] : {};
      const provenance = isObject(attestations['provenance']);
      const signatures = Array.isArray(dist['signatures']) ? dist['signatures'].length : 0;
      const integrityMatchesLock =
        typeof dist['integrity'] === 'string' && dist['integrity'] === entry.integrity;
      const license = typeof metadata['license'] === 'string' ? metadata['license'] : null;
      if (!integrityMatchesLock) {
        violation(
          violations,
          'registry-integrity-mismatch',
          `${entry.name}@${entry.version} registry integrity differs from bun.lock`,
        );
      }
      if (!license || !ALLOWED_LICENSES.has(license)) {
        violation(
          violations,
          'incompatible-license',
          `${entry.name}@${entry.version} registry license is ${license ?? '<missing>'}`,
        );
      }
      if ((entry.name === 'effect' || entry.name.startsWith('@effect/')) && !provenance) {
        violation(
          violations,
          'cohort-provenance-missing',
          `${entry.name}@${entry.version} has no npm provenance attestation`,
        );
      }
      if (!provenance && signatures === 0) {
        violation(
          violations,
          'registry-attestation-missing',
          `${entry.name}@${entry.version} has neither npm provenance nor a registry signature`,
        );
      }
      return {
        name: entry.name,
        version: entry.version,
        license,
        repository: repositoryOf(metadata['repository']),
        integrityMatchesLock,
        provenance,
        signatures,
      };
    }),
  );
  return reports
    .filter((report): report is RegistryPackageReport => report !== null)
    .sort((left, right) =>
      `${left.name}@${left.version}`.localeCompare(`${right.name}@${right.version}`),
    );
}

function parseAudit(stdout: string): unknown {
  const trimmed = stdout.trim();
  if (!trimmed) return {};
  try {
    return JSON.parse(trimmed) as unknown;
  } catch {
    return { unparsed: trimmed };
  }
}

export async function measureEffectCohort(
  report: CohortReport,
  { repoRoot = REPO_ROOT }: { repoRoot?: string } = {},
): Promise<CohortMeasurement> {
  const violations: PolicyViolation[] = [];
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-effect-p2-'));
  try {
    copyFileSync(path.join(repoRoot, 'package.json'), path.join(scratch, 'package.json'));
    copyFileSync(path.join(repoRoot, 'bun.lock'), path.join(scratch, 'bun.lock'));
    const cleanInstallBase = await runBoundedCommand(
      [process.execPath, 'install', '--production', '--frozen-lockfile', '--ignore-scripts'],
      scratch,
    );
    const cleanInstall = {
      ...cleanInstallBase,
      frozenLockfile: true as const,
      ignoredScripts: true as const,
      productionOnly: true as const,
    };
    if (cleanInstall.exitCode !== 0) {
      violation(
        violations,
        'frozen-install-failed',
        `bun install exited ${cleanInstall.exitCode}: ${cleanInstall.stderr.trim()}`,
      );
      return {
        cleanInstall,
        installedTree: { bytes: 0, files: 0 },
        installedPackages: [],
        nativeBinaries: [],
        registry: { packages: [], provenancePresent: 0, provenanceMissing: [] },
        advisories: {
          command: {
            argv: ['bun', 'audit', '--json', '--audit-level=high'],
            cwd: '<scratch>',
            elapsedMs: 0,
            timeoutMs: 60_000,
            timedOut: false,
            exitCode: -1,
            stdout: '',
            stderr: 'skipped after install failure',
          },
          report: {},
          highOrCriticalFound: false,
        },
        violations,
      };
    }

    const nodeModules = path.join(scratch, 'node_modules');
    const installedTree = treeMeasurement(nodeModules);
    if (installedTree.bytes > MAX_INSTALLED_PRODUCTION_BYTES) {
      violation(
        violations,
        'installed-byte-ceiling',
        `${installedTree.bytes} installed bytes exceed ${MAX_INSTALLED_PRODUCTION_BYTES}`,
      );
    }
    const installed = installedPackages(nodeModules, report.closure.packages, violations);
    const native = nativeBinaries(nodeModules);
    for (const binary of native) {
      if (!binary.startsWith(REVIEWED_NATIVE_PREFIX)) {
        violation(violations, 'unreviewed-native-binary', binary);
      }
    }

    const [registry, auditCommand] = await Promise.all([
      registryPackages(report.closure.packages, violations),
      runBoundedCommand([process.execPath, 'audit', '--json', '--audit-level=high'], scratch, {
        timeoutMs: 60_000,
      }),
    ]);
    const auditReport = parseAudit(auditCommand.stdout);
    const highOrCriticalFound = auditCommand.exitCode !== 0;
    if (highOrCriticalFound) {
      violation(
        violations,
        'high-critical-advisory',
        `bun audit --audit-level=high exited ${auditCommand.exitCode}`,
      );
    }
    const provenanceMissing = registry
      .filter((entry) => !entry.provenance)
      .map((entry) => `${entry.name}@${entry.version}`);
    return {
      cleanInstall,
      installedTree,
      installedPackages: installed,
      nativeBinaries: native,
      registry: {
        packages: registry,
        provenancePresent: registry.length - provenanceMissing.length,
        provenanceMissing,
      },
      advisories: { command: auditCommand, report: auditReport, highOrCriticalFound },
      violations,
    };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

function hasViolation(violations: PolicyViolation[], ...codes: string[]): boolean {
  const wanted = new Set(codes);
  return violations.some((entry) => wanted.has(entry.code));
}

function hardGates(report: CohortReport, measurement: CohortMeasurement | null): CohortHardGates {
  const structural = report.violations;
  const measured = measurement?.violations ?? [];
  const gates: CohortHardGates = {
    exactBunFloor: Bun.version === BUN_FLOOR && Bun.revision === BUN_FLOOR_REVISION,
    exactDirectSpecs: !hasViolation(structural, 'mutable-direct-spec', 'direct-cohort-spec'),
    directPackageCeiling: !hasViolation(structural, 'direct-package-ceiling'),
    exactResolvedCohort: !hasViolation(
      structural,
      'resolved-cohort-mismatch',
      'resolved-cohort-missing',
    ),
    platformNodeSharedPresent: !hasViolation(
      structural,
      'resolved-cohort-missing',
      'platform-shared-edge',
    ),
    exactBunTypesFloor: !hasViolation(
      structural,
      'bun-types-floor',
      'lock-bun-types-drift',
      'resolved-bun-types-floor',
    ),
    packageLockAligned: !hasViolation(structural, 'lock-workspace-drift', 'lock-bun-types-drift'),
    resolvedPackageCeiling: !hasViolation(structural, 'production-closure-ceiling'),
    lockIntegrityPresent: !hasViolation(structural, 'missing-registry-integrity'),
    wsVersionsUnchanged: report.wsMovement.unchanged,
    wsVersionsDeduped: report.wsMovement.deduped,
    frozenProductionInstall:
      measurement === null
        ? null
        : !measurement.cleanInstall.timedOut && measurement.cleanInstall.exitCode === 0,
    installedByteCeiling:
      measurement === null ? null : !hasViolation(measured, 'installed-byte-ceiling'),
    installedClosureComplete:
      measurement === null ? null : !hasViolation(measured, 'clean-install-missing-package'),
    licensesApproved: measurement === null ? null : !hasViolation(measured, 'incompatible-license'),
    installScriptsReviewed:
      measurement === null ? null : !hasViolation(measured, 'unreviewed-install-script'),
    nativeBinariesReviewed:
      measurement === null ? null : !hasViolation(measured, 'unreviewed-native-binary'),
    registryMetadataAvailable:
      measurement === null ? null : !hasViolation(measured, 'registry-metadata-unavailable'),
    registryIntegrityMatchesLock:
      measurement === null ? null : !hasViolation(measured, 'registry-integrity-mismatch'),
    registrySignatureOrProvenancePresent:
      measurement === null ? null : !hasViolation(measured, 'registry-attestation-missing'),
    cohortProvenancePresent:
      measurement === null ? null : !hasViolation(measured, 'cohort-provenance-missing'),
    highCriticalAdvisoriesAbsent:
      measurement === null
        ? null
        : !measurement.advisories.highOrCriticalFound && !measurement.advisories.command.timedOut,
    all: false,
  };
  gates.all =
    structural.length === 0 &&
    measured.length === 0 &&
    Object.entries(gates)
      .filter(([name]) => name !== 'all')
      .every(([, value]) => value === true);
  return gates;
}

function evidenceFacts(
  report: CohortReport,
  measurement: CohortMeasurement | null,
): CohortEvidenceFacts {
  const installed = measurement?.installedPackages ?? [];
  return {
    directProductionPackageCount: report.direct.count,
    resolvedProductionPackageCount: report.closure.count,
    installedProductionPackageCount: measurement === null ? null : installed.length,
    nodeModulesBytes: measurement?.installedTree.bytes ?? null,
    nodeModulesFiles: measurement?.installedTree.files ?? null,
    cleanInstallMs: measurement?.cleanInstall.elapsedMs ?? null,
    cleanInstallExitCode: measurement?.cleanInstall.exitCode ?? null,
    cleanInstallTimedOut: measurement?.cleanInstall.timedOut ?? null,
    licenses: installed.map(({ name, version, license }) => ({ name, version, license })),
    nativeBinaries: measurement?.nativeBinaries ?? [],
    installScripts: installed
      .filter((entry) => entry.installScripts.length > 0)
      .map(({ name, version, installScripts }) => ({
        name,
        version,
        scripts: installScripts,
      })),
    provenance:
      measurement === null
        ? null
        : {
            checked: measurement.registry.packages.length,
            present: measurement.registry.provenancePresent,
            missing: measurement.registry.provenanceMissing,
            packages: measurement.registry.packages.map(
              ({ name, version, provenance, signatures, integrityMatchesLock }) => ({
                name,
                version,
                provenance,
                signatures,
                integrityMatchesLock,
              }),
            ),
          },
    advisories:
      measurement === null
        ? null
        : {
            auditExitCode: measurement.advisories.command.exitCode,
            auditTimedOut: measurement.advisories.command.timedOut,
            highOrCriticalFound: measurement.advisories.highOrCriticalFound,
            report: measurement.advisories.report,
          },
    wsMovement: report.wsMovement,
  };
}

function stableFactsHash(
  runtime: { bun: string; revision: string; platform: NodeJS.Platform; arch: string },
  facts: CohortEvidenceFacts,
  gates: CohortHardGates,
): string {
  const comparisonFacts = {
    runtime,
    facts: { ...facts, cleanInstallMs: '<excluded>' },
    hardGates: gates,
  };
  return new Bun.CryptoHasher('sha256').update(JSON.stringify(comparisonFacts)).digest('hex');
}

function optionValue(name: '--out'): string | null {
  const index = Bun.argv.indexOf(name);
  if (index === -1) return null;
  const value = Bun.argv[index + 1];
  if (!value || value.startsWith('--')) throw new Error(`${name} requires a value`);
  return value;
}

async function main(): Promise<void> {
  const manifest = JSON.parse(
    readFileSync(path.join(REPO_ROOT, 'package.json'), 'utf8'),
  ) as unknown;
  const lock = parseBunLock(readFileSync(path.join(REPO_ROOT, 'bun.lock'), 'utf8'));
  const report = analyzeEffectCohort(manifest, lock);
  if (Bun.version !== BUN_FLOOR || Bun.revision !== BUN_FLOOR_REVISION) {
    report.violations.push({
      code: 'bun-floor-mismatch',
      detail: `requires Bun ${BUN_FLOOR} (${BUN_FLOOR_REVISION}); got ${Bun.version} (${Bun.revision})`,
    });
    report.ok = false;
  }
  const measurement =
    Bun.argv.includes('--measure') && report.ok ? await measureEffectCohort(report) : null;
  const violations = [...report.violations, ...(measurement?.violations ?? [])];
  const runtime = {
    bun: Bun.version,
    revision: Bun.revision,
    platform: process.platform,
    arch: process.arch,
  };
  const facts = evidenceFacts(report, measurement);
  const gates = hardGates(report, measurement);
  const result = {
    schema: 2,
    kind: 'fleetdeck-effect-p2-cohort',
    recordedAt: new Date().toISOString(),
    runtime,
    report: { ...report, ok: violations.length === 0, violations },
    measurement,
    facts,
    hardGates: gates,
    comparison: {
      stableFactsSha256: stableFactsHash(runtime, facts, gates),
      normalizedScratchPaths: true,
      excludedVolatileFields: [
        'recordedAt',
        'facts.cleanInstallMs',
        'measurement.cleanInstall.elapsedMs',
        'measurement.advisories.command.elapsedMs',
      ],
    },
  };
  const encoded = `${JSON.stringify(result, null, 2)}\n`;
  const output = optionValue('--out');
  if (output) await Bun.write(path.resolve(output), encoded);
  process.stdout.write(encoded);
  if (violations.length > 0) process.exitCode = 1;
}

if (import.meta.main) await main();
