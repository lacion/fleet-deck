// spawns.mjs — the v1.2/v1.3 board-spawned session lifecycle: spawn / revive /
// enableRemote / spawnKill, the ~10s owned-pane liveness tick, boot
// reconciliation, and the RC-URL harvest + bring-up nudge ephemera. Threaded
// ctx state: q, updateSession, tick, logEvent, onMutate, assignCallsign,
// notifyWatchers, tombstoneCard (the shared offline-tombstone write),
// findScopedWindow, the tmux adapter, port/home, and the NUDGE/REGISTER/
// RC-harvest knobs. spawnState.orphans (the unadopted-window list) is exposed
// on ctx for the snapshot.

import fs from 'node:fs';
import path from 'node:path';
import { randomUUID, randomBytes } from 'node:crypto';
import { deriveRepo, branchOf } from './repo-identity.ts';
import { ticketFromBranch, animalOf } from './tickets.ts';
import {
  claudeEnvArgvPrefix,
  claudeTranscriptPath,
  canonicalPathKey,
  SHELL_RE,
  NOT_RESUMABLE_END,
} from './helpers.ts';
import { errStatus, errMessage } from './errors.ts';
import { execFileP, baseBranch } from './exec.ts';
import { redactDiagnosticText, scrubUrlCredentials } from './payload-capture.ts';
import { redactGitText } from './exec.ts';
import type { ExecResult } from './exec.ts';
import type { Statements, SessionRow, SpawnRow } from './statements.ts';

// ---------------------------------------------------------- strict-typing seam
// Sibling module surfaces obtained as type queries: erased under
// verbatimModuleSyntax, so they add no runtime import and cannot form a cycle,
// yet they pin our ctx field types to exactly what those modules ship.
type SpawnModule = typeof import('./spawn.ts');
type ReposSurface = ReturnType<typeof import('./repos.ts').createRepos>;
type SettingsSurface = ReturnType<typeof import('./settings.ts').createSettings>;
type ScopedWindow = NonNullable<Awaited<ReturnType<SpawnModule['listScopedWindows']>>>[number];
type Materialized = Awaited<ReturnType<ReposSurface['materializeBranch']>>;

// The offline-tombstone options — a structural mirror of derive.ts's
// TombstoneOpts (kept in sync as a supertype so ctx.tombstoneCard stays
// assignable in both directions).
interface TombstoneOpts {
  note: string;
  at?: number;
  tickMsg?: string | null;
  notify?: boolean;
  forgetModel?: boolean;
  mutate?: boolean;
}

// The tmux adapter surface spawns actually calls — a structural slice of the
// spawn.ts module (the production default), also satisfied by derive's wired
// TmuxAdapter. Member types are the module's own exports so they cannot drift.
interface SpawnsTmuxAdapter {
  // Optional to match derive's TmuxAdapter (test adapters legitimately omit it —
  // "absent" means "no override wired"); every call site below `?.`-probes it.
  spawnOverrideCmd?: SpawnModule['spawnOverrideCmd'];
  hasTmux: SpawnModule['hasTmux'];
  tmuxCapability?: SpawnModule['tmuxCapability'];
  fleetServerAbsent?: SpawnModule['fleetServerAbsent'];
  capturePane: SpawnModule['capturePane'];
  sendBringupEnter: SpawnModule['sendBringupEnter'];
  killWindowVerified: SpawnModule['killWindowVerified'];
  launchOverride: SpawnModule['launchOverride'];
  ensureSession: SpawnModule['ensureSession'];
  newWindow: SpawnModule['newWindow'];
  sessionName: SpawnModule['sessionName'];
  windowName: SpawnModule['windowName'];
  typeAndEnter: SpawnModule['typeAndEnter'];
  listScopedWindows: SpawnModule['listScopedWindows'];
  paneCurrentCommand: SpawnModule['paneCurrentCommand'];
}

// The consumer view of the threaded closure state. derive assembles the literal
// and casts it `as unknown as CoreCtx`, so the ONLY assignability check is
// CoreCtx <: SpawnsCtx. Function fields use ARROW-PROPERTY syntax (not method
// shorthand) so createSpawns can destructure them without tripping unbound-method
// — but that means their parameters are checked CONTRAVARIANTLY under
// strictFunctionTypes, NOT bivariantly. So a field that mirrors a repos.ts/
// settings.ts provider is pinned to the provider signature via `ProviderSurface['X']`
// (params AND return) rather than being left loose; a looser `unknown` param would
// make the real narrower provider NON-assignable. The hand-typed closure fields
// (tick, logEvent, …, wired by derive, none reading `this`) stay typed to how
// spawns uses them, at or wider than what CoreCtx provides.
interface SpawnsCtx {
  q: Statements['q'];
  updateSession: Statements['updateSession'];
  port: number;
  home: string;
  NUDGE_MS: number;
  SPAWN_REGISTER_MS: number;
  SETUP_REGISTER_MS: number;
  RC_HARVEST_MS: number;
  ADOPT_ARM_MS: number;
  CLEAR_SUCCESSION_MS: number;
  tmuxAdapter: SpawnsTmuxAdapter;
  // Arrow-property (not method-shorthand) syntax throughout: every member is a
  // plain closure wired by derive's CoreCtx (none reads `this`), and destructuring
  // them in createSpawns would trip unbound-method under method-shorthand. Matches
  // derive.ts's CoreCtx idiom.
  tick: (msg: string) => void;
  logEvent: (sid: string, hookEvent: string, toolName?: unknown, note?: unknown) => void;
  onMutate: () => void;
  notifyWatchers: (sid: string) => void;
  assignCallsign: (sid: string, ticket?: string | null) => string;
  succeedSession: (prev: SessionRow, sid: string, opts?: { rename?: boolean }) => string | null;
  hasLivePane: (sid: string) => boolean;
  tombstoneCard: (sid: string, opts: TombstoneOpts) => void;
  findScopedWindow: (name: string | null) => Promise<ScopedWindow | null | undefined>;
  scopedPaneTarget: (win: { window: string; window_id: string }) => string;
  // Both are `&&`-guarded at their revive call sites: spawns tolerates a ctx
  // that omits them (standalone cores without the keyed mutex), so the honest
  // contract is optional. derive's real CoreCtx wires both unconditionally.
  claimWorktreeCustody?: (p: string, owner: string) => (() => void) | null;
  acquireWorktreePathLock?: (key: string) => Promise<() => void>;
  // These mirror repos.ts providers 1:1, pinned to the provider signature via
  // `ReposSurface['X']` — params AND return, not just the return. Under
  // strictFunctionTypes these arrow-PROPERTY fields (arrow, not method-shorthand,
  // so destructuring them in createSpawns doesn't trip unbound-method) check their
  // params CONTRAVARIANTLY, so the earlier `(x: unknown) => ReturnType<…>` shape
  // REJECTED the real narrower providers (resolveTarget's `ResolveTargetBody`,
  // validateBranch's `string`, …). That looser shape only ever passed because a
  // stale `./spawns.mjs` import upstream left createSpawns typed `any`; once the
  // import resolved to `./spawns.ts`, CoreCtx <: SpawnsCtx started being checked and
  // the mismatch surfaced. Pinning to the provider makes the two exact; the call
  // sites below already pass matching shapes (strings, object literals, and a
  // repo-guarded body). See ts-migration-bugs.md.
  validateBranch: ReposSurface['validateBranch'];
  resolveTarget: ReposSurface['resolveTarget'];
  cloneRepo: ReposSurface['cloneRepo'];
  materializeBranch: ReposSurface['materializeBranch'];
  touchRepo: ReposSurface['touchRepo'];
  claimTarget: ReposSurface['claimTarget'];
  targetOwner: ReposSurface['targetOwner'];
  reserveCloneSlot: () => ReturnType<ReposSurface['reserveCloneSlot']>;
  persistRepoTransport: (v: unknown) => ReturnType<SettingsSurface['persistRepoTransport']>;
  persistRepoDefaultOrg: (v: unknown) => ReturnType<SettingsSurface['persistRepoDefaultOrg']>;
  validateRepoDefaultOrg: (v: unknown) => ReturnType<ReposSurface['validateRepoDefaultOrg']>;
  resolveGateway: () => ReturnType<SettingsSurface['resolveGateway']>;
  resolveGatewayEnv: () => ReturnType<SettingsSurface['resolveGatewayEnv']>;
  // questions is read off ctx directly (never destructured) and only for its
  // dismiss relay; typed loosely so the union-returning real dismiss stays
  // assignable and `.already` (absent on one arm) reads cleanly.
  questions?: {
    dismiss?(
      id: number,
      opts?: { activity?: boolean },
    ): { ok?: boolean; already?: boolean } | null | undefined;
  };
}

// The POST /api/spawn body: every field optional and validated at the top of
// spawn(). The index signature backs the string-validation and shell-forbidden
// loops that read `body[k]` by computed key; the explicit fields carry the
// precise types the rest of spawn() relies on.
interface SpawnBody {
  [key: string]: unknown;
  kind?: string;
  cwd?: string;
  repo?: string;
  branch?: string;
  branch_mode?: string;
  prompt?: string;
  model?: string;
  permission_mode?: string;
  repo_host?: string;
  repo_transport?: string;
  repo_org?: string;
  worktree?: boolean;
  dangerously_skip_permissions?: boolean;
  remote_control?: boolean;
  gateway?: boolean;
  disarm?: boolean;
  arm_token?: string;
  setup_cmd?: string;
  plan_id?: unknown;
}

function runtimeOverrideRefusal(body: SpawnBody): string | null {
  if (body.remote_control != null && typeof body.remote_control !== 'boolean') {
    return 'remote_control must be a boolean';
  }
  if (body.gateway != null && typeof body.gateway !== 'boolean') {
    return 'gateway must be a boolean';
  }
  if (body.arm_token != null && typeof body.arm_token !== 'string') {
    return 'arm_token must be a string';
  }
  return null;
}

interface CreateCardOverrides {
  deriveRepo?: boolean;
  repo_id?: string | null;
  repo_name?: string | null;
  worktree?: string | null;
  branch?: string;
  col?: string;
  note?: string;
  source?: string;
}

interface PlanClaim {
  plan_id: number;
  restoreStatus: string;
  via: string;
}

interface GatewayDecision {
  use: boolean;
  env: Record<string, string> | null;
  error?: string;
}

interface SpawnCompensateArgs {
  spawn_id: string;
  session_id: string;
  callsign: string;
  cwd: string;
  // Callers thread these straight from optional launch args (a `string | null`
  // that may also be absent), and the body treats undefined exactly like a
  // falsy path/window — so accept explicit undefined under exactOptionalPropertyTypes.
  worktree_path?: string | null | undefined;
  tmux_window?: string | null | undefined;
  reason: string;
  created?: { clone: boolean; worktree: boolean };
}

interface LaunchPaneArgs {
  spawn_id: string;
  session_id: string;
  callsign: string;
  tmux_session: string;
  tmux_window: string;
  requestedCwd: string;
  runCwd: string;
  cleanupRoot: string;
  worktree_path?: string | null;
  body: SpawnBody;
  skipPermissions: boolean;
  gatewayEnv?: Record<string, string> | null;
  created?: { clone: boolean; worktree: boolean };
}

// The shared resume-launch options. callsign/tmux_window/requested_cwd arrive
// from a SELECT-* spawn row (nullable in the schema, never null for a real
// provisioned row); launchResume proves the two identity fields non-null with
// an honest internal throw before the pane is built.
interface LaunchResumeArgs {
  session_id: string;
  callsign: string | null;
  tmux_window: string | null;
  runCwd: string;
  requested_cwd: string | null;
  worktree_path?: string | null;
  skip_permissions: boolean;
  remoteWanted: boolean;
  // adopt has no prior row to exclude and passes null; revive passes its own
  // spawn_id. The only reader compares with !== so null is a benign "no self".
  excludeSpawnId: string | null;
  overrideExtra?: Record<string, unknown>;
  note: string;
  tickMsg: string;
  failReason: string;
  bodyExtra?: Record<string, unknown>;
  gatewayEnv?: Record<string, string> | null;
}

export const SETUP_WRAPPER = [
  'cmd=$FLEETDECK_SETUP_CMD; unset FLEETDECK_SETUP_CMD',
  'printf \'▶ fleetdeck setup: %s\\n\' "$cmd"',
  'sh -c "$cmd"; rc=$?',
  'if [ "$rc" -ne 0 ]; then printf \'✗ setup failed (exit %s) — claude not started\\n\' "$rc"; exit "$rc"; fi',
  'exec "$@"',
].join('\n');

const SETUP_CMD_MAX = 2000;
// eslint-disable-next-line no-control-regex -- shell-injection gate: setup_cmd must carry no NUL/control bytes (newline excepted)
const SETUP_CONTROL_RE = /[\x00-\x09\x0b-\x1f\x7f]/;
const STALL_DETAIL_MAX = 2000;
const STALL_DETAIL_LINES = 18;

// The bound on a classified spawn-failure reason: one glance, one log line, one
// 500 body. fail_detail owns the long form; this is the verdict register.
const SPAWN_REASON_MAX = 300;

// A thrown spawn failure becomes the reason string a 500 body, a tombstone
// note and a durable SpawnFailed event all derive from — so it goes through
// the SAME hardening pass as git's own stderr before it goes anywhere
// (redactGitText: URL-userinfo scrub first, then the credential shapes, then
// the caller's exact needles). An Error message is not git output, but a
// failed materialization's message routinely IS git stderr (materializeBranch
// throws it verbatim), and a SQLite/IO error can carry a filesystem path that
// embeds a sensitive directory name. A message is not a stack trace: anything
// reaching here as Error stays message-only, so `internal at …` frames can
// never ride this field. Null/empty collapses to a named fallback — a bare
// 'internal' was exactly the dead-end UX 2.3 exists to kill.
export function spawnFailureReason(err: unknown, fallback = 'internal error') {
  // err is a caught throwable of unknown shape; keep the coalesced value typed
  // `unknown` so `String()` is the sanctioned coercion (errMessage does the same)
  // rather than stringifying a `{}` that would trip no-base-to-string.
  const source: unknown = (err as { message?: unknown } | null)?.message ?? err ?? '';
  const raw = String(source).trim();
  if (!raw) return fallback;
  // One line: a note and a log line are single-line registers.
  const oneLine = redactGitText(raw.replace(/\r/g, '')).replace(/\n+/g, ' ').trim();
  return oneLine.length > SPAWN_REASON_MAX ? oneLine.slice(0, SPAWN_REASON_MAX) : oneLine;
}

// `tmux capture-pane -p` is already rendered plain text (no -e escapes), but
// strip any controls defensively, trim empty screen margins, keep only the tail
// a human would see, redact known credential shapes, then bound the final value
// before it enters SQLite and the /state broadcast.
//
// scrubUrlCredentials rides here for the same reason it rides gitStderrDetail,
// and the omission was not cosmetic: the captured pane of a STALLED spawn very
// often still has the command that stalled it on screen, and for a repo-mode
// spawn that command is `git clone https://user:token@host/x.git`. A shape-only
// scrub provably cannot see that — no entry in SECRET_VALUE_RES matches a GitLab
// PAT or a corporate password (tests/payload-redaction.test.mjs pins that
// absence deliberately) — and stall_detail reaches exactly the same surfaces as
// fail_detail: the drawer <pre>, /state, every /ws frame, and a durable
// SpawnStalled event note. Two sibling excerpts must not disagree about the same
// control. The control class is left as-is here on purpose: a tmux-rendered
// capture has no tabs to preserve and no C1 bytes to strip.
export function stallDiagnosticExcerpt(
  screen: unknown,
  { secrets = [] }: { secrets?: string[] } = {},
): string | null {
  if (typeof screen !== 'string' || !screen) return null;
  const lines = screen
    .replace(/\r/g, '')
    // eslint-disable-next-line no-control-regex -- defensively strip control bytes from tmux-captured scrollback before it enters SQLite/broadcast
    .replace(/[\x00-\x09\x0b-\x1f\x7f]/g, '')
    .split('\n')
    .map((line) => line.replace(/\s+$/g, ''));
  while (lines.length && !lines[0]) lines.shift();
  while (lines.length && !lines[lines.length - 1]) lines.pop();
  if (!lines.length) return null;
  let tail = redactDiagnosticText(scrubUrlCredentials(lines.slice(-STALL_DETAIL_LINES).join('\n')));
  for (const secret of secrets) {
    if (typeof secret === 'string' && secret) tail = tail.split(secret).join('[redacted]');
  }
  // Bytes, not JS code units: a screen of emoji/CJK must still respect the 2KB
  // SQLite/snapshot budget. Start from the byte tail, drop a partial leading
  // UTF-8 character (U+FFFD), then correct any replacement expansion.
  const bytes = Buffer.from(tail);
  if (bytes.length > STALL_DETAIL_MAX) {
    tail = bytes
      .subarray(bytes.length - STALL_DETAIL_MAX)
      .toString('utf8')
      .replace(/^�+/, '');
    while (Buffer.byteLength(tail) > STALL_DETAIL_MAX) tail = tail.slice(1);
  }
  return tail || null;
}

export function createSpawns(ctx: SpawnsCtx) {
  const {
    q,
    updateSession,
    tick,
    logEvent,
    onMutate,
    assignCallsign,
    notifyWatchers,
    tombstoneCard,
    findScopedWindow,
    scopedPaneTarget,
    tmuxAdapter,
    port,
    home,
    NUDGE_MS,
    SPAWN_REGISTER_MS,
    SETUP_REGISTER_MS,
    RC_HARVEST_MS,
    ADOPT_ARM_MS, // 0.7.0 Move-to-tmux: arm-deadline window (default 30 min)
    // 0.7.1: the boot heal for /clear forks stranded BEFORE succession shipped.
    succeedSession,
    CLEAR_SUCCESSION_MS,
    hasLivePane,
    validateBranch,
    resolveTarget,
    cloneRepo,
    materializeBranch,
    touchRepo,
    claimTarget,
    targetOwner,
    reserveCloneSlot,
    persistRepoTransport,
    persistRepoDefaultOrg,
    validateRepoDefaultOrg,
    resolveGateway,
    resolveGatewayEnv,
    acquireWorktreePathLock,
    claimWorktreeCustody, // remove-vs-revive serialization (revive side; derive wires it)
  } = ctx;

  // ------------------------------------------- unsupervised arm gate (0.16.0)
  // The board's red two-step is UI; the API needs its own proof of intent, or
  // any loopback process (a fleet-spawned agent included) can curl itself an
  // unsupervised sibling. POST /api/spawn/arm-unsupervised (token-gated in
  // http.mjs) mints a single-use capability; a spawn/adopt body carrying
  // dangerously_skip_permissions:true or permission_mode:'bypassPermissions'
  // must echo a fresh one as arm_token. Single-use, short-TTL, in-memory — a
  // daemon restart simply forces a re-arm.
  const ARM_TTL_MS = 60_000;
  const armTokens = new Map<string, number>(); // token -> expires_at
  function armUnsupervised() {
    const token = randomBytes(24).toString('base64url');
    armTokens.set(token, Date.now() + ARM_TTL_MS);
    if (armTokens.size > 128) {
      const now = Date.now();
      for (const [t, exp] of armTokens) {
        if (exp <= now) armTokens.delete(t);
      }
    }
    return token;
  }
  function consumeArm(token: unknown) {
    if (typeof token !== 'string' || !armTokens.has(token)) return false;
    const exp = armTokens.get(token);
    armTokens.delete(token);
    return (exp ?? 0) > Date.now();
  }
  function unsupervisedGate(skipPermissions: boolean, body: SpawnBody) {
    if (!skipPermissions) return null;
    if (consumeArm(body.arm_token)) return null;
    return "unsupervised spawns require a fresh arm token from POST /api/spawn/arm-unsupervised — the API half of the board's two-step confirmation";
  }

  // ------------------------------------------------- LLM gateway (0.15.0)
  // Does THIS launch route through the configured gateway, and with what
  // environment? One resolver, shared by fresh spawns and revives, so a revived
  // pane can never disagree with the spawn it descends from about who is being
  // billed.
  //
  // Precedence: an explicit `gateway` on the request wins; silence falls back to
  // the `gateway_default` setting. Both failure modes are LOUD (400) rather than
  // a quiet fall-through to Anthropic — the entire point of this feature is that
  // which provider serves a session is never a surprise, and "your gateway was
  // half-configured so we billed your Anthropic account for six hours instead"
  // is exactly the surprise it exists to prevent. This mirrors browse_root's
  // rule in files.mjs: a CONFIGURED root that has gone missing fails loud, it
  // does not silently become home.
  // `source` names WHERE the request came from, because the caller of a revive
  // did not choose anything — the flag was inherited from the row — and an error
  // that says "gateway:true was requested" when nobody requested it sends the
  // human looking for a bug in their own call. Each source gets the remedy that
  // actually applies to it.
  function gatewayDecision(wanted: unknown, source = 'request'): GatewayDecision {
    const profile = resolveGateway();
    const asked = wanted === true || (wanted == null && profile.default);
    if (!asked) return { use: false, env: null };
    if (!profile.ready) {
      const missing = !profile.base_url
        ? 'gateway_base_url is not set'
        : 'gateway_token is not set';
      const why =
        source === 'inherited'
          ? `this session was spawned through the gateway, but the gateway is no longer configured — ${missing}. Restore it, or revive with {"gateway":false} to resume this conversation against Anthropic instead`
          : wanted === true
            ? `gateway:true was requested but the gateway is not configured — ${missing}`
            : `gateway_default is on but the gateway is not configured — ${missing}`;
      return { use: false, env: null, error: why };
    }
    return { use: true, env: resolveGatewayEnv() };
  }

  // Remote control and the gateway are mutually exclusive, and not by our
  // choice: Claude Code itself disables Remote Control whenever
  // ANTHROPIC_BASE_URL points at a non-Anthropic host, because the claude.ai
  // relay has no route to a session it isn't serving. Accepting both would hand
  // back a spawn whose 📱 link never appears and whose failure has no visible
  // cause — so refuse at the door, where the reason can be stated.
  function gatewayRemoteConflict(gatewayUse: boolean, remoteWanted: boolean): string | null {
    if (!gatewayUse || !remoteWanted) return null;
    return 'remote control is unavailable on a gateway-routed session — Claude Code disables it whenever ANTHROPIC_BASE_URL points at a non-Anthropic host. Spawn with gateway:false to use remote control, or remote_control:false to use the gateway.';
  }

  // ------------------------------------- v1.2 board-spawned sessions (spawns)
  // CONTRACT "v1.2 — dynamic fleet". Spawn = explicit human click ONLY; the
  // spawned command is interactive `claude` behind a deterministic `env`
  // argv wrapper; no auto-respawn, ever. All tmux construction is argv arrays.
  //
  // 0.7.0 Move-to-tmux (adopt) does NOT break "explicit human click ONLY" even
  // in its armed, fires-later form: the click that ARMED the card WAS the human
  // decision. The deferred auto-adopt on the session's own SessionEnd is that
  // single click cashing in — one-shot (the arm is durable intent, CONSUMED by
  // adoptSession inside its single-flight claim; a failed launch never
  // retries, and a deferred call that finds the arm gone or the session live
  // again stands down instead of acting), cancellable (disarm any time,
  // including inside the post-SessionEnd grace window, or it self-expires
  // after FLEETDECK_ADOPT_ARM_MS), visible (the armed state + deadline ride
  // the snapshot's per-session `adopt` object), and expiring (a deadline, not
  // a standing order). The retention sweep completes an armed move orphaned by
  // a daemon death inside the grace window — still the same single click. It
  // is not auto-respawn: nothing the daemon decides on its own ever launches a
  // pane — a human clicked, once.
  //
  // Row state machine:
  //   'spawning'  insert at POST /api/spawn
  //     → 'stalled'    registration deadline passed while claude pane lives
  //     → 'live'       first hook event for the pre-issued session_id
  //                    (the source flip in applyEvent above)
  //     → 'pane-dead'  liveness tick saw #{pane_dead}=1 or a bare shell
  //                    (crash, no SessionEnd), OR SessionEnd landed
  //                    (hookSessionEnd above — graceful end, pane kept)
  //     → 'gone'       boot reconciliation: window (exact scoped name) missing
  //     → 'killed'     POST /api/spawn/:id/kill succeeded (name-verified)
  //   'stalled' → live (late hook) | pane-dead | gone | killed
  //   'live' → pane-dead | gone | killed (same triggers)
  //   'pane-dead' → killed (the dead pane's window still exists until killed)
  //               → gone   (kill found the window already gone: 410)
  //   'pane-dead' / 'killed' / 'gone' are terminal. A stalled live pane still
  //   counts against the cap; the daemon never auto-kills or auto-respawns it.

  // Capability flag (/health and /state). available=false when tmux is absent
  // or FLEETDECK_SPAWN=off; the FLEETDECK_SPAWN_CMD override reports
  // available:true with reason 'test-override'. The board hides ALL spawn UI
  // when available is false.
  //
  // `active` is a COUNT, not a budget: there is no cap on how many agents may
  // be live at once. The board shows it so you know how big your fleet is, and
  // the spawn form makes you look at exactly what you are about to launch —
  // that is the guardrail, not a refusal from the daemon.
  function spawnCapability():
    | { available: false; reason: string; active: number }
    | { available: true; reason?: string; active: number } {
    const base = { active: q.countActiveSpawns.get()?.n ?? 0 };
    if ((process.env['FLEETDECK_SPAWN'] ?? '').toLowerCase() === 'off') {
      return { available: false, reason: 'disabled (FLEETDECK_SPAWN=off)', ...base };
    }
    if (tmuxAdapter.spawnOverrideCmd?.()) {
      return { available: true, reason: 'test-override', ...base };
    }
    if (!tmuxAdapter.hasTmux()) {
      const reason = tmuxAdapter.tmuxCapability?.().reason ?? 'tmux 3.4+ unavailable';
      return { available: false, reason, ...base };
    }
    return { available: true, ...base };
  }

  // CONTRACT flow step 1: pre-create the card so the callsign exists before
  // the pane does. Note is EXACTLY "spawning…"; col queued; source 'spawned'.
  function createSpawnedCard(
    sid: string,
    cwd: string,
    prompt: string | null | undefined,
    overrides: CreateCardOverrides | null = null,
  ): SessionRow {
    const derived: {
      repo_id?: string | null;
      repo_name?: string | null;
      worktree?: string | null;
    } = !overrides || overrides.deriveRepo === true ? deriveRepo(cwd) : {};
    const repo = {
      repo_id: overrides?.repo_id ?? derived.repo_id ?? null,
      repo_name: overrides?.repo_name ?? derived.repo_name ?? null,
      worktree: overrides?.worktree ?? derived.worktree ?? null,
    };
    // Ticket inheritance is a naming moment: derive the ticket from the SOURCE
    // cwd's branch (fresh — bypass the 20s cache) BEFORE naming, so a spawn off a
    // ticket branch is born ticket-first exactly like a hook/agents birth. The
    // whole chain here runs in spawn()'s synchronous pre-await prefix, preserving
    // the naming-collision invariant.
    const branch = overrides?.branch ?? (cwd ? branchOf(cwd, { fresh: true }) : null);
    const ticket = ticketFromBranch(branch);
    const callsign = assignCallsign(sid, ticket);
    const now = Date.now();
    const col = overrides?.col ?? 'queued';
    const note = overrides?.note ?? 'spawning…';
    const source = overrides?.source ?? 'spawned';
    q.insertSpawnedSession.run(
      sid,
      callsign,
      cwd,
      repo.repo_id ?? null,
      repo.repo_name ?? null,
      branch ?? null,
      repo.worktree ?? null,
      col,
      note,
      prompt ? prompt.slice(0, 80) : null,
      now,
      now,
      source,
    );
    if (ticket) updateSession(sid, { ticket, ticket_source: 'branch' });
    const created = q.getSession.get(sid);
    if (!created) throw new Error('spawned session vanished immediately after insert');
    return created;
  }

  function spawnFailed(sid: string, callsign: string, reason: string) {
    // D6: keep the FULL reason durable in the events table (SpawnStalled
    // precedent) even though the card note is clamped — the 80-char clamp that
    // used to be the ONLY record is why a private-repo clone failure could not
    // be diagnosed without re-running it by hand. cloneRepo has already
    // console.error'd git's full stderr to fleetd.log; this is the queryable
    // audit trail. Clamps widened: note 80→200, ticker 60→120 (a distilled
    // `fatal:` line needs the room).
    logEvent(sid, 'SpawnFailed', null, reason.slice(0, 2000));
    // D8: this tombstone also wakes watchers (via tombstoneCard's default),
    // consistent with every other terminal transition — a failed spawn is a
    // card that just went offline, and a watch long-poll should learn that now
    // rather than at its timeout. forgetModel drops the transcript memo (M-G2:
    // terminal — a revive re-stamps the floor).
    tombstoneCard(sid, {
      note: `spawn failed: ${reason}`.slice(0, 200),
      tickMsg: `✗ spawn failed for ${callsign}: ${reason.slice(0, 120)}`,
      forgetModel: true,
      mutate: true,
    });
  }

  // Bring-up nudge — one of four sanctioned pane injections (the others are
  // owned-pane mail and verbatim human typing relayed by the live-terminal
  // modal, plus explicit human-requested /rc enablement): if no hook lands
  // within NUDGE_MS and the pane is alive, send ONE Enter for a bring-up
  // dialog. Never again for that spawn.
  //
  // 0.16.0 TRUST GATE: the nudge used to press Enter through WHATEVER was on
  // screen — including Claude Code's folder-trust and MCP-approval dialogs,
  // which are the human checkpoints standing between a freshly cloned repo's
  // .claude/settings.json hooks / .mcp.json servers and the user's credentials.
  // A daemon that auto-answers them converts "untrusted repo" into "trusted"
  // with no human in the loop. So the nudge now READS the pane first: a
  // trust/MCP dialog is never auto-answered — the card says so and waits for a
  // human (terminal modal or the pane itself). Anything else keeps the
  // historical one-Enter bring-up.
  const TRUST_DIALOG_RE =
    /do you trust the files in this folder|trust this folder|trust the files|trust this workspace|quick safety check|new mcp server|mcp server.{0,40}(approve|allow|trust)|use this and all future mcp servers/i;
  const nudged = new Set<string>();
  function scheduleNudge(spawn_id: string, window: string, callsign: string) {
    // async work fired via a void wrapper below so setTimeout gets a `() => void`
    // (no-misused-promises); the body is fully try/caught so nudge never rejects.
    const nudge = async () => {
      try {
        if (nudged.has(spawn_id)) return;
        const row = q.getSpawn.get(spawn_id);
        if (row?.status !== 'spawning') return; // gone, or hook event landed / terminal
        const win = await findScopedWindow(window);
        if (win === null) return; // lookup UNKNOWN → hold the keystroke
        if (!win || win.pane_dead) return; // pane not alive → no keystroke
        nudged.add(spawn_id);
        let screen: string | null = null;
        const target = scopedPaneTarget(win);
        try {
          screen = await tmuxAdapter.capturePane(target);
        } catch {
          /* fall through to the hold below */
        }
        // Fail CLOSED (0.16.0 adversarial review): an unreadable pane is never
        // a safe pane to press Enter into — the pre-gate behavior was exactly
        // "press and hope", and a mid-redraw trust dialog reads as an empty
        // screen. Hold instead; the human's own Enter always works.
        if (typeof screen !== 'string' || screen.trim() === '') {
          logEvent(row.session_id, 'SpawnNudge', null, 'pane unreadable — bring-up Enter held');
          updateSession(row.session_id, {
            note: 'no bring-up keystroke sent — pane unreadable; check the terminal',
          });
          tick(`🔒 ${callsign} needs a look — no bring-up keystroke sent`);
          onMutate();
          return;
        }
        // Whitespace-insensitive: the CLI's dialogs are centered/wrapped, so
        // phrases can be split by arbitrary spacing on narrow panes.
        const squashed = screen.replace(/\s+/g, ' ');
        if (TRUST_DIALOG_RE.test(squashed)) {
          logEvent(row.session_id, 'SpawnNudge', null, 'trust/MCP dialog held for human approval');
          updateSession(row.session_id, {
            note: 'waiting on the folder-trust dialog — approve it in the terminal',
          });
          tick(`🔒 ${callsign} waits on a trust dialog — approve it in the terminal`);
          onMutate();
          return;
        }
        await tmuxAdapter.sendBringupEnter(target);
        logEvent(row.session_id, 'SpawnNudge', null, 'bring-up Enter sent');
        tick(`⏎ nudged ${callsign} through bring-up`);
        onMutate();
      } catch {
        /* nudge is best-effort; never disturb the daemon */
      }
    };
    const t = setTimeout(() => void nudge(), NUDGE_MS);
    t.unref();
  }

  const RC_URL_RE = /https:\/\/claude\.ai\/\S+/;
  const registrationRemoteHarvests = new Map<string, Promise<{ url: string | null }>>();

  // HIGH (revive single-flight): the SYNCHRONOUS claim that makes revive()
  // single-flight per session. revive() crosses async awaits (window
  // inspection, kill) before it inserts its provisional owner row, so two
  // near-simultaneous revives for one session could BOTH pass the DB guard and
  // BOTH launch a pane. A revive checks-and-adds its session id here with NO
  // await in between, so the second revive is refused (409) until the first
  // settles (the try/finally in revive always releases it). Scoped to this
  // core; a restart naturally clears it (a mid-flight revive is a provisioning
  // row that boot reconciliation settles).
  const revivingSessions = new Set<string>();

  // BUG-053: per-pane serialization for daemon control input into an owned
  // TUI (the enableRemote `/rc` type+Enter). Two concurrent /rc requests for
  // the same pane could otherwise interleave their keystrokes; a promise chain
  // keyed by pane target queues the second behind the first. Settled entries
  // are deleted, so the map cannot grow unboundedly.
  const rcInputLocks = new Map<string, Promise<unknown>>();

  // BUG 3 (condemn hysteresis): consecutive dead-read counter per spawn, keyed
  // by spawn_id. The liveness tick requires CONDEMN_DEAD_READS consecutive dead
  // reads before it flips a LIVE spawn 'pane-dead', so a single transient dead
  // read (a momentary tmux glitch / split-pane flap) cannot condemn a card the
  // resurrect loop would only flip back next tick. Cleared on any live read and
  // on every terminal transition (forgetSpawn), so it never carries stale.
  const condemnStreak = new Map<string, number>();

  // M-G2: per-spawn in-memory ephemera (the bring-up nudge dedupe set and the
  // registration remote-harvest promise) used to be cleared on no terminal
  // path at all, leaking one entry per spawn for the daemon's life. Drop them
  // on EVERY terminal spawn transition (kill / gone / pane-dead / spawn-fail /
  // reconciliation). modelMemo is keyed by session, not spawn, and is cleared
  // alongside session-terminal transitions. condemnStreak (BUG 3) rides along:
  // a spawn that just went terminal has no live streak to remember.
  function forgetSpawn(spawn_id: string) {
    nudged.delete(spawn_id);
    registrationRemoteHarvests.delete(spawn_id);
    condemnStreak.delete(spawn_id);
  }

  // BUG 3: restore a spawn that tmux just proved is a live claude after it was
  // wrongly condemned to 'pane-dead'/'gone' (a /clear tombstone, a 3h-silence
  // presume-dead, a transient tmux misread). Flip the row back 'live' and lift
  // the card off the offline shelf — clear ended_at/archived_at and re-derive a
  // live lane — so the board shows the terminal again. 'idle' reflects a live
  // claude sitting at its prompt; the next real hook refines col/note. Shared
  // by the liveness tick (automatic, next poll) and revive() (human-driven,
  // immediate). The caller has already confirmed pane_dead=0 and
  // pane_current_command='claude' AND that no newer row owns the window.
  //
  // BUG-152: those confirmations happen BEFORE an await (paneCurrentCommand),
  // so the row they describe can be stale — a human kill landing during the
  // probe flips the row 'killed', and an unconditional write here would undo
  // the kill and lift the card back onto the board on a dead window. The
  // status write is therefore a compare-and-set limited to 'pane-dead'/'gone',
  // and the card is only touched when that guarded write won.
  function resurrectSpawn(row: SpawnRow) {
    if (!q.setSpawnResurrected.run(row.spawn_id).changes) return false;
    const shell = row.kind === 'shell';
    // Same reason as the first-hook promotion in events.mjs: a row that is live
    // again has no failure to explain, and leaving the excerpt behind means a
    // later terminal transition re-exposes a stale clone failure on a card whose
    // spawn worked. The snapshot's status gate hides it while 'live'; this is what
    // stops it coming back.
    if (row.fail_detail) q.setSpawnFailDetail.run(null, row.spawn_id);
    const c = q.getSession.get(row.session_id);
    updateSession(row.session_id, {
      col: 'idle',
      ended_at: null,
      archived_at: null,
      // BUG 4: a card condemned while it was 'spawn_stalled'/'permission_prompt'
      // must not come back wearing a stale needs-you reason chip — clear
      // notification_type. And a resurrected card IS being seen right now, so
      // refresh last_seen: it is 'idle' at its prompt, not silent since death
      // (otherwise the very next retention sweep could presume it dead again on
      // the pre-death last_seen).
      notification_type: null,
      last_seen: Date.now(),
      note: shell ? 'shell' : 'pane is a live claude — restored to the board',
    });
    forgetSpawn(row.spawn_id); // drop any stale nudge/harvest ephemera from the death (BUG 3: also clears condemnStreak)
    tick(`✨ ${c?.callsign ?? row.callsign} restored — its pane was live all along`);
    notifyWatchers(row.session_id);
    onMutate();
    return true;
  }

  async function harvestRemote(spawn_id: string): Promise<{ url: string | null }> {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { url: null };
    const win = await findScopedWindow(row.tmux_window);
    if (!win || win.pane_dead) return { url: null };
    let text: string | null = null;
    try {
      text = await tmuxAdapter.capturePane(scopedPaneTarget(win));
    } catch {
      /* best effort */
    }
    // \S+ swallows terminal junk abutting the URL (trailing quotes, brackets,
    // sentence punctuation) — trim the characters a URL never ends with.
    const url =
      typeof text === 'string'
        ? (RC_URL_RE.exec(text)?.[0]?.replace(/[)\]}"'`.,;:]+$/, '') ?? null)
        : null;
    // M-B8: setSpawnRemote/tick/onMutate can throw (SQLITE_BUSY/IOERR, or a bug
    // in a broadcast handler). Harvest runs from an unref timer / detached
    // promise, so an escaping throw becomes an unhandled rejection. fleetd.mjs
    // now installs a global process.on('unhandledRejection') handler that logs
    // rather than crashing, but we do not lean on it: contain the throw HERE so
    // a failed harvest just leaves remote_url unset, which the next /rc attempt
    // or registration hook can retry.
    try {
      q.setSpawnRemote.run(url, spawn_id);
      // Live callsign, not the frozen spawn row: a manual re-ticket renames the
      // session but never the immutable spawn.callsign (like :180/:764).
      const c = q.getSession.get(row.session_id);
      tick(
        `📱 ${c?.callsign ?? row.callsign} remote control enabled${url ? '' : ' (URL not found)'}`,
      );
      onMutate();
    } catch (err) {
      console.error('fleetd remote harvest persist error:', err);
    }
    return { url };
  }

  // CONTRACT: born-remote sessions expose their URL only in TUI scrollback.
  // The first hook schedules exactly one delayed capture, after the status
  // panel has had time to render. The unref timer never keeps fleetd alive.
  function delayedRemoteHarvest(spawn_id: string): Promise<{ url: string | null }> {
    // A zero test knob means "capture on the next microtask"; production's
    // non-zero delay always uses the required unref timer.
    // M-B8: the timer path already funnels rejections into resolve({url:null});
    // the zero path returned a bare promise whose rejection nothing observed.
    // harvestRemote is now internally guarded, but keep the .catch() as the
    // documented belt-and-suspenders against an unhandled rejection.
    if (RC_HARVEST_MS === 0) {
      return Promise.resolve()
        .then(() => harvestRemote(spawn_id))
        .catch(() => ({ url: null }));
    }
    let timer: ReturnType<typeof setTimeout>;
    const promise = new Promise<{ url: string | null }>((resolve) => {
      timer = setTimeout(
        () =>
          void harvestRemote(spawn_id).then(resolve, () => {
            resolve({ url: null });
          }),
        RC_HARVEST_MS,
      );
      timer.unref();
    });
    return promise;
  }

  function scheduleRegistrationRemoteHarvest(spawn_id: string) {
    if (registrationRemoteHarvests.has(spawn_id)) return registrationRemoteHarvests.get(spawn_id);
    const promise = delayedRemoteHarvest(spawn_id);
    registrationRemoteHarvests.set(spawn_id, promise);
    return promise;
  }

  // H-R6 compensation: unwind a spawn's partial EXTERNAL state and settle its
  // durable row+card as failed. A fleet-created worktree is fresh and holds no
  // human work, so force-remove it (git first, then rmSync + prune as the
  // half-created fallback); kill any half-created scoped window by verified
  // name. Only a verified kill/absence permits filesystem cleanup and a terminal
  // 'gone'. UNKNOWN/failed pane cleanup leaves the row 'stalled' as a visible
  // owner, preserving its worktree and preventing a duplicate launch.
  async function spawnCompensate({
    spawn_id,
    session_id,
    callsign,
    cwd,
    worktree_path,
    tmux_window,
    reason,
    created = { clone: false, worktree: !!worktree_path },
  }: SpawnCompensateArgs) {
    if (tmux_window) {
      let killed: {
        ok: boolean;
        gone?: boolean;
        stale?: boolean;
        error?: string;
        window_id?: string;
      };
      try {
        killed = await tmuxAdapter.killWindowVerified(tmux_window);
      } catch (err) {
        killed = { ok: false, error: errMessage(err) };
      }
      if (!killed.ok && !killed.gone) {
        const cleanupError = killed.error ?? 'tmux pane cleanup could not be verified';
        q.setSpawnStatus.run('stalled', spawn_id);
        spawnFailed(session_id, callsign, `${reason}; cleanup unresolved: ${cleanupError}`);
        return { resolved: false, error: cleanupError };
      }
    }
    if (worktree_path && created.worktree) {
      try {
        const rm = await execFileP(
          'git',
          ['-C', cwd, 'worktree', 'remove', '--force', worktree_path],
          { timeout: 30_000 },
        );
        if (!rm.ok) {
          try {
            fs.rmSync(worktree_path, { recursive: true, force: true });
          } catch {
            /* best effort */
          }
        }
        await execFileP('git', ['-C', cwd, 'worktree', 'prune'], { timeout: 30_000 });
      } catch {
        /* best effort — a stranded fleet worktree is surfaced by cleanup() */
      }
    }
    if (cwd && created.clone) {
      try {
        fs.rmSync(cwd, { recursive: true, force: true });
      } catch {
        /* best effort */
      }
    }
    q.setSpawnStatus.run('gone', spawn_id);
    forgetSpawn(spawn_id);
    spawnFailed(session_id, callsign, reason);
    return { resolved: true };
  }

  async function launchPane({
    spawn_id,
    session_id,
    callsign,
    tmux_session,
    tmux_window,
    requestedCwd,
    runCwd,
    cleanupRoot,
    worktree_path,
    body,
    skipPermissions,
    gatewayEnv = null,
    created = { clone: false, worktree: !!worktree_path },
  }: LaunchPaneArgs) {
    const kind = body.kind ?? 'claude';
    const setupCmd = body.setup_cmd ?? null;
    const launchEnv = {
      ...(gatewayEnv ?? {}),
      ...(setupCmd ? { FLEETDECK_SETUP_CMD: setupCmd } : {}),
    };
    const launchEnvOrNull = Object.keys(launchEnv).length ? launchEnv : null;
    // This argv order is a public contract shared by cwd- and repo-mode spawns.
    // `keep` names the gateway variables tmux is about to set via `-e`: without
    // it the prefix's own `env -u` would strip them straight back off (see
    // claudeEnvArgvPrefix). The CREDENTIAL never enters argv — only the names of
    // the variables NOT being unset appear here, which is why the scrub list
    // shrinks rather than the assignment list growing.
    const claudeArgv = ['claude', '--session-id', session_id];
    if (body.model) claudeArgv.push('--model', body.model);
    if (body.permission_mode) claudeArgv.push('--permission-mode', body.permission_mode);
    if (body.dangerously_skip_permissions === true)
      claudeArgv.push('--dangerously-skip-permissions');
    if (body.remote_control === true) claudeArgv.push('--remote-control', callsign);
    // SECURITY (option injection): the prompt is untrusted human text and MUST
    // be the last, positional argv element — but WITHOUT a `--` terminator the
    // claude CLI (commander) parses a prompt like `--dangerously-skip-permissions`
    // as a FLAG, silently granting a bypass the spawn row records as
    // skip_permissions:false. `--` forces everything after it to be positional.
    // Nothing is ever pushed onto argv past this point.
    if (body.prompt) claudeArgv.push('--', body.prompt);

    const shellBin =
      (process.env['SHELL'] ?? '').trim() || (fs.existsSync('/bin/bash') ? 'bash' : 'sh');
    const argv =
      kind === 'shell'
        ? [...claudeEnvArgvPrefix(port, home), shellBin]
        : setupCmd
          ? [
              ...claudeEnvArgvPrefix(port, home, {
                keep: Object.keys(launchEnv),
                boardSession: session_id,
              }),
              'sh',
              '-c',
              SETUP_WRAPPER,
              'fleetdeck-setup',
              ...claudeArgv,
            ]
          : [
              ...claudeEnvArgvPrefix(port, home, {
                keep: gatewayEnv ? Object.keys(gatewayEnv) : [],
                boardSession: session_id,
              }),
              ...claudeArgv,
            ];

    const compensate = (reason: string) =>
      spawnCompensate({
        spawn_id,
        session_id,
        callsign,
        cwd: cleanupRoot,
        worktree_path,
        tmux_window,
        reason,
        created,
      });
    const override = tmuxAdapter.spawnOverrideCmd?.();
    if (override) {
      const spec = {
        spawn_id,
        session_id,
        callsign,
        port,
        cwd: runCwd,
        requested_cwd: requestedCwd,
        prompt: body.prompt ?? null,
        model: body.model ?? null,
        permission_mode: body.permission_mode ?? null,
        worktree_path,
        kind,
        setup_cmd: setupCmd,
        dangerously_skip_permissions: body.dangerously_skip_permissions === true,
        skip_permissions: skipPermissions,
        remote_control: body.remote_control === true,
        gateway: !!gatewayEnv,
        // The fixture receives the gateway environment VERBATIM, credential
        // included, because the only way to prove the routing actually reaches a
        // pane is to assert on what the pane was handed. That is acceptable here
        // and nowhere else: FLEETDECK_SPAWN_CMD is a test seam the README
        // explicitly disclaims as a way to run a real fleet, and it is already
        // the one path that hands a fixture the whole spawn spec. The tmux path
        // below — the only one a real fleet takes — keeps the credential out of
        // argv entirely (see newWindow's `-e` contract).
        gateway_env: gatewayEnv,
        env: launchEnvOrNull,
        tmux: { session: tmux_session, window: tmux_window },
        argv,
      };
      tmuxAdapter.launchOverride(
        override,
        spec,
        (err) =>
          void compensate(`spawn override: ${errMessage(err)}`).catch(() => {
            /* compensation is best-effort; the durable row/card already settled */
          }),
      );
    } else {
      try {
        await tmuxAdapter.ensureSession(port);
        await tmuxAdapter.newWindow({ port, callsign, cwd: runCwd, argv, env: launchEnvOrNull });
      } catch (err) {
        const cleanup = await compensate(errMessage(err));
        const unresolved = !cleanup.resolved ? `; cleanup unresolved: ${cleanup.error}` : '';
        return {
          status: 500,
          body: { ok: false, reason: `tmux spawn failed: ${errMessage(err)}${unresolved}` },
        };
      }
    }

    q.setSpawnStatus.run(kind === 'shell' ? 'live' : 'spawning', spawn_id);
    updateSession(
      session_id,
      kind === 'shell' ? { col: 'idle', note: 'shell' } : { note: 'spawning…' },
    );
    tick(
      `${kind === 'shell' ? '⌨' : '🚀'} spawned ${callsign} — tmux window ${tmux_window}${skipPermissions ? ' (unsupervised)' : ''}${gatewayEnv ? ' (gateway)' : ''}`,
    );
    if (kind !== 'shell') scheduleNudge(spawn_id, tmux_window, callsign);
    onMutate();
    return {
      status: 200,
      body: {
        ok: true,
        spawn_id,
        session_id,
        callsign,
        tmux: { session: tmux_session, window: tmux_window },
      },
    };
  }

  // POST /api/spawn — either the original existing-cwd flow or managed
  // {repo, branch, branch_mode} provisioning.
  async function spawn(body: SpawnBody) {
    const cap = spawnCapability();
    if (!cap.available) {
      return { status: 400, body: { ok: false, reason: `spawning unavailable: ${cap.reason}` } };
    }
    const kind = body.kind ?? 'claude';
    if (kind !== 'claude' && kind !== 'shell') {
      return { status: 400, body: { ok: false, reason: "kind must be 'claude' or 'shell'" } };
    }
    for (const k of [
      'cwd',
      'repo',
      'branch',
      'branch_mode',
      'prompt',
      'model',
      'permission_mode',
      'repo_host',
      'repo_transport',
      'repo_org',
    ]) {
      if (body[k] != null && typeof body[k] !== 'string') {
        return { status: 400, body: { ok: false, reason: `${k} must be a string` } };
      }
    }
    // repo_host only steers the org/repo shorthand (see repos.mjs). Validate its
    // value here so resolveTarget receives a known host, and refuse it without a
    // repo — a host with nothing to steer is a confused request, not a default.
    if (body.repo_host != null) {
      if (body.repo_host !== 'github' && body.repo_host !== 'gitlab') {
        return { status: 400, body: { ok: false, reason: 'repo_host must be github or gitlab' } };
      }
      if (body.repo == null) {
        return { status: 400, body: { ok: false, reason: 'repo_host requires repo' } };
      }
    }
    // repo_transport steers the SAME shorthand (D1) and is refused without a
    // repo, mirroring repo_host exactly — a transport with nothing to steer is
    // a confused request. An explicit value also PERSISTS (D2), below.
    if (body.repo_transport != null) {
      if (body.repo_transport !== 'ssh' && body.repo_transport !== 'https') {
        return { status: 400, body: { ok: false, reason: 'repo_transport must be ssh or https' } };
      }
      if (body.repo == null) {
        return { status: 400, body: { ok: false, reason: 'repo_transport requires repo' } };
      }
    }
    if (body.repo_org != null) {
      if (body.repo == null)
        return { status: 400, body: { ok: false, reason: 'repo_org requires repo' } };
      try {
        validateRepoDefaultOrg(body.repo_org);
      } catch (err) {
        return {
          status: errStatus(err) ?? 400,
          body: { ok: false, reason: errMessage(err) },
        };
      }
    }
    if (body.worktree != null && typeof body.worktree !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'worktree must be a boolean' } };
    }
    if (
      body.dangerously_skip_permissions != null &&
      typeof body.dangerously_skip_permissions !== 'boolean'
    ) {
      return {
        status: 400,
        body: { ok: false, reason: 'dangerously_skip_permissions must be a boolean' },
      };
    }
    const runtimeOverrideError = runtimeOverrideRefusal(body);
    if (runtimeOverrideError) {
      return { status: 400, body: { ok: false, reason: runtimeOverrideError } };
    }
    if (body.setup_cmd != null) {
      if (typeof body.setup_cmd !== 'string') {
        return { status: 400, body: { ok: false, reason: 'setup_cmd must be a string' } };
      }
      if (body.setup_cmd.length > SETUP_CMD_MAX) {
        return {
          status: 400,
          body: {
            ok: false,
            reason: `setup_cmd must be ${SETUP_CMD_MAX} characters or fewer — got ${body.setup_cmd.length}`,
          },
        };
      }
      if (SETUP_CONTROL_RE.test(body.setup_cmd)) {
        return {
          status: 400,
          body: {
            ok: false,
            reason: 'setup_cmd must not contain NUL or control characters other than newline',
          },
        };
      }
    }
    if (kind === 'shell') {
      const forbidden = [
        'repo',
        'branch',
        'branch_mode',
        'repo_host',
        'repo_transport',
        'repo_org',
        'prompt',
        'model',
        'permission_mode',
        'dangerously_skip_permissions',
        'remote_control',
        'gateway',
        'arm_token',
        'setup_cmd',
        'plan_id',
      ].find((k) => body[k] != null);
      if (forbidden || body.worktree === true) {
        const field = forbidden ?? 'worktree';
        return {
          status: 400,
          body: {
            ok: false,
            reason: `shell sessions are cwd-only; ${field} is a Claude-only field`,
          },
        };
      }
    }
    // Decide the routing BEFORE anything is created. A gateway refusal must cost
    // the caller nothing — no clone, no worktree, no pane, no durable row — so
    // it lands here beside the other pure-body gates rather than at launch time.
    const gateway: GatewayDecision =
      kind === 'shell' ? { use: false, env: null } : gatewayDecision(body.gateway);
    if (gateway.error) return { status: 400, body: { ok: false, reason: gateway.error } };
    const rcConflict = gatewayRemoteConflict(gateway.use, body.remote_control === true);
    if (rcConflict) return { status: 400, body: { ok: false, reason: rcConflict } };

    const hasRepo = body.repo != null;
    const hasCwd = body.cwd != null;
    if (hasRepo && hasCwd) {
      return { status: 400, body: { ok: false, reason: 'provide either cwd or repo, not both' } };
    }
    // permission_mode is an enum the CLI parses case-insensitively: validate
    // it that way too, or 'BypassPermissions' would sail past an exact-string
    // arm gate and boot a fully unsupervised pane recorded as supervised
    // (0.16.0 adversarial review). Known modes pass through with their
    // original casing (the CLI's own spelling is what argv carries); a cased
    // bypass is gated, then canonicalized so gate and launch can never drift;
    // anything else 400s rather than handing the CLI a mode we never vetted.
    const PERMISSION_MODES = new Set(['default', 'acceptedits', 'plan', 'bypasspermissions']);
    if (body.permission_mode != null) {
      const lower = body.permission_mode.toLowerCase();
      if (!PERMISSION_MODES.has(lower)) {
        return {
          status: 400,
          body: { ok: false, reason: `unknown permission_mode '${body.permission_mode}'` },
        };
      }
      if (lower === 'bypasspermissions' && body.permission_mode !== 'bypassPermissions') {
        body = { ...body, permission_mode: 'bypassPermissions' };
      }
    }
    const skipPermissions =
      body.dangerously_skip_permissions === true ||
      (typeof body.permission_mode === 'string' &&
        body.permission_mode.toLowerCase() === 'bypasspermissions');
    // 0.16.0: the unsupervised gate refuses before any clone/worktree/pane is
    // created — an arm refusal must cost the caller nothing.
    const armRefusal = unsupervisedGate(skipPermissions, body);
    if (armRefusal) return { status: 403, body: { ok: false, reason: armRefusal } };

    // BUG-040 — atomic pre-spawn execution claim. plan_id on the body means
    // "this spawn IS the execution of that plan" (the board's plan-execute
    // flow; the old client-composed flow spawned first and marked afterwards,
    // so two boards on one stale snapshot both launched before either mark
    // could 409). The claim is a single guarded UPDATE — the plan flips
    // proposed|approved|captured → executed here, BEFORE any clone, worktree,
    // pane, or durable row exists — so concurrent claims serialize in SQLite
    // and exactly one wins; the loser 409s without launching. The transition
    // matrix mirrors plans.mjs's executed-from set exactly. The board drops
    // its post-spawn mark when it sends plan_id, so a double-flip cannot
    // occur; a claim release (below) covers every failure after this point,
    // including a throw escaping launchPane.
    let planClaim: PlanClaim | null = null; // { plan_id, restoreStatus, via } while a claim is owed
    if (body.plan_id != null) {
      const pid = Number(body.plan_id);
      if (!Number.isInteger(pid) || pid < 1) {
        return { status: 400, body: { ok: false, reason: 'plan_id must be a positive integer' } };
      }
      const plan = q.getPlan.get(pid);
      if (!plan) return { status: 404, body: { ok: false, reason: 'no such plan' } };
      const via = `spawn:${randomUUID().slice(0, 8)}`;
      const r = q.claimPlanExecution.run(via, pid);
      if (r.changes !== 1) {
        return {
          status: 409,
          body: {
            ok: false,
            reason: `plan #${pid} is ${plan.status} — already executed or not executable`,
          },
        };
      }
      planClaim = { plan_id: pid, restoreStatus: plan.status, via };
      tick(
        `📚 plan #${pid} execution claimed by spawn${plan.callsign ? ` (planned by ${plan.callsign})` : ''}`,
      );
      // A still-parked planner hold for this plan is retired by the SAME
      // decision that claimed it (plans.mjs's daemon half of BUG-041; the
      // claim just moved earlier than the mark used to).
      if (plan.question_id != null) {
        const qq = ctx.questions?.dismiss?.(plan.question_id, { activity: true });
        if (qq?.ok && !qq.already) {
          tick(`📚 planner hold for plan #${pid} retired — question dismissed`);
        }
      }
    }
    const releasePlanClaim = () => {
      if (!planClaim) return;
      // Restore the pre-claim status ONLY if the row still carries this
      // claim's via — a concurrent archive/mark in the failure window must
      // not be reverted.
      q.releasePlanExecution.run(planClaim.restoreStatus, planClaim.plan_id, planClaim.via);
      tick(
        `📚 plan #${planClaim.plan_id} execution claim released (spawn failed) — back to ${planClaim.restoreStatus}`,
      );
      onMutate();
      planClaim = null;
    };
    const completePlanClaim = (spawn_id: string) => {
      if (!planClaim) return;
      q.setPlanExecuted.run(`spawn:${spawn_id}`, planClaim.plan_id);
      planClaim = null;
      onMutate();
    };
    const wrapSpawnFailure =
      <A extends unknown[], R>(fn: (...args: A) => Promise<R>) =>
      async (...a: A): Promise<R> => {
        try {
          return await fn(...a);
        } catch (err) {
          releasePlanClaim();
          throw err;
        }
      };

    if (hasRepo) {
      if (body.worktree === true) {
        return {
          status: 400,
          body: { ok: false, reason: 'branch_mode replaces worktree in repo mode' },
        };
      }
      if (!body.branch)
        return { status: 400, body: { ok: false, reason: 'branch is required in repo mode' } };
      // Captured here (narrowed to string by the guard above) so the async
      // provisioning closure below can pass it to materializeBranch: inside that
      // closure `body.branch`'s narrowing is lost — property narrowing does not
      // survive a nested-function boundary — but this const keeps its string type.
      const branch = body.branch;
      const branchMode = body.branch_mode ?? 'worktree';
      if (!['worktree', 'in-place'].includes(branchMode)) {
        return {
          status: 400,
          body: { ok: false, reason: 'branch_mode must be worktree or in-place' },
        };
      }
      try {
        await validateBranch(body.branch);
      } catch (err) {
        releasePlanClaim();
        return { status: 400, body: { ok: false, reason: errMessage(err) } };
      }

      // `hasRepo` guaranteed body.repo above, but the `await validateBranch` reset
      // the property-narrowing; re-assert it here. Under exactOptionalPropertyTypes an
      // *optional* body.repo is not assignable to resolveTarget's *required* repo even
      // once its value is narrowed (optional-vs-required is structural), so build its
      // ResolveTargetBody explicitly — the four fields resolveTarget reads — with
      // `?? null` mapping absent overrides to null (its declared `string | null`),
      // matching the untyped pre-migration behavior.
      if (body.repo == null) {
        releasePlanClaim();
        return { status: 400, body: { ok: false, reason: 'repo is required in repo mode' } };
      }

      let target: Awaited<ReturnType<ReposSurface['resolveTarget']>>;
      try {
        target = await resolveTarget({
          repo: body.repo,
          repo_host: body.repo_host ?? null,
          repo_transport: body.repo_transport ?? null,
          repo_org: body.repo_org ?? null,
        });
      } catch (err) {
        releasePlanClaim();
        return {
          status: errStatus(err) ?? 400,
          body: { ok: false, reason: errMessage(err) },
        };
      }
      const targetPath = target.mode === 'clone' ? target.dest : target.root;
      const owner = targetOwner(targetPath);
      if (owner) {
        releasePlanClaim();
        return {
          status: 409,
          body: {
            ok: false,
            reason: `${path.resolve(targetPath)} is already being provisioned by ${owner}`,
          },
        };
      }
      // Reserve a clone slot BEFORE any card/row exists, so a full pool returns a
      // clean 429 with nothing to compensate. Local materialization is uncapped.
      let releaseCloneSlot = () => {
        /* replaced once a clone slot is reserved */
      };
      if (target.mode === 'clone') {
        try {
          releaseCloneSlot = reserveCloneSlot();
        } catch (err) {
          releasePlanClaim();
          return {
            status: errStatus(err) ?? 429,
            body: { ok: false, reason: errMessage(err) },
          };
        }
      }
      // D2: an EXPLICIT transport on an ACCEPTED shorthand spawn becomes the
      // remembered default for the next one (and for curl users) — not a pill
      // click, which is exploratory. It sits AFTER every synchronous rejection
      // (validation 400s, the provisioning-owner 409, the clone-cap 429): a
      // request the daemon refused must never rewrite the remembered choice.
      // From here the spawn is committed to a card. A transport absent-and-
      // resolved-from-the-setting never rewrites it; and it steers shorthand
      // only, so a URL/path/known-local-name target (kind !== 'shorthand')
      // persists nothing. A bare name promoted through repo_org IS shorthand.
      let settingChanged = false;
      if (body.repo_transport != null && target.kind === 'shorthand') {
        persistRepoTransport(body.repo_transport);
        settingChanged = true;
      }
      if (body.repo_org != null && target.kind === 'shorthand') {
        persistRepoDefaultOrg(body.repo_org);
        settingChanged = true;
      }
      if (settingChanged) onMutate();

      const session_id = randomUUID();
      const spawn_id = randomUUID();
      const initialNote =
        target.mode === 'clone' ? `cloning ${target.repo_name}…` : `preparing ${body.branch}…`;
      // UX 2.3 option 4 — the LAST synchronous gate of the clone path. A throw
      // from createSpawnedCard's branchOf probe, the callsign lottery or the
      // card INSERT used to escape spawn() entirely, where http.mjs answered a
      // bare {reason:'internal'} for what was really a boring, retryable fault
      // (a flake in the 1.5s git probe, a busy DB). Classify it HERE, with the
      // hardened/bounded reason, so the caller can retry instead of guessing.
      // Between the slot's reservation and this point nothing external exists
      // (no dir on disk, no pane), so the slot release is the only cleanup a
      // refusal owes. The two materialization paths BELOW already own their
      // failure surfaces (spawnCompensate tombstones + a real status body) —
      // this guard deliberately covers the card-creation prefix only.
      let c: SessionRow;
      try {
        c = createSpawnedCard(session_id, targetPath, body.prompt, {
          repo_name: target.repo_name,
          branch: body.branch,
          note: initialNote,
        });
      } catch (err) {
        releaseCloneSlot();
        releasePlanClaim();
        return {
          status: 500,
          body: {
            ok: false,
            reason: `could not create the spawn card: ${spawnFailureReason(err)}`,
          },
        };
      }
      const callsign = c.callsign;
      // createSpawnedCard runs the callsign lottery and INSERTs it (see the guard
      // comment above), so a freshly created card always carries one — but the
      // row type is SELECT-* nullable. Prove it here rather than thread `string |
      // null` through windowName/claimTarget/spawnCompensate, all of which require
      // a real name. Impossible in practice; an honest throw beats a silent `!`.
      if (callsign == null) throw new Error('spawned card is missing its callsign');
      const releaseTarget = claimTarget(targetPath, callsign);
      const tmux_session = tmuxAdapter.sessionName(port);
      const tmux_window = tmuxAdapter.windowName(port, callsign);
      q.insertProvisionalSpawn.run(
        spawn_id,
        session_id,
        callsign,
        tmux_session,
        tmux_window,
        targetPath,
        null,
        Date.now(),
        skipPermissions ? 1 : 0,
        body.remote_control === true ? 1 : 0,
        target.origin_url ?? null,
        body.branch,
        branchMode,
        gateway.use ? 1 : 0,
        kind,
        body.setup_cmd ?? null,
      );

      const finishMaterialization = async (materialized: Materialized, source: string) => {
        const worktree_path = branchMode === 'worktree' ? materialized.runCwd : null;
        if (worktree_path)
          q.setSpawnWorktree.run(worktree_path, materialized.created.worktree ? 1 : 0, spawn_id);
        const repo = deriveRepo(materialized.runCwd);
        updateSession(session_id, {
          cwd: materialized.runCwd,
          repo_id: repo.repo_id,
          repo_name: repo.repo_name,
          worktree: repo.worktree,
          // updateSession coerces every value through `?? null` at bind time, so
          // an absent body.branch (undefined) already becomes NULL there — spell
          // it here to satisfy SqlValue without changing what SQLite stores.
          branch: branchOf(materialized.runCwd, { fresh: true }) ?? body.branch ?? null,
        });
        const defaultRef = await baseBranch(target.mode === 'clone' ? target.dest : target.root);
        touchRepo({
          repo_id: repo.repo_id,
          repo_name: repo.repo_name,
          root: repo.main_tree,
          origin_url: target.origin_url ?? null,
          default_branch: defaultRef?.ref.replace(/^origin\//, '') ?? null,
          source,
        });
        return worktree_path;
      };

      if (target.mode === 'local') {
        try {
          let materialized: Materialized;
          let worktree_path: string | null = null;
          let paneMayExist = false;
          try {
            materialized = await materializeBranch({
              root: target.root,
              branch: body.branch,
              mode: branchMode,
              spawn_id,
              sid: session_id,
            });
          } catch (err) {
            await spawnCompensate({
              spawn_id,
              session_id,
              callsign,
              cwd: target.root,
              worktree_path: null,
              tmux_window: null,
              reason: errMessage(err),
              created: { clone: false, worktree: false },
            });
            releasePlanClaim();
            return {
              status: errStatus(err) ?? 409,
              body: { ok: false, reason: errMessage(err) },
            };
          }
          worktree_path = branchMode === 'worktree' ? materialized.runCwd : null;
          try {
            await finishMaterialization(materialized, 'spawn');
            paneMayExist = true;
            const out = await wrapSpawnFailure(launchPane)({
              spawn_id,
              session_id,
              callsign,
              tmux_session,
              tmux_window,
              requestedCwd: target.root,
              runCwd: materialized.runCwd,
              cleanupRoot: target.root,
              worktree_path,
              body,
              skipPermissions,
              gatewayEnv: gateway.env,
              created: materialized.created,
            });
            if (out.status >= 400) releasePlanClaim();
            else completePlanClaim(spawn_id);
            return out;
          } catch (err) {
            // in-place already ran `git switch`; compensation won't revert the
            // user's own checkout, so say plainly that it was left on the branch.
            const reason =
              branchMode === 'in-place'
                ? `${errMessage(err)} — ${path.basename(target.root)} was left switched to ${body.branch}`
                : errMessage(err);
            await spawnCompensate({
              spawn_id,
              session_id,
              callsign,
              cwd: target.root,
              worktree_path,
              tmux_window: paneMayExist ? tmux_window : null,
              reason,
              created: materialized.created,
            });
            releasePlanClaim();
            return { status: errStatus(err) ?? 409, body: { ok: false, reason } };
          }
        } finally {
          releaseTarget();
        }
      }

      // Clone provisioning continues after the HTTP 202 response. The guarded
      // detached chain owns both compensation and the single-flight release.
      Promise.resolve()
        .then(async () => {
          let created = { clone: false, worktree: false };
          let worktree_path: string | null = null;
          let paneMayExist = false;
          try {
            await cloneRepo({ origin_url: target.origin_url, dest: target.dest, spawn_id });
            created.clone = true;
            updateSession(session_id, { note: `preparing ${body.branch}…` });
            onMutate();
            const materialized = await materializeBranch({
              root: target.dest,
              branch,
              mode: branchMode,
              spawn_id,
              sid: session_id,
              clone: true,
            });
            created = materialized.created;
            worktree_path = branchMode === 'worktree' ? materialized.runCwd : null;
            await finishMaterialization(materialized, 'clone');
            paneMayExist = true;
            await launchPane({
              spawn_id,
              session_id,
              callsign,
              tmux_session,
              tmux_window,
              requestedCwd: target.dest,
              runCwd: materialized.runCwd,
              cleanupRoot: target.dest,
              worktree_path,
              body,
              skipPermissions,
              created,
              gatewayEnv: gateway.env,
            });
            completePlanClaim(spawn_id);
          } catch (err) {
            const reason =
              branchMode === 'in-place' && created.clone
                ? `${errMessage(err)} — ${path.basename(target.dest)} was left switched to ${body.branch}`
                : errMessage(err);
            await spawnCompensate({
              spawn_id,
              session_id,
              callsign,
              cwd: target.dest,
              worktree_path,
              tmux_window: paneMayExist ? tmux_window : null,
              reason,
              created,
            });
            releasePlanClaim();
          } finally {
            releaseCloneSlot();
            releaseTarget();
          }
        })
        .catch((err: unknown) => {
          console.error('fleetd detached repo provisioning error:', err);
        });

      return {
        status: 202,
        body: {
          ok: true,
          provisioning: true,
          spawn_id,
          session_id,
          callsign,
          clone: { origin_url: target.origin_url, dest: target.dest },
          tmux: { session: tmux_session, window: tmux_window },
        },
      };
    }

    // Original cwd mode remains synchronous through optional worktree creation.
    const cwd = body.cwd ?? '';
    let st: fs.Stats | null = null;
    try {
      st = fs.statSync(cwd);
    } catch {
      /* missing */
    }
    if (!cwd || !st?.isDirectory()) {
      releasePlanClaim();
      return { status: 400, body: { ok: false, reason: 'cwd missing or not a directory' } };
    }
    if (body.worktree === true && !deriveRepo(cwd).is_git) {
      releasePlanClaim();
      return {
        status: 409,
        body: { ok: false, reason: 'cwd is not a git repository — cannot spawn into a worktree' },
      };
    }

    const session_id = randomUUID();
    const spawn_id = randomUUID();
    const c =
      kind === 'shell'
        ? createSpawnedCard(session_id, cwd, null, {
            deriveRepo: true,
            source: 'shell',
            col: 'idle',
            note: 'shell',
          })
        : createSpawnedCard(session_id, cwd, body.prompt);
    const callsign = c.callsign;
    // As in the repo path above: a freshly created card always carries a
    // callsign, but the SELECT-* row type is nullable. Prove it once rather than
    // thread `string | null` through windowName/animalOf/spawnCompensate.
    if (callsign == null) throw new Error('spawned card is missing its callsign');
    const tmux_session = tmuxAdapter.sessionName(port);
    const tmux_window = tmuxAdapter.windowName(port, callsign);
    q.insertProvisionalSpawn.run(
      spawn_id,
      session_id,
      callsign,
      tmux_session,
      tmux_window,
      cwd,
      null,
      Date.now(),
      skipPermissions ? 1 : 0,
      body.remote_control === true ? 1 : 0,
      null,
      null,
      null,
      gateway.use ? 1 : 0,
      kind,
      body.setup_cmd ?? null,
    );

    let worktree_path: string | null = null;
    if (body.worktree === true) {
      const ticketNamed = c.ticket && callsign.endsWith(`-${c.ticket}`);
      const baseName = ticketNamed ? `${c.ticket}-${animalOf(callsign)}` : callsign;
      const pathFor = (name: string) =>
        path.join(path.dirname(cwd), `${path.basename(cwd)}--fd-${name}`);
      const dedup = `${baseName}-${session_id.slice(0, 4)}`;
      const names = fs.existsSync(pathFor(baseName)) ? [dedup] : [baseName, dedup];
      let candidate = '';
      let result: ExecResult = { ok: false, err: '' };
      for (const workname of names) {
        candidate = pathFor(workname);
        result = await execFileP('git', [
          '-C',
          cwd,
          'worktree',
          'add',
          '-b',
          `fd/${workname}`,
          candidate,
        ]);
        if (result.ok) break;
      }
      worktree_path = candidate;
      if (!result.ok) {
        // `git worktree add` is a purely local operation, so a credential in its
        // stderr is unlikely — but repos.mjs now asserts that no path in the clone
        // family puts unscrubbed git stderr into a note or an HTTP body, and a
        // control applied unevenly reads to the next reader as a deliberate
        // posture rather than a gap. One call, no behaviour change on stderr that
        // holds no credential.
        const addErr = scrubUrlCredentials(result.err);
        await spawnCompensate({
          spawn_id,
          session_id,
          callsign,
          cwd,
          worktree_path,
          tmux_window: null,
          reason: `git worktree add: ${addErr}`,
        });
        releasePlanClaim();
        return {
          status: 409,
          body: { ok: false, reason: `git worktree add failed: ${addErr}`.slice(0, 300) },
        };
      }
      q.setSpawnWorktree.run(worktree_path, 1, spawn_id); // cwd mode always creates the worktree above
      const repo = deriveRepo(worktree_path);
      updateSession(session_id, {
        cwd: worktree_path,
        repo_id: repo.repo_id,
        repo_name: repo.repo_name,
        worktree: repo.worktree,
        branch: branchOf(worktree_path),
      });
    }
    // The deterministic-argv build + override/tmux launch + status flip is
    // shared with repo-mode spawns; it lives in launchPane() (incl. the `--`
    // end-of-options fix carried over from origin/main's audit).
    const out = await wrapSpawnFailure(launchPane)({
      spawn_id,
      session_id,
      callsign,
      tmux_session,
      tmux_window,
      requestedCwd: cwd,
      runCwd: worktree_path ?? cwd,
      cleanupRoot: cwd,
      worktree_path,
      body,
      skipPermissions,
      gatewayEnv: gateway.env,
    });
    if (out.status >= 400) releasePlanClaim();
    else completePlanClaim(spawn_id);
    return out;
  }

  // POST /api/spawn/:id/revive — resume a terminal board-owned Claude
  // conversation into a NEW durable spawn row. Historical rows are immutable
  // evidence; the same session id/callsign/window identity is reused.
  async function revive(spawn_id: string, body: SpawnBody = {}) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    if (row.kind === 'shell') {
      return {
        status: 410,
        body: { ok: false, reason: 'shell sessions have no conversation to resume' },
      };
    }
    if (!['pane-dead', 'killed', 'gone'].includes(row.status)) {
      return { status: 409, body: { ok: false, reason: `spawn is ${row.status}, not revivable` } };
    }
    // Every spawn is inserted with its deterministic window name, so a revivable
    // row (pane-dead/killed/gone) always carries one; this proves it to the type
    // system for the window reconciliation below without altering behavior.
    if (row.tmux_window == null) throw new Error('revivable spawn is missing its tmux window');
    const runtimeOverrideError = runtimeOverrideRefusal(body);
    if (runtimeOverrideError) {
      return { status: 400, body: { ok: false, reason: runtimeOverrideError } };
    }
    // 0.16.0 (adversarial review): reviving an UNSUPERVISED lineage launches
    // --dangerously-skip-permissions again, so it is an unsupervised spawn and
    // must pass the same gate — otherwise the 60s single-use arm becomes a
    // permanent replayable capability (arm once, revive forever, tokenless).
    // A supervised lineage revives with no gate, as before.
    if (row.skip_permissions) {
      const reviveArmRefusal = unsupervisedGate(true, body);
      if (reviveArmRefusal) return { status: 403, body: { ok: false, reason: reviveArmRefusal } };
    }
    // Remote control survives death: inherit the dead row's wish unless the
    // human overrides it on this revive.
    const remoteWanted = body.remote_control ?? !!row.remote_control;
    // Gateway routing survives death the same way — and for a stronger reason
    // than symmetry. A revive RESUMES a conversation, and the transcript being
    // resumed was produced by whatever provider served it; silently continuing
    // it against a different one changes who is billed for the rest of that
    // conversation without anything on screen saying so. Inheriting the dead
    // row's routing keeps a lineage on one provider unless a human says
    // otherwise. Note the fallback resolves to a BOOLEAN, never null: null would
    // re-consult gateway_default, so flipping that setting on would quietly
    // reroute every later revive of every pre-existing session. A revive asks
    // "what was this lineage doing", not "what would a new spawn do".
    const gateway = gatewayDecision(
      body.gateway ?? !!row.gateway,
      body.gateway == null ? 'inherited' : 'request',
    );
    if (gateway.error) return { status: 400, body: { ok: false, reason: gateway.error } };
    const rcConflict = gatewayRemoteConflict(gateway.use, remoteWanted);
    if (rcConflict) return { status: 400, body: { ok: false, reason: rcConflict } };
    // HIGH (revive single-flight), part 1 — the DB guard. A live-eligible OR a
    // PROVISIONING spawn for this session means someone is already bringing a
    // pane up; refuse. Including 'provisioning' matters because a revive's own
    // durable row is 'provisioning' from the instant it is inserted until its
    // pane is up, so a later revive request cannot slip past a still-in-flight
    // one (activeSpawnBySession alone would not see it).
    const active =
      q.activeSpawnBySession.get(row.session_id) ??
      q.provisioningSpawnBySession.get(row.session_id);
    if (active) {
      return {
        status: 409,
        body: { ok: false, reason: `session already has active spawn ${active.spawn_id}` },
      };
    }
    // HIGH (revive single-flight), part 2 — the SYNCHRONOUS claim. The DB guard
    // above cannot stop TWO near-simultaneous revives: both read the same
    // pre-insert state, both cross the async window inspection below, and both
    // insert a provisional row + launch a pane → two live panes for one
    // session. Claim the session id here with no await between the check and
    // the add, so the second concurrent revive is refused until the first
    // settles. The try/finally guarantees the claim is released on EVERY exit
    // (success, refusal, or throw). R2-5 stale-kill protection is untouched —
    // the provisional owner row is still inserted below before newWindow.
    if (revivingSessions.has(row.session_id)) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `session ${row.session_id.slice(0, 8)} is already being revived`,
        },
      };
    }
    revivingSessions.add(row.session_id);
    // BUG-060: acquire the worktree path's canonical claim BEFORE validating
    // the cwd/transcript and launching, and hold it through the launch. A
    // worktree removal holds the same claim for its entire inspect → git
    // remove → DB purge sequence, so a revive racing a removal that is already
    // mid-flight QUEUES here behind it and, once the removal has deleted the
    // checkout and released, proceeds only to 410 on the now-missing cwd —
    // never launching a pane into a directory `git worktree remove` was about
    // to delete. Acquired first, so the queueing (path-lock) path decides the
    // race before the synchronous custody fallback below can turn a held
    // removal into an early refusal. Standalone cores without the mutex fall
    // back to `() => {}` and lean on the custody claim instead.
    const releasePathLock =
      row.worktree_path && acquireWorktreePathLock
        ? await acquireWorktreePathLock(canonicalPathKey(row.worktree_path))
        : () => {
            /* no path lock in standalone cores without the keyed mutex */
          };
    // The other half of the remove-vs-revive race (worktrees.mjs owns the
    // remove half): claim custody of the worktree this revive will run in.
    // removeWorktree holds the same per-path lease from its final liveness
    // re-check through the awaited `git worktree remove`/prune/branch/purge
    // tail. When the path-lock mutex is wired the removal already holds it, so
    // a racing revive blocks above and never reaches a held custody claim;
    // this claim is the fallback that keeps standalone cores (no mutex, but
    // derive always wires claimWorktreeCustody) honest — a revive that finds
    // custody held is refused rather than launching into a doomed checkout.
    // A refused revive retries cleanly; a lost worktree does not.
    let releaseCustody: (() => void) | null = null;
    if (row.worktree_path && claimWorktreeCustody) {
      releaseCustody = claimWorktreeCustody(row.worktree_path, 'revive');
      if (!releaseCustody) {
        revivingSessions.delete(row.session_id);
        releasePathLock();
        return {
          status: 409,
          body: {
            ok: false,
            reason: 'this worktree is being removed — retry once the removal settles',
          },
        };
      }
    }
    try {
      // H-R7: validate ALL resume eligibility BEFORE touching tmux. Reviving
      // reuses the deterministic window name, and the old code killed whatever
      // occupied it and THEN checked cwd/transcript — so a missing transcript
      // would 410 only AFTER an unrelated pane had already been destroyed.
      // Prove the resume can actually happen first; only then reconcile the
      // window.
      const runCwd = row.worktree_path ?? row.cwd;
      let st: fs.Stats | null = null;
      try {
        if (runCwd) st = fs.statSync(runCwd);
      } catch {
        /* missing */
      }
      if (!runCwd || !st?.isDirectory()) {
        return { status: 410, body: { ok: false, reason: 'revive cwd no longer exists' } };
      }
      if (!fs.existsSync(claudeTranscriptPath(runCwd, row.session_id))) {
        return { status: 410, body: { ok: false, reason: 'resume transcript no longer exists' } };
      }

      // Exact scoped-name collision defense, now that eligibility is proven. A
      // live Claude pane is ownership proof and must never be duplicated. Only a
      // pane PROVEN dead, or an expected bare shell (claude exited, leaving the
      // login shell in a remain-on-exit window), is a safe remnant to remove by
      // verified name before reusing the window. A live pane running ANYTHING
      // ELSE (the human repurposed the window) is never destroyed — refuse.
      const existing = await findScopedWindow(row.tmux_window);
      if (existing === null) {
        return {
          status: 503,
          body: {
            ok: false,
            reason: 'tmux window lookup failed; revive held to avoid a duplicate session',
          },
        };
      }
      if (existing && !existing.pane_dead && existing.pane_cmd === 'claude') {
        // BUG 3: the deterministic window ALREADY hosts a live claude pane for
        // this session — it was wrongly condemned (BUG 1 /clear, BUG 2 silence)
        // while the agent kept running, and the board hid the terminal. A human
        // clicking Revive here used to hit a dead-end 409 ("already has a live
        // claude pane") and stay stuck until the next liveness poll. There is
        // nothing to resume: the pane IS the live session. ADOPT it — resurrect
        // the row to 'live' and lift the card back onto the board — and return
        // success so the terminal shows NOW. No tmux launch, no kill: we never
        // duplicate a live billed session (the same safety the 409 protected).
        //
        // MED (adoption must mirror the liveness-tick's resurrection guards):
        // the tick only resurrects a 'pane-dead'/'gone' row (never 'killed')
        // AND only when currentWindowOwner still names THAT row. Adoption did
        // NEITHER, which let two things go wrong:
        //   (a) a 'killed' row (a human decision) could be flipped back to
        //       'live' by adoption — breaking "a human kill never resurrects".
        //   (b) reviving a NON-newest 'pane-dead'/'gone' row whose reused window
        //       is a live claude could resurrect the OLDER row while a newer
        //       pane-dead row still outranks it in currentWindowOwner: BOTH end
        //       up 'live' (countActiveSpawns double-counts) and the adopted row
        //       becomes un-killable (spawnKill's owner check points at the other
        //       one). Guard both here before resurrecting.
        if (row.status !== 'pane-dead' && row.status !== 'gone') {
          // 'killed': the window hosts a live claude, but a human kill is never
          // undone by adoption. Refuse rather than resurrect or duplicate.
          return {
            status: 409,
            body: {
              ok: false,
              reason: `spawn ${spawn_id} was killed — its window hosts a live claude, but a killed spawn is never resurrected by adoption`,
            },
          };
        }
        const owner = q.currentWindowOwner.get(row.tmux_window);
        if (owner && owner.spawn_id !== row.spawn_id) {
          return {
            status: 409,
            body: {
              ok: false,
              reason: `window ${row.tmux_window} is owned by spawn ${owner.spawn_id} — revive that one`,
              current_spawn_id: owner.spawn_id,
            },
          };
        }
        resurrectSpawn(row);
        return {
          status: 200,
          body: {
            ok: true,
            adopted: true,
            spawn_id: row.spawn_id,
            session_id: row.session_id,
            callsign: row.callsign,
            tmux: { session: tmuxAdapter.sessionName(port), window: row.tmux_window },
          },
        };
      }
      if (existing && !existing.pane_dead && !SHELL_RE.test(existing.pane_cmd)) {
        return {
          status: 409,
          body: {
            ok: false,
            reason: `window ${row.tmux_window} hosts a live '${existing.pane_cmd}' pane — not a dead remnant; refusing to kill it`,
          },
        };
      }
      if (existing) {
        const killed = await tmuxAdapter.killWindowVerified(row.tmux_window);
        if (!killed.ok && !killed.gone) {
          return {
            status: 500,
            body: { ok: false, reason: killed.error ?? 'tmux kill-window failed' },
          };
        }
      }

      // The launch discipline (R2-5 pre-launch owner re-check, provisional owner
      // row, override/newWindow, live-flip, card update, nudge) is shared with
      // adopt now — launchResume() below is the single source of truth. Reviving
      // reuses the dead row's window name and callsign, carries the dead row's
      // requested cwd + worktree_path + skip-permissions, inherits the remote
      // wish, and excludes ITS OWN terminal row (spawn_id) from the owner
      // re-check (a 'pane-dead' row still naming its window is not a rival).
      return await launchResume({
        session_id: row.session_id,
        callsign: row.callsign,
        tmux_window: row.tmux_window,
        runCwd,
        requested_cwd: row.cwd,
        worktree_path: row.worktree_path,
        skip_permissions: !!row.skip_permissions,
        remoteWanted,
        gatewayEnv: gateway.env,
        excludeSpawnId: spawn_id,
        overrideExtra: { revive_of: spawn_id },
        note: 'reviving…',
        tickMsg: `⟲ reviving ${row.callsign} (resume ${row.session_id.slice(0, 8)})`,
        failReason: 'tmux revive failed',
      });
    } finally {
      // Release the single-flight claim on EVERY exit path.
      revivingSessions.delete(row.session_id);
      releasePathLock();
      // Release the worktree-custody lease on every exit path too: the new
      // spawn row is durable by now ('provisioning'/'spawning'), so
      // worktreePathIsLive keeps any later removal honest without the lease.
      releaseCustody?.();
    }
  }

  // Shared resume-launch discipline — the tail extracted VERBATIM from revive()
  // so revive and adopt build the pane exactly one way. Given a session id +
  // callsign + a scoped window name it: builds the env-wrapped
  // `claude --resume <sid>` argv (+ optional --dangerously-skip-permissions /
  // --remote-control), does the R2-5 pre-launch owner re-check, inserts the
  // PROVISIONAL owner row BEFORE any pane exists (H-R6), launches (test override
  // or tmux), flips the row live-eligible, updates the card (archived_at:null,
  // col:queued, note — ended_at left for the first hook), nudges, and returns
  // the control-API {status, body}. Callers own everything BEFORE this: state
  // gate, single-flight claim, H-R7 cwd/transcript validation, and window
  // collision defense. The caller's try/finally still owns the revivingSessions
  // release — launchResume never touches that Set.
  async function launchResume({
    session_id,
    callsign,
    tmux_window,
    runCwd,
    requested_cwd,
    worktree_path,
    skip_permissions,
    remoteWanted,
    excludeSpawnId,
    overrideExtra = {},
    note,
    tickMsg,
    failReason,
    bodyExtra = {},
    gatewayEnv = null,
  }: LaunchResumeArgs) {
    // Identity proof: a real provisioned row always names both, but the SELECT-*
    // types are nullable — assert the contract rather than launch a pane with a
    // null callsign or window name.
    if (callsign == null || tmux_window == null) {
      throw new Error('launchResume requires a callsign and a scoped window name');
    }
    const new_spawn_id = randomUUID();
    // `keep` mirrors launchPane: the gateway variables tmux sets via `-e` must
    // be excluded from this prefix's own `env -u`, or the resumed pane would be
    // scrubbed back onto Anthropic mid-conversation.
    const argv = [
      ...claudeEnvArgvPrefix(port, home, {
        keep: gatewayEnv ? Object.keys(gatewayEnv) : [],
        boardSession: session_id,
      }),
      'claude',
      '--resume',
      session_id,
    ];
    if (skip_permissions) argv.push('--dangerously-skip-permissions');
    if (remoteWanted) argv.push('--remote-control', callsign);
    const tmux_session = tmuxAdapter.sessionName(port);

    // HIGH (revive single-flight), part 3 / R2-5 — re-check ownership right
    // before we claim the window. The caller's guards ran BEFORE its awaits
    // (listScopedWindows / killWindowVerified). Re-confirm no live-eligible or
    // provisioning row has taken this window in the meantime (belt-and-suspenders
    // behind the single-flight claim; also catches a concurrent spawn that
    // reused this exact scoped name). currentWindowOwner ranks the window's rows
    // newest-first over provisioning|spawning|stalled|live|pane-dead: the
    // rightful owner right now must be either the caller's own excluded row (a
    // revive's 'pane-dead' row still naming its window — excludeSpawnId) or
    // nobody (an adopt onto a fresh callsign-derived name, or a 'gone'/'killed'
    // row that released it). A DIFFERENT row in a live-eligible state means a
    // second pane is already coming up — refuse rather than launch a duplicate.
    // A stale 'pane-dead' SIBLING is not live-eligible and never blocks.
    const preLaunchOwner = q.currentWindowOwner.get(tmux_window);
    if (
      preLaunchOwner &&
      preLaunchOwner.spawn_id !== excludeSpawnId &&
      ['provisioning', 'spawning', 'stalled', 'live'].includes(preLaunchOwner.status)
    ) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `window ${tmux_window} is now owned by active spawn ${preLaunchOwner.spawn_id}`,
          current_spawn_id: preLaunchOwner.spawn_id,
        },
      };
    }

    // R2-5 / H-R6: insert the new spawn's durable row as a PROVISIONAL owner of
    // the window name BEFORE the pane is created. Without a live owning row, a
    // forced stale-id spawnKill arriving during the newWindow await would see no
    // current owner and kill the just-launched pane, leaving a 'spawning' row
    // for a dead pane. currentWindowOwner counts 'provisioning', so the new row
    // owns the window the instant it exists and any OTHER-id kill is refused.
    // remote_url is deliberately NOT carried over: a fresh one is harvested from
    // the resumed pane. Flip to 'spawning' (live-eligible) only once the pane is
    // up. (adopt passes worktree_path:null — it never creates a worktree.)
    q.insertProvisionalSpawn.run(
      new_spawn_id,
      session_id,
      callsign,
      tmux_session,
      tmux_window,
      requested_cwd,
      worktree_path ?? null,
      Date.now(),
      skip_permissions ? 1 : 0,
      remoteWanted ? 1 : 0,
      null,
      null,
      null,
      gatewayEnv ? 1 : 0,
      'claude',
      null,
    );

    const compensateResume = (reason: string) =>
      spawnCompensate({
        spawn_id: new_spawn_id,
        session_id,
        callsign,
        cwd: runCwd,
        worktree_path,
        tmux_window,
        reason,
        created: { clone: false, worktree: false },
      });
    const override = tmuxAdapter.spawnOverrideCmd?.();
    if (override) {
      tmuxAdapter.launchOverride(
        override,
        {
          spawn_id: new_spawn_id,
          ...overrideExtra,
          session_id,
          callsign,
          port,
          cwd: runCwd,
          requested_cwd,
          prompt: null,
          model: null,
          permission_mode: null,
          worktree_path,
          dangerously_skip_permissions: skip_permissions,
          skip_permissions,
          remote_control: remoteWanted,
          gateway: !!gatewayEnv,
          gateway_env: gatewayEnv, // test seam only — see launchPane's spec
          tmux: { session: tmux_session, window: tmux_window },
          argv,
        },
        (err) =>
          void compensateResume(`spawn override: ${errMessage(err)}`).catch(() => {
            /* compensation is best-effort; the durable row/card already settled */
          }),
      );
    } else {
      try {
        await tmuxAdapter.ensureSession(port);
        await tmuxAdapter.newWindow({ port, callsign, cwd: runCwd, argv, env: gatewayEnv });
      } catch (err) {
        const reason = `${failReason}: ${errMessage(err)}`;
        const cleanup = await compensateResume(reason);
        return {
          status: 500,
          body: {
            ok: false,
            reason: cleanup.resolved ? reason : `${reason}; cleanup unresolved: ${cleanup.error}`,
          },
        };
      }
    }

    // Pane exists (or the override was handed off): flip the durable row
    // live-eligible ('provisioning' → 'spawning').
    q.setSpawnStatus.run('spawning', new_spawn_id);
    updateSession(session_id, { archived_at: null, col: 'queued', note });
    tick(tickMsg);
    scheduleNudge(new_spawn_id, tmux_window, callsign);
    onMutate();
    return {
      status: 200,
      body: {
        ok: true,
        ...bodyExtra,
        spawn_id: new_spawn_id,
        session_id,
        callsign,
        tmux: { session: tmux_session, window: tmux_window },
      },
    };
  }

  // POST /api/sessions/:session_id/adopt — "Move to tmux". Adopt a session the
  // board did NOT spawn (source 'hooks' / 'agents-cli' / a remote ccd-cli run)
  // into a board-owned tmux pane via `claude --resume <sid>`. Context-sensitive:
  //   • live session   → ARM (200 {armed, expires_at}); re-arm refreshes; the
  //                      pane is launched by hookSessionEnd when the CLI exits
  //                      (two processes can't drive one conversation).
  //   • ended session  → ADOPT NOW (200 {adopted, spawn_id, …}).
  //   • body {disarm}  → clear a pending arm (200 {armed:false}).
  // Stale-snapshot races degrade correctly in BOTH directions (a card that
  // ended between snapshot and this POST adopts now instead of arming, and vice
  // versa) and the response says which happened. Never sets --remote-control in
  // v1 (/rc is available from the live card once the pane is up).
  async function adoptSession(
    session_id: string,
    body: SpawnBody = {},
    { deferred = false }: { deferred?: boolean } = {},
  ) {
    const c = q.getSession.get(session_id);
    if (!c) return { status: 404, body: { ok: false, reason: 'no such session' } };
    // Every session is registered with a callsign, so an adoptable card always
    // carries one; this proves it to the type system for the window name and the
    // ticker lines below without altering behavior.
    if (c.callsign == null) throw new Error('adoptable session is missing its callsign');

    // Body validation. dangerously_skip_permissions is the two-step unsupervised
    // gate; disarm cancels a pending arm.
    if (
      body.dangerously_skip_permissions != null &&
      typeof body.dangerously_skip_permissions !== 'boolean'
    ) {
      return {
        status: 400,
        body: { ok: false, reason: 'dangerously_skip_permissions must be a boolean' },
      };
    }
    if (body.disarm != null && typeof body.disarm !== 'boolean') {
      return { status: 400, body: { ok: false, reason: 'disarm must be a boolean' } };
    }
    if (body.arm_token != null && typeof body.arm_token !== 'string') {
      return { status: 400, body: { ok: false, reason: 'arm_token must be a string' } };
    }
    const skip = body.dangerously_skip_permissions === true;
    // 0.16.0: same unsupervised gate as /api/spawn — an adopt with skip:true
    // launches a process, so it must echo a fresh arm token. A DEFERRED call
    // (the armed auto-adopt fired by hookSessionEnd, or the boot sweep) is
    // exempt: its body is reconstructed from the adopt_armed_skip column the
    // human's ORIGINAL arm POST wrote — that POST already passed this gate
    // with a fresh arm token, and the single-use token cannot be echoed a
    // second time. Gating it again would 403 every armed move-to-tmux.
    const adoptArmRefusal = deferred ? null : unsupervisedGate(skip, body);
    if (adoptArmRefusal) return { status: 403, body: { ok: false, reason: adoptArmRefusal } };

    // Disarm FIRST, in any state: the click that disarms is the human revoking
    // the earlier arm click, and it must NEVER fall through into an adopt (a
    // card that ended between the armed snapshot and this disarm POST would
    // otherwise be adopted by a cancel click). Because the arm now survives
    // until CONSUMED (see the ended fork), a disarm landing inside the deferred
    // grace window genuinely cancels the scheduled move — the deferred call
    // finds the arm gone and stands down. Idempotent.
    if (body.disarm === true) {
      updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
      tick(`⇥ ${c.callsign} move-to-tmux disarmed`);
      onMutate();
      return { status: 200, body: { ok: true, armed: false, disarmed: true } };
    }

    // Board-owned never adopts — and never ARMS. ANY spawn lineage (dead or
    // alive) means the board already owns this session's pane story: revive
    // owns dead lineages, and arming a board-owned session would fire a second
    // `claude --resume` lineage at its next SessionEnd, fighting the first over
    // the window name and worktree bookkeeping. This sits BEFORE the live/ended
    // fork so neither path can slip past it. A deferred call landing here means
    // a manual adopt/revive won the race and created the row — benign (the
    // caller treats 409 as "someone else already did it").
    const lineage = q.spawnBySession.get(session_id);
    if (lineage) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `session is board-owned (spawn ${lineage.spawn_id}, ${lineage.status}) — revive owns its pane story`,
        },
      };
    }

    // LIVE fork: the CLI is still running (ended_at null). Two processes can't
    // drive one conversation, so we can't grab it now.
    if (c.ended_at == null) {
      // A DEFERRED call reaching a live session is the resurrection race: the
      // CLI exited (arming the move) and something resumed it inside the grace
      // window. The human's click was one-shot — re-arming here would plant a
      // standing 30-minute arm they never asked for, firing a surprise move at
      // the NEXT exit. Cancel instead: consume the arm, say so once.
      if (deferred) {
        updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
        tick(`↷ move-to-tmux canceled for ${c.callsign} — session came back live before the move`);
        onMutate();
        return { status: 200, body: { ok: true, canceled: true } };
      }
      // Manual click → ARM: remember it as a durable deadline; hookSessionEnd
      // fires the adopt the instant the CLI exits. Re-arm refreshes the
      // deadline + bypass choice.
      const expires_at = Date.now() + ADOPT_ARM_MS;
      updateSession(session_id, { adopt_armed_until: expires_at, adopt_armed_skip: skip ? 1 : 0 });
      tick(
        `⧗ ${c.callsign} armed for move-to-tmux — exit the CLI to move it${skip ? ' (unsupervised)' : ''}`,
      );
      onMutate();
      return { status: 200, body: { ok: true, armed: true, expires_at } };
    }

    // ENDED fork: the card is offline — adopt NOW.
    // A deferred call whose arm is already gone stands down silently: the human
    // disarmed inside the grace window (their cancel must win), or another
    // actor already consumed the arm. Not a failure — no ticker line.
    if (deferred && c.adopt_armed_until == null) {
      return { status: 200, body: { ok: true, canceled: true } };
    }
    // A deferred call whose arm has EXPIRED must stand down too: SessionEnd
    // validated the deadline when it scheduled the grace timer, but the move
    // only runs AFTER the grace delay — launching now would start a process
    // past the documented human authorization window. Clear the stale columns
    // and say so once, BEFORE claiming single-flight or touching tmux.
    if (deferred && c.adopt_armed_until != null && c.adopt_armed_until <= Date.now()) {
      updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
      tick(
        `↷ move-to-tmux canceled for ${c.callsign} — the arm deadline expired before the move fired`,
      );
      onMutate();
      return { status: 200, body: { ok: true, canceled: true, expired: true } };
    }
    // Immediate adopt requires an end that is BOTH proven and final — the
    // NOT_RESUMABLE_END allowlist in helpers.mjs owns that judgement (a NULL is
    // unproven, 'presumed' is a silence guess, and 0.7.1's 'superseded' means
    // the session did not stop at all: it continued under a new id after a
    // /clear, and the heir already owns the pane). Resuming any of them mints a
    // second billed session → refuse; arm it instead. Sharing the set with
    // sessionAdoptableNow is what keeps the board's chip and this guard honest
    // about exactly the same cards.
    if (NOT_RESUMABLE_END.has(c.end_reason ?? null)) {
      return {
        status: 409,
        body: {
          ok: false,
          reason:
            c.end_reason === 'superseded'
              ? 'session was superseded by a /clear — its heir owns the card now'
              : 'session has no hook-proven end (presumed/unstamped) — arm it instead',
        },
      };
    }
    // Single-flight: reuse the revive Set so a manual adopt, an armed auto-adopt,
    // and a revive can never race two panes onto one session. The try/finally
    // releases it on EVERY exit path.
    if (revivingSessions.has(session_id)) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `session ${session_id.slice(0, 8)} is already being moved/revived`,
        },
      };
    }
    revivingSessions.add(session_id);
    try {
      // Consume the arm INSIDE the single-flight claim, before any external op:
      // one-shot by construction — whoever acts first (deferred timer, manual
      // click, sweep) burns it, and a failed launch never retries. Skipped when
      // the columns are already clear (the common manual adopt-now).
      if (c.adopt_armed_until != null || c.adopt_armed_skip != null) {
        updateSession(session_id, { adopt_armed_until: null, adopt_armed_skip: null });
      }
      // H-R7: validate ALL resume eligibility BEFORE touching tmux. runCwd is
      // the hook-reported sessions.cwd (the dir claudeTranscriptPath munges) —
      // NEVER sessions.worktree (the git worktree root); adopt creates no
      // worktree, so the spawn row's worktree_path stays NULL.
      const runCwd = c.cwd;
      let st: fs.Stats | null = null;
      try {
        if (runCwd) st = fs.statSync(runCwd);
      } catch {
        /* missing */
      }
      if (!runCwd || !st?.isDirectory()) {
        return { status: 410, body: { ok: false, reason: 'session cwd no longer exists' } };
      }
      if (!fs.existsSync(claudeTranscriptPath(runCwd, session_id))) {
        return { status: 410, body: { ok: false, reason: 'resume transcript no longer exists' } };
      }

      // Window collision defense — mirrors revive minus resurrection. The window
      // name comes from the LIVE callsign (windowName(port, callsign), the
      // enableRemote precedent). Adopt has NO prior ownership claim, so unlike
      // revive there is no self-row to ADOPT: a live claude pane on this name is
      // some other live session → 409; a live pane running anything else (the
      // human repurposed the window) → 409; only a pane PROVEN dead or an
      // expected bare shell is a safe remnant to kill by verified name and reuse.
      const tmux_window = tmuxAdapter.windowName(port, c.callsign);
      const existing = await findScopedWindow(tmux_window);
      if (existing === null) {
        return {
          status: 503,
          body: {
            ok: false,
            reason: 'tmux window lookup failed; adopt held to avoid a duplicate session',
          },
        };
      }
      if (existing && !existing.pane_dead && existing.pane_cmd === 'claude') {
        return {
          status: 409,
          body: { ok: false, reason: `window ${tmux_window} already hosts a live claude pane` },
        };
      }
      if (existing && !existing.pane_dead && !SHELL_RE.test(existing.pane_cmd)) {
        return {
          status: 409,
          body: {
            ok: false,
            reason: `window ${tmux_window} hosts a live '${existing.pane_cmd}' pane — not a dead remnant; refusing to kill it`,
          },
        };
      }
      if (existing) {
        const killed = await tmuxAdapter.killWindowVerified(tmux_window);
        if (!killed.ok && !killed.gone) {
          return {
            status: 500,
            body: { ok: false, reason: killed.error ?? 'tmux kill-window failed' },
          };
        }
      }

      // Launch. adopt carries the LIVE callsign, the hook cwd as both effective
      // and requested cwd, NULL worktree_path, the human's bypass choice, and
      // NEVER remote_control (v1). excludeSpawnId is null — adopt has no prior
      // row, so ANY active owner on the window is a rival to refuse.
      //
      // Gateway routing: unlike revive, adopt has NO evidence to inherit — the
      // session it resumes was started by a human in their own terminal, and
      // whether that terminal had a gateway exported is not knowable from here
      // (the pane is gone by the time we resume). So this is the one path that
      // consults gateway_default, which is exactly what a default is for: the
      // stated answer to "what should a fleet pane do when nothing else says".
      // A human who runs everything through a proxy sets it and adopt follows;
      // anyone else gets Anthropic, as before. Deliberately NOT an error when
      // the profile is half-configured — an adopt is a move, not a new billed
      // session, and refusing it would strand a live conversation outside the
      // board over a settings typo.
      const adoptGateway = gatewayDecision(null);
      return await launchResume({
        session_id,
        callsign: c.callsign,
        tmux_window,
        runCwd,
        requested_cwd: runCwd,
        worktree_path: null,
        skip_permissions: skip,
        remoteWanted: false,
        gatewayEnv: adoptGateway.env,
        excludeSpawnId: null,
        overrideExtra: { adopt_of: session_id },
        note: 'moving to tmux…',
        tickMsg: `⇥ moving ${c.callsign} to tmux (resume ${session_id.slice(0, 8)})${skip ? ' (unsupervised)' : ''}`,
        failReason: 'tmux adopt failed',
        bodyExtra: { adopted: true },
      });
    } finally {
      // Release the single-flight claim on EVERY exit path.
      revivingSessions.delete(session_id);
    }
  }

  // POST /api/spawn/:id/rc — an explicit human board action, relayed as
  // literal TUI input. Never inject into a working/needsyou turn boundary.
  //
  // Single-flight (BUG-052): enableRemote crosses several awaits (window
  // lookup, typeKeys, sendEnter, the harvest race) with no DB state written
  // until the harvest lands, so two concurrent /rc requests for one spawn both
  // passed every gate and typed/submitted `/rc` TWICE — real tmux rendered the
  // concatenation `/rc a/rc a` — while both HTTP calls returned 200. Latch the
  // enable per spawn like the liveness tick: the first caller runs the body,
  // a concurrent caller shares the SAME in-flight promise and never sends a
  // second keystroke sequence. The latch releases once submission AND harvest
  // persistence settle; a later request then re-reads the row and answers
  // idempotently from the stored remote_url.
  const remoteEnables = new Map<string, Promise<unknown>>();
  function enableRemote(spawn_id: string) {
    const inFlight = remoteEnables.get(spawn_id);
    if (inFlight) return inFlight;
    const promise = enableRemoteOnce(spawn_id).finally(() => {
      // Delete only if the map still points at THIS run — a caller that
      // arrived after release starts a fresh entry and must not lose it.
      if (remoteEnables.get(spawn_id) === promise) remoteEnables.delete(spawn_id);
    });
    remoteEnables.set(spawn_id, promise);
    return promise;
  }
  async function enableRemoteOnce(spawn_id: string) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    if (row.kind === 'shell') {
      return {
        status: 409,
        body: { ok: false, reason: 'remote control is unavailable for shell sessions' },
      };
    }
    if (row.status !== 'live') {
      return { status: 409, body: { ok: false, reason: `spawn is ${row.status}, not live` } };
    }
    if (row.remote_control && row.remote_url) {
      return {
        status: 200,
        body: { ok: true, enabled: true, url: row.remote_url, pending: false },
      };
    }
    const session = q.getSession.get(row.session_id);
    if (!session || !['queued', 'idle'].includes(session.col)) {
      return {
        status: 409,
        body: { ok: false, reason: `session is ${session?.col ?? 'missing'}, not queued or idle` },
      };
    }
    const win = await findScopedWindow(row.tmux_window);
    if (win === null) {
      return {
        status: 503,
        body: { ok: false, reason: 'tmux window lookup failed; remote control was not sent' },
      };
    }
    if (!win || win.pane_dead || win.pane_cmd !== 'claude') {
      const observed = !win
        ? 'missing'
        : win.pane_dead
          ? 'dead'
          : `running ${win.pane_cmd || 'unknown'}`;
      return { status: 409, body: { ok: false, reason: `claude pane is not alive (${observed})` } };
    }
    // TOCTOU recheck: findScopedWindow awaited above, and in that gap another
    // client's prompt can move this card from idle/queued to working/needs-you
    // while the pane stays a live `claude` (so the window check still passes).
    // Re-read the turn-state FRESH — the same q.getSession query the initial
    // idle/queued gate used — and bail before typing `/rc`: injecting it into a
    // now-active TUI corrupts the human's in-flight turn. (Mirrors the initial
    // gate; closes the check-then-send race that the window-only recheck missed.)
    const fresh = q.getSession.get(row.session_id);
    if (!fresh || !['queued', 'idle'].includes(fresh.col)) {
      return {
        status: 409,
        body: { ok: false, reason: `session is ${fresh?.col ?? 'missing'}, not queued or idle` },
      };
    }
    // `/rc <name>` — named by callsign so the session the human finds on
    // claude.ai carries the same name as the card on the board. Verified live
    // on CLI 2.1.207: no confirmation dialog, and the
    // https://claude.ai/code/session_… URL lands in the pane's scrollback.
    // Use the LIVE session callsign (a manual re-ticket renames the card but not
    // the frozen spawn.callsign) so claude.ai shows today's name.
    const target = scopedPaneTarget(win);
    // BUG-053: the FINAL turn-state validation happens here, after every
    // remaining pre-mutation await, and text + Enter then go to tmux as ONE
    // send-keys argument list — a single tmux command queue the server runs
    // back-to-back against the pane. The former split (typeKeys → recheck →
    // sendEnter) was self-defeating: the recheck could only fire after the TUI
    // was already mutated, so a session flipping active while typeKeys was in
    // flight got a 409 that left `/rc <callsign>` sitting in the active input
    // buffer, and a flip during sendEnter submitted it outright. A turn-state
    // flip is hook-driven and lands on a LATER event-loop tick than the daemon
    // action that caused it, so checking now and submitting as one tmux
    // invocation never precedes a flip with unsent `/rc` text in the buffer.
    const idleNow = (s: SessionRow | undefined) => s && ['queued', 'idle'].includes(s.col);
    const attempt = async () => {
      const current = q.getSession.get(row.session_id);
      if (!idleNow(current)) {
        return {
          status: 409,
          body: {
            ok: false,
            reason: `session is ${current?.col ?? 'missing'}, not queued or idle`,
          },
        };
      }
      const sent = await tmuxAdapter.typeAndEnter(
        target,
        `/rc ${current?.callsign ?? row.callsign}`,
      );
      if (!sent) {
        return {
          status: 500,
          body: { ok: false, reason: 'failed to type remote-control command into pane' },
        };
      }
      return null; // submitted — proceed to the harvest
    };
    // Serialize daemon control input per pane (the recommended "same lease"):
    // a second /rc for THIS pane waits for the first attempt's type+Enter to
    // settle, then re-validates before it mutates — two concurrent enables can
    // never interleave keystrokes in one input buffer. The promise chain is
    // keyed by pane target and pruned on settle, so the map stays empty in the
    // steady state.
    const prior = rcInputLocks.get(target) ?? Promise.resolve();
    const run = prior
      .catch(() => {
        /* a prior lease's rejection is that attempt's concern, not this one's */
      })
      .then(attempt);
    rcInputLocks.set(target, run);
    let refused: Awaited<ReturnType<typeof attempt>>;
    try {
      refused = await run;
    } finally {
      if (rcInputLocks.get(target) === run) rcInputLocks.delete(target);
    }
    if (refused) return refused;
    const harvest = delayedRemoteHarvest(spawn_id);
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const timed = new Promise<{ pending: boolean; url: string | null }>((resolve) => {
      timeout = setTimeout(() => {
        resolve({ pending: true, url: null });
      }, 6_000);
      timeout.unref();
    });
    const result = await Promise.race([
      harvest.then(({ url }) => ({ pending: false, url })),
      timed,
    ]);
    clearTimeout(timeout);
    return {
      status: 200,
      body: { ok: true, enabled: true, url: result.url, pending: result.pending },
    };
  }

  // POST /api/spawn/:id/kill — name-verified tmux kill-window (404 unknown
  // id, 409 card not offline without force, 410 window already gone).
  // "Stop" needs no endpoint: the board mails the session instead.
  async function spawnKill(spawn_id: string, force: unknown) {
    const row = q.getSpawn.get(spawn_id);
    if (!row) return { status: 404, body: { ok: false, reason: 'no such spawn' } };
    // Every spawn is inserted with its deterministic window name; this proves it
    // to the type system for the owner check and verified kill-window below
    // without altering behavior.
    if (row.tmux_window == null) throw new Error('spawn is missing its tmux window');
    // H-R5: a revive reuses the dead row's tmux_window, so one physical window
    // can be named by several rows across a session's lifetime. Killing by a
    // STALE id would kill the window the NEWEST (revived) row now owns while
    // marking only the old row 'killed' — liveness then disagrees with reality
    // (the new row still says 'live', its pane is dead). Refuse a historical
    // id even under force: the window belongs to whichever non-terminal row
    // most recently claimed it, and that is the only id allowed to kill it.
    const owner = q.currentWindowOwner.get(row.tmux_window);
    if (owner && owner.spawn_id !== spawn_id) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `spawn ${spawn_id} is a historical row; tmux window ${row.tmux_window} is now owned by spawn ${owner.spawn_id} — kill that one`,
          current_spawn_id: owner.spawn_id,
        },
      };
    }
    const c = q.getSession.get(row.session_id);
    if (row.kind !== 'shell' && c && c.col !== 'offline' && force !== true) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: `session ${c.callsign} is ${c.col}, not offline — pass force:true to kill anyway`,
        },
      };
    }
    const res = await tmuxAdapter.killWindowVerified(row.tmux_window);
    if (!res.ok && res.gone) {
      // Discovery: the pane is already gone — settle the row AND the card
      // (same tombstone every terminal row state applies; only reachable for
      // a non-offline card via an explicit force:true).
      if (['spawning', 'stalled', 'live', 'pane-dead'].includes(row.status)) {
        q.setSpawnStatus.run('gone', spawn_id);
        forgetSpawn(spawn_id); // M-G2
        if (c && c.ended_at == null) {
          tombstoneCard(row.session_id, { note: 'spawned pane window gone' }); // D8
        }
        onMutate();
      }
      return { status: 410, body: { ok: false, reason: 'window already gone' } };
    }
    if (!res.ok)
      return { status: 500, body: { ok: false, reason: res.error ?? 'tmux kill-window failed' } };
    q.setSpawnStatus.run('killed', spawn_id);
    forgetSpawn(spawn_id); // M-G2
    if (c && c.ended_at == null) {
      tombstoneCard(row.session_id, { note: 'pane killed from the board' }); // D8
    }
    tick(`🗡 killed pane ${row.tmux_window}${force === true ? ' (forced)' : ''}`);
    onMutate();
    return { status: 200, body: { ok: true, spawn_id, status: 'killed' } };
  }

  // Owned-pane liveness (CONTRACT) — rides the agents-poll tick (~10 s), for
  // spawn rows in spawning|stalled|live:
  //   pane_dead or a bare shell (sh|bash|zsh|zsh-*)  → confidently dead:
  //     row 'pane-dead', card offline with the --resume note
  //   'claude'                                        → alive
  //   anything else (incl. window/tmux unreachable)   → UNKNOWN, NO action —
  //     never confidently dead (firstmate's hard-won rule; a wrong "dead"
  //     costs a duplicate billed session, a wrong "alive" costs nothing).
  // The pane_dead flag matters because remain-on-exit panes keep reporting
  // the ORIGINAL command name after death (verified on tmux 3.7b) — the
  // command string alone would read "claude" forever. (SHELL_RE — the
  // bare-shell command matcher — is a shared helper now; see helpers.mjs.)
  // BUG 3 (hysteresis): consecutive dead reads a LIVE spawn must accumulate
  // before the condemn loop flips it 'pane-dead'. Two is enough to swallow a
  // single transient dead read (~one tick) while still condemning a genuinely
  // dead pane on the very next tick.
  const CONDEMN_DEAD_READS = 2;

  // ------------------------------------------------------ tmux server watchdog
  // The liveness tick is the only thing watching tmux, and it watched SILENTLY:
  // when tmux stopped answering, the board simply froze — every card kept its
  // last state and nothing said why. Announce the transition ONCE (not every
  // ~10s tick), after enough consecutive failures to ride out a blip, and
  // announce the recovery too, so "the board went quiet" is never a mystery.
  const TMUX_UNREACHABLE_READS = 3;
  const tmuxWatchdog = { unreachableStreak: 0, announcedUnreachable: false };
  function noteTmuxUnreachable() {
    // Two ways of having no tmux are NOT outages and must stay silent, or every
    // tick turns into a false alarm: the FLEETDECK_SPAWN_CMD override seam runs
    // without tmux BY CONTRACT (derive.mjs reads a null listing as authoritative
    // absence there), and a box with no tmux installed never had a server to
    // lose. Only a tmux that should be answering and isn't is news.
    if (tmuxAdapter.spawnOverrideCmd?.() || !tmuxAdapter.hasTmux()) return;
    tmuxWatchdog.unreachableStreak += 1;
    if (tmuxWatchdog.announcedUnreachable) return;
    if (tmuxWatchdog.unreachableStreak < TMUX_UNREACHABLE_READS) return;
    tmuxWatchdog.announcedUnreachable = true;
    tick('⚠ tmux is not answering — holding every card as-is until it does');
  }

  /** The fleet's tmux server is PROVEN gone (see tmuxAdapter.fleetServerAbsent).
   * Settle the rows it took down and stand a fresh server back up, so the board
   * stops showing panes that no longer exist and the human's ⟲ revive has
   * somewhere to land. Deliberately does NOT relaunch anyone: resuming an agent
   * spends money, so that decision stays with the human. */
  async function mournFleetServer(rows: SpawnRow[]) {
    tick(
      `💀 the tmux server died — ${rows.length} spawn(s) went with it; ⟲ revive brings them back`,
    );
    for (const row of rows) {
      q.setSpawnStatus.run('gone', row.spawn_id);
      forgetSpawn(row.spawn_id);
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        tombstoneCard(row.session_id, {
          note: `tmux server died — resume with claude --resume ${row.session_id}`,
        });
      }
      logEvent(row.session_id, 'SpawnGone', null, 'tmux server died');
    }
    onMutate();
    // Recovery. Claiming a fresh server also clears the death certificate, so
    // fleetServerAbsent goes false and this cannot fire twice for one death.
    try {
      await tmuxAdapter.ensureSession(port);
      tick('⟲ tmux server restarted — the fleet is ready to revive');
    } catch (err) {
      // Revive still works without a server (it creates one), so this is a
      // convenience, not a dependency. Say so and let the next tick retry.
      tick(`⚠ could not restart the tmux server (${errMessage(err).slice(0, 80)})`);
    }
  }

  // Single-flight (adversarial-review R4): the fire-and-forget reconcile fired
  // from the viewer path (http.mjs, gone pane) and per-card dismiss bypasses the
  // agents-poll scheduler's own in-flight guard, so two ticks could overlap —
  // double-advancing a spawn's condemnStreak past CONDEMN_DEAD_READS in one wall
  // moment, or a stale in-flight tick resurrecting a row spawnKill had just
  // marked 'killed' back to 'live'. Latch the tick to ONE run at a time: a
  // concurrent caller gets the in-flight promise back and never makes a second
  // pass over the rows. Both entry points are covered without touching the
  // scheduler.
  let livenessInFlight: Promise<void> | null = null;
  function spawnLivenessTick() {
    if (livenessInFlight) return livenessInFlight;
    livenessInFlight = runSpawnLivenessTick().finally(() => {
      livenessInFlight = null;
    });
    return livenessInFlight;
  }
  async function runSpawnLivenessTick() {
    // BEFORE the early return below, and before any tmux probe: a fleet whose
    // every row is already settled takes that return, and a card stranded
    // mid-resume is exactly the case where no row is left in flight to probe.
    // Healing only after a probe would mean the wedge outlives the daemon that
    // could clear it. A row this tick settles further down heals on the next one.
    healInterruptedRevives();
    const rows = q.activeSpawns.all();
    // BUG 3: resurrection candidates count too — if EVERY spawn is already
    // 'pane-dead'/'gone' (rows empty) we must still probe tmux, or a fleet
    // that was wholly (and wrongly) condemned could never come back.
    const resurrectable = q.resurrectableSpawns.all();
    if (!rows.length && !resurrectable.length && !spawnState.orphans.length) return;
    const wins = await tmuxAdapter.listScopedWindows(port);
    if (wins === null) {
      noteTmuxUnreachable();
      return;
    } // UNKNOWN: preserve rows, streaks, orphans
    tmuxWatchdog.unreachableStreak = 0;
    if (tmuxWatchdog.announcedUnreachable) {
      tmuxWatchdog.announcedUnreachable = false;
      tick('✓ tmux is answering again');
    }
    // TMUX SERVER WATCHDOG. Per-window absence is deliberately UNKNOWN below
    // ("gone/unreachable at runtime"), which is right for ONE window and wrong
    // for the whole server: when the tmux server itself dies, every row keeps
    // its 'live' status forever, the board keeps showing cards that no longer
    // exist, and Revive answers 409 "spawn is live, not revivable" — the human
    // is stuck with no way to say "it's dead, bring it back". Ask the one
    // question that separates a healthy-but-empty server from a dead one, and
    // act only on proof: a pane cannot outlive the server that ran it, so a
    // proven-absent server settles its rows without probing panes that are
    // provably gone. Same evidence boot reconciliation already condemns on.
    if (!wins.length && rows.length && (await tmuxAdapter.fleetServerAbsent?.(port))) {
      await mournFleetServer(rows);
      return;
    }
    for (const row of rows) {
      const win = wins.find((w) => w.window === row.tmux_window);
      if (!win) continue; // gone/unreachable at runtime = unknown; boot reconciliation owns 'gone'
      // BUG 3 (consistent probe): decide liveness with the SAME signal the
      // resurrect loop below trusts. pane_dead is authoritative; otherwise the
      // ACTIVE-pane command (paneCurrentCommand) is the truth. The scoped
      // win.pane_cmd is the LOWEST-index pane and reads stale on remain-on-exit
      // / split panes — letting it CONDEMN while the resurrect loop RESURRECTS
      // off a different pane was the thrash: a split or flapping window
      // oscillated a card live↔pane-dead every ~10s tick (feed spam, re-waking
      // every watcher/broadcast). Trust one pane for both decisions.
      //   deadSignal: true = looks dead, false = live claude, null = some other
      //   command → UNKNOWN, no action (unchanged firstmate rule).
      let deadSignal: boolean | null;
      let immediateDead = false;
      const shell = row.kind === 'shell';
      const setupPhase = !!row.setup_cmd && (row.status === 'spawning' || row.status === 'stalled');
      if (win.pane_dead) {
        deadSignal = true;
        immediateDead = setupPhase;
      } else if (shell) {
        // A shell card exists to run arbitrary interactive commands. bash,
        // vim, or a hand-started claude are all equally alive.
        deadSignal = false;
      } else if (win.pane_cmd === 'claude') {
        deadSignal = false; // fast path: lowest pane already reads claude, no extra probe needed
      } else {
        const pane = await tmuxAdapter.paneCurrentCommand(scopedPaneTarget(win));
        if (pane && !pane.dead && pane.cmd === 'claude')
          deadSignal = false; // active pane IS a live claude
        else if (pane?.dead) {
          deadSignal = true;
          immediateDead = setupPhase;
        } else if (setupPhase && pane) {
          deadSignal = false; // setup may be sh, sleep, python, npm, …
        } else if (pane && SHELL_RE.test(pane.cmd))
          deadSignal = true; // bare shell remnant
        else deadSignal = null; // failed probe / other cmd → unknown
      }
      if (deadSignal === false) {
        condemnStreak.delete(row.spawn_id); // a live read resets the hysteresis counter
        const registerMs = row.setup_cmd ? SETUP_REGISTER_MS : SPAWN_REGISTER_MS;
        if (row.status === 'spawning' && Date.now() - row.requested_at > registerMs) {
          const note = row.setup_cmd
            ? `pane up but never registered — setup may still be running; check window ${row.tmux_window}`
            : `pane up but never registered — open diagnostics or terminal; window ${row.tmux_window}`;
          // Capture what the human would see BEFORE declaring the stall. The
          // await creates a real race with a late SessionStart hook, so the SQL
          // write is a compare-and-set (`AND status='spawning'`) and we update the
          // card only if it won. A hook that registered meanwhile wins cleanly.
          let detail: string | null = null;
          try {
            const exactSecrets: string[] = [];
            try {
              const token = fs.readFileSync(path.join(home, 'token'), 'utf8').trim();
              if (token) exactSecrets.push(token);
            } catch {
              /* token file unavailable — shape redaction still applies */
            }
            const gateway = resolveGatewayEnv() ?? {};
            for (const [name, value] of Object.entries(gateway)) {
              if (/(TOKEN|KEY|SECRET|PASSWORD)/i.test(name) && value) exactSecrets.push(value);
            }
            detail = stallDiagnosticExcerpt(await tmuxAdapter.capturePane(scopedPaneTarget(win)), {
              secrets: exactSecrets,
            });
          } catch {
            /* diagnostics are best-effort; the stall itself still lands */
          }
          if (!q.setSpawnStalled.run(detail, row.spawn_id).changes) continue;
          // Loud lane, deliberately: a stalled spawn is a human's problem now
          // (fail loud, never auto-respawn). The first late hook re-derives col.
          updateSession(row.session_id, {
            col: 'needsyou',
            notification_type: 'spawn_stalled',
            note,
          });
          const c = q.getSession.get(row.session_id);
          tick(
            `⚠ ${c?.callsign ?? row.callsign} pane is up but never phoned home${detail ? ' — diagnostics captured' : ''}`,
          );
          logEvent(row.session_id, 'SpawnStalled', null, detail ? `${note}\n${detail}` : note);
          onMutate();
        }
        continue; // alive; stalled is fail-loud state only, never remediation
      }
      if (deadSignal === null) {
        condemnStreak.delete(row.spawn_id); // not consecutive dead evidence
        continue; // unknown → no action
      }
      // deadSignal === true: HYSTERESIS — require CONDEMN_DEAD_READS consecutive
      // dead reads before condemning a LIVE spawn, so a single transient dead
      // read cannot condemn a card the resurrect loop would only flip back next
      // tick. A genuinely dead pane just condemns one tick later.
      if (!immediateDead) {
        const streak = (condemnStreak.get(row.spawn_id) ?? 0) + 1;
        if (streak < CONDEMN_DEAD_READS) {
          condemnStreak.set(row.spawn_id, streak);
          continue;
        }
      }
      q.setSpawnStatus.run('pane-dead', row.spawn_id);
      forgetSpawn(row.spawn_id); // M-G2 (also clears the condemnStreak entry)
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        const note = setupPhase
          ? 'pane exited during setup/bring-up — open the terminal for the error'
          : shell
            ? 'shell pane exited — window kept for scrollback'
            : `pane idle — resume with claude --resume ${row.session_id}`;
        tombstoneCard(row.session_id, {
          // D8
          note,
          tickMsg: `💀 ${c.callsign} pane died — window kept for scrollback`,
        });
      }
      onMutate();
    }
    // BUG 3 — resurrection: re-validate the terminal-but-recoverable rows
    // ('pane-dead'/'gone', never 'killed') against tmux and bring back any
    // whose window is a live claude. This is the ONLY exit from those states
    // back to 'live'; without it BUG 1/BUG 2 (and any transient misread) are a
    // permanent one-way door, and revive() deadlocks on the very-alive pane.
    for (const row of resurrectable) {
      const win = wins.find((w) => w.window === row.tmux_window);
      if (!win || win.pane_dead) continue; // gone/dead/unreachable → stays condemned (firstmate rule)
      // A NEWER row may now own this reused window name (a revive lineage);
      // only resurrect when this row is still the window's rightful owner.
      // currentWindowOwner ranks provisioning|spawning|stalled|live|pane-dead
      // newest-first: for a 'pane-dead' row it returns that row unless a newer
      // live row exists; for a 'gone' row (excluded from that query) it returns
      // null when no other row claims the window — both mean "resurrect me".
      const owner = q.currentWindowOwner.get(row.tmux_window);
      if (owner && owner.spawn_id !== row.spawn_id) continue;
      // Second probe confirms 'claude' (the scoped row's pane_cmd can read
      // stale on remain-on-exit panes; pane_dead already screened above). This
      // is the exact liveness test ownedPaneDeliverable trusts to type mail in.
      const pane = await tmuxAdapter.paneCurrentCommand(scopedPaneTarget(win));
      if (!pane || pane.dead || (row.kind !== 'shell' && pane.cmd !== 'claude')) continue;
      // BUG-152: the row and owner were snapshotted BEFORE this await — a
      // human kill (or any terminal transition / newer-row claim) landing
      // during the probe must not be overwritten by the stale verdict.
      // Re-read both: only resurrect a row still 'pane-dead'/'gone' (never
      // 'killed') that still owns its window; the write itself is a
      // compare-and-set (resurrectSpawn returns false when it lost the race).
      const fresh = q.getSpawn.get(row.spawn_id);
      if (!fresh || (fresh.status !== 'pane-dead' && fresh.status !== 'gone')) continue;
      const ownerNow = q.currentWindowOwner.get(row.tmux_window);
      if (ownerNow && ownerNow.spawn_id !== row.spawn_id) continue;
      resurrectSpawn(fresh);
    }
    // Keep the boot-computed orphan list honest: windows that disappear
    // stop being listed (informational only — no ops are ever offered).
    const owned = new Set(q.allSpawns.all().map((r) => r.tmux_window));
    const orphans = wins.filter((w) => !owned.has(w.window)).map((w) => ({ window: w.window }));
    if (JSON.stringify(orphans) !== JSON.stringify(spawnState.orphans)) {
      spawnState.orphans = orphans;
      onMutate();
    }
  }

  // Restart reconciliation (fleetd boot): spawn rows outlive the daemon in
  // SQLite while the panes outlive it in tmux — re-join the two. Rows in
  // spawning|stalled|live whose exact scoped window is gone → 'gone' + card
  // offline; provisioning rows (H-R6, launch interrupted) → gone + offline;
  // scoped windows with no row at all → spawn_orphans ("unadopted" on the
  // board; surfaced, never operated on).
  const spawnState: { orphans: { window: string }[] } = { orphans: [] };
  async function reconcileSpawns() {
    // BOOT TOCTOU: snapshot the rows to reconcile BEFORE awaiting the tmux
    // window list. reconcile exists to settle rows that PRE-EXIST this boot
    // against the current windows; it runs fire-and-forget the instant the
    // server starts listening, so a human's POST /api/spawn (or a revive) can
    // land DURING the listScopedWindows await below. That new spawn inserts its
    // row and flips it 'spawning' synchronously — and if we read `active` AFTER
    // the await, the brand-new row is in the set but its window is absent from a
    // `wins` snapshot taken before the window could exist, so the loop condemns
    // it 'gone' and tombstones the just-created card. (Observed as the
    // intermittent Node-24 "spawn never reaches live": the later first hook
    // finds the row already 'gone' and the live-flip gate skips it.) Reading the
    // candidate rows first excludes anything created concurrently — that spawn
    // is owned by the live spawn/liveness path, never by boot reconciliation.
    // This mirrors spawnLivenessTick, which already snapshots its rows before
    // its own await. Provisioning rows are captured here for the same reason
    // (staleProvisioningSpawns has no age floor, so a fresh provisional row
    // read post-await would be condemned identically).
    const active = q.activeSpawns.all();
    const staleProvisioning = q.staleProvisioningSpawns.all();
    const wins = await tmuxAdapter.listScopedWindows(port);
    if (wins === null) {
      // Failed/malformed boot listing → leave every row UNKNOWN. Never call
      // ensureSession here: an unlinked live socket could create a replacement
      // server and turn inaccessible live panes into a false authoritative empty.
      const count = active.length + staleProvisioning.length;
      // A fresh/empty fleet has no state whose verdict is UNKNOWN. Keeping that
      // expected first-boot condition out of the feed makes real recovery
      // warnings actionable instead of training users to ignore them.
      if (count > 0) {
        tick(
          `⚠ tmux window lookup failed at restart — leaving ${count} spawn row(s) as-is (unknown, not gone)`,
        );
      }
      // Safe on this path too, and strictly better than waiting: the heal reads
      // no tmux state, and an unsettled row still saying 'spawning' is exactly
      // what its predicate treats as in-flight — so an unreachable tmux makes it
      // conservative, never wrong. Without this, a card stranded by the very
      // restart that cannot see tmux stays stranded until a tick gets an answer.
      healInterruptedRevives();
      onMutate();
      return;
    }
    const names = new Set(wins.map((w) => w.window));
    for (const row of active) {
      if (names.has(String(row.tmux_window))) continue;
      q.setSpawnStatus.run('gone', row.spawn_id);
      forgetSpawn(row.spawn_id); // M-G2
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        tombstoneCard(row.session_id, {
          // D8 (reconcile never woke watchers)
          note: 'spawned pane gone (daemon restart reconciliation)',
          tickMsg: `${c.callsign} pane gone — noticed at daemon restart`,
          notify: false,
        });
      }
      onMutate();
    }
    // H-R6: a 'provisioning' row is a spawn that died between its durable
    // insert and the completion of its external ops (worktree/window). It
    // never had a live pane; boot is where we finish the job — settle the row
    // and tombstone the half-born card. (Snapshot taken pre-await above so a
    // spawn created concurrently with reconcile is not swept up as stale.)
    for (const row of staleProvisioning) {
      // A daemon death during git clone leaves only the request-scoped temp
      // directory. The final destination is never removed here: it may have
      // pre-existed or the rename may have completed before the crash.
      try {
        fs.rmSync(`${row.cwd}.fd-cloning-${row.spawn_id.slice(0, 8)}`, {
          recursive: true,
          force: true,
        });
      } catch {
        /* best effort */
      }
      // BUG-153: the daemon can also die AFTER the spawn's worktree was
      // created and persisted but BEFORE its pane launched — the gap this
      // loop settles. Without removal here that worktree (its directory, its
      // fd/ branch, its git registration) strands on every such restart, and
      // repeated interruptions accumulate trees that can block later branch
      // or worktree creation. The verified-removal half of spawnCompensate —
      // the same removal the normal failure path applies — runs ONLY for a
      // spawn-owned tree (worktree_owned === 1: a fleet-created worktree is
      // fresh and holds no human work). A reused tree (0) or a pre-fix row
      // (NULL — the bit was never recorded) is left exactly as before.
      if (row.worktree_path && row.worktree_owned === 1) {
        try {
          const branch = branchOf(row.worktree_path, { fresh: true });
          const rm = await execFileP(
            'git',
            ['-C', String(row.cwd), 'worktree', 'remove', '--force', row.worktree_path],
            { timeout: 30_000 },
          );
          if (!rm.ok) {
            try {
              fs.rmSync(row.worktree_path, { recursive: true, force: true });
            } catch {
              /* best effort */
            }
          }
          await execFileP('git', ['-C', String(row.cwd), 'worktree', 'prune'], { timeout: 30_000 });
          // The spawn died before its pane launched, so the fd/ branch the
          // worktree add created holds no human work either — delete it with
          // the tree or it strands and blocks a same-named worktree later.
          if (branch)
            await execFileP('git', ['-C', String(row.cwd), 'branch', '-D', branch], {
              timeout: 30_000,
            });
          tick(
            `🧹 removed stranded worktree ${row.worktree_path} — spawn ${row.callsign} was interrupted before launch`,
          );
        } catch {
          /* best effort — a stranded fleet worktree is surfaced by cleanup() */
        }
      }
      q.setSpawnStatus.run('gone', row.spawn_id);
      forgetSpawn(row.spawn_id);
      const c = q.getSession.get(row.session_id);
      if (c && c.ended_at == null) {
        tombstoneCard(row.session_id, {
          // D8 (reconcile never woke watchers)
          note: 'spawn interrupted before launch (daemon restart)',
          tickMsg: `${c.callsign} spawn was interrupted before launch — cleaned up at restart`,
          notify: false,
        });
      }
      onMutate();
    }
    const owned = new Set(q.allSpawns.all().map((r) => r.tmux_window));
    spawnState.orphans = wins
      .filter((w) => !owned.has(w.window))
      .map((w) => ({ window: w.window }));
    if (spawnState.orphans.length) {
      tick(
        `⚠ ${spawnState.orphans.length} unadopted fleetdeck window(s) in tmux (fd${port}-* with no spawn row)`,
      );
      onMutate();
    }
    // LAST, deliberately: the loops above settle rows this boot inherited, and
    // the heal's whole predicate is "no row is in flight". Running it first would
    // read a row this daemon has not settled yet — still 'spawning' from the dead
    // daemon — decide the revive is live, and leave the card stranded anyway.
    healInterruptedRevives();
  }

  // Release a card stranded mid-resume. See q.staleRevivingSessions for why this
  // cannot race a real in-flight revive (the provisional row always precedes the
  // card flip). Pure SQLite: it asks tmux nothing, which is what lets the
  // liveness tick call it even on the paths that return before probing.
  function healInterruptedRevives() {
    const stranded = q.staleRevivingSessions.all();
    for (const row of stranded) {
      updateSession(row.session_id, {
        col: 'offline',
        note: 'revive was interrupted before the pane registered — revive again',
      });
      tick(`⟲ ${row.callsign} revive was interrupted — the card is revivable again`);
    }
    if (stranded.length) onMutate();
    return stranded.length;
  }

  // 0.7.1 boot heal — the retroactive half of /clear succession.
  //
  // Sessions forked by a /clear BEFORE succession shipped are still sitting on
  // the board as split pairs: the predecessor holds the spawn row and the tmux
  // window (so the human's terminal button drives a session that never updates)
  // while its heir collects every hook with no pane to its name. Those rows have
  // no cleared_at — the column did not exist when they forked — so this
  // reconstructs the link from the EVENT LOG instead (retained 24h, which is the
  // window in which a stranded pair is still live enough to matter).
  //
  // Detection mirrors the hook-time rule exactly: a session whose LAST event is a
  // /clear end, and a brand-new session born from a clear moments later in the
  // same cwd that never got a pane of its own. Idempotent (succeedSession stamps
  // succeeded_by, and a healed predecessor is archived out of the candidate set),
  // and a no-op on a fleet that never forked.
  function reconcileClearForks() {
    let healed = 0;
    // Walk the HEIRS, not the predecessors. Walking predecessors invites two of
    // them to claim the same heir (two pane-less sessions clearing minutes apart
    // in one repo both see the earlier heir as "theirs"), which would merge two
    // unrelated conversations onto one card — strictly worse than the split this
    // heals. One heir continues at most one lineage, and it must be unambiguous
    // about WHICH: exactly the rule the live path enforces.
    const born = q.clearBornSessionsSince.all(Date.now() - 24 * 3600 * 1000, Date.now());
    for (const b of born) {
      const heir = q.getSession.get(b.session_id);
      if (!heir || heir.archived_at != null || heir.ended_at != null) continue;
      if (heir.succeeded_by != null) continue; // it was itself superseded later
      if (q.successorClaimed.get(heir.session_id)) continue; // already healed → IDEMPOTENT
      if (q.spawnBySession.get(heir.session_id)) continue; // owns a pane → nothing was stranded
      if (!heir.cwd) continue;

      // Its predecessor: a still-live card in the same cwd whose LAST word was a
      // /clear, in the correlation window before this heir was born. Rows are
      // re-read FRESH every iteration — an earlier heir may already have renamed
      // one (a session that clears twice in a day forms a chain), and inheriting
      // from a stale snapshot would hand this heir a callsign nobody has ever
      // seen, while the name the human knows would belong to no visible card.
      const cands: { row: SessionRow; at: number }[] = [];
      for (const p of q.visibleSessions.all()) {
        if (p.session_id === heir.session_id) continue;
        if (p.ended_at != null || p.succeeded_by != null) continue; // already claimed
        if (p.cwd !== heir.cwd) continue;
        const last = q.lastEventOf.get(p.session_id);
        if (last?.hook_event !== 'SessionEnd') continue;
        if (!(last.note ?? '').startsWith('context cleared')) continue;
        if (last.at < b.at - CLEAR_SUCCESSION_MS || last.at > b.at + 2_000) continue;
        cands.push({ row: p, at: last.at });
      }
      if (!cands.length) continue;
      // Pair only on PROOF, never on order. Event arrival order is not lineage
      // identity: two sessions can /clear in one cwd with end order A,B while
      // their heirs are born B′,A′ (SessionEnd is an async hook, SessionStart
      // is not, so nothing orders one lineage's two hooks against the
      // other's). Handing each heir the OLDEST unclaimed clear then
      // cross-wires the lineages — A′ inherits B's callsign, pane, mail and
      // questions, and the merge is irreversible. The live path has known this
      // all along (findClearedPredecessor: "an ambiguous match is simply not a
      // match"); the heal must hold the same line, so it merges only when the
      // candidate set corroborates itself, never on (time-window, cwd) alone.
      // Two cases are unambiguous:
      //
      //  - exactly ONE candidate: the only clear in this heir's window is its
      //    parent, no pairing decision to get wrong;
      //  - exactly ONE candidate owns a live pane: pane ownership is
      //    independent corroboration (a stranded pane is also the case that
      //    actually costs something).
      //
      // Anything else — several pane-less clears, or several paned ones —
      // stays split. A spare card is recoverable by hand; a wrong merge is not.
      let prev: SessionRow | null = null;
      if (cands.length === 1) {
        prev = cands[0]?.row ?? null;
      } else {
        const paned = cands.filter((c) => hasLivePane(c.row.session_id));
        if (paned.length === 1) prev = paned[0]?.row ?? null;
      }
      if (!prev) continue;
      if (succeedSession(prev, heir.session_id, { rename: true })) healed += 1;
    }
    if (healed) {
      tick(`🧹 healed ${healed} card${healed === 1 ? '' : 's'} split by a /clear before 0.7.1`);
      onMutate();
    }
    return { healed };
  }

  return {
    spawn,
    revive,
    adoptSession,
    enableRemote,
    spawnKill,
    spawnCapability,
    spawnLivenessTick,
    reconcileSpawns,
    reconcileClearForks,
    scheduleRegistrationRemoteHarvest,
    forgetSpawn,
    spawnState,
    armUnsupervised,
  };
}
