// takeover.mjs — version-takeover contract, shared by the daemon and the
// SessionStart hook.
//
// The rule: the NEWEST installed plugin version must always end up owning the
// daemon on port 4711. The election in fleetd.mjs is "first port-bind wins,
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

// ---------------------------------------------------------------- pid helpers
// Moved VERBATIM from fleetd.mjs (claimHome/removeOwnedPidFile still consume
// them, now via import) so the hook's takeover path and the daemon's own HOME
// ownership lock share one implementation and can never drift apart.

function pidRecord(text) {
  try {
    const parsed = JSON.parse(String(text));
    if (Number.isInteger(parsed?.pid) && parsed.pid > 0) {
      return { pid: parsed.pid, port: Number.isInteger(parsed.port) ? parsed.port : null };
    }
  } catch { /* pre-port fleetd.pid was a plain PID; accept it below */ }
  const pid = Number(String(text).trim());
  return Number.isInteger(pid) && pid > 0 ? { pid, port: null } : null;
}

function pidIsLive(pid) {
  try { process.kill(pid, 0); return true; } catch (err) {
    // EPERM means the process exists but belongs to another user. Treat that as
    // live: opening its database would be unsafe even though we cannot signal it.
    return err?.code !== 'ESRCH';
  }
}

function livePidLooksLikeFleetd(pid) {
  if (process.platform !== 'linux') return true;
  try {
    // `/proc/<pid>/comm` is the main thread name, not a stable executable
    // identity: Node 24 names it `MainThread` instead of `node`. Resolve the
    // executable symlink so upgrades cannot make a live fleetd look recycled.
    // The dual node:sqlite⇔bun:sqlite seam means a fleetd may legitimately run
    // under bun (basename `bun`) as well as node — accept either runtime.
    const executable = path.basename(fs.readlinkSync(`/proc/${pid}/exe`)).replace(/ \(deleted\)$/, '');
    const argv = fs.readFileSync(`/proc/${pid}/cmdline`).toString('utf8').split('\0').filter(Boolean);
    const runtimeLike = /^(?:node|nodejs|bun|fleetd)$/i.test(executable);
    const fleetdScript = argv.some(arg => /(?:^|[/\\])fleetd(?:\.bundle)?\.mjs$/.test(arg));
    return runtimeLike && fleetdScript;
  } catch (err) {
    // WHY ENOENT is decisive: the PID died after kill(0), so it no longer owns
    // HOME. Permission and transient I/O failures are not decisive; retaining
    // the lock is safer than opening a live daemon's SQLite database twice.
    return err?.code !== 'ENOENT';
  }
}

export { pidRecord, pidIsLive, livePidLooksLikeFleetd };

// --------------------------------------------------------------------- semver
// Full SemVer precedence over major.minor.patch PLUS prerelease identifiers
// (only build metadata after `+` is ignored, per spec — a prerelease suffix
// like `-rc.1` participates in ordering, so rc.2 > rc.1 and final > rc).
// parseSemver returns { core: [major, minor, patch], pre: [identifiers] } —
// numeric prerelease identifiers as numbers, alphanumeric as strings, and an
// empty `pre` for a final release — or null when it cannot establish an
// order. null on either side means "no takeover".

export function parseSemver(input) {
  if (typeof input !== 'string') return null;
  // Strip a leading `v`, split off any build metadata (never ordered), then
  // split off the prerelease. Require exactly three all-digit core segments
  // and well-formed (non-empty) prerelease identifiers. Anything else (empty,
  // `latest`, `1.x`, `1.0.0-`) is null.
  const noBuild = input.trim().replace(/^v/i, '').split('+', 1)[0];
  const dash = noBuild.indexOf('-');
  const coreText = dash === -1 ? noBuild : noBuild.slice(0, dash);
  const parts = coreText.split('.');
  if (parts.length !== 3) return null;
  const core = [];
  for (const p of parts) {
    if (!/^\d+$/.test(p)) return null;
    core.push(Number(p));
  }
  const pre = [];
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

function isZeroVersion(nums) {
  return nums.every(n => n === 0);
}

// -1 / 0 / 1 full SemVer precedence of two parseSemver results. Core compares
// numerically; on a core tie the prerelease decides per semver.org §11: a
// version WITH a prerelease precedes the same core without one, identifiers
// compare numerically when both numeric, numeric always sorts below
// alphanumeric, and a shorter identifier list is lower when all shared
// identifiers are equal (rc.1 < rc.2 < rc.10 < rc.1.1 < final).
export function compareSemver(a, b) {
  for (let i = 0; i < a.core.length; i += 1) {
    if (a.core[i] > b.core[i]) return 1;
    if (a.core[i] < b.core[i]) return -1;
  }
  if (a.pre.length === 0 && b.pre.length === 0) return 0;
  if (a.pre.length === 0) return 1;
  if (b.pre.length === 0) return -1;
  const len = Math.min(a.pre.length, b.pre.length);
  for (let i = 0; i < len; i += 1) {
    const x = a.pre[i];
    const y = b.pre[i];
    if (x === y) continue;
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
export function shouldTakeOver(ownVersion, daemonVersion) {
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

// Gate before any SIGTERM: prove the /health pid is really OUR daemon. The pid
// reported by /health must match the pid recorded in HOME/fleetd.pid (the HOME
// ownership lock) AND the live process must still carry a fleetd /proc shape.
// Any mismatch → false, and the caller fails open onto the running daemon
// rather than signalling a process it cannot positively identify.
export function verifyDaemonPid(pid, home) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  let record = null;
  try {
    record = pidRecord(fs.readFileSync(path.join(home, 'fleetd.pid'), 'utf8'));
  } catch {
    // No/unreadable pidfile: cannot confirm ownership → do not kill.
    return false;
  }
  if (record?.pid !== pid) return false;
  // livePidLooksLikeFleetd also folds in liveness: a dead pid fails the
  // /proc/<pid>/exe read with ENOENT and is rejected. On non-linux the /proc
  // check is unavailable and returns true, so there the pidfile match is the
  // whole gate (matches the daemon's own claimHome behaviour).
  return livePidLooksLikeFleetd(pid);
}

// A replaced daemon must report EXACTLY the hook's own build before the hook
// accepts it. Two newer hooks (0.20.1 and 0.20.2) can evict the same stale
// daemon together: both observe the old pid die, both spawn, and the port-bind
// election keeps only one — with no notion of version. A bare truthy /health
// would let the 0.20.2 hook accept 0.20.1's code (or vice versa), settling the
// upgrade on whichever build happened to bind first instead of the newest
// installed one. After a takeover spawn the hook therefore re-checks
// health.version; a different version means a competitor won the race, so the
// hook re-enters ensureServer with that daemon now healthy and the normal
// strictly-newer takeover resolves the ordering. A shared home is trusted to
// be same-user (claimHome + the pidfile + token all live there), so the
// identity check is deliberately string equality — not "at least as new":
// exact match is the whole contract and cannot settle on a WRONG build even if
// the env is somehow shared across users.
export function replacementMatches(ownVersion, healthVersion) {
  return typeof ownVersion === 'string' && typeof healthVersion === 'string'
    && ownVersion.length > 0 && ownVersion === healthVersion;
}

const defaultSleep = (ms) => new Promise(resolve => setTimeout(resolve, ms));

// SIGTERM the daemon and resolve to whether it actually DIED within timeoutMs.
// Graceful only — SIGTERM invokes the daemon's tested shutdown; we then poll
// kill(pid,0) until ESRCH so the caller only spawns a replacement once the old
// process is gone (and the port + pidfile are released). Returns:
//   true  — the pid is gone (ESRCH), safe to spawn a replacement.
//   false — still alive after the timeout, or not ours to signal (EPERM): the
//           caller must fail open and leave the running daemon in place.
// `sleep` is injectable so unit tests can drive the poll without real waits.
export async function terminateDaemon(pid, { timeoutMs = 2000, sleep = defaultSleep } = {}) {
  try {
    process.kill(pid, 'SIGTERM');
  } catch (err) {
    // ESRCH: already gone — that IS a successful handoff (port/pidfile free).
    if (err?.code === 'ESRCH') return true;
    // EPERM or anything else: not our process to end. Report not-dead so the
    // caller fails open instead of assuming a clean takeover.
    return false;
  }
  const stepMs = 100;
  const steps = Math.max(1, Math.ceil(timeoutMs / stepMs));
  for (let i = 0; i < steps; i += 1) {
    await sleep(stepMs);
    if (!pidIsLive(pid)) return true;
  }
  // Wedged daemon that ignored SIGTERM. NO SIGKILL escalation — a stale daemon
  // still serving is safer than force-killing one out from under its panes.
  return !pidIsLive(pid);
}
