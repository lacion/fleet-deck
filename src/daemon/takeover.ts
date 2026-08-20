// takeover.ts — version-takeover contract, shared by the daemon and the
// SessionStart hook.
//
// The rule: the NEWEST installed plugin version must always end up owning the
// daemon on port 4711. The election in the daemon (fleetd.ts) is "first port-bind wins,
// losers exit 3" — it has no notion of version, so after a plugin upgrade the
// OLD daemon keeps the port forever and new-version hooks silently use its old
// code. This module is how a newer hook evicts an older daemon: it SIGTERMs the
// stale daemon (which already has a tested graceful shutdown — mDNS goodbye,
// pidfile removal, DB close, exit 0), WAITS for that pid to actually die, and
// then lets the caller spawn its own newer build onto the freed port.
//
// Design invariants (settled — see the plan's "version takeover" section):
//   - STRICTLY newer only. Equal or older never evicts. Both versions must
//     parse, and the 0.0.0 sentinel on EITHER side never triggers a takeover
//     (a standalone/unpackaged daemon reports 0.0.0 forever — treating that as
//     "older" would kill+respawn on every SessionStart, an infinite loop).
//   - VERIFY before killing. The /health pid must match HOME/fleetd.pid AND the
//     live process must still look like a fleetd (/proc exe + cmdline). Any
//     disagreement → no kill, the caller fails open onto the running daemon.
//   - WAIT for death, don't escalate. We poll kill(pid,0) until ESRCH (the
//     replacement's claimHome() would startupFatal if it saw the old live pid),
//     but a wedged daemon that ignores SIGTERM is left serving — NO SIGKILL.
//   - FAIL OPEN everywhere. Every uncertain branch keeps the running daemon;
//     the next SessionStart simply retries.
//
// The SessionStart hook imports this as SOURCE from its sibling fleetd/ dir
// (same pattern as ./fleetd/env-scrub.mjs), so this module MUST stay
// dependency-free — node builtins only — to work unbundled.

import fs from 'node:fs';
import path from 'node:path';
import { errCode } from './errors.ts';
import { sleep } from './helpers.ts';

// The HOME ownership lock record: the daemon's pid and (post-0.15) the port it
// bound. `port` is null for a pre-port pidfile that held a bare PID.
interface PidRecord {
  pid: number;
  port: number | null;
}

// ---------------------------------------------------------------- pid helpers
// Moved VERBATIM out of the daemon entry (claimHome/removeOwnedPidFile still consume
// them, now via import) so the hook's takeover path and the daemon's own HOME
// ownership lock share one implementation and can never drift apart.

function pidRecord(text: string): PidRecord | null {
  try {
    const parsed: unknown = JSON.parse(text);
    if (typeof parsed === 'object' && parsed !== null) {
      const rec = parsed as { pid?: unknown; port?: unknown };
      if (typeof rec.pid === 'number' && Number.isInteger(rec.pid) && rec.pid > 0) {
        return {
          pid: rec.pid,
          port: typeof rec.port === 'number' && Number.isInteger(rec.port) ? rec.port : null,
        };
      }
    }
  } catch {
    /* pre-port fleetd.pid was a plain PID; accept it below */
  }
  const pid = Number(text.trim());
  return Number.isInteger(pid) && pid > 0 ? { pid, port: null } : null;
}

function pidIsLive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    // EPERM means the process exists but belongs to another user. Treat that as
    // live: opening its database would be unsafe even though we cannot signal it.
    return errCode(err) !== 'ESRCH';
  }
}

function fleetdProcessIdentity(executable: string, argv: string[]): boolean {
  // Bun's official npm package names its Linux binary `bun.exe` even though it
  // is a native ELF executable. Coder installs that package through nvm, so the
  // exact production process uses `.../node_modules/bun/bin/bun.exe` with
  // `.../fleetdeck.mjs serve`. Keep the runtime allowlist exact while accepting
  // both official Bun distribution names.
  const runtimeLike = /^(?:node|nodejs|bun(?:\.exe)?|fleetd)$/i.test(executable);
  // Match every name the daemon boots under: the production bundle
  // (fleetd.bundle.mjs), the TypeScript source on a full checkout (fleetd.ts,
  // run via Bun's native type-stripping), and the legacy fleetd.mjs. The
  // standalone service intentionally stays in the CLI process while `serve`
  // dynamically imports the bundle, so its Linux argv is
  // `bun .../bin/fleetdeck.mjs serve`; require the adjacent `serve` verb so a
  // harmless `fleetdeck status` process can never satisfy the ownership gate.
  const fleetdScript = argv.some((arg) => /(?:^|[/\\])fleetd(?:\.bundle)?\.(?:mjs|ts)$/.test(arg));
  const fleetdeckServe = argv.some(
    (arg, index) => /(?:^|[/\\])fleetdeck\.(?:mjs|ts)$/.test(arg) && argv[index + 1] === 'serve',
  );
  return runtimeLike && (fleetdScript || fleetdeckServe);
}

function livePidLooksLikeFleetd(pid: number): boolean {
  if (process.platform !== 'linux') return true;
  try {
    // `/proc/<pid>/comm` is the main thread name, not a stable executable
    // identity: Node 24 names it `MainThread` instead of `node`. Resolve the
    // executable symlink so upgrades cannot make a live fleetd look recycled.
    // The dual node:sqlite⇔bun:sqlite seam means a fleetd may legitimately run
    // under Bun (native install `bun`, npm install `bun.exe`) as well as Node.
    const executable = path
      .basename(fs.readlinkSync(`/proc/${pid}/exe`))
      .replace(/ \(deleted\)$/, '');
    const argv = fs
      .readFileSync(`/proc/${pid}/cmdline`)
      .toString('utf8')
      .split('\0')
      .filter(Boolean);
    return fleetdProcessIdentity(executable, argv);
  } catch (err) {
    // WHY ENOENT is decisive: the PID died after kill(0), so it no longer owns
    // HOME. Permission and transient I/O failures are not decisive; retaining
    // the lock is safer than opening a live daemon's SQLite database twice.
    return errCode(err) !== 'ENOENT';
  }
}

export { pidRecord, pidIsLive, fleetdProcessIdentity, livePidLooksLikeFleetd };

// --------------------------------------------------------------------- semver
// Full SemVer precedence over major.minor.patch PLUS prerelease identifiers
// (only build metadata after `+` is ignored, per spec — a prerelease suffix
// like `-rc.1` participates in ordering, so rc.2 > rc.1 and final > rc).
// parseSemver returns { core: [major, minor, patch], pre: [identifiers] } —
// numeric prerelease identifiers as numbers, alphanumeric as strings, and an
// empty `pre` for a final release — or null when it cannot establish an
// order. null on either side means "no takeover".

// A parsed SemVer: core is [major, minor, patch]; pre holds the prerelease
// identifiers (numeric ones as numbers, alphanumeric as strings), empty for a
// final release.
interface Semver {
  core: number[];
  pre: (number | string)[];
}

export function parseSemver(input: unknown): Semver | null {
  if (typeof input !== 'string') return null;
  // Strip a leading `v`, split off any build metadata (never ordered), then
  // split off the prerelease. Require exactly three all-digit core segments
  // and well-formed (non-empty) prerelease identifiers. Anything else (empty,
  // `latest`, `1.x`, `1.0.0-`) is null.
  // `?? ''` satisfies noUncheckedIndexedAccess ([0] is string|undefined); split
  // on a non-empty string always yields at least one element, so it never fires.
  const noBuild = input.trim().replace(/^v/i, '').split('+', 1)[0] ?? '';
  const dash = noBuild.indexOf('-');
  const coreText = dash === -1 ? noBuild : noBuild.slice(0, dash);
  const parts = coreText.split('.');
  if (parts.length !== 3) return null;
  const core: number[] = [];
  for (const p of parts) {
    if (!/^\d+$/.test(p)) return null;
    core.push(Number(p));
  }
  const pre: (number | string)[] = [];
  if (dash !== -1) {
    const preText = noBuild.slice(dash + 1);
    if (preText === '') return null;
    for (const ident of preText.split('.')) {
      if (ident === '') return null;
      pre.push(/^\d+$/.test(ident) ? Number(ident) : ident);
    }
  }
  return { core, pre };
}

function isZeroVersion(nums: number[]): boolean {
  return nums.every((n) => n === 0);
}

// -1 / 0 / 1 full SemVer precedence of two parseSemver results. Core compares
// numerically; on a core tie the prerelease decides per semver.org §11: a
// version WITH a prerelease precedes the same core without one, identifiers
// compare numerically when both numeric, numeric always sorts below
// alphanumeric, and a shorter identifier list is lower when all shared
// identifiers are equal (rc.1 < rc.2 < rc.10 < rc.1.1 < final).
export function compareSemver(a: Semver, b: Semver): number {
  for (let i = 0; i < a.core.length; i += 1) {
    // Both arrays are length-3 (parseSemver enforces it), so the indices are in
    // bounds; the guard satisfies noUncheckedIndexedAccess and never fires.
    const ai = a.core[i];
    const bi = b.core[i];
    if (ai === undefined || bi === undefined) break;
    if (ai > bi) return 1;
    if (ai < bi) return -1;
  }
  if (a.pre.length === 0 && b.pre.length === 0) return 0;
  if (a.pre.length === 0) return 1;
  if (b.pre.length === 0) return -1;
  const len = Math.min(a.pre.length, b.pre.length);
  for (let i = 0; i < len; i += 1) {
    const x = a.pre[i];
    const y = b.pre[i];
    if (x === undefined || y === undefined) break;
    if (x === y) continue;
    // Capturing the typeof into a const lets TS narrow x/y to number inside the
    // `xNum && yNum` branch (aliased-const narrowing), so the `>` is well-typed.
    const xNum = typeof x === 'number';
    const yNum = typeof y === 'number';
    if (xNum && yNum) return x > y ? 1 : -1;
    if (xNum) return -1;
    if (yNum) return 1;
    return x > y ? 1 : -1;
  }
  if (a.pre.length === b.pre.length) return 0;
  return a.pre.length > b.pre.length ? 1 : -1;
}

// The single takeover predicate: is `ownVersion` a strictly-newer, non-sentinel
// build that should evict a daemon reporting `daemonVersion`?
export function shouldTakeOver(ownVersion: unknown, daemonVersion: unknown): boolean {
  const own = parseSemver(ownVersion);
  const other = parseSemver(daemonVersion);
  // Both sides must parse: an unknown version on either end means we cannot
  // prove an ordering, so we never evict on a guess.
  if (!own || !other) return false;
  // 0.0.0 loop guard: a standalone/unpackaged daemon (or a failed package.json
  // read) reports 0.0.0 forever. Treating that as "older than our real version"
  // would make every SessionStart kill it and respawn a daemon that comes back
  // reporting 0.0.0 again — an endless takeover loop. A genuine upgrade always
  // carries a non-zero version on BOTH ends, so refuse whenever either is 0.0.0.
  if (isZeroVersion(own.core) || isZeroVersion(other.core)) return false;
  return compareSemver(own, other) > 0;
}

// ----------------------------------------------------------------- verify/kill

// Trust gate before accepting daemon output OR sending SIGTERM. The pid
// reported by /health must match HOME/fleetd.pid, an expected listener port
// must match that same ownership record exactly, and the live process must
// still carry a fleetd /proc shape. A legacy portless record cannot satisfy a
// caller that knows its port. Any mismatch is unowned and fails closed.
export function verifyDaemonPid(pid: number, home: string, expectedPort?: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  if (
    expectedPort !== undefined &&
    (!Number.isInteger(expectedPort) || expectedPort < 1 || expectedPort > 65535)
  ) {
    return false;
  }
  // No `= null` seed: the only path past the catch is the try succeeding, so TS
  // proves definite assignment and the seed would be a dead write.
  let record: PidRecord | null;
  try {
    record = pidRecord(fs.readFileSync(path.join(home, 'fleetd.pid'), 'utf8'));
  } catch {
    // No/unreadable pidfile: cannot confirm ownership → do not kill.
    return false;
  }
  if (record?.pid !== pid) return false;
  if (expectedPort !== undefined && record.port !== expectedPort) return false;
  // livePidLooksLikeFleetd also folds in liveness: a dead pid fails the
  // /proc/<pid>/exe read with ENOENT and is rejected. On non-linux the /proc
  // check is unavailable and returns true, so there the pidfile match is the
  // whole gate (matches the daemon's own claimHome behaviour).
  return livePidLooksLikeFleetd(pid);
}

// A daemon observed after ANY spawn must report EXACTLY the hook's own build
// before that spawn is accepted. Hooks from 0.20.1 and 0.20.2 can both observe
// a cold port (or evict the same stale daemon), both spawn, and then poll the
// one candidate the port-bind election kept — an election with no notion of
// version. A bare truthy /health would let the 0.20.2 hook accept 0.20.1's code
// (or vice versa), settling on whichever build bound first instead of the
// newest installed one. The hook therefore checks every post-spawn
// health.version; a different version means a competitor won, so it re-enters
// ensureServer with that daemon now healthy and the normal strictly-newer
// takeover resolves the ordering. A shared home is trusted to be same-user
// (claimHome + the pidfile + token all live there), so the identity check is
// deliberately string equality — not "at least as new": exact match is the
// whole contract and cannot settle on a WRONG build even if the env is somehow
// shared across users.
export function replacementMatches(ownVersion: unknown, healthVersion: unknown): boolean {
  return (
    typeof ownVersion === 'string' &&
    typeof healthVersion === 'string' &&
    ownVersion.length > 0 &&
    ownVersion === healthVersion
  );
}

const defaultSleep = sleep;

// SIGTERM the daemon and resolve to whether it actually DIED within timeoutMs.
// Graceful only — SIGTERM invokes the daemon's tested shutdown; we then poll
// kill(pid,0) until ESRCH so the caller only spawns a replacement once the old
// process is gone (and the port + pidfile are released). Returns:
//   true  — the pid is gone (ESRCH), safe to spawn a replacement.
//   false — still alive after the timeout, or not ours to signal (EPERM): the
//           caller must fail open and leave the running daemon in place.
// `sleep` is injectable so unit tests can drive the poll without real waits.
export async function terminateDaemon(
  pid: number,
  {
    timeoutMs = 2000,
    sleep = defaultSleep,
    signal,
  }: { timeoutMs?: number; sleep?: (ms: number) => Promise<void>; signal?: AbortSignal } = {},
): Promise<boolean> {
  signal?.throwIfAborted();
  try {
    process.kill(pid, 'SIGTERM');
  } catch (err) {
    // ESRCH: already gone — that IS a successful handoff (port/pidfile free).
    if (errCode(err) === 'ESRCH') return true;
    // EPERM or anything else: not our process to end. Report not-dead so the
    // caller fails open instead of assuming a clean takeover.
    return false;
  }
  const stepMs = 100;
  const steps = Math.max(1, Math.ceil(timeoutMs / stepMs));
  for (let i = 0; i < steps; i += 1) {
    if (!signal) {
      await sleep(stepMs);
    } else {
      await new Promise<void>((resolve, reject) => {
        let settled = false;
        const finish = (error?: unknown): void => {
          if (settled) return;
          settled = true;
          signal.removeEventListener('abort', onAbort);
          if (error === undefined) resolve();
          else reject(error);
        };
        const onAbort = (): void => {
          try {
            signal.throwIfAborted();
          } catch (error) {
            finish(error);
          }
        };
        signal.addEventListener('abort', onAbort, { once: true });
        if (signal.aborted) {
          onAbort();
          return;
        }
        void sleep(stepMs).then(() => finish(), finish);
      });
    }
    if (!pidIsLive(pid)) return true;
  }
  // Wedged daemon that ignored SIGTERM. NO SIGKILL escalation — a stale daemon
  // still serving is safer than force-killing one out from under its panes.
  return !pidIsLive(pid);
}
