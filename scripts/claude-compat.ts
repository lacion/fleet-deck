// claude-compat.ts -- per-Claude-process compatibility lease for hook shims.
//
// SessionStart resolves the exact executable that owns CLAUDE_PID, asks that
// executable for its version once, and atomically records a small owner-only
// verdict. Every later hook performs only the small verdict read plus a process-
// generation check and touches no network. Linux uses procfs; Darwin rechecks a
// bounded `lsof` executable and `ps` start-token probes (150 ms each) so PID
// reuse stays fail-closed.
// Unknown is inactive: Fleet Deck is optional, while the developer's Claude
// session is not.

import { execFile, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import compatibilityJson from '../compatibility.json' with { type: 'json' };
import packageJson from '../package.json' with { type: 'json' };
import { runKey } from '../src/daemon/run-nonce.ts';

export interface ClaudeCompatibilityPolicy {
  schema: 2;
  claudeCode: {
    min: string;
  };
}

interface StableVersion {
  raw: string;
  parts: [number, number, number];
}

interface CompatibilityIdentity {
  key: number;
  source: 'CLAUDE_PID' | 'proc-ancestor' | 'ppid';
}

interface CompatibilityVerdict {
  schema: 1;
  identity: CompatibilityIdentity;
  generation: ProcessGeneration;
  fleetdeckVersion: string;
  policy: string;
  claudeVersion: string | null;
  active: boolean;
  createdAt: number;
  expiresAt: number;
}

export interface ExecutableFingerprint {
  path: string;
  dev: string;
  ino: string;
  size: number;
  mtimeMs: number;
}

type ProcessGeneration =
  | { kind: 'linux'; startTicks: string; executable: ExecutableFingerprint }
  | { kind: 'darwin'; startToken: string; executable: ExecutableFingerprint }
  | { kind: 'test'; value: string };
type TestGeneration = Extract<ProcessGeneration, { kind: 'test' }>;

interface DetectedClaude {
  version: string | null;
  generation: ProcessGeneration | null;
}

interface DetectOptions {
  env?: NodeJS.ProcessEnv;
  ppid?: number;
  platform?: NodeJS.Platform;
  commandTimeoutMs?: number;
}

interface LeaseOptions extends DetectOptions {
  policy?: ClaudeCompatibilityPolicy;
  fleetdeckVersion?: string;
  now?: number;
}

const DEFAULT_COMMAND_TIMEOUT_MS = 750;
const DARWIN_GENERATION_PROBE_TIMEOUT_MS = 150;
const MAX_VERDICT_BYTES = 4096;
const VERDICT_LIFETIME_MS = 30 * 24 * 3600_000;
const STALE_RETENTION_MS = VERDICT_LIFETIME_MS + 24 * 3600_000;
const POLICY = compatibilityJson as ClaudeCompatibilityPolicy;
const FLEETDECK_VERSION = packageJson.version;

/** Parse only a stable Claude Code X.Y.Z version (with its known CLI label). */
export function parseStableClaudeVersion(output: unknown): StableVersion | null {
  if (typeof output !== 'string') return null;
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?: \(Claude Code\))?$/.exec(
    output.trim(),
  );
  if (!match) return null;
  const parts = [Number(match[1]), Number(match[2]), Number(match[3])] as [number, number, number];
  if (parts.some((part) => !Number.isSafeInteger(part))) return null;
  return { raw: `${parts[0]}.${parts[1]}.${parts[2]}`, parts };
}

function compareVersion(a: StableVersion, b: StableVersion): number {
  for (let i = 0; i < 3; i += 1) {
    const delta = (a.parts[i] ?? 0) - (b.parts[i] ?? 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

function normalizedPolicy(policy: ClaudeCompatibilityPolicy): {
  min: StableVersion;
  signature: string;
} | null {
  if (policy.schema !== 2) return null;
  const min = parseStableClaudeVersion(policy.claudeCode?.min);
  if (!min) return null;
  return { min, signature: `2:${min.raw}` };
}

/** Stable releases at or above the tested floor are active. Invalid policies and prereleases are not. */
export function supportsClaudeVersion(
  version: unknown,
  policy: ClaudeCompatibilityPolicy = POLICY,
): boolean {
  const parsed = parseStableClaudeVersion(version);
  const normalized = normalizedPolicy(policy);
  return Boolean(parsed && normalized && compareVersion(parsed, normalized.min) >= 0);
}

function identity(env: NodeJS.ProcessEnv, ppid: number): CompatibilityIdentity {
  return runKey(env, ppid);
}

export function compatibilityVerdictFile(
  home: string,
  env: NodeJS.ProcessEnv = process.env,
  ppid: number = process.ppid,
): string {
  return path.join(home, `claude-compat-${identity(env, ppid).key}.json`);
}

function policySignature(policy: ClaudeCompatibilityPolicy): string | null {
  return normalizedPolicy(policy)?.signature ?? null;
}

function boundedExecFile(
  executable: string,
  args: string[],
  timeoutMs: number,
): Promise<string | null> {
  return new Promise((resolve) => {
    let settled = false;
    let hardTimer: ReturnType<typeof setTimeout> | null = null;
    const finish = (value: string | null): void => {
      if (settled) return;
      settled = true;
      if (hardTimer) clearTimeout(hardTimer);
      resolve(value);
    };
    let child: ReturnType<typeof execFile>;
    try {
      child = execFile(
        executable,
        args,
        {
          encoding: 'utf8',
          maxBuffer: 16 * 1024,
          timeout: timeoutMs,
          killSignal: 'SIGKILL',
          windowsHide: true,
        },
        (error, stdout) => {
          finish(error ? null : stdout);
        },
      );
    } catch {
      resolve(null);
      return;
    }
    // execFile's timeout waits for process close. This outer bound is the
    // caller-facing guarantee even if a pathological executable ignores it.
    hardTimer = setTimeout(() => {
      try {
        child.kill('SIGKILL');
      } catch {
        /* already gone */
      }
      finish(null);
    }, timeoutMs + 50);
    hardTimer.unref();
  });
}

function commandVersion(executable: string, timeoutMs: number): Promise<string | null> {
  return boundedExecFile(executable, ['--version'], timeoutMs);
}

function linuxExecutable(pid: number): string | null {
  try {
    const executable = fs.readlinkSync(`/proc/${pid}/exe`);
    return path.isAbsolute(executable) ? executable : null;
  } catch {
    return null;
  }
}

async function macExecutable(pid: number, timeoutMs: number): Promise<string | null> {
  const lsof = fs.existsSync('/usr/sbin/lsof') ? '/usr/sbin/lsof' : 'lsof';
  const output = await boundedExecFile(
    lsof,
    ['-a', '-p', String(pid), '-d', 'txt', '-Fn'],
    timeoutMs,
  );
  return output === null ? null : lsofExecutable(output);
}

function lsofExecutable(output: string): string | null {
  return (
    output
      .split(/\r?\n/)
      .find((line) => line.startsWith('n') && path.isAbsolute(line.slice(1)))
      ?.slice(1) ?? null
  );
}

function macExecutableSync(pid: number): string | null {
  try {
    if (!fs.existsSync('/usr/sbin/lsof')) return null;
    const output = execFileSync('/usr/sbin/lsof', ['-a', '-p', String(pid), '-d', 'txt', '-Fn'], {
      encoding: 'utf8',
      maxBuffer: 16 * 1024,
      timeout: DARWIN_GENERATION_PROBE_TIMEOUT_MS,
      killSignal: 'SIGKILL',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return lsofExecutable(output);
  } catch {
    return null;
  }
}

function executableFingerprint(executable: string): ExecutableFingerprint | null {
  try {
    const stat = fs.statSync(executable);
    if (!stat.isFile()) return null;
    return {
      path: executable,
      dev: String(stat.dev),
      ino: String(stat.ino),
      size: stat.size,
      mtimeMs: stat.mtimeMs,
    };
  } catch {
    return null;
  }
}

function linuxStartTicks(pid: number): string | null {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    const tail = stat
      .slice(stat.lastIndexOf(')') + 1)
      .trim()
      .split(/\s+/);
    // /proc stat field 22 (process start ticks); tail[0] is field 3.
    const startTicks = tail[19];
    return typeof startTicks === 'string' && /^\d+$/.test(startTicks) ? startTicks : null;
  } catch {
    return null;
  }
}

function darwinStartToken(pid: number): string | null {
  try {
    const output = execFileSync('/bin/ps', ['-o', 'lstart=', '-p', String(pid)], {
      encoding: 'utf8',
      maxBuffer: 4096,
      timeout: DARWIN_GENERATION_PROBE_TIMEOUT_MS,
      killSignal: 'SIGKILL',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const token = output.trim().replace(/\s+/g, ' ');
    return token && !token.includes('\n') ? token : null;
  } catch {
    return null;
  }
}

function testGeneration(
  env: NodeJS.ProcessEnv,
  processIdentity: CompatibilityIdentity,
): TestGeneration {
  return {
    kind: 'test',
    value: `${processIdentity.key}:${env['FLEETDECK_TEST_CLAUDE_VERSION'] ?? ''}`,
  };
}

async function detectClaude(options: DetectOptions = {}): Promise<DetectedClaude> {
  const env = options.env ?? process.env;
  const ppid = options.ppid ?? process.ppid;
  const processIdentity = identity(env, ppid);
  // Explicit presence (including an empty value) is authoritative in tests: it
  // makes unknown-version cases deterministic without invoking a local binary.
  if (env['FLEETDECK_TEST_CLAUDE_VERSION'] !== undefined) {
    return {
      version: parseStableClaudeVersion(env['FLEETDECK_TEST_CLAUDE_VERSION'])?.raw ?? null,
      generation: testGeneration(env, processIdentity),
    };
  }
  const platform = options.platform ?? process.platform;
  const timeoutMs = Math.max(
    50,
    Math.min(options.commandTimeoutMs ?? DEFAULT_COMMAND_TIMEOUT_MS, 2000),
  );
  let executable: string | null = null;
  if (platform === 'linux') executable = linuxExecutable(processIdentity.key);
  else if (platform === 'darwin') executable = await macExecutable(processIdentity.key, timeoutMs);
  if (!executable) return { version: null, generation: null };
  const fingerprint = executableFingerprint(executable);
  if (!fingerprint) return { version: null, generation: null };
  const generation: ProcessGeneration | null =
    platform === 'linux'
      ? (() => {
          const startTicks = linuxStartTicks(processIdentity.key);
          return startTicks ? { kind: 'linux', startTicks, executable: fingerprint } : null;
        })()
      : (() => {
          const startToken = darwinStartToken(processIdentity.key);
          return startToken ? { kind: 'darwin', startToken, executable: fingerprint } : null;
        })();
  if (!generation) return { version: null, generation: null };
  const output = await commandVersion(executable, timeoutMs);
  return { version: parseStableClaudeVersion(output)?.raw ?? null, generation };
}

/** Resolve and interrogate the exact executable for this Claude process once. */
export async function detectRunningClaudeVersion(
  options: DetectOptions = {},
): Promise<string | null> {
  return (await detectClaude(options)).version;
}

function writeVerdict(file: string, verdict: CompatibilityVerdict): boolean {
  let temp: string | null = null;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    temp = `${file}.${process.pid}.${Date.now()}.tmp`;
    fs.writeFileSync(temp, `${JSON.stringify(verdict)}\n`, { mode: 0o600, flag: 'wx' });
    fs.renameSync(temp, file);
    fs.chmodSync(file, 0o600);
    return true;
  } catch {
    if (temp)
      try {
        fs.unlinkSync(temp);
      } catch {
        /* no partial verdict to trust */
      }
    return false;
  }
}

/** SessionStart-only: detect, decide, and replace this run's cached verdict. */
export async function establishClaudeCompatibility(
  home: string,
  options: LeaseOptions = {},
): Promise<boolean> {
  const env = options.env ?? process.env;
  const ppid = options.ppid ?? process.ppid;
  const processIdentity = identity(env, ppid);
  const policy = options.policy ?? POLICY;
  const signature = policySignature(policy);
  const fleetdeckVersion = options.fleetdeckVersion ?? FLEETDECK_VERSION;
  const file = compatibilityVerdictFile(home, env, ppid);
  // Invalidate synchronously BEFORE the first await. If this new SessionStart
  // is killed while resolving or interrogating Claude, no stale active verdict
  // from a recycled PID can survive and authorize its later hooks.
  try {
    fs.unlinkSync(file);
  } catch {
    /* missing/unwritable already means inactive */
  }
  const detected = await detectClaude({ ...options, env, ppid });
  const claudeVersion = detected.version;
  const active = Boolean(
    signature && detected.generation && supportsClaudeVersion(claudeVersion, policy),
  );
  const now = options.now ?? Date.now();
  pruneClaudeCompatibilityVerdicts(home, now);
  const verdict: CompatibilityVerdict = {
    schema: 1,
    identity: processIdentity,
    generation:
      detected.generation ?? testGeneration({ FLEETDECK_TEST_CLAUDE_VERSION: '' }, processIdentity),
    fleetdeckVersion,
    policy: signature ?? 'invalid',
    claudeVersion,
    active,
    createdAt: now,
    expiresAt: now + VERDICT_LIFETIME_MS,
  };
  return writeVerdict(file, verdict) && active;
}

function sameExecutable(a: ExecutableFingerprint, b: ExecutableFingerprint): boolean {
  return (
    a.path === b.path &&
    a.dev === b.dev &&
    a.ino === b.ino &&
    a.size === b.size &&
    a.mtimeMs === b.mtimeMs
  );
}

export function sameDarwinProcessGeneration(
  expectedStart: string,
  expectedExecutable: ExecutableFingerprint,
  currentStart: string | null,
  currentExecutable: ExecutableFingerprint | null,
): boolean {
  return Boolean(
    currentStart &&
      currentExecutable &&
      currentStart === expectedStart &&
      sameExecutable(currentExecutable, expectedExecutable),
  );
}

function generationIsCurrent(
  expected: ProcessGeneration,
  processIdentity: CompatibilityIdentity,
  env: NodeJS.ProcessEnv,
): boolean {
  if (expected.kind === 'test') {
    return (
      env['FLEETDECK_TEST_CLAUDE_VERSION'] !== undefined &&
      expected.value === testGeneration(env, processIdentity).value
    );
  }
  try {
    process.kill(processIdentity.key, 0);
  } catch {
    return false;
  }
  if (expected.kind === 'linux') {
    const executable = linuxExecutable(processIdentity.key);
    const fingerprint = executable ? executableFingerprint(executable) : null;
    return Boolean(
      fingerprint &&
        linuxStartTicks(processIdentity.key) === expected.startTicks &&
        sameExecutable(fingerprint, expected.executable),
    );
  }
  // Resolve the executable attached to the CURRENT process, never restat the
  // saved path. A recycled PID can have the same one-second `ps` start token;
  // binding the token to lsof's current txt vnode closes that false lease.
  const currentExecutable = macExecutableSync(processIdentity.key);
  const fingerprint = currentExecutable ? executableFingerprint(currentExecutable) : null;
  return sameDarwinProcessGeneration(
    expected.startToken,
    expected.executable,
    darwinStartToken(processIdentity.key),
    fingerprint,
  );
}

/** Keep compatibility state bounded without ever traversing outside HOME. */
export function pruneClaudeCompatibilityVerdicts(home: string, now = Date.now()): number {
  let names: string[];
  try {
    names = fs.readdirSync(home).slice(0, 512);
  } catch {
    return 0;
  }
  let removed = 0;
  for (const name of names) {
    if (!/^claude-compat-\d+\.json$/.test(name)) continue;
    const file = path.join(home, name);
    try {
      if (now - fs.statSync(file).mtimeMs <= STALE_RETENTION_MS) continue;
      fs.unlinkSync(file);
      removed += 1;
    } catch {
      /* raced, unreadable, or already gone */
    }
  }
  return removed;
}

function verdictIsOwnerOnly(file: string): boolean {
  try {
    const stat = fs.statSync(file);
    if (!stat.isFile() || (stat.mode & 0o077) !== 0) return false;
    return typeof process.getuid !== 'function' || stat.uid === process.getuid();
  } catch {
    return false;
  }
}

/** Later hooks: one bounded owner-only file read, with every field fail-closed. */
export function hasActiveClaudeCompatibility(home: string, options: LeaseOptions = {}): boolean {
  const env = options.env ?? process.env;
  const ppid = options.ppid ?? process.ppid;
  const expectedIdentity = identity(env, ppid);
  const policy = options.policy ?? POLICY;
  const signature = policySignature(policy);
  if (!signature) return false;
  const fleetdeckVersion = options.fleetdeckVersion ?? FLEETDECK_VERSION;
  const now = options.now ?? Date.now();
  const file = compatibilityVerdictFile(home, env, ppid);
  if (!verdictIsOwnerOnly(file)) return false;
  try {
    const stat = fs.statSync(file);
    if (stat.size <= 0 || stat.size > MAX_VERDICT_BYTES) return false;
    const parsed: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return false;
    const verdict = parsed as Partial<CompatibilityVerdict>;
    return (
      verdict.schema === 1 &&
      verdict.identity?.key === expectedIdentity.key &&
      verdict.identity.source === expectedIdentity.source &&
      verdict.fleetdeckVersion === fleetdeckVersion &&
      verdict.policy === signature &&
      verdict.active === true &&
      typeof verdict.createdAt === 'number' &&
      typeof verdict.expiresAt === 'number' &&
      verdict.createdAt <= now + 60_000 &&
      verdict.expiresAt >= now &&
      verdict.expiresAt - verdict.createdAt === VERDICT_LIFETIME_MS &&
      verdict.generation !== undefined &&
      generationIsCurrent(verdict.generation, expectedIdentity, env) &&
      supportsClaudeVersion(verdict.claudeVersion, policy)
    );
  } catch {
    return false;
  }
}
