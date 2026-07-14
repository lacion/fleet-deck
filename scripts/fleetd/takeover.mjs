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
    const executable = path.basename(fs.readlinkSync(`/proc/${pid}/exe`)).replace(/ \(deleted\)$/, '');
    const argv = fs.readFileSync(`/proc/${pid}/cmdline`).toString('utf8').split('\0').filter(Boolean);
    const nodeLike = /^(?:node|nodejs|fleetd)$/i.test(executable);
    const fleetdScript = argv.some(arg => /(?:^|[/\\])fleetd(?:\.bundle)?\.mjs$/.test(arg));
    return nodeLike && fleetdScript;
  } catch (err) {
    // WHY ENOENT is decisive: the PID died after kill(0), so it no longer owns
    // HOME. Permission and transient I/O failures are not decisive; retaining
    // the lock is safer than opening a live daemon's SQLite database twice.
    return err?.code !== 'ENOENT';
  }
}

export { pidRecord, pidIsLive, livePidLooksLikeFleetd };

// --------------------------------------------------------------------- semver
// Version comparison uses ONLY the leading numeric major.minor.patch segments
// (a build/prerelease suffix like `-rc.1` or `+sha` is ignored for ordering).
// parseSemver returns an integer array for a well-formed version, or null when
// it cannot establish an order — and null on either side means "no takeover".

export function parseSemver(input) {
  if (typeof input !== 'string') return null;
  // Strip a leading `v` and any prerelease/build suffix, then require exactly
  // three all-digit segments. Anything else (empty, `latest`, `1.x`) is null.
  const core = input.trim().replace(/^v/i, '').split(/[-+]/, 1)[0];
  const parts = core.split('.');
  if (parts.length !== 3) return null;
  const nums = [];
  for (const p of parts) {
    if (!/^\d+$/.test(p)) return null;
    nums.push(Number(p));
  }
  return nums;
}

function isZeroVersion(nums) {
  return nums.every(n => n === 0);
}

// -1 / 0 / 1 numeric comparison of two parseSemver arrays (same length by
// construction — both are three-segment).
export function compareSemver(a, b) {
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] > b[i]) return 1;
    if (a[i] < b[i]) return -1;
  }
  return 0;
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
  if (isZeroVersion(own) || isZeroVersion(other)) return false;
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
