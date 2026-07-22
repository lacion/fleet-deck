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
import { createHttp, isLoopbackAddress, parseTrustedOrigins } from './http.mjs';
import { startAgentsPoll } from './agents-poll.mjs';
import { createPayloadCapture } from './payload-capture.mjs';
import { createMdns } from './mdns.mjs';
// HOME-ownership pid helpers now live in takeover.mjs (the version-takeover
// contract), so the daemon's own claimHome lock and the SessionStart hook's
// evict-a-stale-daemon path share one implementation and can never drift.
import { pidRecord, pidIsLive, livePidLooksLikeFleetd } from './takeover.mjs';
import { resolveHome, resolvePort } from './config.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = resolvePort();
const BIND = (process.env.FLEETDECK_BIND || '127.0.0.1').trim() || '127.0.0.1';
const LAN_MODE = !isLoopbackAddress(BIND);
const HOME = resolveHome();
// The tmux adapter is imported before runtime config resolves, but reads this
// value lazily. Export the resolved default too so generation identity is never
// silently disabled merely because the operator accepted ~/.fleetdeck.
process.env.FLEETDECK_HOME = HOME;
// MANAGED CONTRACT: set by `fleetdeck serve`, i.e. this daemon is owned by a
// service supervisor (systemd, or the supervised wrapper) rather than lazily
// spawned by a SessionStart hook. It changes exactly one thing — a plugin hook
// must never SIGTERM us (see fleet-sessionstart.mjs). Without this the hook and
// the supervisor race: the hook kills the daemon and spawns its own replacement
// while the supervisor simultaneously restarts it, and one of the two loses the
// port bind and exits 3. Surfaced on /health, which is what the hook already reads.
const MANAGED = process.env.FLEETDECK_MANAGED === '1';

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
// STATE DIR CONFIDENTIALITY CONTRACT: HOME holds fleetd.db (session cwds,
// callsigns, mail, plan text, raw permission payloads), the access token and
// fleetd.log — all owner-only individually. `mkdir -p` never tightens a dir
// that already exists, so pin 0700 explicitly: a private state dir is the
// PRIMARY guarantee that other local users cannot traverse in, and it backstops
// the DB's 0600 during the window where a lazily recreated WAL/SHM sidecar is
// momentarily 0644 (see db.mjs openDb). Best-effort — the board is reached over
// HTTP, never the filesystem, so no multi-user access model depends on HOME
// being group/other-traversable; a chmod refusal must not block startup.
try { fs.chmodSync(HOME, 0o700); } catch { /* dir confidentiality is best effort */ }

const PID_FILE = path.join(HOME, 'fleetd.pid');
let ownsPidFile = false;

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

// PROXY CONFIG. Both knobs are security-relevant, so a malformed value is a
// startup refusal, never a silent fallback to something laxer: an operator who
// typos a trusted origin must find out at boot, not discover later that the
// board has been refusing their proxy (or worse, accepting the wrong one).
let TRUSTED_ORIGINS = [];
try {
  TRUSTED_ORIGINS = parseTrustedOrigins(process.env.FLEETDECK_TRUSTED_ORIGINS);
} catch (err) {
  startupFatal(`FLEETDECK_TRUSTED_ORIGINS — ${err?.message || 'unparseable'}`);
}
const PROXY_AUTH = (process.env.FLEETDECK_PROXY_AUTH || 'token').trim().toLowerCase() || 'token';
if (PROXY_AUTH !== 'token' && PROXY_AUTH !== 'trust') {
  startupFatal(`FLEETDECK_PROXY_AUTH must be 'token' or 'trust' (got '${PROXY_AUTH}')`);
}
if (PROXY_AUTH === 'trust' && !TRUSTED_ORIGINS.length) {
  startupFatal("FLEETDECK_PROXY_AUTH=trust requires FLEETDECK_TRUSTED_ORIGINS — there is nothing to trust");
}

// FLEETDECK_REQUIRE_TOKEN=on — opt into the token even on pure loopback. On a
// multi-user machine every other OS user can reach 127.0.0.1 and today inherits
// the loopback exemption (tokenless /state, /api/spawn, the lot); this closes
// that, and the documented Host-rewriting-proxy residual with it.
const REQUIRE_TOKEN = (process.env.FLEETDECK_REQUIRE_TOKEN || '').trim().toLowerCase() === 'on';

// TOKEN CONTRACT (0.16.0: a token ALWAYS exists). Since 0.16.0 the daemon
// mints/persists a token on every boot, default loopback included: the
// authenticated hook shims, the token-gated loopback powers (/ws/term, POST
// /mail, gateway settings writes, the unsupervised-spawn arm) and the local
// board's ?t= URL all need a credential to present, and none of that may be
// conditional on the operator opting into LAN mode. TOKEN_REQUIRED now means
// something narrower: a token READ failure is FATAL (the board is reachable
// from outside this machine, or the operator demanded the token everywhere) —
// where the default mode may fall back to minting a fresh one.
const TOKEN_REQUIRED = LAN_MODE || REQUIRE_TOKEN || (TRUSTED_ORIGINS.length > 0 && PROXY_AUTH === 'token');

const TOKEN_FILE = path.join(HOME, 'token');
let TOKEN;
if (Object.hasOwn(process.env, 'FLEETDECK_TOKEN')) {
  TOKEN = String(process.env.FLEETDECK_TOKEN).trim();
  if (TOKEN.length < 16) startupFatal('FLEETDECK_TOKEN must be at least 16 characters after trimming');
  // The token rides query strings (?t=) and prints into a URL, so whitespace,
  // control characters and the URL delimiters &/#/? are refused — a pinned
  // token containing one prints a mangled local board URL and can never match
  // what the browser sends back. Base64's +/= stay legal (documented, and
  // encodeURIComponent handles them on the printed URL).
  if (!/^[A-Za-z0-9_+\-/=]{16,}$/.test(TOKEN)) {
    startupFatal('FLEETDECK_TOKEN must be 16+ characters from [A-Za-z0-9_+-/=] (no whitespace, control characters, or URL delimiters like & and #)');
  }
} else {
  try {
    const persisted = fs.readFileSync(TOKEN_FILE, 'utf8').trim();
    if (persisted.length >= 16) TOKEN = persisted;
    else if (TOKEN_REQUIRED) startupFatal('FLEETDECK_HOME/token must contain at least 16 characters');
  } catch (err) {
    if (err?.code !== 'ENOENT' && TOKEN_REQUIRED) {
      startupFatal(`cannot read FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'})`);
    }
  }
}

if (!TOKEN) {
  try { TOKEN = crypto.randomBytes(32).toString('hex'); } catch (err) {
    startupFatal(`cannot generate access token (${err?.code || err?.message || 'unknown error'})`);
  }
  try {
    // TOKEN FILE CONTRACT: an explicitly supplied mode keeps the credential
    // owner-only even under a permissive umask. Persistence failure is fatal
    // only when the token is REQUIRED (a LAN/proxy/REQUIRE_TOKEN daemon whose
    // secret never reaches the file is unusable); in default loopback mode a
    // read-only HOME degrades to a hook-shim lockout (their 401s fail open),
    // never a boot failure.
    fs.writeFileSync(TOKEN_FILE, TOKEN, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  } catch (err) {
    if (TOKEN_REQUIRED) {
      startupFatal(`cannot persist FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'})`);
    }
    console.error(`fleetd: WARNING: cannot persist FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'}) — hook shims and the gated loopback routes will not authenticate this boot`);
  }
}

// PERSIST AN ENV-SUPPLIED TOKEN TOO. The generate path above writes HOME/token
// only when it MINTS a token. When the operator PINS FLEETDECK_TOKEN in the env
// (the documented way — and how the test suite starts the daemon), TOKEN is set
// but no file was written, and HOME/token was never even read. But the fleet's
// own clients — fleet-watch.mjs / fleet-sessionstart.mjs / fleet-hook.mjs —
// read the bearer ONLY from HOME/token, so with no file they present no token
// and every gated call 401s: the flag's own clients locked out. So whenever a
// token exists, make the file match it (0600), writing only when it is absent
// or differs (a differing file is a stale token — e.g. a previously generated
// one — that would otherwise mislead every file-only client). The
// just-generated case above already matches, so this no-ops it. Persistence
// failure is fatal only when the token is REQUIRED; default loopback warns and
// boots (same degraded-hook contract as the mint path above).
if (TOKEN) {
  let onDisk = null;
  try { onDisk = fs.readFileSync(TOKEN_FILE, 'utf8'); } catch (err) {
    if (err?.code !== 'ENOENT') {
      startupFatal(`cannot read FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'})`);
    }
  }
  if (onDisk === null || onDisk.trim() !== TOKEN) {
    try {
      // mode 0o600 applies on create; an existing (stale) file is rewritten in
      // place — no 'wx', we INTEND to replace a differing token — then chmod in
      // case it pre-existed with looser permissions (writeFileSync ignores mode
      // on an existing file). A persistence failure is fatal: a file-only client
      // that cannot read the current token is silently locked out otherwise.
      fs.writeFileSync(TOKEN_FILE, TOKEN, { encoding: 'utf8', mode: 0o600 });
      try { fs.chmodSync(TOKEN_FILE, 0o600); } catch { /* best-effort tighten */ }
    } catch (err) {
      if (TOKEN_REQUIRED) {
        startupFatal(`cannot persist FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'})`);
      }
      console.error(`fleetd: WARNING: cannot persist FLEETDECK_HOME/token (${err?.code || err?.message || 'unknown error'}) — hook shims and the gated loopback routes will not authenticate this boot`);
    }
  }
}

let version = '0.0.0';
// Test-only override: FLEETDECK_VERSION_OVERRIDE lets the takeover suite stand
// up an "older" or "newer" daemon deterministically without editing (or
// depending on the current value of) package.json. Trimmed, and it wins over
// the package.json read below when present. Production installs never set it.
const versionOverride = process.env.FLEETDECK_VERSION_OVERRIDE?.trim();
if (versionOverride) {
  version = versionOverride;
} else {
  try {
    version = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../../package.json'), 'utf8')).version || version;
  } catch { /* standalone install; /health just reports 0.0.0 */ }
}

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
const core = createCore(db, { port: PORT, version }); // holdMs resolves from FLEETDECK_HOLD_MS inside

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
  version,
  trustedOrigins: TRUSTED_ORIGINS,
  proxyAuth: PROXY_AUTH,
  managed: MANAGED,
  requireToken: REQUIRE_TOKEN,
  // validation aid: first 3 raw payloads per hook event → HOME/hook-payloads.jsonl
  capture: createPayloadCapture(HOME, { secrets: TOKEN ? [TOKEN] : [] }),
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
  if (!LAN_MODE) {
    // 0.16.0: the token always exists, and the local board needs it for the
    // gated powers (/ws/term typing, /mail, gateway settings, the unsupervised
    // arm). Print the credentialed LOCAL URL — loopback-only, and fleetd.log
    // is chmod 0600 by the launcher for exactly this class of secret.
    console.log(`fleetd board http://127.0.0.1:${PORT}/?t=${encodeURIComponent(TOKEN)}`);
  }
  // TEST-SEAM ANNOUNCEMENT: these env vars swap a real subprocess (spawn / term
  // / the daemon script) or override identity (version) for the test suite. In
  // production they must be unset; announcing each active one at boot means a
  // leaked seam (the 2026-07-11 env scar) is visible in fleetd.log rather than
  // silently reshaping the daemon. Provenance only — the value is never logged.
  for (const seam of ['FLEETDECK_SPAWN_CMD', 'FLEETDECK_TERM_CMD', 'FLEETDECK_TEST_DAEMON_SCRIPT', 'FLEETDECK_VERSION_OVERRIDE']) {
    if (process.env[seam]) console.error(`fleetd WARNING: test seam ${seam} active`);
  }
  // FLEETDECK_AGENTS_CMD is a seam ONLY when it names a real command: '' and
  // 'false' are the documented DISABLE sentinels (agents-poll resolveArgv), and
  // unset is the default real CLI — none of those is an injected seam. Mirror
  // resolveArgv's exact trim-but-not-lowercase test so the two can never drift.
  const agentsCmd = process.env.FLEETDECK_AGENTS_CMD;
  if (agentsCmd !== undefined) {
    const trimmedAgents = agentsCmd.trim();
    if (trimmedAgents !== '' && trimmedAgents !== 'false') {
      console.error('fleetd WARNING: test seam FLEETDECK_AGENTS_CMD active');
    }
  }
  // Upgrade-takeover banner: a SessionStart hook set FLEETDECK_REPLACED to the
  // version it SIGTERMed (and waited for the death of) before spawning us onto
  // the freed port — see takeover.mjs. Surface the handoff both in fleetd.log
  // and on the board feed so an automatic upgrade is observable, not silent.
  const replacedVersion = process.env.FLEETDECK_REPLACED;
  if (replacedVersion) {
    console.log(`fleetd v${version} replaced v${replacedVersion} (plugin upgrade takeover)`);
    // The ticker write is best-effort: a banner must never crash the listen
    // callback (an uncaught throw here would take the fresh daemon down).
    try { core.tick(`⬆️ fleetd v${version} replaced v${replacedVersion}`); } catch { /* feed line is non-essential */ }
  }
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
  // 0.7.1: heal cards split by a /clear fork before succession shipped (the CLI
  // mints a new session id on /clear, which used to strand the predecessor's
  // pane on a card that never updates again). Synchronous, idempotent, a no-op
  // on a fleet that never forked — and it runs BEFORE reconcileSpawns so the
  // pane rows it moves are already on the right session when tmux is consulted.
  try {
    core.reconcileClearForks();
  } catch (err) {
    console.error('fleetd /clear fork heal error:', err);
  }
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
