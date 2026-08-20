// fleetd — Fleet Deck daemon (Phase 1: daemon parity).
// One process per FLEETDECK_HOME, one port, loopback by default (explicit LAN opt-in).
// State lives in SQLite (FLEETDECK_HOME/fleetd.db, WAL) so it survives daemon
// restarts, including a deliberate port change. The HOME pid guard prevents two
// different ports from reconciling that same state — with ONE version-aware
// exception (BUG-156): a strictly newer, unmanaged boot supersedes a strictly
// older, unmanaged incumbent instead of refusing, so concurrent upgrade
// takeovers settle on the newest build. Port bind remains the election between
// daemons using different homes, and EADDRINUSE losers exit 3.

import fs from 'node:fs';
import crypto from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../db.ts';
import { DaemonResources } from '../daemon-resources.ts';
import { createCore } from '../derive.ts';
import { bindExecFileDelegate } from '../exec.ts';
import { createHttp, isLoopbackAddress, parseTrustedOrigins } from '../http.ts';
import { startAgentsPoll } from '../agents-poll.ts';
import { createPayloadCapture } from '../payload-capture.ts';
import { createMdns, hostLabel } from '../mdns.ts';
import { startNetworkWatch } from '../network-watch.ts';
import { networkInterfaces } from '../os-net.ts';
// Runtime-agnostic test seam (foundations-hardening §16): every export is a
// no-op unless FLEETDECK_TEST_NET_MOCK / a record-sink var is set, so this is
// inert in production and announced at boot with the other seams below.
import { installConsoleRecorder, mdnsDgramInject, recordRefreshLan } from '../test-seam.ts';
// HOME-ownership pid helpers now live in takeover.mjs (the version-takeover
// contract), so the daemon's own claimHome lock and the SessionStart hook's
// evict-a-stale-daemon path share one implementation and can never drift.
import {
  pidRecord,
  pidIsLive,
  livePidLooksLikeFleetd,
  shouldTakeOver,
  verifyDaemonPid,
  terminateDaemon,
} from '../takeover.ts';
import { resolveHome, resolvePort } from '../config.ts';
import { errText, errCode } from '../errors.ts';
import { DaemonStartupRefusalError, HttpBindStartupError } from './errors.ts';
import { makeIngressExecFileDelegate } from './legacy-process-facade.ts';
import type { RootIngressSupervisorService } from './services/ingress-supervisor.ts';

const moduleDirectory = path.dirname(fileURLToPath(import.meta.url));
// program.ts is nested below the historical source entrypoint, while Bun's
// single-file build resolves import.meta.url beside fleetd.ts. Normalize both
// layouts so package version discovery keeps the same source and bundle path.
const daemonDirectory =
  path.basename(moduleDirectory) === 'app' ? path.dirname(moduleDirectory) : moduleDirectory;

// takeover.ts exports the pidRecord PARSER but not its result interface, so
// derive the record shape from the parser's return type rather than reaching
// for an unexported name — the same { pid, port } contract, kept in lockstep.
type PidRecord = NonNullable<ReturnType<typeof pidRecord>>;

export type DaemonShutdownExitCode = 0 | 1;

/**
 * Narrow, explicitly injected acquisition seam used by the P4 partial-startup
 * matrix. Production never supplies it. Checkpoints sit only after ownership
 * has been published to DaemonResources, so a throwing/interrupting hook must
 * traverse the exact same prefix cleanup as a real constructor failure.
 */
export type DaemonAcquisitionCheckpoint =
  | 'pid-claim'
  | 'durable-config'
  | 'process-runtime'
  | 'database'
  | 'core'
  | 'http-owner'
  | 'background-owners'
  | 'listener'
  | 'discovery-network'
  | 'pollers-boot';

export type DaemonAcquisitionOwner =
  | 'pid-claim'
  | 'process-runtime'
  | 'database'
  | 'core'
  | 'http-owner'
  | 'boot-reconciliation'
  | 'agents-poll'
  | 'network-watch'
  | 'mdns';

export interface DaemonAcquisitionTestHooks {
  readonly afterAcquire?: (checkpoint: DaemonAcquisitionCheckpoint) => void | PromiseLike<void>;
  readonly afterRelease?: (owner: DaemonAcquisitionOwner) => void;
}

function startupRefusal(
  reason: string,
  cleanupCause: unknown | null = null,
): DaemonStartupRefusalError {
  return new DaemonStartupRefusalError({
    reason,
    message: `fleetd refused to start: ${reason}`,
    cleanupCause,
  });
}

/**
 * Typed aggregate acquisition result owned by the root Layer. The imperative
 * resources remain explicit while every Effect submission uses the one
 * already-built root IngressSupervisor.
 */
export interface AcquiredDaemonResources {
  readonly resources: DaemonResources;
  readonly readiness: Promise<void>;
  readonly shutdownExitCode: () => DaemonShutdownExitCode;
  /**
   * Idempotent synchronous fallback used by the custom host teardown only.
   * Policy shutdown deliberately retains it through every Effect Layer
   * finalizer; host teardown invokes it at the exact process-exit boundary so
   * a successor cannot claim HOME while this process can still run callbacks.
   */
  readonly releaseProcessAtHostExit: () => void;
}

async function bootDaemon(
  signal: AbortSignal,
  ingress: RootIngressSupervisorService,
  testHooks?: DaemonAcquisitionTestHooks,
): Promise<AcquiredDaemonResources> {
  signal.throwIfAborted();

  const acquisitionCheckpoint = async (checkpoint: DaemonAcquisitionCheckpoint): Promise<void> => {
    await testHooks?.afterAcquire?.(checkpoint);
    signal.throwIfAborted();
  };
  const observeRelease = <Owner extends { close: () => unknown }>(
    ownerName: DaemonAcquisitionOwner,
    owner: Owner,
  ): Owner => {
    if (!testHooks?.afterRelease) return owner;
    return {
      ...owner,
      close: async () => {
        try {
          await owner.close.call(owner);
        } finally {
          // Observation must never become another fallible finalizer. Failure
          // injection belongs exclusively to afterAcquire above.
          try {
            testHooks.afterRelease?.(ownerName);
          } catch {
            /* test observation is best effort */
          }
        }
      },
    } as Owner;
  };

  // Arm the console tee before any banner line is logged (no-op unless the test
  // sink var is set), so a spawned audit-regression daemon's startup/roam output
  // reaches the parent test.
  installConsoleRecorder();

  // The port is the daemon's identity (pidfile, hooks, board URLs), so an
  // invalid FLEETDECK_PORT must refuse startup BEFORE HOME is claimed. Two
  // guards cover both resolvePort contracts: if resolvePort throws on a
  // malformed value, catch it and raise the exact typed refusal for the root
  // stderr interpreter, ahead of HOME resolution and pidfile cleanup; if
  // resolvePort cannot throw (the hook scripts import it too and
  // must always be able to REPORT to some port), the daemon — the only process
  // that LISTENS — rejects the out-of-range result itself. Without this,
  // the native HTTP bind's synchronous argument validation throws AFTER claimHome
  // wrote the pidfile unless the typed bind boundary owns cleanup, and
  // HOME stays claimed by a stale pidfile whose recorded port is garbage,
  // wedging supervised restarts until the pidfile is removed by hand. Both
  // guards run before mkdir/claimHome, so a refused boot touches nothing and
  // owns nothing.
  let PORT: number;
  try {
    PORT = resolvePort();
  } catch (err) {
    throw startupRefusal(err instanceof Error ? err.message : String(err));
  }
  if (!Number.isInteger(PORT) || PORT < 0 || PORT > 65535) {
    throw startupRefusal(
      `FLEETDECK_PORT must be an integer between 0 and 65535 (got '${String(process.env['FLEETDECK_PORT'])}')`,
    );
  }
  const BIND = (process.env['FLEETDECK_BIND'] ?? '').trim() || '127.0.0.1';
  const LAN_MODE = !isLoopbackAddress(BIND);
  const HOME = resolveHome();
  const PID_FILE = path.join(HOME, 'fleetd.pid');
  let ownsPidFile = false;
  // The tmux adapter is imported before runtime config resolves, but reads this
  // value lazily. Export the resolved default too so generation identity is never
  // silently disabled merely because the operator accepted ~/.fleetdeck.
  process.env['FLEETDECK_HOME'] = HOME;
  // MANAGED CONTRACT: set by `fleetdeck serve`, i.e. this daemon is owned by a
  // service supervisor (systemd, or the supervised wrapper) rather than lazily
  // spawned by a SessionStart hook. It changes exactly one thing — a plugin hook
  // must never SIGTERM us (see fleet-sessionstart.mjs). Without this the hook and
  // the supervisor race: the hook kills the daemon and spawns its own replacement
  // while the supervisor simultaneously restarts it, and one of the two loses the
  // port bind and exits 3. Surfaced on /health, which is what the hook already reads.
  const MANAGED = process.env['FLEETDECK_MANAGED'] === '1';

  // LAST-RESORT CONTRACT: individual async entry points should still catch their
  // own failures, but one forgotten rejection must not kill the fleet coordinator
  // and every terminal it is supervising. Logging here keeps the daemon alive and
  // makes the programming error visible without pretending it was handled.
  const onUnhandledRejection = (reason: unknown): void => {
    console.error('fleetd unhandled rejection (daemon kept alive):', reason);
  };
  process.on('unhandledRejection', onUnhandledRejection);
  let ownsUnhandledRejectionListener = true;
  const removeUnhandledRejectionListener = (): void => {
    if (!ownsUnhandledRejectionListener) return;
    ownsUnhandledRejectionListener = false;
    process.removeListener('unhandledRejection', onUnhandledRejection);
  };

  // LAN AUTH CONTRACT: widening the listener changes fleetd from a local
  // dashboard into a network-reachable remote-control API. Token derivation is
  // therefore completed (or startup is refused) before SQLite, HTTP, tmux, or
  // any other daemon capability is opened. There is deliberately no insecure LAN
  // fallback: a HOME/token read/write failure must never leave an open listener.
  function startupFatal(reason: string): never {
    // WHY cleanup here: HOME ownership is claimed before token validation and
    // several other startup steps. Returning any of those refusals without
    // releasing our exact pid record leaves a stale lock behind. The helper is a
    // no-op before claimHome succeeds, including a very early HOME mkdir failure.
    let cleanupCause: unknown | null = null;
    try {
      removeOwnedPidFile();
    } catch (error) {
      // Preserve the operator-facing refusal while retaining a failed pid
      // release for diagnostics. Ownership stays latched so any already-built
      // aggregate/fallback path can retry the exact verified release.
      cleanupCause = error;
    }
    removeUnhandledRejectionListener();
    throw startupRefusal(reason, cleanupCause);
  }

  try {
    fs.mkdirSync(HOME, { recursive: true });
  } catch (err) {
    startupFatal(`cannot create FLEETDECK_HOME (${errText(err, 'unknown error')})`);
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
  try {
    fs.chmodSync(HOME, 0o700);
  } catch {
    /* dir confidentiality is best effort */
  }

  // The daemon's OWN version, resolved BEFORE claimHome: the same-HOME
  // arbitration below needs it to decide whether a live incumbent is a strictly
  // older build this boot should supersede (BUG-156). Historically this block
  // sat further down; the rules are unchanged.
  let version = '0.0.0';
  // Test-only override: FLEETDECK_VERSION_OVERRIDE lets the takeover suite stand
  // up an "older" or "newer" daemon deterministically without editing (or
  // depending on the current value of) package.json. Trimmed, and it wins over
  // the package.json read below when present. Production installs never set it.
  const versionOverride = process.env['FLEETDECK_VERSION_OVERRIDE']?.trim();
  if (versionOverride) {
    version = versionOverride;
  } else {
    try {
      version =
        (
          JSON.parse(
            fs.readFileSync(path.resolve(daemonDirectory, '../../package.json'), 'utf8'),
          ) as {
            version?: string;
          }
        ).version ?? version;
    } catch {
      /* standalone install; /health just reports 0.0.0 */
    }
  }

  function removeOwnedPidFile(): void {
    if (!ownsPidFile) return;

    let record: PidRecord | null;
    try {
      record = pidRecord(fs.readFileSync(PID_FILE, 'utf8'));
    } catch (error) {
      if (errCode(error) === 'ENOENT') {
        ownsPidFile = false;
        return;
      }
      // An unreadable path might still be our live ownership record. Keep the
      // latch set and surface the failure so lifecycle/host policy exits 1 and
      // the exact release can be retried at host teardown.
      throw error;
    }

    if (record?.pid !== process.pid) {
      // The path is readable and no longer names this process. Never delete a
      // successor's record, and retire our stale in-memory ownership claim.
      ownsPidFile = false;
      return;
    }

    try {
      fs.unlinkSync(PID_FILE);
    } catch (error) {
      if (errCode(error) === 'ENOENT') {
        ownsPidFile = false;
        return;
      }
      // Do not clear ownsPidFile: the custom host teardown is the final retry
      // boundary and must be able to turn this operational failure into exit 1.
      throw error;
    }
    ownsPidFile = false;
  }

  const daemonResources = new DaemonResources({
    onCloseError(name, error) {
      console.error(`fleetd ${name} shutdown error:`, error);
    },
  });
  let ownsProcessExitFallback = false;
  const releaseHostProcessOwnershipAtExit = (): void => {
    try {
      releaseHostProcessOwnership();
    } catch {
      // The host is already exiting (every partial-startup path is non-zero).
      // The normal HostControl teardown invokes the throwing form before this
      // boundary so an acquired daemon can still upgrade a clean exit to 1.
    }
  };
  const releaseHostProcessOwnership = (): void => {
    const hadExitFallback = ownsProcessExitFallback;
    if (hadExitFallback) {
      ownsProcessExitFallback = false;
      process.removeListener('exit', releaseHostProcessOwnershipAtExit);
    }
    try {
      // Host callbacks retire before the HOME lock, leaving pidfile removal as
      // the final observable release immediately before the runtime exits the
      // process, including failure and hard-deadline paths.
      removeUnhandledRejectionListener();
      removeOwnedPidFile();
    } catch (error) {
      // A transient unlink/read failure must not discard the only fallback
      // that can retry at actual process exit. Re-arm exactly once after the
      // failed attempt; the ownership latch itself also remains set.
      if (hadExitFallback && !ownsProcessExitFallback) {
        process.once('exit', releaseHostProcessOwnershipAtExit);
        ownsProcessExitFallback = true;
      }
      throw error;
    }
  };
  // An acquisition can fail after claiming HOME but before it can publish an
  // AcquiredDaemonResources value for HostControl. Retain one synchronous
  // last-chance release for that prefix; every explicit release unregisters it.
  process.once('exit', releaseHostProcessOwnershipAtExit);
  ownsProcessExitFallback = true;
  daemonResources.setProcess(
    'host-process',
    observeRelease('pid-claim', { close: releaseHostProcessOwnership }),
  );

  // BUG-156: when the pidfile names a LIVE fleetd, "HOME is taken" used to be
  // unconditionally fatal — but a same-HOME challenger is usually a takeover
  // REPLACEMENT (two concurrent newer hooks both spawn after evicting the stale
  // daemon; the port-bind election that resolves them has no notion of version,
  // so the OLDER candidate's build can claim HOME+port first and the newest
  // build would die here, settling the upgrade on superseded code). Arbitrate
  // by version instead: probe the incumbent's loopback /health — deliberately
  // token-free — and when THIS boot is a strictly newer, unmanaged build, evict
  // the incumbent (SIGTERM + wait-for-death, the takeover contract's own
  // terminateDaemon, no SIGKILL) and take HOME. Every guard of the hook-side
  // takeover applies unchanged (strictly newer only, both versions parse,
  // neither is the 0.0.0 sentinel, a managed daemon on either side never
  // fights), and every uncertain answer falls back to the historical refusal.
  // Async (fetch + death-poll), so claimHome awaits it.
  async function supersedeIfNewer(record: PidRecord): Promise<boolean> {
    // A MANAGED daemon never fights for HOME: it is owned by a supervisor that
    // will restart whatever is killed — evicting from inside boot restarts the
    // very race FLEETDECK_MANAGED exists to prevent (the SessionStart hook's
    // managed no-evict guard is the other half of this rule).
    if (MANAGED) return false;
    let incumbent: { managed?: boolean; version?: string; pid?: number } | null;
    try {
      // The incumbent's port comes from its OWN pidfile record; a legacy
      // port-less record cannot be probed, so it keeps the historical refusal.
      if (record.port === null || !Number.isInteger(record.port)) return false;
      const res = await fetch(`http://127.0.0.1:${record.port}/health`, {
        signal: AbortSignal.any([signal, AbortSignal.timeout(1500)]),
      });
      incumbent = (await res.json()) as {
        managed?: boolean;
        version?: string;
        pid?: number;
      } | null;
    } catch {
      signal.throwIfAborted();
      return false;
    } // unreachable/unparseable: unknown incumbent, refuse as before
    signal.throwIfAborted();
    if (!incumbent) return false;
    // A managed INCUMBENT owns its port+HOME outright (a `fleetdeck serve`
    // service); a hook-spawned challenger must never SIGTERM it.
    if (incumbent.managed) return false;
    // The incumbent's /health pid must agree with the pidfile record we are
    // arbitrating — otherwise we would signal a process the record does not
    // name (verifyDaemonPid re-checks this against the pidfile + /proc shape).
    if (!shouldTakeOver(version, incumbent.version)) return false;
    if (incumbent.pid !== record.pid) return false;
    if (!verifyDaemonPid(record.pid, HOME, record.port)) return false;
    if (!(await terminateDaemon(record.pid, { signal }))) return false; // wedged: leave it serving, refuse
    signal.throwIfAborted();
    console.log(
      `fleetd v${version} superseded v${String(incumbent.version)}: a strictly newer build claimed FLEETDECK_HOME`,
    );
    return true; // the caller's next attempt sees the freed (or stale, soon-reaped) pidfile
  }

  async function claimHome(): Promise<void> {
    // WHY `wx`: checking then writing is a race when two launchers start together.
    // The pidfile is the HOME ownership lock, not merely diagnostic bookkeeping.
    for (let attempt = 0; attempt < 3; attempt += 1) {
      try {
        fs.writeFileSync(PID_FILE, JSON.stringify({ pid: process.pid, port: PORT }), {
          encoding: 'utf8',
          mode: 0o600,
          flag: 'wx',
        });
        ownsPidFile = true;
        return;
      } catch (err) {
        if (errCode(err) !== 'EEXIST') {
          startupFatal(`cannot claim FLEETDECK_HOME pidfile (${errText(err, 'unknown error')})`);
        }
      }

      let recordText: string | null = null;
      let record: PidRecord | null = null;
      try {
        recordText = fs.readFileSync(PID_FILE, 'utf8');
        record = pidRecord(recordText);
      } catch (err) {
        if (errCode(err) === 'ENOENT') continue; // the owner exited between EEXIST and read
        startupFatal(`cannot read FLEETDECK_HOME pidfile (${errText(err, 'unknown error')})`);
      }
      if (
        record &&
        pidIsLive(record.pid) &&
        (record.port === null || livePidLooksLikeFleetd(record.pid))
      ) {
        // Version arbitration (BUG-156): a live incumbent is not automatically
        // the winner — a strictly newer, unmanaged boot supersedes a strictly
        // older, unmanaged one. supersedeIfNewer SIGTERMs the incumbent and
        // waits for its death; the loop's next attempt then claims the freed
        // pidfile (or re-reads the incumbent's stale record and clears it via
        // the dead-record path below, since terminateDaemon resolved only after
        // ESRCH). False → the historical refusal, byte for byte.
        const superseded = await supersedeIfNewer(record);
        signal.throwIfAborted();
        if (superseded) continue;
        const port =
          record.port === null ? 'an unknown port (legacy pidfile)' : `port ${record.port}`;
        startupFatal(
          `FLEETDECK_HOME is already used by live fleetd pid ${record.pid} on ${port}; use a separate FLEETDECK_HOME for another daemon (if that PID was recycled, remove stale pidfile ${PID_FILE})`,
        );
      }

      // WHY compare immediately before unlink: after a crash, two replacements
      // can both inspect the same dead record. If one has already claimed HOME,
      // the other must re-evaluate its fresh live record instead of deleting it
      // and creating a second SQLite owner on another port.
      try {
        if (fs.readFileSync(PID_FILE, 'utf8') !== recordText) continue;
      } catch (err) {
        if (errCode(err) === 'ENOENT') continue;
        startupFatal(
          `cannot re-read stale FLEETDECK_HOME pidfile (${errText(err, 'unknown error')})`,
        );
      }
      try {
        fs.unlinkSync(PID_FILE);
      } catch (err) {
        if (errCode(err) !== 'ENOENT')
          startupFatal(
            `cannot clear stale FLEETDECK_HOME pidfile (${errText(err, 'unknown error')})`,
          );
      }
    }
    startupFatal('could not claim FLEETDECK_HOME pidfile after concurrent startup attempts');
  }

  try {
    await claimHome();
    if (testHooks) await acquisitionCheckpoint('pid-claim');
    else signal.throwIfAborted();
  } catch (error) {
    await daemonResources.close();
    throw error;
  }

  // PROXY CONFIG. Both knobs are security-relevant, so a malformed value is a
  // startup refusal, never a silent fallback to something laxer: an operator who
  // typos a trusted origin must find out at boot, not discover later that the
  // board has been refusing their proxy (or worse, accepting the wrong one).
  let TRUSTED_ORIGINS: ReturnType<typeof parseTrustedOrigins> = [];
  try {
    TRUSTED_ORIGINS = parseTrustedOrigins(process.env['FLEETDECK_TRUSTED_ORIGINS']);
  } catch (err) {
    startupFatal(
      `FLEETDECK_TRUSTED_ORIGINS — ${err instanceof Error && err.message ? err.message : 'unparseable'}`,
    );
  }
  const PROXY_AUTH = (process.env['FLEETDECK_PROXY_AUTH'] ?? '').trim().toLowerCase() || 'token';
  if (PROXY_AUTH !== 'token' && PROXY_AUTH !== 'trust') {
    startupFatal(`FLEETDECK_PROXY_AUTH must be 'token' or 'trust' (got '${PROXY_AUTH}')`);
  }
  if (PROXY_AUTH === 'trust' && !TRUSTED_ORIGINS.length) {
    startupFatal(
      'FLEETDECK_PROXY_AUTH=trust requires FLEETDECK_TRUSTED_ORIGINS — there is nothing to trust',
    );
  }

  // FLEETDECK_REQUIRE_TOKEN=on — opt into the token even on pure loopback. On a
  // multi-user machine every other OS user can reach 127.0.0.1 and today inherits
  // the loopback exemption (tokenless /state, /api/spawn, the lot); this closes
  // that, and the documented Host-rewriting-proxy residual with it.
  const REQUIRE_TOKEN =
    (process.env['FLEETDECK_REQUIRE_TOKEN'] ?? '').trim().toLowerCase() === 'on';
  // The power-gate opt-out is valid only inside the existing plain-loopback trust
  // zone. It cannot weaken hook auth, a LAN listener, or REQUIRE_TOKEN mode.
  const TRUST_LOOPBACK_VALUE = (process.env['FLEETDECK_TRUST_LOOPBACK'] ?? '').trim().toLowerCase();
  if (
    TRUST_LOOPBACK_VALUE !== '' &&
    TRUST_LOOPBACK_VALUE !== 'on' &&
    TRUST_LOOPBACK_VALUE !== 'off'
  ) {
    startupFatal(`FLEETDECK_TRUST_LOOPBACK must be 'on' or 'off' (got '${TRUST_LOOPBACK_VALUE}')`);
  }
  const TRUST_LOOPBACK = TRUST_LOOPBACK_VALUE === 'on';
  if (TRUST_LOOPBACK && REQUIRE_TOKEN) {
    startupFatal('FLEETDECK_TRUST_LOOPBACK=on conflicts with FLEETDECK_REQUIRE_TOKEN=on');
  }
  if (TRUST_LOOPBACK && LAN_MODE) {
    startupFatal('FLEETDECK_TRUST_LOOPBACK=on requires a loopback FLEETDECK_BIND');
  }

  // TOKEN CONTRACT (0.16.0: a token ALWAYS exists). Since 0.16.0 the daemon
  // mints/persists a token on every boot, default loopback included: hook shims
  // and the local board always have a credential, and the four power gates use it
  // unless the explicit plain-loopback opt-out applies. TOKEN_REQUIRED means
  // something narrower: a token READ failure is FATAL (the board is reachable
  // from outside this machine, or the operator demanded the token everywhere) —
  // where the default mode may fall back to minting a fresh one.
  const TOKEN_REQUIRED =
    LAN_MODE || REQUIRE_TOKEN || (TRUSTED_ORIGINS.length > 0 && PROXY_AUTH === 'token');

  const TOKEN_FILE = path.join(HOME, 'token');
  let TOKEN: string | undefined;
  if (Object.hasOwn(process.env, 'FLEETDECK_TOKEN')) {
    TOKEN = String(process.env['FLEETDECK_TOKEN']).trim();
    if (TOKEN.length < 16)
      startupFatal('FLEETDECK_TOKEN must be at least 16 characters after trimming');
    // The token rides query strings (?t=) and prints into a URL, so whitespace,
    // control characters and the URL delimiters &/#/? are refused — a pinned
    // token containing one prints a mangled local board URL and can never match
    // what the browser sends back. Base64's +/= stay legal (documented, and
    // encodeURIComponent handles them on the printed URL).
    if (!/^[A-Za-z0-9_+\-/=]{16,}$/.test(TOKEN)) {
      startupFatal(
        'FLEETDECK_TOKEN must be 16+ characters from [A-Za-z0-9_+-/=] (no whitespace, control characters, or URL delimiters like & and #)',
      );
    }
  } else {
    let persisted: string | null = null;
    try {
      persisted = fs.readFileSync(TOKEN_FILE, 'utf8').trim();
    } catch (err) {
      if (errCode(err) !== 'ENOENT' && TOKEN_REQUIRED) {
        startupFatal(`cannot read FLEETDECK_HOME/token (${errText(err, 'unknown error')})`);
      }
    }
    if (persisted !== null) {
      if (persisted.length >= 16) TOKEN = persisted;
      else if (TOKEN_REQUIRED)
        startupFatal('FLEETDECK_HOME/token must contain at least 16 characters');
    }
  }

  if (!TOKEN) {
    try {
      TOKEN = crypto.randomBytes(32).toString('hex');
    } catch (err) {
      startupFatal(`cannot generate access token (${errText(err, 'unknown error')})`);
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
        startupFatal(`cannot persist FLEETDECK_HOME/token (${errText(err, 'unknown error')})`);
      }
      console.error(
        `fleetd: WARNING: cannot persist FLEETDECK_HOME/token (${errText(err, 'unknown error')}) — hook shims and the gated loopback routes will not authenticate this boot`,
      );
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
    let onDisk: string | null = null;
    try {
      onDisk = fs.readFileSync(TOKEN_FILE, 'utf8');
    } catch (err) {
      if (errCode(err) !== 'ENOENT') {
        startupFatal(`cannot read FLEETDECK_HOME/token (${errText(err, 'unknown error')})`);
      }
    }
    if (onDisk?.trim() !== TOKEN) {
      try {
        // mode 0o600 applies on create; an existing (stale) file is rewritten in
        // place — no 'wx', we INTEND to replace a differing token — and the chmod
        // below covers a file that pre-existed with looser permissions
        // (writeFileSync ignores mode on an existing file). A persistence failure
        // is fatal: a file-only client that cannot read the current token is
        // silently locked out otherwise.
        fs.writeFileSync(TOKEN_FILE, TOKEN, { encoding: 'utf8', mode: 0o600 });
      } catch (err) {
        if (TOKEN_REQUIRED) {
          startupFatal(`cannot persist FLEETDECK_HOME/token (${errText(err, 'unknown error')})`);
        }
        console.error(
          `fleetd: WARNING: cannot persist FLEETDECK_HOME/token (${errText(err, 'unknown error')}) — hook shims and the gated loopback routes will not authenticate this boot`,
        );
      }
    }
    // TIGHTEN THE TOKEN FILE ON EVERY BOOT, not only when it was (re)written: a
    // matching file keeps its old mode, so an operator-preprovisioned 0644 token
    // in a group/other-traversable HOME stays readable by another local account —
    // a cross-UID bearer leak against the documented owner-only contract. (A file
    // this boot created already got 0600 from the write paths above; umask can
    // only strip bits.) When the token is REQUIRED, a chmod refusal is fatal —
    // the owner-only contract cannot be honored — while default loopback degrades
    // to a warning, mirroring the best-effort HOME chmod at startup.
    if (onDisk !== null) {
      try {
        fs.chmodSync(TOKEN_FILE, 0o600);
      } catch (err) {
        const why = errText(err, 'unknown error');
        if (TOKEN_REQUIRED) {
          startupFatal(`cannot tighten FLEETDECK_HOME/token to owner-only 0600 (${why})`);
        }
        console.error(
          `fleetd: WARNING: cannot tighten FLEETDECK_HOME/token to owner-only 0600 (${why}) — the token stays readable by other local accounts this boot`,
        );
      }
    }
  }
  // TOKEN is finalized above: every path either set it (pinned / persisted /
  // freshly minted) or exited via startupFatal, so it is a plain string here.
  // Capture it in a const the closures below (lanInfoFor, the listen banner) can
  // read as `string` — a reassignable `let` de-narrows back to `string |
  // undefined` when captured, which would fail the URL builders under strict.
  const AUTH_TOKEN = TOKEN;

  // mDNS name: `fleetdeck.local` by default, so a peer can reach the board
  // without knowing an IP. Peers running their OWN fleet would collide on that
  // name, hence the override. Canonicalized ONCE through mdns.mjs's hostLabel:
  // the responder rewrites dots/controls and truncates to a 63-byte DNS label,
  // so the share URL, the startup log line and the HTTP Host allowlist must be
  // built from the same label — interpolating the raw configured value would
  // publish a name that never resolves and refuse the name actually advertised.
  // The empty string canonicalizes away to nothing, so the 'fleetdeck' default
  // is applied BOTH before the call (a blank env value) and as hostLabel's own
  // fallback (a value that is nothing but dots/controls).
  const MDNS_NAME = hostLabel(
    (process.env['FLEETDECK_MDNS_NAME'] ?? '').trim() || 'fleetdeck',
    'fleetdeck',
  );
  function mdnsInstanceName(): string {
    // Discovery must remain optional even if the platform RNG fails after an
    // explicit token was supplied. The generic fallback still leaks no hostname.
    try {
      return `Fleet Deck ${crypto.randomBytes(3).toString('hex')}`;
    } catch {
      return 'Fleet Deck';
    }
  }

  try {
    if (testHooks) await acquisitionCheckpoint('durable-config');
  } catch (error) {
    await daemonResources.close();
    throw error;
  }

  const DB_FILE = path.join(HOME, 'fleetd.db');

  let db: ReturnType<typeof openDb>;
  let core: ReturnType<typeof createCore>;
  try {
    // Bind the temporary Promise facade synchronously to the already-built root
    // Context before SQLite/core can reach an exec caller. This adapter owns no
    // runtime; the registered ingress owner closes admission, interrupts, joins,
    // and finally unbinds the facade during aggregate cleanup.
    signal.throwIfAborted();
    const unbindExecFile = bindExecFileDelegate(makeIngressExecFileDelegate(ingress));
    try {
      daemonResources.setProcessRuntime(
        'root-ingress',
        observeRelease('process-runtime', {
          quiesce: ingress.quiesce,
          interrupt: ingress.interrupt,
          join: ingress.join,
          close: async () => {
            try {
              await ingress.close();
            } finally {
              unbindExecFile();
            }
          },
        }),
      );
    } catch (error) {
      unbindExecFile();
      throw error;
    }
    if (testHooks) await acquisitionCheckpoint('process-runtime');
    else signal.throwIfAborted();
    db = openDb(DB_FILE);
    daemonResources.setStore('sqlite', observeRelease('database', { close: () => db.close() }));
    if (testHooks) await acquisitionCheckpoint('database');
    else signal.throwIfAborted();
    core = createCore(db, { port: PORT, version }); // holdMs resolves from FLEETDECK_HOLD_MS inside
    daemonResources.setCore(observeRelease('core', core.lifecycle));
    if (testHooks) await acquisitionCheckpoint('core');
    else signal.throwIfAborted();
  } catch (error) {
    // A constructor may fail after HOME was claimed or SQLite was opened. Close
    // the exact acquired prefix before preserving the original startup failure.
    await daemonResources.close();
    throw error;
  }

  // BOOT-RECONCILIATION READINESS: successful bind kicks the boot heals
  // fire-and-forget (see below), so /health answering 200 has never meant they
  // ran. createCore itself also fires one async sweep (the boot retentionSweep);
  // fold it into the same readiness so `settled` truly closes the startup
  // mutation window a strict /ws client must wait out before connecting. The
  // settled PROMISE object is wired in after createHttp returns so a response
  // never awaits a heal — /health stays sub-millisecond even with tmux down —
  // and settling 'settled' from the heals' own .finally is safe even in the
  // artificial all-synchronous path (readiness() is never consulted before that).
  let settleReconciliation: (() => void) | null = null;
  const bootReconciliation = new Promise<void>((resolve) => {
    settleReconciliation = () => {
      resolve();
    };
  });
  const bootReadiness = {
    reconciliationStatus: (): 'settled' | 'reconciling' =>
      settleReconciliation === null ? 'settled' : 'reconciling',
    readiness: bootReconciliation,
  };

  // The board's share panel owns the complete credentialed URLs. Startup logs only
  // describe the same endpoints with the credential deliberately redacted.
  const MDNS_ENABLED = LAN_MODE && process.env['FLEETDECK_MDNS']?.trim().toLowerCase() !== 'off';
  // One builder so startup and the interface-change refresh below can never drift
  // on how a LAN status object is shaped.
  interface LanInfo {
    enabled: boolean;
    urls: string[];
    mdns?: string | null;
  }
  function lanInfoFor(addresses: string[]): LanInfo {
    return LAN_MODE
      ? {
          enabled: true,
          urls: addresses.map((a) => `http://${a}:${PORT}/?t=${encodeURIComponent(AUTH_TOKEN)}`),
          mdns: MDNS_ENABLED
            ? `http://${MDNS_NAME}.local:${PORT}/?t=${encodeURIComponent(AUTH_TOKEN)}`
            : null,
        }
      : { enabled: false, urls: [] };
  }
  const LAN_INFO = lanInfoFor(lanAddresses());

  let http: ReturnType<typeof createHttp>;
  try {
    signal.throwIfAborted();
    http = createHttp(core, {
      port: PORT,
      token: AUTH_TOKEN,
      // lan.mdns reflects the responder's LIVE state, not the boot snapshot: if the
      // responder disables itself after start() (no multicast membership, a socket
      // error), the share panel stops offering a URL that cannot resolve.
      lan: () => (LAN_INFO.mdns && mdns && !mdns.alive() ? { ...LAN_INFO, mdns: null } : LAN_INFO),
      version,
      trustedOrigins: TRUSTED_ORIGINS,
      proxyAuth: PROXY_AUTH,
      managed: MANAGED,
      requireToken: REQUIRE_TOKEN,
      trustLoopback: TRUST_LOOPBACK,
      startup: bootReadiness,
      // validation aid: first 3 raw payloads per hook event → HOME/hook-payloads.jsonl
      capture: createPayloadCapture(HOME, { secrets: AUTH_TOKEN ? [AUTH_TOKEN] : [] }),
    });
    daemonResources.setHttp(observeRelease('http-owner', http.lifecycle));
    if (testHooks) await acquisitionCheckpoint('http-owner');
    else signal.throwIfAborted();
  } catch (error) {
    await daemonResources.close();
    throw error;
  }
  const { whenBroadcastIdle, refreshLan } = http;

  // Every non-internal IPv4 this host answers on. Wildcard and interface-specific
  // binds have no single portable hostname, so the board, the startup banner and
  // the mDNS advertisement all speak in terms of this set.
  function lanAddresses(): string[] {
    const addresses = new Set<string>();
    try {
      for (const entries of Object.values(networkInterfaces())) {
        for (const entry of entries ?? []) {
          // @types/node types NetworkInterfaceInfo.family as the string 'IPv4' |
          // 'IPv6'; older node reported the numeric family 4, so accept both. Read
          // it opaquely (`unknown`) — a `string | number` annotation still gets
          // control-flow-narrowed to the 'IPv4' | 'IPv6' literal union, which then
          // rejects `=== 4` as a no-overlap comparison.
          const fam: unknown = entry.family;
          if ((fam === 'IPv4' || fam === 4) && !entry.internal) addresses.add(entry.address);
        }
      }
    } catch (err) {
      console.error(`fleetd could not enumerate LAN addresses (${errText(err, 'unknown error')})`);
    }
    return [...addresses];
  }

  let mdns: ReturnType<typeof createMdns> | null = null;

  // NETWORK LIFECYCLE (BUG-118 / BUG-129): the address snapshot baked into
  // LAN_INFO (share URLs), the mDNS advertisement, and the HTTP Host allowlist
  // all go stale when the network moves (Wi-Fi roam, DHCP renewal, VPN up/down)
  // — a long-lived daemon would keep answering with dead A records and reject
  // its own new address until restart. So LAN mode polls the interface list
  // (there is no portable interface-change event worth the netlink/notify
  // platform code); on any change the share URLs and the allowlist are
  // refreshed atomically from the same snapshot and the responder
  // withdraws/announces records for the delta. The allowlist ALSO refreshes
  // per request from the same interface data, so even between polls a request
  // arriving via a fresh address is recognized as ours.
  const LAN_REFRESH_MS = (() => {
    const n = Number(process.env['FLEETDECK_LAN_REFRESH_MS']);
    return Number.isFinite(n) && n > 0 ? n : 30_000;
  })();

  function startMdns(addresses: string[]): void {
    // A spawned audit-regression daemon swaps node:dgram for a socket that never
    // touches the network; undefined in production, so the real dgram is used.
    const inject = mdnsDgramInject();
    mdns = createMdns({
      port: PORT,
      name: MDNS_NAME,
      // DNS-SD instance labels are broadcast beyond the machine. A short
      // random discriminator avoids collisions without disclosing its OS name.
      instance: mdnsInstanceName(),
      addresses,
      log: (msg) => {
        console.error(`fleetd mdns: ${msg}`);
      },
      // exactOptionalPropertyTypes: add `inject` only when present — never as `undefined`.
      ...(inject ? { inject } : {}),
    });
    mdns.start();
    // start() cannot report readiness: bind and multicast membership only
    // resolve asynchronously. Announce the .local URL only on a tick where the
    // responder is actually alive — never after it has already stood down, or
    // the banner and share panel would offer a URL that cannot resolve (the
    // disable itself is logged by mdns.mjs via onDown/log).
    setImmediate(() => {
      if (mdns?.alive()) {
        console.log(
          `fleetd LAN http://${MDNS_NAME}.local:${PORT}/?t=<hidden> (mDNS; credential available in share panel)`,
        );
      }
    });
  }

  // The IP LAN banner. Printed ONCE at startup and never again: the credentialed
  // URLs live in the board's share panel, so the log identifies the endpoint with
  // the token elided. NOT a refreshLan call — the share panel's initial state
  // comes from createHttp's `lan` getter, and routing a startup refresh through
  // refreshLan would put the full-credential URL into a log line that is commonly
  // world-readable (BUG-072 token-log contract).
  function announceLanUrls(addresses: string[]): void {
    for (const address of addresses) {
      console.log(
        `fleetd LAN http://${address}:${PORT}/?t=<hidden> (credential available in share panel)`,
      );
    }
  }

  // A live network change (roam / DHCP renewal): re-derive the share URLs and the
  // Host allowlist, retire/announce the responder's A records for the delta, and
  // say what the address set is NOW. refreshLan carries the real credentialed URLs
  // to the share panel BY DESIGN — that is the panel's whole point — so it is the
  // one surface allowed the token; the console line below never is.
  function refreshNetwork(addresses: string[]): void {
    lastLanAddresses = addresses;
    // One builder (see lanInfoFor above) so startup and this refresh can never
    // drift on how a LAN status object is shaped.
    try {
      const info = lanInfoFor(addresses);
      refreshLan(info);
      // No-op in production; under the audit-regression seam it records the roam so
      // the test can prove the share panel followed the interface list.
      recordRefreshLan(info);
    } catch (err) {
      console.error('fleetd share-URL refresh error:', err);
    }
    try {
      mdns?.update({ addresses });
    } catch {
      /* discovery is never load-bearing */
    }
  }

  let lastLanAddresses: string[] | null = null;

  function watchNetwork(): ReturnType<typeof startNetworkWatch> {
    return startNetworkWatch({
      enabled: LAN_MODE,
      intervalMs: LAN_REFRESH_MS,
      readAddresses: lanAddresses,
      previousAddresses: () => lastLanAddresses,
      onChange(addresses, previous) {
        const gone = previous.filter((a) => !addresses.includes(a));
        refreshNetwork(addresses);
        if (addresses.length) {
          console.log(
            `fleetd LAN addresses now ${addresses.join(', ')}${gone.length ? ` (was ${gone.join(', ')})` : ''}`,
          );
        } else {
          console.log(
            'fleetd LAN interface lost; board still reachable at its last addresses only until the link returns',
          );
        }
        // A responder that never started (no address at boot) is created on the
        // same transition — the network coming up is exactly its cue.
        if (!mdns && MDNS_ENABLED && addresses.length) startMdns(addresses);
        // A board left open across a roam keeps showing the stale URL set until
        // its next /state poll otherwise. tick() rides the same coalesced
        // broadcast as every other feed line.
        try {
          core.tick(
            `🌐 LAN address changed — share panel updated${gone.length ? ` (was ${gone.join(', ')})` : ''}`,
          );
        } catch {
          /* feed line is non-essential */
        }
      },
      onError(error) {
        console.error('fleetd network watcher error:', error);
      },
    });
  }

  let networkWatchOwner: ReturnType<typeof watchNetwork> | null = null;
  let agentsPollOwner: ReturnType<typeof startAgentsPoll> | null = null;
  let bootWork: Promise<unknown> | null = null;
  let discoveryShutdownTimedOut = false;

  async function stopMdnsOwned(): Promise<void> {
    const stopping = mdns?.stop();
    if (!stopping) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const watchdog = new Promise<void>((resolve) => {
      timer = setTimeout(() => {
        discoveryShutdownTimedOut = true;
        console.error('fleetd shutdown timed out waiting for discovery; continuing cleanup');
        resolve();
      }, 1000);
      timer.unref();
    });
    try {
      await Promise.race([stopping, watchdog]);
    } catch {
      /* discovery is never load-bearing */
    } finally {
      if (timer) clearTimeout(timer);
    }
  }

  // Register dynamic owners before bind can publish success. Their close
  // functions resolve the handle that was actually acquired (or no-op for a
  // partial startup), so shutdown never needs to mutate the aggregate after its
  // admission latch has closed.
  try {
    daemonResources.addProducer(
      'boot-reconciliation',
      observeRelease('boot-reconciliation', {
        close: async () => {
          await bootWork;
        },
      }),
    );
    daemonResources.addProducer(
      'agents-poll',
      observeRelease('agents-poll', {
        close: async () => {
          await agentsPollOwner?.stop();
        },
      }),
    );
    daemonResources.addProducer(
      'network-watch',
      observeRelease('network-watch', {
        close: async () => {
          await networkWatchOwner?.stop();
        },
      }),
    );
    daemonResources.addDiscovery(
      'mdns',
      observeRelease('mdns', {
        close: stopMdnsOwned,
      }),
    );
    if (testHooks) await acquisitionCheckpoint('background-owners');
  } catch (error) {
    await daemonResources.close();
    throw error;
  }

  try {
    signal.throwIfAborted();
    const result = await http.bind(PORT, BIND);
    if (result._tag === 'BindFailed') {
      throw new HttpBindStartupError({
        reason: result.reason,
        origin: result.origin,
        code: result.code,
        errno: result.errno,
        message:
          result.reason === 'address-in-use'
            ? 'fleetd already running (port bind lost the election)'
            : result.message,
        cause: result.error,
      });
    }
    if (testHooks) await acquisitionCheckpoint('listener');
    else signal.throwIfAborted();
  } catch (error) {
    await daemonResources.close();
    throw error;
  }

  // A successful awaitable bind is the readiness boundary for banners,
  // discovery, producers, and boot reconciliation. No startup work below can
  // race an EADDRINUSE loser or run after an interrupted partial acquisition.
  try {
    const boundHost = BIND.includes(':') && !BIND.startsWith('[') ? `[${BIND}]` : BIND;
    console.log(`fleetd up on http://${boundHost}:${PORT} (pid ${process.pid}, db ${DB_FILE})`);
    if (!LAN_MODE) {
      // 0.16.0: the token always exists, and the local board needs it for the
      // gated powers (/ws/term typing, /mail, gateway settings, the unsupervised
      // arm). Print the credentialed LOCAL URL — loopback-only, and fleetd.log
      // is chmod 0600 by the launcher for exactly this class of secret.
      console.log(`fleetd board http://127.0.0.1:${PORT}/?t=${encodeURIComponent(AUTH_TOKEN)}`);
    }
    // TEST-SEAM ANNOUNCEMENT: these env vars swap a real subprocess (spawn / term
    // / the daemon script) or override identity (version) for the test suite. In
    // production they must be unset; announcing each active one at boot means a
    // leaked seam (the 2026-07-11 env scar) is visible in fleetd.log rather than
    // silently reshaping the daemon. Provenance only — the value is never logged.
    for (const seam of [
      'FLEETDECK_SPAWN_CMD',
      'FLEETDECK_TERM_CMD',
      'FLEETDECK_TEST_DAEMON_SCRIPT',
      'FLEETDECK_VERSION_OVERRIDE',
      'FLEETDECK_TEST_FAIL_PLAN_INSERT',
      // The runtime-agnostic network mock (test-seam.ts + os-net.ts): mocks
      // dgram/the interface list and tees console.log to a file. Master flag first.
      'FLEETDECK_TEST_NET_MOCK',
      'FLEETDECK_TEST_CONSOLE_RECORD',
    ]) {
      if (process.env[seam]) console.error(`fleetd WARNING: test seam ${seam} active`);
    }
    // FLEETDECK_AGENTS_CMD is a seam ONLY when it names a real command: '' and
    // 'false' are the documented DISABLE sentinels (agents-poll resolveArgv), and
    // unset is the default real CLI — none of those is an injected seam. Mirror
    // resolveArgv's exact trim-but-not-lowercase test so the two can never drift.
    const agentsCmd = process.env['FLEETDECK_AGENTS_CMD'];
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
    const replacedVersion = process.env['FLEETDECK_REPLACED'];
    if (replacedVersion) {
      console.log(`fleetd v${version} replaced v${replacedVersion} (plugin upgrade takeover)`);
      // The ticker write is best-effort: a banner must never crash post-bind
      // startup (an uncaught throw here would take the fresh daemon down).
      try {
        core.tick(`⬆️ fleetd v${version} replaced v${replacedVersion}`);
      } catch {
        /* feed line is non-essential */
      }
    }
    if (LAN_MODE) {
      // LOG CREDENTIAL CONTRACT: the real query-bearing URLs live in the board's
      // share panel. stdout is commonly redirected to fleetd.log (often 0644), so
      // it may identify the endpoint but must never become a second token store.
      // The share panel's initial LAN state comes from createHttp's `lan` getter
      // (no startup refreshLan — that would log the credentialed URL); the watcher
      // below refreshes it on the first roam.
      lastLanAddresses = lanAddresses();
      announceLanUrls(lastLanAddresses);
      // Discovery is a convenience, never a dependency: mdns.mjs degrades to a
      // no-op on EADDRINUSE (a real avahi owns 5353), EPERM or a network that
      // drops multicast. The IP URLs above always work regardless. No address at
      // boot no longer forfeits discovery: the network poll starts the responder
      // the moment the first address appears.
      if (MDNS_ENABLED && lastLanAddresses.length) {
        startMdns(lastLanAddresses);
      }
      networkWatchOwner = watchNetwork();
    }
    if (testHooks) await acquisitionCheckpoint('discovery-network');
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
    // reconcileSpawns is typed `(...args) => unknown` on the core surface (it is
    // destructured out of the untyped spawns factory); it returns a promise at
    // runtime, so assert it for the boot-readiness chain. `void` marks the chain
    // fire-and-forget exactly as the .mjs did — /health answering 200 never meant
    // the heals ran; the settle below is what flips reconciliationStatus.
    bootWork = (core.reconcileSpawns() as Promise<unknown>)
      .catch((err: unknown) => {
        console.error('fleetd spawn reconciliation error:', err);
      })
      // The boot retentionSweep kicked inside createCore is the other half of
      // the startup mutation window — `settled` means BOTH are done. Each leg
      // already carries its own .catch above, so this chain cannot reject.
      .then(() => core.bootRetention)
      // The heals' onMutate calls only SCHEDULE a coalesced broadcast; the flush
      // fires up to BROADCAST_COALESCE_MS later. Settling 'settled' the instant
      // the heals resolve would let a strict /ws client connect into that
      // trailing flush and take a broadcast it did not cause (BUG-066). Wait out
      // the pending flush before flipping the signal.
      .then(() => whenBroadcastIdle())
      .finally(() => {
        // Settling is irreversible: flip the status /health reports FIRST, then
        // resolve the (unexposed) readiness promise for in-process embedders.
        settleReconciliation?.();
        settleReconciliation = null;
      });
    agentsPollOwner = startAgentsPoll(core); // F1 secondary session source; first run shortly after bind
    if (testHooks) await acquisitionCheckpoint('pollers-boot');
  } catch (error) {
    // A synchronous post-bind producer/banner failure occurs before the Layer
    // can publish its acquireRelease finalizer. Retire the complete prefix here
    // so the listener, DB, ingress facade, host listener, and pidfile cannot leak.
    await daemonResources.close();
    throw error;
  }

  const acquired: AcquiredDaemonResources = {
    resources: daemonResources,
    readiness: bootReconciliation,
    shutdownExitCode: () =>
      discoveryShutdownTimedOut || daemonResources.closeErrors.length > 0 ? 1 : 0,
    releaseProcessAtHostExit: releaseHostProcessOwnership,
  };
  return acquired;
}

/** Acquire the complete daemon inside the already-built root Context. */
export function acquireDaemonResources(
  signal: AbortSignal,
  ingress: RootIngressSupervisorService,
  testHooks?: DaemonAcquisitionTestHooks,
): Promise<AcquiredDaemonResources> {
  return bootDaemon(signal, ingress, testHooks);
}
