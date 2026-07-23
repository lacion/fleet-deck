// spawn.mjs — the v1.2 tmux adapter (v1.2 — dynamic fleet). One backend,
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
import { execFileP } from './exec.mjs';
import { randomUUID } from 'node:crypto';
import { constants as fsConstants } from 'node:fs';
import { link, open, rename, unlink } from 'node:fs/promises';
import path from 'node:path';
import { MIN_TMUX_VERSION, tmuxVersionCapability } from '../../bin/tmux-version.mjs';

const TMUX_TIMEOUT_MS = 5_000;
// tmux's formatted-output printer escapes a literal unit separator as "\\037".
// TAB survives as a delimiter on supported tmux versions; the strict field,
// session, and id validation below rejects malformed or shifted records.
const FIELD_SEP = '\t';

// Run one tmux command (argv), retaining failure details for probes whose
// callers must distinguish authoritative absence from UNKNOWN.
async function tmuxResult(args, { noStart = false } = {}) {
  try {
    const socket = process.env.FLEETDECK_TMUX_SOCKET?.trim();
    const argv = [
      ...(socket ? ['-L', socket] : []),
      ...(noStart ? ['-N'] : []),
      ...args,
    ];
    const r = await execFileP('tmux', argv, { timeout: TMUX_TIMEOUT_MS });
    return r.ok
      ? { ok: true, out: r.out ?? '' }
      : { ok: false, code: r.code, error: r.err ?? '' };
  } catch (err) {
    return { ok: false, error: String(err?.message || err || '') };
  }
}

// Most probes intentionally keep the historical null-or-stdout contract.
async function tmux(args) {
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
const generationLocks = new Map();

function generationPort(port) {
  const value = String(port);
  if (!/^\d+$/.test(value)) throw new Error('invalid fleet port for tmux generation identity');
  return value;
}

const generationOption = port => `@fleetdeck_generation_${generationPort(port)}`;
const generationFile = (home, port) => path.join(home, `tmux-generation-${generationPort(port)}`);
// The DEATH CERTIFICATE for a retired claim, kept beside it. Proving an owner
// dead by ESRCH is the only evidence that its panes died with it, and unlinking
// the claim used to destroy that evidence at the instant it was obtained: the
// retiring call could answer "authoritatively empty" and every call after it was
// back to UNKNOWN. Recording the proof instead makes absence a STABLE answer, so
// revive/adopt/rc keep working rather than wedging until a human starts a tmux
// server by hand. Kept in its own file so claiming a replacement server still
// uses the first-writer-wins link() publish on the claim path, untouched.
const retiredGenerationFile = (home, port) => `${generationFile(home, port)}.retired`;

function generationHome() {
  const home = process.env.FLEETDECK_HOME?.trim();
  return home || null;
}

async function readPersistedGeneration(home, port) {
  const file = generationFile(home, port);
  let handle;
  try {
    // O_NOFOLLOW prevents a substituted symlink from redirecting either the
    // confidentiality chmod or the identity read outside FLEETDECK_HOME.
    handle = await open(file, fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0));
  } catch (err) {
    if (err?.code === 'ENOENT') return null;
    throw new Error(`cannot read persisted tmux generation (${err?.code || err?.message || err})`);
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
    let record;
    try { record = JSON.parse(value); } catch { /* strict error below */ }
    const keys = record && typeof record === 'object' && !Array.isArray(record)
      ? Object.keys(record).sort()
      : [];
    if (keys.length !== 2 || keys[0] !== 'generation' || keys[1] !== 'serverPid'
      || !GENERATION_UUID_RE.test(record.generation)
      || !Number.isSafeInteger(record.serverPid) || record.serverPid <= 1) {
      throw new Error('persisted tmux generation is malformed');
    }
    return { generation: record.generation.toLowerCase(), serverPid: record.serverPid, legacy: false };
  } finally {
    await handle.close();
  }
}

async function persistGeneration(home, port, record) {
  const file = generationFile(home, port);
  const temp = path.join(home, `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let handle;
  try {
    handle = await open(temp, 'wx', 0o600);
    await handle.writeFile(`${JSON.stringify({ generation: record.generation, serverPid: record.serverPid })}\n`, 'utf8');
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = null;
    try {
      // link is an atomic no-replace publish on the same filesystem. A second
      // claimant can never overwrite the first durable expected generation.
      await link(temp, file);
    } catch (err) {
      if (err?.code !== 'EEXIST') throw err;
    }
  } catch (err) {
    throw new Error(`cannot persist tmux generation (${err?.code || err?.message || err})`);
  } finally {
    try { await handle?.close(); } catch { /* primary error wins */ }
    try { await unlink(temp); } catch (err) { if (err?.code !== 'ENOENT') { /* best-effort temp cleanup */ } }
  }
  return readPersistedGeneration(home, port);
}

async function replacePersistedGeneration(home, port, record) {
  const file = generationFile(home, port);
  const temp = path.join(home, `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let handle;
  try {
    handle = await open(temp, 'wx', 0o600);
    await handle.writeFile(`${JSON.stringify({ generation: record.generation, serverPid: record.serverPid })}\n`, 'utf8');
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temp, file); // atomic old-record -> strict-record migration
  } catch (err) {
    throw new Error(`cannot replace persisted tmux generation (${err?.code || err?.message || err})`);
  } finally {
    try { await handle?.close(); } catch { /* primary error wins */ }
    try { await unlink(temp); } catch (err) { if (err?.code !== 'ENOENT') { /* best-effort temp cleanup */ } }
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
const SERVER_ABSENT_RE = /(?:^|\n)(?:no server running on |error connecting to .*\(No such file or directory\))/i;

async function readServerGeneration(port) {
  // Read both fields in one command from one reachable server. Separate tmux
  // clients could straddle a socket replacement and manufacture an identity no
  // server ever held.
  const result = await tmuxResult([
    'display-message', '-p', `#{${generationOption(port)}}${FIELD_SEP}#{pid}`,
  ], { noStart: true });
  if (!result.ok) {
    return {
      reachable: false,
      absent: SERVER_ABSENT_RE.test(String(result.error ?? '')),
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
    generation: GENERATION_UUID_RE.test(generation) ? generation.toLowerCase() : null,
    serverPid: extra.length === 0 && Number.isSafeInteger(serverPid) && serverPid > 1 ? serverPid : null,
  };
}

function pidState(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 1) return 'unknown';
  try {
    process.kill(pid, 0);
    return 'alive'; // includes PID reuse: a live unrelated process blocks reset
  } catch (err) {
    if (err?.code === 'ESRCH') return 'dead';
    return 'unknown'; // EPERM and every platform ambiguity fail closed
  }
}

const sameRecord = (left, right) => !!left && !!right
  && left.generation === right.generation && left.serverPid === right.serverPid;

/** Record the death certificate, then drop the claim. Written FIRST and by
 * atomic rename: a crash between the two leaves both files, and the claim wins
 * on the next read (a live claim is always more specific than a certificate for
 * an older one), so the only cost is a stale certificate that the next
 * successful claim clears. */
async function recordRetiredGeneration(home, port, expected) {
  const file = retiredGenerationFile(home, port);
  const temp = path.join(home, `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`);
  let handle;
  try {
    handle = await open(temp, 'wx', 0o600);
    await handle.writeFile(`${JSON.stringify({
      retiredGeneration: expected.generation, retiredServerPid: expected.serverPid,
    })}\n`, 'utf8');
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temp, file);
  } catch (err) {
    throw new Error(`cannot record retired tmux generation (${err?.code || err?.message || err})`);
  } finally {
    try { await handle?.close(); } catch { /* primary error wins */ }
    try { await unlink(temp); } catch (err) { if (err?.code !== 'ENOENT') { /* best-effort temp cleanup */ } }
  }
}

/** Is a proven-dead owner on record for this port? O_NOFOLLOW for the same
 * reason the claim read uses it. Anything unreadable proves nothing and is
 * reported as absent-of-proof, which fails closed at the caller. */
async function hasRetiredGeneration(home, port) {
  let handle;
  try {
    handle = await open(retiredGenerationFile(home, port), fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0));
  } catch {
    return false;
  }
  try {
    return (await handle.stat()).isFile();
  } catch {
    return false;
  } finally {
    try { await handle.close(); } catch { /* proof-read only */ }
  }
}

async function clearRetiredGeneration(home, port) {
  try { await unlink(retiredGenerationFile(home, port)); } catch (err) {
    if (err?.code !== 'ENOENT') { /* best-effort: a stale certificate only matters while unclaimed */ }
  }
}

async function retireDeadGeneration(home, port, expected) {
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
    if (err?.code === 'ENOENT') return false;
    throw new Error(`cannot retire persisted tmux generation (${err?.code || err?.message || err})`);
  }
}

async function prepareServerGenerationUnlocked(home, port) {
  let expected = await readPersistedGeneration(home, port);
  let server = await readServerGeneration(port);
  if (expected !== null) {
    if (server.reachable && server.generation === expected.generation
      && (expected.serverPid === null || server.serverPid === expected.serverPid)) {
      // A matching legacy UUID is upgraded only using the PID read alongside
      // it from that same server. Legacy data still cannot recover a mismatch.
      if (expected.serverPid === null) {
        if (server.serverPid === null) return { enabled: true, expected, verified: false };
        expected = await replacePersistedGeneration(home, port, {
          generation: server.generation,
          serverPid: server.serverPid,
        });
        server = await readServerGeneration(port);
      }
      return {
        enabled: true,
        expected,
        verified: server.reachable && server.generation === expected.generation
          && server.serverPid === expected.serverPid,
      };
    }
    if (expected.serverPid !== null && await retireDeadGeneration(home, port, expected)) {
      // The recorded owner is definitively gone. Resume first-contact: claim a
      // reachable replacement, or fall through to the no-claim case below and
      // let ensureSession create a fresh server.
      expected = null;
      server = await readServerGeneration(port);
    } else {
      return { enabled: true, expected, verified: false };
    }
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
    if (server.absent && await hasRetiredGeneration(home, port)) {
      return { enabled: true, expected: null, verified: false, authoritativeEmpty: true };
    }
    return { enabled: true, expected: null, verified: false };
  }

  // Upgrade/first-contact claim. Preserve a valid option set by an earlier
  // claimant; otherwise mint one, set it, and trust only the value read back.
  if (server.generation === null) {
    const candidate = randomUUID();
    const set = await tmuxResult(['set-option', '-g', generationOption(port), candidate], { noStart: true });
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
    verified: server.reachable && server.generation === expected.generation
      && server.serverPid === expected.serverPid,
  };
}

async function prepareServerGeneration(port) {
  const home = generationHome();
  if (home === null) return { enabled: false, expected: null, verified: true };
  const key = `${home}\u0000${generationPort(port)}`;
  const prior = generationLocks.get(key) ?? Promise.resolve();
  const current = prior.catch(() => {}).then(() => prepareServerGenerationUnlocked(home, port));
  generationLocks.set(key, current);
  try {
    return await current;
  } finally {
    if (generationLocks.get(key) === current) generationLocks.delete(key);
  }
}

// Run a read command and print the server UUID + PID in the same tmux client
// command queue. The socket cannot switch servers between header and listing.
async function generationVerifiedResult(port, args) {
  let state;
  try { state = await prepareServerGeneration(port); }
  catch (err) { return { ok: false, generationError: String(err?.message || err) }; }
  if (!state.enabled) return tmuxResult(args);
  if (state.authoritativeEmpty) return { ok: true, out: '', authoritativeEmpty: true };
  if (!state.verified || state.expected === null) {
    return { ok: false, generationError: 'tmux server generation unavailable or changed' };
  }
  const result = await tmuxResult([
    'display-message', '-p', `${GENERATION_HEADER}#{${generationOption(port)}}${FIELD_SEP}#{pid}`,
    ';', ...args,
  ], { noStart: true });
  if (!result.ok) return result;
  const newline = result.out.indexOf('\n');
  if (newline === -1 || result.out.slice(0, newline)
    !== `${GENERATION_HEADER}${state.expected.generation}${FIELD_SEP}${state.expected.serverPid}`) {
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
 * True requires the same proof an authoritatively-empty listing does: a death
 * certificate for the server we claimed (its PID proven gone by ESRCH) AND
 * tmux's own absence verdict. A reachable server, a failed probe, or no
 * certificate all return false, so callers never act on a guess — the caller
 * that condemns rows must never be the one to invent the evidence. */
export async function fleetServerAbsent(port) {
  try {
    const state = await prepareServerGeneration(port);
    return state.enabled === true && state.authoritativeEmpty === true;
  } catch {
    return false; // an unreadable identity proves nothing
  }
}

// ------------------------------------------------------------- capability
let probe = { available: false, reason: `tmux ${MIN_TMUX_VERSION}+ required`, at: 0 };
const PROBE_TTL_MS = 60_000;

/** tmux binary reachable? Cached (60 s TTL) — this runs inside /health and
 * /state snapshots, so it must not fork a subprocess on every heartbeat. */
export function hasTmux() {
  return tmuxCapability().available;
}

export function tmuxCapability() {
  const now = Date.now();
  if (now - probe.at < PROBE_TTL_MS) return { ...probe };
  let next = { available: false, reason: `tmux ${MIN_TMUX_VERSION}+ not found on PATH` };
  try {
    const output = execFileSync('tmux', ['-V'], {
      timeout: 1_500, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'],
    });
    next = tmuxVersionCapability(output);
  } catch { /* not installed / not executable */ }
  probe = { ...next, at: now };
  return { ...probe };
}

/** FLEETDECK_SPAWN_CMD override, or null when unset/blank. */
export function spawnOverrideCmd() {
  const v = process.env.FLEETDECK_SPAWN_CMD;
  return v && v.trim() ? v : null;
}

// ------------------------------------------------------------ scoped names
export const sessionName = port => `fleetdeck-${port}`;
export const windowName = (port, callsign) => `fd${port}-${callsign}`;

/** Exact session + exact window target. A generation-mismatched replacement
 * never receives this fleet session from ensureSession, so a reusable @id on
 * that replacement cannot redirect pane operations after a verified lookup. */
export function exactWindowTarget(port, window) {
  const normalizedPort = generationPort(port);
  const value = String(window);
  const prefix = `fd${normalizedPort}-`;
  if (!value.startsWith(prefix) || !/^[A-Za-z0-9-]+$/.test(value.slice(prefix.length))) {
    throw new Error('invalid scoped tmux window name');
  }
  return `=${sessionName(normalizedPort)}:=${value}`;
}

function exactTargetPort(target) {
  const match = /^=fleetdeck-(\d+):=fd(\d+)-[A-Za-z0-9-]+$/.exec(String(target));
  return match && match[1] === match[2] ? match[1] : null;
}

// ----------------------------------------------------------------- session
/** `has-session` is a PREDICATE: the ok-ness of the result IS the answer. An
 * authoritatively-empty short circuit reports {ok:true, out:''} for every
 * command, which is correct for a LISTING (there is nothing to list) and a lie
 * for a predicate (there is no server, so the session cannot exist). Never let
 * absence manufacture a session that was never created. */
const sessionConfirmed = result => result.ok && !result.authoritativeEmpty;

/** Ensure the detached daemon-owned session `fleetdeck-<port>` exists.
 * `=` prefix = exact session-name match (verified; prefix matching could
 * otherwise confuse fleetdeck-4711 with fleetdeck-47110). */
export async function ensureSession(port) {
  const name = sessionName(port);
  const state = await prepareServerGeneration(port);
  if (!state.enabled) {
    if (await tmux(['has-session', '-t', '=' + name]) !== null) return name;
    if (await tmux(['new-session', '-d', '-s', name]) !== null) return name;
    if (await tmux(['has-session', '-t', '=' + name]) !== null) return name;
    throw new Error(`tmux could not create session ${name}`);
  }

  // With no expected generation and no reachable server, first-run creation is
  // allowed. Every path after this call either claims that server or fails.
  if (state.expected === null) {
    const created = await tmuxResult(['new-session', '-d', '-s', name]);
    if (created.ok) {
      const claimed = await prepareServerGeneration(port);
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
  const created = await tmuxResult([
    'if-shell', '-F', `#{&&:#{==:#{${generationOption(port)}},${refreshed.expected.generation}},#{==:#{pid},${refreshed.expected.serverPid}}}`,
    `new-session -d -s ${name}`,
    `display-message -p ${GENERATION_MISMATCH}`,
  ], { noStart: true });
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
export async function newWindow({ port, callsign, cwd, argv, env = null }) {
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
  // Arm the session's default BEFORE starting the command. The per-window
  // set below is too late for a setup command that exits immediately (`exit
  // 7`): without this pre-arm tmux can delete the window between new-window
  // returning and set-option, losing the error screen the setup contract
  // promises to preserve. Scoped to this fleet session, never the user's tmux
  // server globally; best-effort like the reinforcing per-window write below.
  await tmux(['set-option', '-w', '-g', '-t', '=' + session, 'remain-on-exit', 'on']);
  const out = await tmux([
    'new-window', '-d', '-P', '-F', '#{window_id}',
    '-t', '=' + session + ':', // exact session, next free window index
    '-n', window,
    '-c', cwd,
    ...envArgs,
    '--', ...argv,
  ]);
  if (out === null) throw new Error(`tmux new-window failed for ${window}`);
  const window_id = out.trim();
  if (generation.enabled) {
    const confirmed = await generationVerifiedResult(port, [
      'display-message', '-p', '-t', target,
      ['#{session_name}', '#{window_name}', '#{window_id}'].join(FIELD_SEP),
    ]);
    const expected = [session, window, window_id].join(FIELD_SEP) + '\n';
    if (!confirmed.ok || confirmed.out !== expected) {
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
export async function paneCurrentCommand(target) {
  const args = ['display-message', '-p', '-t', target, `#{pane_dead}${FIELD_SEP}#{pane_current_command}`];
  const port = exactTargetPort(target);
  const result = port === null ? await tmuxResult(args) : await generationVerifiedResult(port, args);
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
export async function listScopedWindows(port) {
  const expectedSession = sessionName(port);
  const listed = await generationVerifiedResult(port, [
    'list-panes', '-a', '-f', `#{==:#{session_name},${expectedSession}}`, '-F',
    ['#{session_name}', '#{window_name}', '#{window_id}', '#{pane_dead}', '#{pane_current_command}'].join(FIELD_SEP),
  ]);
  if (!listed.ok) return null;
  if (listed.out === '') return [];
  const output = listed.out.endsWith('\n') ? listed.out.slice(0, -1) : listed.out;
  if (output === '') return null;
  const prefix = `fd${port}-`;
  const seen = new Set();
  const seenNames = new Set();
  const wins = [];
  for (const line of output.split('\n')) {
    const [session, window, window_id, dead, ...cmd] = line.split(FIELD_SEP);
    if (!session || !window || !/^@\d+$/.test(window_id ?? '')
      || (dead !== '0' && dead !== '1') || cmd.length === 0) return null;
    if (session !== expectedSession || !window?.startsWith(prefix)) continue;
    if (seen.has(window_id)) continue; // human split the pane: original pane wins
    if (seenNames.has(window)) return null; // duplicate scoped names are ambiguous ownership
    seen.add(window_id);
    seenNames.add(window);
    wins.push({ session, window, window_id, pane_dead: dead === '1', pane_cmd: cmd.join(FIELD_SEP) });
  }
  return wins;
}

/** Name-verified kill (CONTRACT): re-locate the window by its EXACT scoped
 * name at kill time and kill by window_id — a renamed/recycled window can
 * never be mis-killed via a stale index. Returns:
 *   {ok:true, window_id}   killed
 *   {ok:false, gone:true}  no window with that exact name exists (410)
 *   {ok:false, error}      tmux kill-window itself failed */
export async function killWindowVerified(name) {
  const scope = typeof name === 'string' ? /^fd(\d+)-[^\u0000-\u001f\u007f]+$/.exec(name) : null;
  if (!scope) return { ok: false, error: 'invalid scoped tmux window name' };
  const expectedSession = sessionName(scope[1]);
  const format = ['#{session_name}', '#{window_name}', '#{window_id}'].join(FIELD_SEP);
  const listArgs = ['list-panes', '-a', '-f', `#{==:#{session_name},${expectedSession}}`, '-F', format];
  const parse = output => {
    if (output === '') return [];
    const body = output.endsWith('\n') ? output.slice(0, -1) : output;
    if (body === '') return null;
    const rows = body.split('\n').map(line => line.split(FIELD_SEP));
    if (rows.some(fields => fields.length !== 3 || !fields[0] || !fields[1] || !/^@\d+$/.test(fields[2]))) return null;
    return rows;
  };
  const exactMatches = rows => {
    const byWindowId = new Map();
    for (const fields of rows) {
      if (fields[0] === expectedSession && fields[1] === name) byWindowId.set(fields[2], fields);
    }
    return [...byWindowId.values()];
  };
  const listed = await generationVerifiedResult(scope[1], listArgs);
  if (!listed.ok) return { ok: false, error: listed.generationError || 'tmux window lookup failed' };
  const rows = parse(listed.out);
  if (rows === null) return { ok: false, error: 'malformed tmux window listing' };
  const matches = exactMatches(rows);
  if (matches.length > 1) return { ok: false, error: 'ambiguous scoped tmux window name' };
  if (matches.length === 0) return { ok: false, gone: true };
  const hit = matches[0];
  let killGeneration;
  try { killGeneration = await prepareServerGeneration(scope[1]); }
  catch (err) {
    return { ok: false, error: `tmux server generation verification failed: ${err?.message || err}` };
  }
  let killed;
  if (!killGeneration.enabled) {
    killed = await tmuxResult(['kill-window', '-t', hit[2]]);
  } else if (!killGeneration.verified || killGeneration.expected === null) {
    return { ok: false, error: 'tmux server generation unavailable or changed' };
  } else {
    // The conditional and kill execute in one server command queue. A socket
    // swap after lookup cannot redirect @id at a replacement server: its absent
    // or different generation/PID selects the harmless marker branch instead.
    killed = await tmuxResult([
      'if-shell', '-F', `#{&&:#{==:#{${generationOption(scope[1])}},${killGeneration.expected.generation}},#{==:#{pid},${killGeneration.expected.serverPid}}}`,
      `kill-window -t ${hit[2]}`,
      `display-message -p ${GENERATION_MISMATCH}`,
    ], { noStart: true });
    if (killed.ok && killed.out.trim() === GENERATION_MISMATCH) {
      return { ok: false, error: 'tmux server generation unavailable or changed' };
    }
  }
  if (killed.ok) return { ok: true, window_id: hit[2] };
  // kill failed — vanished between list and kill, or a real tmux error?
  const rechecked = await generationVerifiedResult(scope[1], listArgs);
  if (!rechecked.ok) return {
    ok: false,
    error: rechecked.generationError || 'tmux window recheck failed after kill error',
  };
  const again = parse(rechecked.out);
  if (again === null) return { ok: false, error: 'malformed tmux window recheck after kill error' };
  const remaining = exactMatches(again);
  if (remaining.length > 1) return { ok: false, error: 'ambiguous scoped tmux window name after kill error' };
  if (remaining.length === 0) return { ok: false, gone: true };
  return { ok: false, error: 'tmux kill-window failed' };
}

/** Neutralize the BRACKETED-PASTE BREAKOUT before any owned-pane paste
 * (CONTRACT). pasteText delivers with `-p`, which wraps the buffer in tmux's
 * bracketed-paste markers ESC[200~ … ESC[201~. Mail delivery (mail.mjs) pipes
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
export function sanitizePaneText(text) {
  let out = String(text).replace(/\r\n?/g, '\n');
  let prev;
  do { prev = out; out = out.replace(/\u001b\[20[01]~/g, ''); } while (out !== prev);
  // Strip C0 controls (keep \t and \n), DEL, AND the C1 range 0x80-0x9f. The
  // C1 strip closes the 8-bit-CSI form: a lone U+009B is the single-byte CSI a
  // terminal could read as the start of a `\u001b[201~` bracketed-paste terminator,
  // so removing it denies that alternative escape. C1 code points are control
  // codes, never legitimate text; everything above U+009F (accented Latin-1,
  // CJK, emoji) is untouched.
  return out.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, '');
}

/** The four sanctioned owned-pane injections (CONTRACT) are: one bring-up
 * Enter for the trust dialog, bracketed-paste mail followed by Enter, and
 * verbatim human typing relayed by the live-terminal modal, plus a human's
 * explicit board action enabling remote control via a literally typed /rc
 * command. All user text still travels without a shell; terminal input uses
 * control-mode hex bytes. */
export async function pasteText(target, text) {
  // Bracketed-paste breakout defense: sanitize BEFORE the buffer is set, so the
  // `-p` paste below can never carry an END marker that turns mail content into
  // live keystrokes (see sanitizePaneText).
  const safe = sanitizePaneText(text);
  // tmux buffers are server-global, so a constant name lets concurrent mail
  // deliveries overwrite each other between set-buffer and paste-buffer. A
  // UUID makes the two-command handoff private to this call; `-d` removes the
  // buffer on success, while finally covers a failed/timed-out paste.
  const buffer = `fdmail-${randomUUID()}`;
  if (await tmux(['set-buffer', '-b', buffer, '--', safe]) === null) return false;
  try {
    return (await tmux(['paste-buffer', '-p', '-d', '-b', buffer, '-t', target])) !== null;
  } finally {
    // Best-effort and deliberately awaited: do not leave mail text resident
    // in tmux when paste-buffer fails before its `-d` cleanup can take effect.
    await tmux(['delete-buffer', '-b', buffer]);
  }
}

export async function sendEnter(target) {
  const port = exactTargetPort(target);
  if (port === null) return (await tmux(['send-keys', '-t', target, 'Enter'])) !== null;
  let state;
  try { state = await prepareServerGeneration(port); } catch { return false; }
  if (!state.enabled) return (await tmux(['send-keys', '-t', target, 'Enter'])) !== null;
  if (!state.verified || state.expected === null) return false;
  // target is produced by exactWindowTarget (restricted alnum/dash grammar),
  // and Enter is static, so this tmux parser string contains no untrusted data.
  const result = await tmuxResult([
    'if-shell', '-F', `#{&&:#{==:#{${generationOption(port)}},${state.expected.generation}},#{==:#{pid},${state.expected.serverPid}}}`,
    `send-keys -t ${target} Enter`,
    `display-message -p ${GENERATION_MISMATCH}`,
  ], { noStart: true });
  return result.ok && result.out.trim() !== GENERATION_MISMATCH;
}

/** Literal keystrokes for TUI commands. `-l --` prevents tmux key-name
 * parsing; unlike bracketed paste this reaches Claude as typed slash input. */
export async function typeKeys(target, text) {
  return (await tmux(['send-keys', '-t', target, '-l', '--', String(text)])) !== null;
}

/** Independent pane-scrollback capture for remote-control URL harvesting.
 * Keep this adapter local rather than coupling daemon state to termbridge. */
export async function capturePane(target) {
  const args = ['capture-pane', '-p', '-t', target];
  const port = exactTargetPort(target);
  if (port === null) return tmux(args);
  const result = await generationVerifiedResult(port, args);
  return result.ok ? result.out : null;
}

/** Bring-up compatibility export; caller enforces at-most-once per spawn. */
export async function sendBringupEnter(target) {
  return sendEnter(target);
}

// ------------------------------------------------------------ test override
/** Launch the FLEETDECK_SPAWN_CMD fixture: argv [cmd, JSON.stringify(spec)],
 * detached, output ignored. onError fires if the process can't start at all
 * (bad path) — asynchronous by nature, so the spawn row simply stays
 * 'spawning' and the caller's note explains why. */
export function launchOverride(cmd, spec, onError = () => {}) {
  try {
    const child = spawnChild(cmd, [JSON.stringify(spec)], { stdio: 'ignore', detached: true });
    child.on('error', err => { try { onError(err); } catch { /* reporting only */ } });
    child.unref();
  } catch (err) {
    try { onError(err); } catch { /* reporting only */ }
  }
}
