#!/usr/bin/env node
// fleetd — Fleet Deck daemon (Phase 1: daemon parity).
// One process per FLEETDECK_HOME, one port, loopback by default (explicit LAN opt-in).
// State lives in SQLite (FLEETDECK_HOME/fleetd.db, WAL) so it survives daemon
// restarts, including a deliberate port change. The HOME pid guard prevents two
// different ports from reconciling that same state; port bind remains the election
// between daemons using different homes, and EADDRINUSE losers exit 3.

import fs from 'node:fs';
import crypto from 'node:crypto';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from './db.mjs';
import { createCore } from './derive.mjs';
import { createHttp, isLoopbackAddress } from './http.mjs';
import { startAgentsPoll } from './agents-poll.mjs';
import { createPayloadCapture } from './payload-capture.mjs';
import { createMdns } from './mdns.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.FLEETDECK_PORT || 4711);
const BIND = (process.env.FLEETDECK_BIND || '127.0.0.1').trim() || '127.0.0.1';
const LAN_MODE = !isLoopbackAddress(BIND);
const HOME = process.env.FLEETDECK_HOME || path.join(os.homedir() || '/tmp', '.fleetdeck');

// LAST-RESORT CONTRACT: individual async entry points should still catch their
// own failures, but one forgotten rejection must not kill the fleet coordinator
// and every terminal it is supervising. Logging here keeps the daemon alive and
// makes the programming error visible without pretending it was handled.
process.on('unhandledRejection', (reason) => {
  console.error('fleetd unhandled rejection (daemon kept alive):', reason);
});

// LAN AUTH CONTRACT: widening the listener changes fleetd from a local
// dashboard into a network-reachable remote-control API. Token derivation is
// therefore completed (or the process exits) before SQLite, HTTP, tmux, or any
// other daemon capability is opened. There is deliberately no insecure LAN
// fallback: a HOME/token read/write failure must never leave an open listener.
function startupFatal(reason) {
  // WHY cleanup here: HOME ownership is claimed before token validation and
  // several other startup steps. Exiting from any of those refusals without
  // releasing our exact pid record leaves a stale lock behind. The helper is a
  // no-op before claimHome succeeds; the try also covers the very early mkdir
  // failure, when its module-scoped ownership flag is not initialized yet.
  try { removeOwnedPidFile(); } catch { /* startup refusal must still exit */ }
  // stderr may be a pipe (launchers/tests); synchronous emission guarantees
  // the refusal reason is not truncated by the immediate non-zero exit.
  try { fs.writeSync(2, `fleetd refused to start: ${reason}\n`); } catch { /* exit still wins */ }
  process.exit(1);
}

try { fs.mkdirSync(HOME, { recursive: true }); } catch (err) {
  startupFatal(`cannot create FLEETDECK_HOME (${err?.code || err?.message || 'unknown error'})`);
}

const PID_FILE = path.join(HOME, 'fleetd.pid');
let ownsPidFile = false;

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
    const comm = fs.readFileSync(`/proc/${pid}/comm`, 'utf8').trim();
    const argv = fs.readFileSync(`/proc/${pid}/cmdline`).toString('utf8').split('\0').filter(Boolean);
    const nodeLike = /^(?:node|nodejs|fleetd)$/i.test(comm);
    const fleetdScript = argv.some(arg => /(?:^|[/\\])fleetd(?:\.bundle)?\.mjs$/.test(arg));
    return nodeLike && fleetdScript;
  } catch (err) {
    // WHY ENOENT is decisive: the PID died after kill(0), so it no longer owns
    // HOME. Permission and transient I/O failures are not decisive; retaining
    // the lock is safer than opening a live daemon's SQLite database twice.
    return err?.code !== 'ENOENT';
  }
}

function removeOwnedPidFile() {
  if (!ownsPidFile) return;
  try {
    const record = pidRecord(fs.readFileSync(PID_FILE, 'utf8'));
    if (record?.pid === process.pid) fs.unlinkSync(PID_FILE);
  } catch { /* shutdown/election cleanup is best effort */ }
  ownsPidFile = false;
}

function claimHome() {
  // WHY `wx`: checking then writing is a race when two launchers start together.
  // The pidfile is the HOME ownership lock, not merely diagnostic bookkeeping.
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      fs.writeFileSync(PID_FILE, JSON.stringify({ pid: process.pid, port: PORT }), {
        encoding: 'utf8', mode: 0o600, flag: 'wx',
      });
      ownsPidFile = true;
      return;
    } catch (err) {
      if (err?.code !== 'EEXIST') {
        startupFatal(`cannot claim FLEETDECK_HOME pidfile (${err?.code || err?.message || 'unknown error'})`);
      }
    }

    let recordText = null;
    let record = null;
    try {
      recordText = fs.readFileSync(PID_FILE, 'utf8');
      record = pidRecord(recordText);
    } catch (err) {
      if (err?.code === 'ENOENT') continue; // the owner exited between EEXIST and read
      startupFatal(`cannot read FLEETDECK_HOME pidfile (${err?.code || err?.message || 'unknown error'})`);
    }
    if (record && pidIsLive(record.pid) && (record.port === null || livePidLooksLikeFleetd(record.pid))) {
      const port = record.port === null ? 'an unknown port (legacy pidfile)' : `port ${record.port}`;
      startupFatal(`FLEETDECK_HOME is already used by live fleetd pid ${record.pid} on ${port}; use a separate FLEETDECK_HOME for another daemon (if that PID was recycled, remove stale pidfile ${PID_FILE})`);
    }

    // WHY compare immediately before unlink: after a crash, two replacements
    // can both inspect the same dead record. If one has already claimed HOME,
    // the other must re-evaluate its fresh live record instead of deleting it
    // and creating a second SQLite owner on another port.
    try {
      if (fs.readFileSync(PID_FILE, 'utf8') !== recordText) continue;
    } catch (err) {
      if (err?.code === 'ENOENT') continue;
      startupFatal(`cannot re-read stale FLEETDECK_HOME pidfile (${err?.code || err?.message || 'unknown error'})`);
    }
    try { fs.unlinkSync(PID_FILE); } catch (err) {
      if (err?.code !== 'ENOENT') startupFatal(`cannot clear stale FLEETDECK_HOME pidfile (${err?.code || err?.message || 'unknown error'})`);
    }
  }
  startupFatal('could not claim FLEETDECK_HOME pidfile after concurrent startup attempts');
}

claimHome();

const TOKEN_FILE = path.join(HOME, 'token');
let TOKEN;
if (Object.hasOwn(process.env, 'FLEETDECK_TOKEN')) {
  TOKEN = String(process.env.FLEETDECK_TOKEN).trim();
  if (TOKEN.length < 16) startupFatal('FLEETDECK_TOKEN must be at least 16 characters after trimming');
} else {
  try {
    const persisted = fs.readFileSync(TOKEN_FILE, 'utf8').trim();
    if (persisted.length >= 16) TOKEN = persisted;
    else if (LAN_MODE) startupFatal('FLEETDECK_HOME/token must contain at least 16 characters');
  } catch (err) {
    if (err?.code !== 'ENOENT' && LAN_MODE) {
      startupFatal(`cannot read FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'})`);
    }
  }
}

if (LAN_MODE && !TOKEN) {
  try { TOKEN = crypto.randomBytes(32).toString('hex'); } catch (err) {
    startupFatal(`cannot generate LAN token (${err?.code || err?.message || 'unknown error'})`);
  }
  try {
    // TOKEN FILE CONTRACT: an explicitly supplied mode keeps the credential
    // owner-only even under a permissive umask. Any persistence error is fatal
    // because an ephemeral secret would be lost and LAN access unusable.
    fs.writeFileSync(TOKEN_FILE, TOKEN, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  } catch (err) {
    startupFatal(`cannot persist FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'})`);
  }
}

let version = '0.0.0';
try {
  version = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../../package.json'), 'utf8')).version || version;
} catch { /* standalone install; /health just reports 0.0.0 */ }

// mDNS name: `fleetdeck.local` by default, so a peer can reach the board
// without knowing an IP. Peers running their OWN fleet would collide on that
// name, hence the override.
const MDNS_NAME = (process.env.FLEETDECK_MDNS_NAME || 'fleetdeck').trim() || 'fleetdeck';
function mdnsInstanceName() {
  // Discovery must remain optional even if the platform RNG fails after an
  // explicit token was supplied. The generic fallback still leaks no hostname.
  try { return `Fleet Deck ${crypto.randomBytes(3).toString('hex')}`; } catch { return 'Fleet Deck'; }
}

const DB_FILE = path.join(HOME, 'fleetd.db');
const db = openDb(DB_FILE);
const core = createCore(db, { port: PORT }); // holdMs resolves from FLEETDECK_HOLD_MS inside

// The board's share panel owns the complete credentialed URLs. Startup logs only
// describe the same endpoints with the credential deliberately redacted.
const MDNS_ENABLED = LAN_MODE && process.env.FLEETDECK_MDNS?.trim().toLowerCase() !== 'off';
const LAN_INFO = LAN_MODE
  ? {
    enabled: true,
    urls: lanAddresses().map(a => `http://${a}:${PORT}/?t=${encodeURIComponent(TOKEN)}`),
    mdns: MDNS_ENABLED ? `http://${MDNS_NAME}.local:${PORT}/?t=${encodeURIComponent(TOKEN)}` : null,
  }
  : { enabled: false, urls: [] };

const { server } = createHttp(core, {
  port: PORT,
  token: TOKEN,
  lan: LAN_INFO,
  // the Phase 1 spike board, kept verbatim at GET /plain; the real board
  // (GET / + /assets/*) is served from board-dist, resolved inside http.mjs
  boardFile: path.join(__dirname, 'board.html'),
  version,
  // validation aid: first 3 raw payloads per hook event → HOME/hook-payloads.jsonl
  capture: createPayloadCapture(HOME),
});

// -------------------------------------------------------------- election
server.on('error', (e) => {
  if (e.code === 'EADDRINUSE') {
    console.error('fleetd already running (port bind lost the election)');
    removeOwnedPidFile();
    try { db.close(); } catch { /* process is exiting */ }
    process.exit(3);
  }
  // A bind/runtime setup failure is terminal too. Release only our exact pid
  // record before the uncaught throw exits, so startup errors do not orphan HOME.
  removeOwnedPidFile();
  try { db.close(); } catch { /* the original server error still wins */ }
  throw e;
});

// Every non-internal IPv4 this host answers on. Wildcard and interface-specific
// binds have no single portable hostname, so the board, the startup banner and
// the mDNS advertisement all speak in terms of this set.
function lanAddresses() {
  const addresses = new Set();
  try {
    for (const entries of Object.values(os.networkInterfaces())) {
      for (const entry of entries || []) {
        if ((entry.family === 'IPv4' || entry.family === 4) && !entry.internal) addresses.add(entry.address);
      }
    }
  } catch (err) {
    console.error(`fleetd could not enumerate LAN addresses (${err?.code || err?.message || 'unknown error'})`);
  }
  return [...addresses];
}

let mdns = null;

server.listen(PORT, BIND, () => {
  const boundHost = BIND.includes(':') && !BIND.startsWith('[') ? `[${BIND}]` : BIND;
  console.log(`fleetd up on http://${boundHost}:${PORT} (pid ${process.pid}, db ${DB_FILE})`);
  if (LAN_MODE) {
    // LOG CREDENTIAL CONTRACT: the real query-bearing URLs live in the board's
    // share panel. stdout is commonly redirected to fleetd.log (often 0644), so
    // it may identify the endpoint but must never become a second token store.
    const addresses = lanAddresses();
    for (const address of addresses) {
      console.log(`fleetd LAN http://${address}:${PORT}/?t=<hidden> (credential available in share panel)`);
    }
    // Discovery is a convenience, never a dependency: mdns.mjs degrades to a
    // no-op on EADDRINUSE (a real avahi owns 5353), EPERM or a network that
    // drops multicast. The IP URLs above always work regardless.
    if (process.env.FLEETDECK_MDNS?.trim().toLowerCase() !== 'off' && addresses.length) {
      mdns = createMdns({
        port: PORT,
        name: MDNS_NAME,
        // DNS-SD instance labels are broadcast beyond the machine. A short
        // random discriminator avoids collisions without disclosing its OS name.
        instance: mdnsInstanceName(),
        addresses,
        log: msg => console.error(`fleetd mdns: ${msg}`),
      });
      mdns.start();
      console.log(`fleetd LAN http://${MDNS_NAME}.local:${PORT}/?t=<hidden> (mDNS; credential available in share panel)`);
    }
  }
  // v1.2 restart reconciliation: spawn rows survive in SQLite, panes survive
  // in tmux — re-join them (rows with a missing window → 'gone' + card
  // offline; scoped fd<PORT>-* windows with no row → /state spawn_orphans).
  core.reconcileSpawns().catch(err => console.error('fleetd spawn reconciliation error:', err));
  startAgentsPoll(core); // F1 secondary session source; first run shortly after listen
});

let shuttingDown = false;
async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;

  // The responder normally caps goodbye delivery at ~250 ms. This outer guard
  // protects shutdown even if a future responder regression returns a promise
  // that never settles. `unref` means the guard itself never prolongs shutdown.
  const hardExit = setTimeout(() => {
    console.error('fleetd shutdown timed out waiting for discovery; forcing exit');
    // These operations are synchronous and best effort, so they cannot await
    // the discovery promise that this watchdog exists to escape.
    removeOwnedPidFile();
    try { db.close(); } catch { /* forced exit still wins */ }
    process.exit(1);
  }, 1000);
  hardExit.unref?.();

  // Goodbye records (TTL 0) retire the .local name immediately; without them a
  // peer's resolver would keep pointing at a dead board for the record's TTL.
  try { await mdns?.stop(); } catch { /* discovery is never load-bearing */ }
  removeOwnedPidFile();
  try { db.close(); } catch { /* already closed */ }
  clearTimeout(hardExit);
  process.exit(0);
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
