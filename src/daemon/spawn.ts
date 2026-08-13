// spawn.ts — the v1.2 tmux adapter (v1.2 — dynamic fleet). One backend,
// tmux, verbatim from the firstmate analysis: create/exists/alive/kill
// primitives, nothing more.
//
// Non-negotiables enforced here:
//   - ALL tmux command construction is argv arrays through execFile — never a
//     shell string containing user text. Verified on tmux 3.7b: multi-arg
//     new-window execvp()s the command verbatim (argc preserved, `;`/`$()`/
//     quotes arrive as literal bytes in the child's argv — no shell exists to
//     interpret them).
//   - Scoped names everywhere (firstmate's cross-home collision lesson):
//     session `fleetdeck-<port>`, windows `fd<port>-<callsign>`. Every
//     list/kill path matches the exact scoped name, never a bare index.
//   - Windows get `remain-on-exit on`: claude's exit (or SIGKILL) must not
//     vaporize the pane — the human may want the scrollback (CONTRACT), and a
//     dead pane is the deterministic crash signal for owned-pane liveness.
//     Verified: a dead pane keeps reporting the ORIGINAL command in
//     #{pane_current_command} (with #{pane_dead}=1), so liveness checks MUST
//     read pane_dead too — the command name alone would say "claude" forever.
//   - FLEETDECK_TMUX_SOCKET selects an isolated tmux server with `-L <name>`
//     for every adapter command. Blank values retain tmux's default socket.
//
// Test override (CONTRACT): FLEETDECK_SPAWN_CMD — when set, the daemon runs
// argv [FLEETDECK_SPAWN_CMD, JSON.stringify(spec)] instead of tmux; the
// fixture records the spec and may itself POST /hook/SessionStart with the
// pre-issued session_id. Capability reports available:true, reason
// 'test-override'.

import { execFileSync, spawn as spawnChild } from 'node:child_process';
import { execFileP } from './exec.ts';
import { randomUUID } from 'node:crypto';
import { constants as fsConstants } from 'node:fs';
import { link, open, rename, unlink } from 'node:fs/promises';
import type { FileHandle } from 'node:fs/promises';
import path from 'node:path';
import { MIN_TMUX_VERSION, tmuxVersionCapability } from '../../bin/tmux-version.ts';

type Port = number | string;
type TmuxResult =
  | { ok: true; out: string }
  | { ok: false; code?: string | number | null | undefined; error: string };
type GenResult =
  | { ok: true; out: string; authoritativeEmpty?: boolean; generation?: string }
  | {
      ok: false;
      code?: string | number | null | undefined;
      error?: string;
      generationError?: string;
    };
interface ServerGeneration {
  reachable: boolean;
  absent: boolean;
  generation: string | null;
  serverPid: number | null;
}
interface PersistedGeneration {
  generation: string;
  serverPid: number | null;
  legacy: boolean;
}
interface GenerationRecord {
  generation: string;
  serverPid: number;
}
interface PrepareState {
  enabled: boolean;
  expected: PersistedGeneration | null;
  verified: boolean;
  authoritativeEmpty?: boolean;
  oldGenerationLost?: boolean;
  blockedByCertificate?: boolean;
}
interface TmuxCapability {
  available: boolean;
  reason?: string;
  version?: string;
}
interface ProbeState extends TmuxCapability {
  at: number;
}
interface ScopedWindow {
  session: string;
  window: string;
  window_id: string;
  pane_dead: boolean;
  pane_cmd: string;
}
type KillResult =
  { ok: true; window_id: string } | { ok: false; gone?: boolean; stale?: boolean; error?: string };
interface NewWindowSpec {
  port: Port;
  callsign: string;
  cwd: string;
  argv: readonly string[];
  env?: Record<string, string> | null;
}

function errMessage(err: unknown): string {
  return err instanceof Error && err.message ? err.message : String(err);
}
function errDetail(err: unknown): string {
  if (err instanceof Error) {
    const e = err as NodeJS.ErrnoException;
    if (e.code) return e.code;
    if (e.message) return e.message;
  }
  return String(err);
}
function errCode(err: unknown): string | undefined {
  return err instanceof Error ? (err as NodeJS.ErrnoException).code : undefined;
}

const TMUX_TIMEOUT_MS = 5_000;
// SEPARATOR CONTRACT — printable ASCII only, and here is why that is not a
// style preference.
//
// tmux sanitizes its own formatted output, and what counts as "safe to print"
// depends on the SERVER's locale, not ours. Under a UTF-8 locale a literal TAB
// round-trips intact; under the C/POSIX locale tmux rewrites it to "_" — in
// `display-message -p` and in `list-* -F` alike. That is not a version quirk:
// verified identical on tmux 3.4 and 3.7b.
//
//   LC_ALL=C.UTF-8  tmux display-message -p "A<TAB>B"  ->  A<TAB>B
//   LC_ALL=C        tmux display-message -p "A<TAB>B"  ->  A_B
//
// A C locale is the DEFAULT in minimal containers, and is common for systemd
// units and cron, so this is a mainstream configuration rather than an exotic
// one. With TAB as the separator every round-trip below collapses to a single
// field there: the generation read yields no pid, the UUID match fails, the
// server generation can never be claimed, and EVERY spawn fails — while a
// UTF-8 developer machine and a UTF-8 CI runner both look perfectly healthy.
//
// So the separator must be a character tmux never rewrites in any locale, and
// one that cannot occur inside a delimited value. "~" satisfies both: the
// fields are a hex+dash UUID, a pid, `fleetdeck-<port>`, `fd<port>-<callsign>`
// (callsign charset is enforced elsewhere), `@N`, 0/1, and finally a process
// name — and "~" carries no meaning in tmux format syntax. A literal unit
// separator is NOT usable: tmux escapes it as "\\037". The strict field,
// session, and id validation below still rejects malformed or shifted records.
// Exported so fake-tmux fixtures build their canned listings from the SAME
// constant the parser reads. Hard-coding the separator in a fixture silently
// decouples it from the daemon the moment this changes.
export const FIELD_SEP = '~';

// Run one tmux command (argv), retaining failure details for probes whose
// callers must distinguish authoritative absence from UNKNOWN.
async function tmuxResult(
  args: readonly string[],
  { noStart = false }: { noStart?: boolean } = {},
): Promise<TmuxResult> {
  try {
    const socket = process.env['FLEETDECK_TMUX_SOCKET']?.trim();
    const argv = [...(socket ? ['-L', socket] : []), ...(noStart ? ['-N'] : []), ...args];
    const r = await execFileP('tmux', argv, { timeout: TMUX_TIMEOUT_MS });
    return r.ok ? { ok: true, out: r.out } : { ok: false, code: r.code, error: r.err };
  } catch (err) {
    return { ok: false, error: errMessage(err) };
  }
}

// Most probes intentionally keep the historical null-or-stdout contract.
async function tmux(args: readonly string[]): Promise<string | null> {
  const result = await tmuxResult(args);
  return result.ok ? result.out : null;
}

// --------------------------------------------------- server generation identity
// A tmux socket pathname is not an identity: unlinking a live server's socket
// lets a second server bind the same -L label while the original panes keep
// running but become unreachable. Persisting the generation UUID and owning
// tmux PID turns an empty listing from that replacement into UNKNOWN instead of
// authoritative absence, while a definitive ESRCH permits safe recovery after
// normal server exit. Direct adapter tests historically omit FLEETDECK_HOME;
// that remains the explicit legacy/test seam, while production supplies HOME.
const GENERATION_UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const GENERATION_HEADER = '__fleetdeck_tmux_generation__=';
const GENERATION_MISMATCH = '__fleetdeck_tmux_generation_mismatch__';
const generationLocks = new Map<string, Promise<PrepareState>>();
// O_NOFOLLOW may be absent on some platforms; the `?? 0` keeps the open flags
// well-formed there. Hoisted so readPersistedGeneration and hasRetiredGeneration
// share one definition.
const RD_NOFOLLOW = fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0); // eslint-disable-line @typescript-eslint/no-unnecessary-condition -- O_NOFOLLOW may be undefined on some platforms

function generationPort(port: Port): string {
  const value = String(port);
  if (!/^\d+$/.test(value)) throw new Error('invalid fleet port for tmux generation identity');
  return value;
}

const generationOption = (port: Port): string => `@fleetdeck_generation_${generationPort(port)}`;
const generationFile = (home: string, port: Port): string =>
  path.join(home, `tmux-generation-${generationPort(port)}`);
// The DEATH CERTIFICATE for a retired claim, kept beside it. Proving an owner
// dead by ESRCH is the only evidence that its panes died with it, and unlinking
// the claim used to destroy that evidence at the instant it was obtained: the
// retiring call could answer "authoritatively empty" and every call after it was
// back to UNKNOWN. Recording the proof instead makes absence a STABLE answer, so
// revive/adopt/rc keep working rather than wedging until a human starts a tmux
// server by hand. Kept in its own file so claiming a replacement server still
// uses the first-writer-wins link() publish on the claim path, untouched.
const retiredGenerationFile = (home: string, port: Port): string =>
  `${generationFile(home, port)}.retired`;

function generationHome(): string | null {
  const home = process.env['FLEETDECK_HOME']?.trim();
  return home || null; // eslint-disable-line @typescript-eslint/prefer-nullish-coalescing -- empty/whitespace HOME is not a valid home
}

async function readPersistedGeneration(
  home: string,
  port: Port,
): Promise<PersistedGeneration | null> {
  const file = generationFile(home, port);
  let handle: FileHandle;
  try {
    // O_NOFOLLOW prevents a substituted symlink from redirecting either the
    // confidentiality chmod or the identity read outside FLEETDECK_HOME.
    handle = await open(file, RD_NOFOLLOW);
  } catch (err) {
    if (errCode(err) === 'ENOENT') return null;
    throw new Error(`cannot read persisted tmux generation (${errDetail(err)})`, { cause: err });
  }
  try {
    const stat = await handle.stat();
    if (!stat.isFile()) throw new Error('persisted tmux generation is not a regular file');
    // Tighten files created by older/prerelease builds too. Failure is not
    // best-effort: the contract says this persistent identity is owner-only.
    await handle.chmod(0o600);
    const text = await handle.readFile('utf8');
    const value = text.endsWith('\n') ? text.slice(0, -1) : text;
    // Legacy prerelease files contained only the UUID. They may corroborate a
    // reachable server and then be upgraded, but cannot authorize recovery:
    // without a PID there is no proof that the old server is dead.
    if (GENERATION_UUID_RE.test(value)) {
      return { generation: value.toLowerCase(), serverPid: null, legacy: true };
    }
    let record: unknown = null;
    try {
      record = JSON.parse(value);
    } catch {
      /* strict error below */
    }
    if (record === null || typeof record !== 'object' || Array.isArray(record)) {
      throw new Error('persisted tmux generation is malformed');
    }
    const keys = Object.keys(record).sort();
    const rec = record as { generation?: unknown; serverPid?: unknown };
    const gen = rec.generation;
    const pid = rec.serverPid;
    if (
      keys.length !== 2 ||
      keys[0] !== 'generation' ||
      keys[1] !== 'serverPid' ||
      typeof gen !== 'string' ||
      !GENERATION_UUID_RE.test(gen) ||
      typeof pid !== 'number' ||
      !Number.isSafeInteger(pid) ||
      pid <= 1
    ) {
      throw new Error('persisted tmux generation is malformed');
    }
    return { generation: gen.toLowerCase(), serverPid: pid, legacy: false };
  } finally {
    await handle.close();
  }
}

async function persistGeneration(
  home: string,
  port: Port,
  record: GenerationRecord,
): Promise<PersistedGeneration | null> {
  const file = generationFile(home, port);
  const temp = path.join(home, `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let handle: FileHandle | null = null;
  try {
    handle = await open(temp, 'wx', 0o600);
    await handle.writeFile(
      `${JSON.stringify({ generation: record.generation, serverPid: record.serverPid })}\n`,
      'utf8',
    );
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = null;
    try {
      // link is an atomic no-replace publish on the same filesystem. A second
      // claimant can never overwrite the first durable expected generation.
      await link(temp, file);
    } catch (err) {
      if (errCode(err) !== 'EEXIST') throw err;
    }
  } catch (err) {
    throw new Error(`cannot persist tmux generation (${errDetail(err)})`, { cause: err });
  } finally {
    try {
      await handle?.close();
    } catch {
      /* primary error wins */
    }
    try {
      await unlink(temp);
    } catch (err) {
      if (errCode(err) !== 'ENOENT') {
        /* best-effort temp cleanup */
      }
    }
  }
  return readPersistedGeneration(home, port);
}

async function replacePersistedGeneration(
  home: string,
  port: Port,
  record: GenerationRecord,
): Promise<PersistedGeneration | null> {
  const file = generationFile(home, port);
  const temp = path.join(home, `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let handle: FileHandle | null = null;
  try {
    handle = await open(temp, 'wx', 0o600);
    await handle.writeFile(
      `${JSON.stringify({ generation: record.generation, serverPid: record.serverPid })}\n`,
      'utf8',
    );
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temp, file); // atomic old-record -> strict-record migration
  } catch (err) {
    throw new Error(`cannot replace persisted tmux generation (${errDetail(err)})`, { cause: err });
  } finally {
    try {
      await handle?.close();
    } catch {
      /* primary error wins */
    }
    try {
      await unlink(temp);
    } catch (err) {
      if (errCode(err) !== 'ENOENT') {
        /* best-effort temp cleanup */
      }
    }
  }
  return readPersistedGeneration(home, port);
}

// Absence has to be PROVEN, never inferred from a probe that merely failed.
// tmux has exactly two verdicts that mean "nothing is listening here": a socket
// file with no listener behind it (`no server running on <path>`) and no socket
// file at all (`error connecting to <path> (No such file or directory)`).
// Everything else that makes the probe fail — a timeout, a missing or shadowed
// tmux binary, EACCES on the socket directory, fork exhaustion, an over-long
// socket path — is a TRANSPORT fault that says nothing about whether panes are
// running. Only the two verdicts below may be treated as authoritative absence;
// conflating them with transport faults would let a single broken tmux
// invocation tell boot reconciliation that a live fleet is gone.
const SERVER_ABSENT_RE =
  /(?:^|\n)(?:no server running on |error connecting to .*\(No such file or directory\))/i;

async function readServerGeneration(port: Port): Promise<ServerGeneration> {
  // Read both fields in one command from one reachable server. Separate tmux
  // clients could straddle a socket replacement and manufacture an identity no
  // server ever held.
  const result = await tmuxResult(
    ['display-message', '-p', `#{${generationOption(port)}}${FIELD_SEP}#{pid}`],
    { noStart: true },
  );
  if (!result.ok) {
    return {
      reachable: false,
      absent: SERVER_ABSENT_RE.test(result.error),
      generation: null,
      serverPid: null,
    };
  }
  const value = result.out.endsWith('\n') ? result.out.slice(0, -1) : result.out;
  const [generation, pidText, ...extra] = value.split(FIELD_SEP);
  const serverPid = Number(pidText);
  return {
    reachable: true,
    absent: false,
    generation:
      generation !== undefined && GENERATION_UUID_RE.test(generation)
        ? generation.toLowerCase()
        : null,
    serverPid:
      extra.length === 0 && Number.isSafeInteger(serverPid) && serverPid > 1 ? serverPid : null,
  };
}

function pidState(pid: number | null): 'unknown' | 'alive' | 'dead' {
  if (pid === null || !Number.isSafeInteger(pid) || pid <= 1) return 'unknown';
  try {
    process.kill(pid, 0);
    return 'alive'; // includes PID reuse: a live unrelated process blocks reset
  } catch (err) {
    if (errCode(err) === 'ESRCH') return 'dead';
    return 'unknown'; // EPERM and every platform ambiguity fail closed
  }
}

const sameRecord = (left: PersistedGeneration | null, right: PersistedGeneration | null): boolean =>
  left !== null &&
  right !== null &&
  left.generation === right.generation &&
  left.serverPid === right.serverPid;

/** Record the death certificate, then drop the claim. Written FIRST and by
 * atomic rename: a crash between the two leaves both files, and the claim wins
 * on the next read (a live claim is always more specific than a certificate for
 * an older one), so the only cost is a stale certificate that the next
 * successful claim clears. */
async function recordRetiredGeneration(
  home: string,
  port: Port,
  expected: PersistedGeneration,
): Promise<void> {
  const file = retiredGenerationFile(home, port);
  const temp = path.join(home, `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let handle: FileHandle | null = null;
  try {
    handle = await open(temp, 'wx', 0o600);
    await handle.writeFile(
      `${JSON.stringify({
        retiredGeneration: expected.generation,
        retiredServerPid: expected.serverPid,
      })}\n`,
      'utf8',
    );
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temp, file);
  } catch (err) {
    throw new Error(`cannot record retired tmux generation (${errDetail(err)})`, { cause: err });
  } finally {
    try {
      await handle?.close();
    } catch {
      /* primary error wins */
    }
    try {
      await unlink(temp);
    } catch (err) {
      if (errCode(err) !== 'ENOENT') {
        /* best-effort temp cleanup */
      }
    }
  }
}

/** Is a proven-dead owner on record for this port? O_NOFOLLOW for the same
 * reason the claim read uses it. Anything unreadable proves nothing and is
 * reported as absent-of-proof, which fails closed at the caller. */
async function hasRetiredGeneration(home: string, port: Port): Promise<boolean> {
  let handle: FileHandle;
  try {
    handle = await open(retiredGenerationFile(home, port), RD_NOFOLLOW);
  } catch {
    return false;
  }
  try {
    return (await handle.stat()).isFile();
  } catch {
    return false;
  } finally {
    try {
      await handle.close();
    } catch {
      /* proof-read only */
    }
  }
}

async function clearRetiredGeneration(home: string, port: Port): Promise<void> {
  try {
    await unlink(retiredGenerationFile(home, port));
  } catch (err) {
    if (errCode(err) !== 'ENOENT') {
      /* best-effort: a stale certificate only matters while unclaimed */
    }
  }
}

async function retireDeadGeneration(
  home: string,
  port: Port,
  expected: PersistedGeneration,
): Promise<boolean> {
  if (expected.serverPid === null || pidState(expected.serverPid) !== 'dead') return false;
  // Re-read and re-probe immediately before unlink. A changed record, reused
  // PID, EPERM, or any unknown result leaves the identity untouched.
  const current = await readPersistedGeneration(home, port);
  if (!sameRecord(current, expected) || pidState(expected.serverPid) !== 'dead') return false;
  await recordRetiredGeneration(home, port, expected);
  try {
    await unlink(generationFile(home, port));
    return true;
  } catch (err) {
    if (errCode(err) === 'ENOENT') return false;
    throw new Error(`cannot retire persisted tmux generation (${errDetail(err)})`, { cause: err });
  }
}

async function prepareServerGenerationUnlocked(home: string, port: Port): Promise<PrepareState> {
  let expected = await readPersistedGeneration(home, port);
  let server = await readServerGeneration(port);
  if (expected !== null) {
    if (
      server.reachable &&
      server.generation === expected.generation &&
      (expected.serverPid === null || server.serverPid === expected.serverPid)
    ) {
      // A matching legacy UUID is upgraded only using the PID read alongside
      // it from that same server. Legacy data still cannot recover a mismatch.
      if (expected.serverPid === null) {
        if (server.serverPid === null) return { enabled: true, expected, verified: false };
        expected = await replacePersistedGeneration(home, port, {
          generation: expected.generation,
          serverPid: server.serverPid,
        });
        // A failed re-read of the just-migrated record yields no expected
        // identity to verify against; report unverified rather than deref null.
        if (expected === null) return { enabled: true, expected: null, verified: false };
        server = await readServerGeneration(port);
      }
      return {
        enabled: true,
        expected,
        verified:
          server.reachable &&
          server.generation === expected.generation &&
          server.serverPid === expected.serverPid,
      };
    }
    if (expected.serverPid !== null && (await retireDeadGeneration(home, port, expected))) {
      // The recorded owner is definitively gone, and every pane it hosted went
      // with it — report THIS operation as the old generation's loss before
      // anything else. The liveness tick settles its rows off this signal
      // (fleetServerAbsent), so it must fire on the retiring call itself; if
      // this call instead fell through and claimed a reachable replacement
      // (which also deletes the death certificate), no later call could ever
      // prove that loss again, and the old fleet's rows would read 'live'
      // forever. The loss is ALSO authoritatively empty for readers — nothing
      // the dead server hosted can outlive it — while writers (ensureSession)
      // see the certificate below and recover on their re-invocation.
      return {
        enabled: true,
        expected: null,
        verified: false,
        oldGenerationLost: true,
        authoritativeEmpty: true,
      };
    }
    return { enabled: true, expected, verified: false };
  }
  if (!server.reachable) {
    // Nothing answering the socket. An empty fleet is a CLAIM ABOUT LIVE PANES,
    // so it needs proof, and a silent socket is not proof: an unlinked socket is
    // indistinguishable from no server while the original tmux keeps running
    // panes behind it. Only two facts together license "authoritatively empty":
    //
    //   1. a death certificate — we once claimed a server for this port and
    //      proved that exact PID gone by ESRCH, which is what proves its panes
    //      died with it. Absent a certificate (never claimed, or the home was
    //      wiped) we have no evidence at all and must stay UNKNOWN, because the
    //      live-server-behind-an-unlinked-socket case looks exactly like this.
    //   2. server.absent — tmux's own absence verdict rather than a probe that
    //      merely failed, so a timeout or a shadowed binary cannot impersonate
    //      an empty fleet and have boot reconciliation tombstone live cards.
    //
    // Recording the certificate is what makes this STABLE. The proof used to be
    // destroyed by the very call that obtained it (retire unlinked the claim),
    // so exactly one call could answer empty and every later one said UNKNOWN —
    // and UNKNOWN here cannot heal, because revive, adopt and /rc ask "is this
    // window free?" BEFORE ensureSession, the only code that ever creates a
    // server. Refusing them meant no server was created, so the next attempt
    // refused too: one dead tmux server wedged the board permanently, and the
    // liveness tick consumed the single recovery seconds after the crash.
    if (server.absent && (await hasRetiredGeneration(home, port))) {
      return { enabled: true, expected: null, verified: false, authoritativeEmpty: true };
    }
    return { enabled: true, expected: null, verified: false };
  }

  // A REACHABLE server with no claim on file. While a death certificate covers
  // this port, that server is an interloper until proven otherwise: the label
  // was just vacated by a server we proved dead, and anything already bound to
  // it (an orphaned replacement, a human's scratch server, another fleet that
  // collided with this label) is foreign state we must not claim, mutate, or
  // trust. Claiming it would also delete the certificate — the only remaining
  // proof that the old generation's panes are gone — so adoption here would
  // erase the fleet-death signal before settlement ever sees it. Decline: the
  // first claimant that MUST have a server (ensureSession) proves this one
  // foreign with the certificate, stops it, and creates its own.
  if (await hasRetiredGeneration(home, port)) {
    return {
      enabled: true,
      expected: null,
      verified: false,
      blockedByCertificate: true,
      authoritativeEmpty: true,
    };
  }

  // Upgrade/first-contact claim. Preserve a valid option set by an earlier
  // claimant; otherwise mint one, set it, and trust only the value read back.
  if (server.generation === null) {
    const candidate = randomUUID();
    const set = await tmuxResult(['set-option', '-g', generationOption(port), candidate], {
      noStart: true,
    });
    if (!set.ok) return { enabled: true, expected: null, verified: false };
    server = await readServerGeneration(port);
    if (!server.reachable || server.generation === null) {
      return { enabled: true, expected: null, verified: false };
    }
  }

  if (server.serverPid === null) return { enabled: true, expected: null, verified: false };
  expected = await persistGeneration(home, port, {
    generation: server.generation,
    serverPid: server.serverPid,
  });
  // A failed re-read of the just-published claim yields no expected identity;
  // report unverified rather than deref null on the return below.
  if (expected === null) return { enabled: true, expected: null, verified: false };
  // A live claim supersedes any death certificate: this port now has a server
  // again, and the old owner's proof must not outlive it and later license an
  // "empty" verdict about THIS server's panes.
  await clearRetiredGeneration(home, port);
  // Re-read after publishing: a socket replacement between set/read/persist is
  // caught here, and a concurrent file claimant's first-writer value wins.
  server = await readServerGeneration(port);
  return {
    enabled: true,
    expected,
    verified:
      server.reachable &&
      server.generation === expected.generation &&
      server.serverPid === expected.serverPid,
  };
}

async function prepareServerGeneration(port: Port): Promise<PrepareState> {
  const home = generationHome();
  if (home === null) return { enabled: false, expected: null, verified: true };
  const key = `${home}\x00${generationPort(port)}`;
  const prior = generationLocks.get(key) ?? Promise.resolve();
  const current = prior
    .catch(() => {
      /* sequence regardless of prior outcome */
    })
    .then(() => prepareServerGenerationUnlocked(home, port));
  generationLocks.set(key, current);
  try {
    return await current;
  } finally {
    if (generationLocks.get(key) === current) generationLocks.delete(key);
  }
}

// Run a read command and print the server UUID + PID in the same tmux client
// command queue. The socket cannot switch servers between header and listing.
async function generationVerifiedResult(port: Port, args: readonly string[]): Promise<GenResult> {
  let state: PrepareState;
  try {
    state = await prepareServerGeneration(port);
  } catch (err) {
    return { ok: false, generationError: errMessage(err) };
  }
  if (!state.enabled) return tmuxResult(args);
  if (state.authoritativeEmpty === true) return { ok: true, out: '', authoritativeEmpty: true };
  if (!state.verified || state.expected === null) {
    return { ok: false, generationError: 'tmux server generation unavailable or changed' };
  }
  const result = await tmuxResult(
    [
      'display-message',
      '-p',
      `${GENERATION_HEADER}#{${generationOption(port)}}${FIELD_SEP}#{pid}`,
      ';',
      ...args,
    ],
    { noStart: true },
  );
  if (!result.ok) return result;
  const newline = result.out.indexOf('\n');
  if (
    newline === -1 ||
    result.out.slice(0, newline) !==
      `${GENERATION_HEADER}${state.expected.generation}${FIELD_SEP}${String(state.expected.serverPid)}`
  ) {
    return { ok: false, generationError: 'tmux server generation unavailable or changed' };
  }
  return { ok: true, out: result.out.slice(newline + 1), generation: state.expected.generation };
}

/** Can we PROVE that no tmux server is hosting this fleet right now?
 *
 * An empty window listing has two very different causes: a healthy server whose
 * windows were killed, and a server that DIED taking every pane with it. Only
 * the second is a fleet-wide loss, and only it licenses settling live rows
 * without probing their panes — a pane cannot outlive the server that ran it.
 *
 * Two proofs qualify. One is the death certificate plus tmux's own absence
 * verdict, exactly as for an authoritatively-empty listing. The other is the
 * retirement itself: this call proved the claimed owner dead by ESRCH, which is
 * stronger than any listing, and it must stand on its own because whatever now
 * answers the socket — even an unrelated replacement server someone started on
 * the same label — cannot resurrect the old generation's panes. A reachable
 * replacement therefore does NOT turn this false on the retiring call; only a
 * certificate-less probe, a failed probe, or a live claim do, so callers never
 * act on a guess — the caller that condemns rows must never be the one to
 * invent the evidence. */
export async function fleetServerAbsent(port: Port): Promise<boolean> {
  try {
    const state = await prepareServerGeneration(port);
    return state.enabled && (state.authoritativeEmpty === true || state.oldGenerationLost === true);
  } catch {
    return false; // an unreadable identity proves nothing
  }
}

// ------------------------------------------------------------- capability
let probe: ProbeState = { available: false, reason: `tmux ${MIN_TMUX_VERSION}+ required`, at: 0 };
const PROBE_TTL_MS = 60_000;

/** tmux binary reachable? Cached (60 s TTL) — this runs inside /health and
 * /state snapshots, so it must not fork a subprocess on every heartbeat. */
export function hasTmux(): boolean {
  return tmuxCapability().available;
}

export function tmuxCapability(): TmuxCapability {
  const now = Date.now();
  if (now - probe.at < PROBE_TTL_MS) return { ...probe };
  let next: TmuxCapability = {
    available: false,
    reason: `tmux ${MIN_TMUX_VERSION}+ not found on PATH`,
  };
  try {
    const output = execFileSync('tmux', ['-V'], {
      timeout: 1_500,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      // Live env, not the startup snapshot (see exec.ts): under Bun a runtime
      // TMUX_TMPDIR/PATH mutation only reaches the child when env is explicit;
      // a no-op under Node.
      env: process.env,
    });
    next = tmuxVersionCapability(output);
  } catch {
    /* not installed / not executable */
  }
  probe = { ...next, at: now };
  return { ...probe };
}

/** FLEETDECK_SPAWN_CMD override, or null when unset/blank. */
export function spawnOverrideCmd(): string | null {
  const v = process.env['FLEETDECK_SPAWN_CMD'];
  return v?.trim() ? v : null;
}

// ------------------------------------------------------------ scoped names
export const sessionName = (port: Port): string => `fleetdeck-${port}`;
export const windowName = (port: Port, callsign: string): string => `fd${port}-${callsign}`;

/** Exact session + exact window target. A generation-mismatched replacement
 * never receives this fleet session from ensureSession, so a reusable @id on
 * that replacement cannot redirect pane operations after a verified lookup. */
export function exactWindowTarget(port: Port, window: Port): string {
  const normalizedPort = generationPort(port);
  const value = String(window);
  const prefix = `fd${normalizedPort}-`;
  if (!value.startsWith(prefix) || !/^[A-Za-z0-9-]+$/.test(value.slice(prefix.length))) {
    throw new Error('invalid scoped tmux window name');
  }
  return `=${sessionName(normalizedPort)}:=${value}`;
}

function exactTargetPort(target: string): string | null {
  const match = /^=fleetdeck-(\d+):=fd(\d+)-[A-Za-z0-9-]+$/.exec(target);
  return match?.[1] !== undefined && match[1] === match[2] ? match[1] : null;
}

// ----------------------------------------------------------------- session
/** `has-session` is a PREDICATE: the ok-ness of the result IS the answer. An
 * authoritatively-empty short circuit reports {ok:true, out:''} for every
 * command, which is correct for a LISTING (there is nothing to list) and a lie
 * for a predicate (there is no server, so the session cannot exist). Never let
 * absence manufacture a session that was never created. */
const sessionConfirmed = (result: GenResult): boolean => result.ok && !result.authoritativeEmpty;

/** Ensure the detached daemon-owned session `fleetdeck-<port>` exists.
 * `=` prefix = exact session-name match (verified; prefix matching could
 * otherwise confuse fleetdeck-4711 with fleetdeck-47110). */
export async function ensureSession(port: Port): Promise<string> {
  const name = sessionName(port);
  const state = await prepareServerGeneration(port);
  if (!state.enabled) {
    if ((await tmux(['has-session', '-t', '=' + name])) !== null) return name;
    if ((await tmux(['new-session', '-d', '-s', name])) !== null) return name;
    if ((await tmux(['has-session', '-t', '=' + name])) !== null) return name;
    throw new Error(`tmux could not create session ${name}`);
  }

  // With no expected generation and no reachable server, first-run creation is
  // allowed. Every path after this call either claims that server or fails.
  if (state.expected === null) {
    if (state.blockedByCertificate === true) {
      // A death certificate covers this label, so the server answering it is
      // FOREIGN by definition (a replacement that raced the old server's death,
      // a human's scratch server, another fleet). Its panes are not ours, but
      // neither are they recoverable by anyone through this fleet, and it is
      // squatter's-rights-blocking the only recovery path the fleet has: stop
      // the exact PID read from it — never a bare `kill-server`, which would
      // shoot whatever the label points at when it fires — then create below.
      // SIGTERM first, exactly as `kill-server` would ask, escalating to
      // SIGKILL only if it lingers; the SIGKILL fallback covers wedged foreign
      // servers, because a live one answering the label blocks recovery just
      // the same.
      const interloper = await readServerGeneration(port);
      if (interloper.reachable && interloper.serverPid !== null) {
        if (pidState(interloper.serverPid) === 'alive') {
          try {
            process.kill(interloper.serverPid, 'SIGTERM');
          } catch {
            /* already gone */
          }
          for (let i = 0; i < 50 && pidState(interloper.serverPid) === 'alive'; i += 1) {
            await new Promise<void>((resolve) => setTimeout(resolve, 20));
          }
          if (pidState(interloper.serverPid) === 'alive') {
            try {
              process.kill(interloper.serverPid, 'SIGKILL');
            } catch {
              /* exited between probe and signal */
            }
          }
        }
        // SIGKILL'd tmux leaves its socket file behind until the filesystem
        // catches up; wait for the label to drain so the create below cannot
        // land on the corpse of the server it just replaced.
        for (let i = 0; i < 50; i += 1) {
          if (!(await readServerGeneration(port)).reachable) break;
          await new Promise<void>((resolve) => setTimeout(resolve, 20));
        }
      }
    }
    const created = await tmuxResult(['new-session', '-d', '-s', name]);
    if (created.ok) {
      let claimed = await prepareServerGeneration(port);
      // The certificate was honored: it declined the interloper above and we
      // evicted it by PID. The create then made a server we MUST claim — but
      // the claim path declines any reachable server while a certificate
      // covers the label, and this fresh server looks exactly like a second
      // interloper. The certificate already spent its authority (the eviction
      // happened inside this call), so retire it and re-ask: the retry takes
      // the normal first-contact claim path for the server we just made.
      if (!claimed.verified && claimed.blockedByCertificate === true) {
        const certHome = generationHome();
        if (certHome !== null) await clearRetiredGeneration(certHome, port);
        claimed = await prepareServerGeneration(port);
      }
      if (claimed.verified) {
        const confirmed = await generationVerifiedResult(port, ['has-session', '-t', '=' + name]);
        if (sessionConfirmed(confirmed)) return name;
      }
      throw new Error(`tmux server generation could not be claimed for ${name}`);
    }
  } else if (!state.verified) {
    throw new Error(`tmux server generation unavailable or changed for ${name}`);
  }

  const existing = await generationVerifiedResult(port, ['has-session', '-t', '=' + name]);
  if (sessionConfirmed(existing)) return name;

  // -N is present in tmux 3.4+: if the verified server disappears, new-session
  // must fail rather than silently starting a replacement at the same label.
  // The server-side format guard also refuses to create the session if the
  // socket already points at a reachable server with a different generation.
  const refreshed = await prepareServerGeneration(port);
  if (!refreshed.verified || refreshed.expected === null) {
    throw new Error(`tmux server generation unavailable or changed for ${name}`);
  }
  const created = await tmuxResult(
    [
      'if-shell',
      '-F',
      `#{&&:#{==:#{${generationOption(port)}},${refreshed.expected.generation}},#{==:#{pid},${String(refreshed.expected.serverPid)}}}`,
      `new-session -d -s ${name}`,
      `display-message -p ${GENERATION_MISMATCH}`,
    ],
    { noStart: true },
  );
  if (created.ok && created.out === '') {
    const confirmed = await generationVerifiedResult(port, ['has-session', '-t', '=' + name]);
    if (sessionConfirmed(confirmed)) return name;
  }
  // Lost a same-generation session-creation race? Accept only a fresh verified
  // exact-session probe, never an uncorroborated new-session failure.
  const raced = await generationVerifiedResult(port, ['has-session', '-t', '=' + name]);
  if (sessionConfirmed(raced)) return name;
  throw new Error(`tmux could not create session ${name}`);
}

// ----------------------------------------------------------------- windows
/** Create a detached window named fd<port>-<callsign> in fleetdeck-<port>,
 * cwd set, running `argv` DIRECTLY (execvp, no shell — see header). Returns
 * {session, window, window_id} — window_id (@n) is the stable kill/inspect
 * target, immune to renames and index shuffles.
 *
 * `env` (0.15.0) is an optional {NAME: value} map delivered through tmux's
 * `new-window -e`, which sets the variable in the new window's environment
 * rather than in the command line it runs. That distinction is the entire point
 * and it is a SECURITY one: the only caller supplies an LLM-gateway credential,
 * and an `env NAME=secret claude …` argv would publish it in `ps` output for
 * the whole multi-hour life of the pane — readable by every other OS user on the
 * box, which SECURITY.md names as the honest caveat of the loopback trust zone.
 * Via `-e` the secret appears only in THIS tmux command's own argv, for the
 * milliseconds it takes to return, and thereafter lives in the tmux server's
 * per-window environment (reachable only by someone who can already reach the
 * socket — i.e. already this user).
 *
 * Values are passed as their own argv elements exactly like every other tmux
 * argument here, so no quoting or shell metacharacter can escape them. Verified
 * on tmux 3.7b; `-e` has been available on new-window since tmux 3.0. */
export async function newWindow({
  port,
  callsign,
  cwd,
  argv,
  env = null,
}: NewWindowSpec): Promise<{ session: string; window: string; window_id: string }> {
  const session = sessionName(port);
  const window = windowName(port, callsign);
  const generation = await prepareServerGeneration(port);
  if (generation.enabled && (!generation.verified || generation.expected === null)) {
    throw new Error(`tmux server generation unavailable or changed for ${window}`);
  }
  const envArgs = env
    ? Object.entries(env).flatMap(([name, value]) => ['-e', `${name}=${value}`])
    : [];
  // Do not generation-condition this through if-shell: cwd, env, and argv are
  // untrusted argv-safe values and must never be embedded in tmux parser text.
  // The exact fleet target is the guard: ensureSession never creates/accepts it
  // on a generation-mismatched replacement, so new-window fails there.
  const target = exactWindowTarget(port, window);
  // Arm remain-on-exit BEFORE the command starts. The per-window set below is
  // too late for a setup command that exits immediately (`exit 7`): tmux can
  // delete the window between new-window returning and set-option, losing the
  // error screen the setup contract promises to preserve. A SESSION-SCOPED
  // after-new-window hook closes that race — tmux runs it synchronously in the
  // new window's context — and, unlike `set-option -w -g` (which despite a -t
  // target writes the SERVER-GLOBAL window default and would leak
  // remain-on-exit onto every window of a user's shared default socket,
  // verified on tmux 3.7b), a hook set on '=session:' provably applies to this
  // fleet session alone. Idempotent; best-effort like the per-window write.
  await tmux([
    'set-hook',
    '-t',
    '=' + session + ':',
    'after-new-window',
    'set-option -w remain-on-exit on',
  ]);
  // Launch under an adapter-owned UNIQUE temporary name, never the final
  // scoped name: tmux happily permits duplicate window names, and starting the
  // (potentially billed) agent before learning the name is occupied used to
  // leave that agent running after the exact-name postcondition failed — with
  // name-based compensation refusing the now-ambiguous duplicate set, so
  // nothing owned, killed, or cleaned it up. The temp name contains the final
  // name but no '-' after the fd<port>- prefix, so scoped listings, probes,
  // and kills (fd<port>-*) never match it. The returned @id is the ONLY handle
  // used from here on; rollback kills by that id, which duplicate names
  // cannot make ambiguous.
  const provisional = `${window}~${randomUUID()}`;
  const out = await tmux([
    'new-window',
    '-d',
    '-P',
    '-F',
    '#{window_id}',
    '-t',
    '=' + session + ':', // exact session, next free window index
    '-n',
    provisional,
    '-c',
    cwd,
    ...envArgs,
    '--',
    ...argv,
  ]);
  if (out === null) throw new Error(`tmux new-window failed for ${window}`);
  const window_id = out.trim();
  // Best-effort and deliberately awaited: every failure past this point kills
  // the window we just launched, by its id, so a failed spawn never leaves a
  // running agent behind.
  const rollback = (): Promise<string | null> => tmux(['kill-window', '-t', window_id]);
  const inspect = (args: readonly string[]): Promise<GenResult> =>
    generation.enabled ? generationVerifiedResult(port, args) : tmuxResult(args);
  // Verify the final name is FREE before taking it — after the launch, so a
  // same-generation race in which two spawns pass an earlier check together
  // still cannot leave two same-name windows: the loser's rename fails. The
  // probe CANNOT target the final name: tmux prefix-matches window targets
  // even with the '=' exact prefix (verified on tmux 3.4), which would match
  // the provisional window itself and report the name as occupied. Listing
  // the exact fleet session's windows and comparing names verbatim is exact.
  const occupancy = await inspect([
    'list-windows',
    '-t',
    '=' + session + ':',
    '-F',
    '#{window_name}',
  ]);
  if (!occupancy.ok) {
    await rollback();
    throw new Error(
      generation.enabled
        ? `tmux new-window generation postcondition failed for ${window}`
        : `tmux new-window occupancy check failed for ${window}`,
    );
  }
  const names = occupancy.out.endsWith('\n') ? occupancy.out.slice(0, -1) : occupancy.out;
  if (names.split('\n').some((name) => name === window)) {
    await rollback();
    throw new Error(`tmux new-window refused: ${window} already exists`);
  }
  // Rename by id. tmux rejects the rename when the final name is already
  // taken (the launch-time race above), which fails closed with rollback.
  const renamed = await tmuxResult(['rename-window', '-t', window_id, window]);
  if (!renamed.ok) {
    await rollback();
    throw new Error(`tmux new-window could not claim the scoped name ${window}`);
  }
  if (generation.enabled) {
    const confirmed = await generationVerifiedResult(port, [
      'display-message',
      '-p',
      '-t',
      target,
      ['#{session_name}', '#{window_name}', '#{window_id}'].join(FIELD_SEP),
    ]);
    const expected = [session, window, window_id].join(FIELD_SEP) + '\n';
    if (!confirmed.ok || confirmed.out !== expected) {
      await rollback();
      throw new Error(`tmux new-window generation postcondition failed for ${window}`);
    }
  }
  // Best-effort: keep the pane (scrollback + deterministic pane_dead crash
  // signal) when the command exits. A failure here degrades gracefully — the
  // window just closes on exit and boot reconciliation marks the row 'gone'.
  await tmux(['set-option', '-w', '-t', target, 'remain-on-exit', 'on']);
  return { session, window, window_id };
}

/** {dead, cmd} for a pane/window target (@id or scoped name), or null when
 * the target is gone / tmux unreachable (= UNKNOWN, never confidently dead). */
export async function paneCurrentCommand(
  target: string,
): Promise<{ dead: boolean; cmd: string } | null> {
  const args = [
    'display-message',
    '-p',
    '-t',
    target,
    `#{pane_dead}${FIELD_SEP}#{pane_current_command}`,
  ];
  const port = exactTargetPort(target);
  const result =
    port === null ? await tmuxResult(args) : await generationVerifiedResult(port, args);
  if (!result.ok) return null;
  const out = result.out;
  const [dead, ...cmd] = out.replace(/\n$/, '').split(FIELD_SEP);
  if (dead !== '0' && dead !== '1') return null;
  return { dead: dead === '1', cmd: cmd.join(FIELD_SEP) };
}

/** All windows on the server whose name matches this fleet's scope
 * (`fd<port>-*`), with the first (lowest-index) pane speaking for each
 * window: [{session, window, window_id, pane_dead, pane_cmd}]. Returns [] only
 * after a successful, fully validated empty listing; null means UNKNOWN because
 * tmux failed or a successful response contained a malformed row. */
export async function listScopedWindows(port: Port): Promise<ScopedWindow[] | null> {
  const expectedSession = sessionName(port);
  const listed = await generationVerifiedResult(port, [
    'list-panes',
    '-a',
    '-f',
    `#{==:#{session_name},${expectedSession}}`,
    '-F',
    [
      '#{session_name}',
      '#{window_name}',
      '#{window_id}',
      '#{pane_dead}',
      '#{pane_current_command}',
    ].join(FIELD_SEP),
  ]);
  if (!listed.ok) return null;
  if (listed.out === '') return [];
  const output = listed.out.endsWith('\n') ? listed.out.slice(0, -1) : listed.out;
  if (output === '') return null;
  const prefix = `fd${port}-`;
  const seen = new Set<string>();
  const seenNames = new Set<string>();
  const wins: ScopedWindow[] = [];
  for (const line of output.split('\n')) {
    const [session, window, window_id, dead, ...cmd] = line.split(FIELD_SEP);
    if (
      !session ||
      !window ||
      window_id === undefined ||
      !/^@\d+$/.test(window_id) ||
      (dead !== '0' && dead !== '1') ||
      cmd.length === 0
    )
      return null;
    if (session !== expectedSession || !window.startsWith(prefix)) continue;
    if (seen.has(window_id)) continue; // human split the pane: original pane wins
    if (seenNames.has(window)) return null; // duplicate scoped names are ambiguous ownership
    seen.add(window_id);
    seenNames.add(window);
    wins.push({
      session,
      window,
      window_id,
      pane_dead: dead === '1',
      pane_cmd: cmd.join(FIELD_SEP),
    });
  }
  return wins;
}

/** Name-verified kill (CONTRACT): re-locate the window by its EXACT scoped
 * name at kill time and kill BY THAT EXACT NAME — tmux re-resolves the
 * `=<session>:=<name>` target atomically at the moment of the kill, so a
 * renamed/recycled window can never be mis-killed via a stale index or a
 * reused @id.
 *
 * Optional `opts` (BUG-046 — a scoped window name is REUSABLE, so the tmux-side
 * checks alone cannot see a same-name replacement a concurrent revive stood up
 * while a dismiss/cleanup kill was awaiting):
 *   opts.expectWindowId — the window_id the caller's listing observed for this
 *     name. When the kill-time re-resolve finds a DIFFERENT id, the name has
 *     been recycled onto a new window (a replacement pane): stale no-op.
 *   opts.expect — a synchronous predicate re-run immediately before the kill,
 *     AFTER the final name re-resolve, with no intervening await. It is the
 *     caller's last word on DB ownership/generation (spawns.mjs owns no DB).
 *     Any falsy return is a stale no-op: {ok:false, stale:true}. The kill
 *     command itself is the only await after the last expect pass, and the
 *     existing generation guard already pins that await to the same server, so
 *     a failing predicate can never be followed by a kill of what it rejected.
 * Returns:
 *   {ok:true, window_id}   killed
 *   {ok:false, gone:true}  no window with that exact name exists (410)
 *   {ok:false, stale:true} an opts expectation failed — the kill was refused
 *   {ok:false, error}      tmux kill-window itself failed */
export async function killWindowVerified(
  name: string,
  opts?: { expectWindowId?: string; expect?: () => boolean },
): Promise<KillResult> {
  const scope = /^fd(\d+)-[^\x00-\x1f\x7f]+$/.exec(name); // eslint-disable-line no-control-regex -- scoped names must exclude C0/DEL; this class is the gate
  if (!scope) return { ok: false, error: 'invalid scoped tmux window name' };
  // Names with tmux target syntax characters are rejected here: the kill below
  // targets the exact name `=<session>:=<name>` resolved atomically server-side,
  // and tmux cannot express arbitrary names containing those characters in that
  // syntax. Daemon-minted callsigns never contain them.
  if (/[=;:.]/.test(name)) return { ok: false, error: 'invalid scoped tmux window name' };
  const scopePort = scope[1];
  if (scopePort === undefined) return { ok: false, error: 'invalid scoped tmux window name' };
  const expectedSession = sessionName(scopePort);
  const format = ['#{session_name}', '#{window_name}', '#{window_id}'].join(FIELD_SEP);
  const listArgs = [
    'list-panes',
    '-a',
    '-f',
    `#{==:#{session_name},${expectedSession}}`,
    '-F',
    format,
  ];
  const parse = (output: string): [string, string, string][] | null => {
    if (output === '') return [];
    const body = output.endsWith('\n') ? output.slice(0, -1) : output;
    if (body === '') return null;
    const rows = body.split('\n').map((line) => line.split(FIELD_SEP));
    if (
      rows.some(
        (fields) =>
          fields.length !== 3 || !fields[0] || !fields[1] || !/^@\d+$/.test(fields[2] ?? ''),
      )
    )
      return null;
    return rows as [string, string, string][];
  };
  const exactMatches = (rows: readonly [string, string, string][]): [string, string, string][] => {
    const byWindowId = new Map<string, [string, string, string]>();
    for (const fields of rows) {
      if (fields[0] === expectedSession && fields[1] === name) byWindowId.set(fields[2], fields);
    }
    return [...byWindowId.values()];
  };
  const listed = await generationVerifiedResult(scopePort, listArgs);
  if (!listed.ok)
    return { ok: false, error: listed.generationError ?? 'tmux window lookup failed' };
  const rows = parse(listed.out);
  if (rows === null) return { ok: false, error: 'malformed tmux window listing' };
  const matches = exactMatches(rows);
  if (matches.length > 1) return { ok: false, error: 'ambiguous scoped tmux window name' };
  const hit = matches[0];
  if (hit === undefined) return { ok: false, gone: true };
  if (opts?.expectWindowId !== undefined && hit[2] !== opts.expectWindowId) {
    return { ok: false, stale: true, error: 'window id changed — the scoped name was recycled' };
  }
  if (opts?.expect && !opts.expect())
    return { ok: false, stale: true, error: 'stale window owner' };
  // Kill by the exact fleet name, never by the looked-up @id: tmux resolves
  // `=<session>:=<name>` ATOMICALLY inside the same server command queue the
  // kill runs in, so a rename/recycle between the lookup above and the kill
  // cannot redirect it onto a repurposed window — the target is still
  // corroborated against the verified session and name at the instant it acts.
  // An @id captured earlier would not be: window ids are reusable, and only
  // generation+PID at that queue would still match.
  const killTarget = `=${expectedSession}:=${name}`;
  let killGeneration: PrepareState;
  try {
    killGeneration = await prepareServerGeneration(scopePort);
  } catch (err) {
    return { ok: false, error: `tmux server generation verification failed: ${errMessage(err)}` };
  }
  // prepareServerGeneration awaited — re-run the caller's expectation now, so
  // only the kill command itself can interleave after the final verdict (and
  // the generation guard pins that last await to the same server).
  if (opts?.expect && !opts.expect())
    return { ok: false, stale: true, error: 'stale window owner' };
  let killed: TmuxResult;
  if (!killGeneration.enabled) {
    killed = await tmuxResult(['kill-window', '-t', killTarget]);
  } else if (!killGeneration.verified || killGeneration.expected === null) {
    return { ok: false, error: 'tmux server generation unavailable or changed' };
  } else {
    // The conditional and kill execute in one server command queue. A socket
    // swap after lookup cannot redirect the kill at a replacement server: its
    // absent or different generation/PID selects the harmless marker branch
    // instead.
    killed = await tmuxResult(
      [
        'if-shell',
        '-F',
        `#{&&:#{==:#{${generationOption(scopePort)}},${killGeneration.expected.generation}},#{==:#{pid},${String(killGeneration.expected.serverPid)}}}`,
        `kill-window -t ${killTarget}`,
        `display-message -p ${GENERATION_MISMATCH}`,
      ],
      { noStart: true },
    );
    if (killed.ok && killed.out.trim() === GENERATION_MISMATCH) {
      return { ok: false, error: 'tmux server generation unavailable or changed' };
    }
  }
  if (killed.ok) return { ok: true, window_id: hit[2] };
  // kill failed — vanished between list and kill, or a real tmux error?
  const rechecked = await generationVerifiedResult(scopePort, listArgs);
  if (!rechecked.ok)
    return {
      ok: false,
      error: rechecked.generationError ?? 'tmux window recheck failed after kill error',
    };
  const again = parse(rechecked.out);
  if (again === null) return { ok: false, error: 'malformed tmux window recheck after kill error' };
  const remaining = exactMatches(again);
  if (remaining.length > 1)
    return { ok: false, error: 'ambiguous scoped tmux window name after kill error' };
  if (remaining.length === 0) return { ok: false, gone: true };
  return { ok: false, error: 'tmux kill-window failed' };
}

/** Neutralize the BRACKETED-PASTE BREAKOUT before any owned-pane paste
 * (CONTRACT). pasteText delivers with `-p`, which wraps the buffer in tmux's
 * bracketed-paste markers ESC[200~ … ESC[201~. Mail delivery (mail.ts) pipes
 * VERBATIM message content through pasteText, so content carrying a literal END
 * marker `\x1b[201~` would close the bracket EARLY inside the receiving Claude
 * TUI — everything after it is then processed as LIVE keystrokes, which the
 * daemon's own sendEnter promptly submits. That is keystroke/command injection
 * into a daemon-owned pane. This is the one chokepoint every pane paste flows
 * through, so sanitizing here protects every caller.
 *
 * Pure and conservative: normalize CRLF / lone CR to LF, delete the
 * bracketed-paste START/END markers (looped so a crafted overlap cannot
 * reconstitute one after a single pass), then strip every remaining C0 control
 * byte — bare ESC `\x1b` included, since it could open a fresh control sequence —
 * plus DEL and the C1 controls (0x80–0x9f, e.g. the 8-bit CSI U+009B) — EXCEPT
 * `\t` (0x09) and `\n` (0x0A), both legitimate in pasted text. Code points above
 * U+009F are never touched, so normal UTF-8 (accented Latin-1 at U+00A0–U+00FF,
 * CJK, emoji) is intact. The control strip is the load-bearing guarantee: with no
 * ESC or C1 CSI left, no functional paste marker can survive whatever the input
 * tried to smuggle in. */
export function sanitizePaneText(text: unknown): string {
  // CONTRACT: coerce rather than throw — callers pass mail bodies whose type is
  // not statically guaranteed (see the "non-string input is coerced" test).
  let out = String(text).replace(/\r\n?/g, '\n');
  let prev: string;
  do {
    prev = out;
    out = out.replace(/\x1b\[20[01]~/g, ''); // eslint-disable-line no-control-regex -- deletes bracketed-paste markers; stripping ESC is the point
  } while (out !== prev);
  // Strip C0 controls (keep \t and \n), DEL, AND the C1 range 0x80-0x9f. The
  // C1 strip closes the 8-bit-CSI form: a lone U+009B is the single-byte CSI a
  // terminal could read as the start of a `\x1b[201~` bracketed-paste terminator,
  // so removing it denies that alternative escape. C1 code points are control
  // codes, never legitimate text; everything above U+009F (accented Latin-1,
  // CJK, emoji) is untouched.
  return out.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, ''); // eslint-disable-line no-control-regex -- the control strip is the load-bearing sanitizer
}

/** The four sanctioned owned-pane injections (CONTRACT) are: one bring-up
 * Enter for the trust dialog, bracketed-paste mail followed by Enter, and
 * verbatim human typing relayed by the live-terminal modal, plus a human's
 * explicit board action enabling remote control via a literally typed /rc
 * command. All user text still travels without a shell; terminal input uses
 * control-mode hex bytes. */
export async function pasteText(target: string, text: string): Promise<boolean> {
  // Bracketed-paste breakout defense: sanitize BEFORE the buffer is set, so the
  // `-p` paste below can never carry an END marker that turns mail content into
  // live keystrokes (see sanitizePaneText).
  const safe = sanitizePaneText(text);
  // tmux buffers are server-global, so a constant name lets concurrent mail
  // deliveries overwrite each other between set-buffer and paste-buffer. A
  // UUID makes the two-command handoff private to this call; `-d` removes the
  // buffer on success, while finally covers a failed/timed-out paste.
  const buffer = `fdmail-${randomUUID()}`;
  if ((await tmux(['set-buffer', '-b', buffer, '--', safe])) === null) return false;
  try {
    return (await tmux(['paste-buffer', '-p', '-d', '-b', buffer, '-t', target])) !== null;
  } finally {
    // Best-effort and deliberately awaited: do not leave mail text resident
    // in tmux when paste-buffer fails before its `-d` cleanup can take effect.
    await tmux(['delete-buffer', '-b', buffer]);
  }
}

export async function sendEnter(target: string): Promise<boolean> {
  const port = exactTargetPort(target);
  if (port === null) return (await tmux(['send-keys', '-t', target, 'Enter'])) !== null;
  let state: PrepareState;
  try {
    state = await prepareServerGeneration(port);
  } catch {
    return false;
  }
  if (!state.enabled) return (await tmux(['send-keys', '-t', target, 'Enter'])) !== null;
  if (!state.verified || state.expected === null) return false;
  // target is produced by exactWindowTarget (restricted alnum/dash grammar),
  // and Enter is static, so this tmux parser string contains no untrusted data.
  const result = await tmuxResult(
    [
      'if-shell',
      '-F',
      `#{&&:#{==:#{${generationOption(port)}},${state.expected.generation}},#{==:#{pid},${String(state.expected.serverPid)}}}`,
      `send-keys -t ${target} Enter`,
      `display-message -p ${GENERATION_MISMATCH}`,
    ],
    { noStart: true },
  );
  return result.ok && result.out.trim() !== GENERATION_MISMATCH;
}

/** Literal keystrokes for TUI commands. `-l --` prevents tmux key-name
 * parsing; unlike bracketed paste this reaches Claude as typed slash input. */
export async function typeKeys(target: string, text: string): Promise<boolean> {
  return (await tmux(['send-keys', '-t', target, '-l', '--', text])) !== null;
}

/** Literal text + Enter as ONE tmux invocation: `;` separates two send-keys
 * commands inside a single tmux command queue, which the server executes
 * back-to-back against the pane — so no other client (human keystrokes, mail
 * delivery, a second daemon action) can interleave input between the text and
 * its submission, and a caller that validated turn-state immediately
 * beforehand never strands typed-but-unsent text in an active TUI the way a
 * separate typeKeys → recheck → sendEnter sequence could (BUG-053). NOTE: `-l`
 * applies to EVERY following key argument, so a trailing bare `Enter` in the
 * same send-keys would be typed as the literal string "Enter" — the Enter must
 * be its own non-`-l` send-keys command in the queue. */
export async function typeAndEnter(target: string, text: string): Promise<boolean> {
  return (
    (await tmux([
      'send-keys',
      '-t',
      target,
      '-l',
      '--',
      text,
      ';',
      'send-keys',
      '-t',
      target,
      'Enter',
    ])) !== null
  );
}

/** Independent pane-scrollback capture for remote-control URL harvesting.
 * Keep this adapter local rather than coupling daemon state to termbridge. */
export async function capturePane(target: string): Promise<string | null> {
  const args = ['capture-pane', '-p', '-t', target];
  const port = exactTargetPort(target);
  if (port === null) return tmux(args);
  const result = await generationVerifiedResult(port, args);
  return result.ok ? result.out : null;
}

/** Bring-up compatibility export; caller enforces at-most-once per spawn. */
export function sendBringupEnter(target: string): Promise<boolean> {
  return sendEnter(target);
}

// ------------------------------------------------------------ test override
/** Launch the FLEETDECK_SPAWN_CMD fixture: argv [cmd, JSON.stringify(spec)],
 * detached, output ignored. onError fires if the process can't start at all
 * (bad path) — asynchronous by nature, so the spawn row simply stays
 * 'spawning' and the caller's note explains why. */
export function launchOverride(
  cmd: string,
  spec: unknown,
  onError: (err: unknown) => void = () => {
    /* default: no error reporting */
  },
): void {
  try {
    // env: process.env — live env, not Bun's startup snapshot (see exec.ts); no-op under Node.
    const child = spawnChild(cmd, [JSON.stringify(spec)], {
      stdio: 'ignore',
      detached: true,
      env: process.env,
    });
    child.on('error', (err) => {
      try {
        onError(err);
      } catch {
        /* reporting only */
      }
    });
    child.unref();
  } catch (err) {
    try {
      onError(err);
    } catch {
      /* reporting only */
    }
  }
}
