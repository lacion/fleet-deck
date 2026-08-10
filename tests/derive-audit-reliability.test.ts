// tests/derive-audit-reliability.test.ts
//
// Regression tests for the derive.mjs audit-cleanup wave (owned by the
// derive.mjs agent). Each test names the audit finding it pins:
//   H-R1  removeWorktree must not rm -rf uncommitted work without force
//   H-R2  boot reconciliation must not tombstone the fleet on a tmux hiccup
//   H-R5  a stale spawn_id must not kill a newer revived session
//   H-R6  a launch failure must leave no orphan worktree/window/row
//   H-R7  revive validates eligibility BEFORE touching tmux
//   M-B2  a PreToolUse conflict whisper carries hookEventName:'PreToolUse'
//   M-B4  a corrupt conflicts row must not 500 /state
//   M-B5  a resurrected card leaves the offline column on tool activity
//   M-B6  ExitPlanMode question + plan are inserted atomically
//   M-B8  a guarded remote harvest never rejects
//   M-G1  the append-only ledgers are aged out; snapshot windows touches
//   R2-5  a stale-id kill during a revive's window creation is refused
//   R2-6  snapshot/cleanup drop a conflicts row whose sessions_json is
//         valid JSON of the wrong shape ('{}', 'null', a string)
//   R2-7  the per-card file cap keeps a card's NEWEST touches, not oldest
//   R2-8  removeWorktree's FINAL git-status guard refuses a TOCTOU-dirty tree
//
// These drive createCore() directly with an injected fake tmux adapter and an
// in-memory SQLite db — the same harness shape as daemon-maintenance.test.mjs,
// but self-contained (no edits to tests/helpers/*).

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.ts';
import { createStatements } from '../scripts/fleetd/statements.ts';
import { claudeTranscriptPath, createCore } from '../scripts/fleetd/derive.ts';

const HOUR = 3_600_000;

// The harness bridges a narrow, test-owned fake onto createCore's tmux seam.
// The double-cast through `unknown` is the documented sibling idiom
// (dismiss.test.ts / fleet-bugs.test.ts): FakeTmuxAdapter is a usage-shaped
// slice, not the full spawn.ts surface, so a structural assignment would fail.
type Db = ReturnType<typeof openDb>;
type Core = ReturnType<typeof createCore>;
type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;
type Snapshot = ReturnType<Core['snapshot']>;

interface FakeWindow {
  session: string;
  window: string;
  window_id: string;
  pane_dead: boolean;
  pane_cmd: string;
}
interface FakeTmuxState {
  windows: FakeWindow[];
  killed: string[];
  calls: unknown[];
  argv: unknown;
}
interface NewWindowSpec {
  argv: unknown;
  port: number;
  callsign: string;
}
interface NewWindowResult {
  session: string;
  window: string;
  window_id: string;
}
interface PaneCommand {
  dead: boolean;
  cmd: string;
}
interface KillResult {
  ok: boolean;
  window_id?: string;
  error?: string;
}
interface FakeTmuxAdapter {
  spawnOverrideCmd: () => null;
  hasTmux: () => boolean;
  sessionName: (p: number) => string;
  windowName: (p: number, callsign: string) => string;
  ensureSession: (p: number) => Promise<string>;
  newWindow: (spec: NewWindowSpec) => Promise<NewWindowResult>;
  listScopedWindows: () => Promise<FakeWindow[] | null>;
  paneCurrentCommand: (target: string) => Promise<PaneCommand | null>;
  killWindowVerified: (name: string) => Promise<KillResult>;
  pasteText: () => Promise<boolean>;
  sendEnter: () => Promise<boolean>;
  typeKeys: () => Promise<boolean>;
  typeAndEnter: () => Promise<boolean>;
  sendBringupEnter: () => Promise<boolean>;
  capturePane: () => Promise<string>;
  launchOverride: () => void;
  fleetServerAbsent?: () => Promise<boolean>;
}

// Result shapes for the core methods that return `unknown` on the createCore
// surface (spawn/revive/spawnKill/enableRemote/removeWorktree/cleanup) — read
// only the fields the assertions touch. snapshot() and the hook* methods are
// already typed by their factories, so they are consumed directly.
interface SpawnResult {
  status: number;
  body: { spawn_id: string; session_id: string; reason?: string };
}
interface ReviveResult {
  status: number;
  body: { spawn_id?: string; reason?: string; tmux?: { window: string } };
}
interface SpawnKillResult {
  status: number;
  body: { current_spawn_id?: string };
}
interface EnableRemoteResult {
  status: number;
  body: { ok: boolean; url: string | null };
}
interface RemoveWorktreeResult {
  status: number;
  body: { ok?: boolean; verdict?: string; removed?: boolean };
}
interface CleanupResult {
  ok: boolean;
}
interface HookOutput {
  hookSpecificOutput?: { hookEventName: string; additionalContext?: string };
}

// Row shapes for the untyped `db.prepare(...)` reads. The seam's default row
// type is `Record<string, SqlValue>`, whose members are only bracket-accessible
// under noPropertyAccessFromIndexSignature — so each read carries an explicit
// generic to keep dot access and give `.get()` a precise `T | undefined`.
interface StatusRow {
  status: string;
}
interface ColRow {
  col: string;
}
interface CountRow {
  n: number;
}
interface RelPathRow {
  rel_path: string;
}
interface SpawnStarRow {
  status: string;
  callsign: string;
  session_id: string;
  spawn_id: string;
}
interface SpawnIdStatusRow {
  spawn_id: string;
  status: string;
}
interface StatusArchivedRow {
  status: string;
  archived_at: number | null;
}
interface SessionStarRow {
  col: string;
  note: string | null;
  ended_at: number | null;
  end_reason: string | null;
  model: string | null;
  last_tool: string | null;
  task: string | null;
  source: string | null;
  events: number;
}
interface ColEventsNoteRow {
  col: string;
  events: number;
  note: string | null;
}
interface ColEventsRow {
  col: string;
  events: number;
}
interface PlanRow {
  plan_md: string;
  status: string;
}

const noop = (): void => {
  /* test stub */
};

function setEnv(t: TestContext, values: Record<string, string | number>): void {
  const before = new Map<string, string | undefined>(
    Object.keys(values).map((k): [string, string | undefined] => [k, process.env[k]]),
  );
  for (const [k, v] of Object.entries(values)) process.env[k] = String(v);
  t.after(() => {
    for (const [k, v] of before) {
      if (v === undefined) Reflect.deleteProperty(process.env, k);
      else process.env[k] = v;
    }
  });
}

// A controllable stand-in for scripts/fleetd/spawn.mjs. Defaults model a
// reachable tmux with one live claude window created per newWindow(); override
// any method per test. state.killed / state.windows / state.calls are the
// observable surface.
function makeAdapter(
  port = 4711,
  overrides: Partial<FakeTmuxAdapter> = {},
): { state: FakeTmuxState; adapter: FakeTmuxAdapter; port: number } {
  const state: FakeTmuxState = { windows: [], killed: [], calls: [], argv: null };
  const adapter: FakeTmuxAdapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: (p) => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: (p) => Promise.resolve(`fleetdeck-${p}`),
    newWindow: (spec) => {
      state.argv = spec.argv;
      const win: FakeWindow = {
        session: `fleetdeck-${spec.port}`,
        window: `fd${spec.port}-${spec.callsign}`,
        window_id: '@1',
        pane_dead: false,
        pane_cmd: 'claude',
      };
      state.windows.push(win);
      return Promise.resolve({
        session: win.session,
        window: win.window,
        window_id: win.window_id,
      });
    },
    listScopedWindows: () => Promise.resolve(state.windows),
    paneCurrentCommand: (target) => {
      const w = state.windows.find((x) => x.window_id === target || x.window === target);
      return Promise.resolve(w ? { dead: w.pane_dead, cmd: w.pane_cmd } : null);
    },
    killWindowVerified: (name) => {
      state.killed.push(name);
      return Promise.resolve({ ok: true, window_id: '@1' });
    },
    pasteText: () => Promise.resolve(true),
    sendEnter: () => Promise.resolve(true),
    typeKeys: () => Promise.resolve(true),
    typeAndEnter: () => Promise.resolve(true),
    sendBringupEnter: () => Promise.resolve(true),
    capturePane: () => Promise.resolve(''),
    launchOverride: noop,
    ...overrides,
  };
  return { state, adapter, port };
}

// createCore with the maintenance-timer knobs pinned high so no unref timer
// interferes with a synchronous assertion.
function memoryCore(
  t: TestContext,
  {
    tmux = makeAdapter(),
    home = '/daemon-home',
    env = {},
  }: {
    tmux?: ReturnType<typeof makeAdapter>;
    home?: string;
    env?: Record<string, string | number>;
  } = {},
): {
  db: Db;
  core: Core;
  state: FakeTmuxState;
  adapter: FakeTmuxAdapter;
  port: number;
  home: string;
} {
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000, ...env });
  const db = openDb(':memory:');
  t.after(() => {
    try {
      db.close();
    } catch {
      /* already closed */
    }
  });
  const core = createCore(db, {
    port: tmux.port,
    home,
    tmuxAdapter: tmux.adapter as unknown as CoreTmuxAdapter,
  });
  return { db, core, ...tmux, home };
}

function git(args: string[], cwd: string): string {
  return execFileSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

// A throwaway repo with one commit; ready for `git worktree add`.
function initRepo(t: TestContext, name = 'repo'): { base: string; root: string } {
  const base = mkdtempSync(path.join(tmpdir(), 'fd-derive-git-'));
  const root = path.join(base, name);
  mkdirSync(root, { recursive: true });
  git(['init', '-q', '-b', 'main'], root);
  git(['config', 'user.email', 't@fleetdeck.local'], root);
  git(['config', 'user.name', 'Fleet Deck Tests'], root);
  writeFileSync(path.join(root, 'a.txt'), 'one\n');
  git(['add', '-A'], root);
  git(['commit', '-qm', 'base'], root);
  t.after(() => {
    rmSync(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { base, root };
}

function ownWorktree(
  db: Db,
  {
    sessionId,
    callsign = 'otter',
    spawnId,
    cwd,
    worktreePath,
  }: { sessionId: string; callsign?: string; spawnId: string; cwd: string; worktreePath: string },
): void {
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions
    (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, archived_at, source)
    VALUES (?, ?, ?, 'wt', 'offline', 'test', 0, ?, ?, ?, ?, 'spawned')`,
  ).run(sessionId, callsign, worktreePath, now, now, now, now);
  db.prepare(
    `INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
    VALUES (?, ?, ?, 'fleetdeck-4711', ?, ?, ?, ?, 'gone')`,
  ).run(spawnId, sessionId, callsign, `fd4711-${callsign}`, cwd, worktreePath, now);
}

// ---------------------------------------------------------------------------
// H-R1
// ---------------------------------------------------------------------------

test('H-R1: the rmSync fall-through preserves uncommitted work when force is not set, but still cleans a clean-but-locked worktree', async (t) => {
  const { db, core } = memoryCore(t);
  const { base, root } = initRepo(t, 'repo');

  // A clean worktree that git nonetheless REFUSES to `worktree remove` without
  // --force (it is locked). Reaching the fall-through, the new guard reads a
  // fresh `git status --porcelain`, finds it empty, and removes it — the benign
  // half-removed/locked recovery must still work.
  const cleanWt = path.join(base, 'repo--fd-clean');
  git(['worktree', 'add', '-q', '-b', 'fd/clean', cleanWt], root);
  git(['worktree', 'lock', cleanWt], root);
  ownWorktree(db, {
    sessionId: 's-clean',
    callsign: 'clean',
    spawnId: 'sp-clean',
    cwd: root,
    worktreePath: cleanWt,
  });

  const okRes = await (core.removeWorktree({ path: cleanWt }) as Promise<RemoveWorktreeResult>); // no force
  assert.equal(
    okRes.status,
    200,
    `a clean (if locked) worktree is removable without force: ${JSON.stringify(okRes.body)}`,
  );
  assert.equal(existsSync(cleanWt), false, 'the clean worktree is gone from disk');

  // A DIRTY worktree with uncommitted, unignored work. A request that never
  // set force must NOT destroy it — the file survives and a 409 is returned.
  const dirtyWt = path.join(base, 'repo--fd-dirty');
  git(['worktree', 'add', '-q', '-b', 'fd/dirty', dirtyWt], root);
  writeFileSync(path.join(dirtyWt, 'precious.txt'), 'UNCOMMITTED — do not delete\n');
  ownWorktree(db, {
    sessionId: 's-dirty',
    callsign: 'dirty',
    spawnId: 'sp-dirty',
    cwd: root,
    worktreePath: dirtyWt,
  });

  const refused = await (core.removeWorktree({ path: dirtyWt }) as Promise<RemoveWorktreeResult>); // no force
  assert.equal(refused.status, 409, 'a dirty worktree without force must be refused');
  assert.equal(refused.body.ok, false);
  assert.equal(
    existsSync(path.join(dirtyWt, 'precious.txt')),
    true,
    'uncommitted work is never destroyed without force',
  );

  // ...and force still removes it.
  const forced = await (core.removeWorktree({
    path: dirtyWt,
    force: true,
  }) as Promise<RemoveWorktreeResult>);
  assert.equal(
    forced.status,
    200,
    `force removes the dirty worktree: ${JSON.stringify(forced.body)}`,
  );
  assert.equal(existsSync(dirtyWt), false, 'force:true does remove the dirty worktree from disk');
});

// ---------------------------------------------------------------------------
// H-R2
// ---------------------------------------------------------------------------

test('H-R2: boot reconciliation leaves rows UNKNOWN on list failure without creating a replacement server', async (t) => {
  function seedActiveSpawn(db: Db): void {
    const now = Date.now();
    db.prepare(
      `INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
      VALUES ('s1', 'a1', 'working', 'running', 1, ?, ?, 'spawned')`,
    ).run(now, now);
    db.prepare(
      `INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
      VALUES ('sp1', 's1', 'a1', 'fleetdeck-4711', 'fd4711-a1', ?, 'live')`,
    ).run(now);
  }

  // Failed listing is explicitly UNKNOWN. Reconciliation must not call
  // ensureSession: with an unlinked live socket that would create a replacement
  // server and make the inaccessible original fleet look authoritatively gone.
  {
    let ensureCalls = 0;
    const tmux = makeAdapter(4711, {
      listScopedWindows: () => Promise.resolve(null),
      ensureSession: () => {
        ensureCalls++;
        return Promise.resolve('fleetdeck-4711');
      },
    });
    const { db, core } = memoryCore(t, { tmux });
    seedActiveSpawn(db);
    const now = Date.now();
    db.prepare(
      `INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
      VALUES ('s2', 'b2', 'queued', 'provisioning', 0, ?, ?, 'spawned')`,
    ).run(now, now);
    db.prepare(
      `INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
      VALUES ('sp2', 's2', 'b2', 'fleetdeck-4711', 'fd4711-b2', ?, 'provisioning')`,
    ).run(now);
    await (core.reconcileSpawns() as Promise<void>);
    assert.equal(
      db.prepare<StatusRow>("SELECT status FROM spawns WHERE spawn_id='sp1'").get()?.status,
      'live',
      'tmux unreachable → the live row stays live (unknown), never gone',
    );
    assert.equal(
      db.prepare<ColRow>("SELECT col FROM sessions WHERE session_id='s1'").get()?.col,
      'working',
      'tmux unreachable → the card is not tombstoned offline',
    );
    assert.equal(
      db.prepare<StatusRow>("SELECT status FROM spawns WHERE spawn_id='sp2'").get()?.status,
      'provisioning',
      'tmux UNKNOWN also leaves interrupted provisioning rows unchanged',
    );
    assert.equal(
      db.prepare<ColRow>("SELECT col FROM sessions WHERE session_id='s2'").get()?.col,
      'queued',
    );
    assert.equal(
      ensureCalls,
      0,
      'reconciliation never creates/probes a session after an unknown list',
    );
    assert.ok(
      core.snapshot().ticker.some((x) => x.msg?.includes('tmux window lookup failed at restart')),
      'the skip is announced on the feed',
    );
  }

  // Reachable but the fleet owns no windows: the empty list is authoritative,
  // so the stale row IS reconciled to gone + offline (the existing contract).
  {
    let ensureCalls = 0;
    const tmux = makeAdapter(4711, {
      listScopedWindows: () => Promise.resolve([]),
      ensureSession: () => {
        ensureCalls++;
        return Promise.reject(new Error('must not be called'));
      },
    });
    const { db, core } = memoryCore(t, { tmux });
    seedActiveSpawn(db);
    await (core.reconcileSpawns() as Promise<void>);
    assert.equal(
      db.prepare<StatusRow>("SELECT status FROM spawns WHERE spawn_id='sp1'").get()?.status,
      'gone',
      'tmux reachable + no windows → the row is reconciled to gone',
    );
    assert.equal(
      db.prepare<ColRow>("SELECT col FROM sessions WHERE session_id='s1'").get()?.col,
      'offline',
      'tmux reachable + no windows → the card is tombstoned offline',
    );
    assert.equal(ensureCalls, 0, 'validated empty needs no reachability side effect');
  }
});

// tmux server watchdog. Per-window absence is UNKNOWN at runtime, which is
// right for one window and wrong for the whole server: a SIGKILLed tmux left
// every row 'live' forever, so the board showed panes that no longer existed
// and revive answered 409 "spawn is live, not revivable" — no way out. The
// watchdog acts ONLY on proof that the server itself is gone.
test('tmux watchdog: a proven-dead server settles its fleet, says so, and stands a new server up', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-watchdog-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-watchdog-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  let serverAbsent = false;
  const ensured: number[] = [];
  const tmux = makeAdapter(4711, {
    fleetServerAbsent: () => Promise.resolve(serverAbsent),
    ensureSession: (p: number) => {
      ensured.push(p);
      return Promise.resolve(`fleetdeck-${p}`);
    },
  });
  const { db, core, state } = memoryCore(t, { tmux, env: { HOME: userHome } });

  const { body } = await (core.spawn({ cwd }) as Promise<SpawnResult>);
  const { spawn_id: spawnId, session_id: sid } = body;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });
  db.prepare("UPDATE spawns SET status='live' WHERE spawn_id=?").run(spawnId);
  const statusOf = (): string | undefined =>
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(spawnId)?.status;
  ensured.length = 0; // spawn() stood the fleet session up; watch only what the tick does

  // A HEALTHY server that simply lists no windows must never condemn: the
  // window went missing, which is exactly the UNKNOWN the tick already owns.
  state.windows.length = 0;
  await core.spawnLivenessTick();
  assert.equal(statusOf(), 'live', 'an empty listing from a reachable server condemns nothing');
  assert.deepEqual(ensured, [], 'and never restarts a server that is already there');

  // Now the server itself is provably gone — a pane cannot outlive it.
  serverAbsent = true;
  await core.spawnLivenessTick();
  assert.equal(statusOf(), 'gone', 'the rows it took down are settled, so revive is offered');
  const died = core.snapshot().ticker.filter((x) => x.msg?.includes('tmux server died'));
  assert.equal(died.length, 1, 'the loss is announced exactly once');
  assert.match(died[0]?.msg ?? '', /1 spawn\(s\) went with it/);
  assert.deepEqual(ensured, [tmux.port], 'and a fresh fleet session is stood back up');

  // Self-limiting: the fleet is settled, so a second tick re-announces nothing.
  await core.spawnLivenessTick();
  assert.equal(
    core.snapshot().ticker.filter((x) => x.msg?.includes('tmux server died')).length,
    1,
    'the watchdog does not re-announce a death it already settled',
  );
});

test('tmux watchdog: an unreachable tmux is announced once and never condemns anything', async (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-watchdog-unknown-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  let reachable = true;
  const tmux = makeAdapter(4711, {
    listScopedWindows: () => Promise.resolve(reachable ? [] : null),
    // A failed probe must never be mistaken for proof the server is gone.
    fleetServerAbsent: () => Promise.resolve(false),
  });
  const { db, core } = memoryCore(t, { tmux });
  const { body } = await (core.spawn({ cwd }) as Promise<SpawnResult>);
  db.prepare("UPDATE spawns SET status='live' WHERE spawn_id=?").run(body.spawn_id);

  reachable = false;
  const held = (): number =>
    core.snapshot().ticker.filter((x) => x.msg?.includes('tmux is not answering')).length;
  await core.spawnLivenessTick();
  assert.equal(held(), 0, 'a single failed read is a blip, not news');
  await core.spawnLivenessTick();
  await core.spawnLivenessTick();
  assert.equal(held(), 1, 'a sustained outage is announced');
  await core.spawnLivenessTick();
  assert.equal(held(), 1, 'and announced only once');
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(body.spawn_id)?.status,
    'live',
    'an unreachable tmux never condemns a row',
  );

  reachable = true;
  await core.spawnLivenessTick();
  assert.ok(
    core.snapshot().ticker.some((x) => x.msg?.includes('tmux is answering again')),
    'recovery is announced too, so a frozen board is never a mystery',
  );
});

test('revive and adopt return 5xx on UNKNOWN window lookup without launching', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-unknown-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-unknown-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  let ensureCalls = 0;
  let newWindowCalls = 0;
  const tmux = makeAdapter(4711, {
    listScopedWindows: () => Promise.resolve(null),
    ensureSession: () => {
      ensureCalls++;
      return Promise.resolve('fleetdeck-4711');
    },
    newWindow: () => {
      newWindowCalls++;
      return Promise.reject(new Error('must not launch'));
    },
  });
  const { db, core } = memoryCore(t, { tmux, env: { HOME: userHome } });
  const now = Date.now();

  db.prepare(
    `INSERT INTO sessions
    (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, end_reason, source)
    VALUES ('revive-unknown', 'otter', ?, 'offline', 'ended', 0, ?, ?, ?, 'other', 'spawned')`,
  ).run(cwd, now, now, now);
  db.prepare(
    `INSERT INTO spawns
    (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES ('sp-unknown', 'revive-unknown', 'otter', 'fleetdeck-4711', 'fd4711-otter', ?, ?, 'gone')`,
  ).run(cwd, now);
  const reviveTranscript = claudeTranscriptPath(cwd, 'revive-unknown', userHome);
  mkdirSync(path.dirname(reviveTranscript), { recursive: true });
  writeFileSync(reviveTranscript, '{}\n');

  const revived = await (core.revive('sp-unknown') as Promise<ReviveResult>);
  assert.equal(revived.status, 503);
  assert.match(revived.body.reason ?? '', /lookup failed.*duplicate/i);
  assert.equal(
    db.prepare<StatusRow>("SELECT status FROM spawns WHERE spawn_id='sp-unknown'").get()?.status,
    'gone',
  );

  db.prepare(
    `INSERT INTO sessions
    (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, end_reason, source)
    VALUES ('adopt-unknown', 'badger', ?, 'offline', 'ended', 0, ?, ?, ?, 'other', 'hooks')`,
  ).run(cwd, now, now, now);
  const adoptTranscript = claudeTranscriptPath(cwd, 'adopt-unknown', userHome);
  writeFileSync(adoptTranscript, '{}\n');

  // The core surface hand-declares adoptSession(sid, opts, meta) with all three
  // required; the .mjs called it with one arg (opts/meta defaulted at runtime).
  // { dangerously_skip_permissions: false } + { deferred: false } reproduce
  // those defaults exactly: skip=false takes the same non-armed path as the
  // 1-arg call, and unsupervisedGate(false, ...) never refuses.
  const adopted = await core.adoptSession(
    'adopt-unknown',
    { dangerously_skip_permissions: false },
    { deferred: false },
  );
  assert.ok(adopted, 'adopt returned a response object');
  assert.equal(adopted.status, 503);
  assert.match(adopted.body?.reason ?? '', /lookup failed.*duplicate/i);
  assert.equal(
    db.prepare<CountRow>("SELECT COUNT(*) AS n FROM spawns WHERE session_id='adopt-unknown'").get()
      ?.n,
    0,
  );
  assert.equal(ensureCalls, 0, 'UNKNOWN lookup never reaches ensureSession');
  assert.equal(newWindowCalls, 0, 'UNKNOWN lookup never reaches newWindow');
});

// ---------------------------------------------------------------------------
// H-R5
// ---------------------------------------------------------------------------

test('H-R5: killing by a stale (historical) spawn_id is refused; the newest owner is killable', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-hr5-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hr5-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core, state } = memoryCore(t, { env: { HOME: userHome } });

  const first = await (core.spawn({ cwd }) as Promise<SpawnResult>);
  const { spawn_id: oldId, session_id: sid } = first.body;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // The session dies; its row is terminal and the pane is dead — the exact
  // precondition for a revive that reuses the same tmux_window.
  db.prepare("UPDATE spawns SET status='gone' WHERE spawn_id=?").run(oldId);
  db.prepare("UPDATE sessions SET col='offline', ended_at=?, archived_at=? WHERE session_id=?").run(
    Date.now(),
    Date.now(),
    sid,
  );
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  const remnant = state.windows[0];
  assert.ok(remnant, 'precondition: the spawn created a window to mark dead');
  remnant.pane_dead = true; // dead remnant on the reused window name

  const revived = await (core.revive(oldId) as Promise<ReviveResult>);
  assert.equal(revived.status, 200, JSON.stringify(revived.body));
  const newId = revived.body.spawn_id;
  assert.ok(newId, 'a 200 revive minted a fresh spawn_id');
  assert.notEqual(newId, oldId);
  state.killed.length = 0; // ignore the revive's own remnant kill

  // Killing via the OLD (historical) id must be refused even under force — the
  // window now belongs to the revived row.
  const stale = await (core.spawnKill(oldId, true) as Promise<SpawnKillResult>);
  assert.equal(stale.status, 409, 'a historical spawn_id must not kill the reused window');
  assert.equal(stale.body.current_spawn_id, newId, 'the refusal names the current owner');
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(newId)?.status,
    'spawning',
    'the newer, live row is untouched by the stale kill',
  );
  assert.deepEqual(state.killed, [], 'no tmux window was killed by the stale request');

  // Killing via the current id proceeds.
  const good = await (core.spawnKill(newId, true) as Promise<SpawnKillResult>);
  assert.equal(good.status, 200, JSON.stringify(good.body));
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(newId)?.status,
    'killed',
  );
  assert.deepEqual(state.killed, [revived.body.tmux?.window]);
});

// ---------------------------------------------------------------------------
// H-R6
// ---------------------------------------------------------------------------

test('H-R6: a tmux launch failure leaves NO orphan — the worktree, window, and row are all cleaned', async (t) => {
  const { root, base } = initRepo(t, 'repo');
  const tmux = makeAdapter(4711, {
    ensureSession: () => Promise.resolve('fleetdeck-4711'),
    newWindow: () => Promise.reject(new Error('tmux new-window boom')),
    listScopedWindows: () => Promise.resolve([]),
  });
  const { db, core } = memoryCore(t, { tmux });

  const res = await (core.spawn({ cwd: root, worktree: true }) as Promise<SpawnResult>);
  assert.equal(res.status, 500, `a failed launch fails loud: ${JSON.stringify(res.body)}`);

  const rows = db.prepare<SpawnStarRow>('SELECT * FROM spawns').all();
  assert.equal(rows.length, 1, 'the provisional row exists (durable-before-external-ops)');
  const firstRow = rows[0];
  assert.ok(firstRow, 'the provisional row is readable');
  assert.equal(
    firstRow.status,
    'gone',
    'the provisional row was settled terminal, not left provisioning/spawning',
  );

  assert.equal(
    db
      .prepare<CountRow>(
        "SELECT COUNT(*) AS n FROM spawns WHERE status IN ('provisioning','spawning','stalled','live')",
      )
      .get()?.n,
    0,
    'no active/provisioning row survives a launch failure',
  );

  const wt = path.join(base, `repo--fd-${firstRow.callsign}`);
  assert.equal(existsSync(wt), false, 'the partial worktree was removed — no orphan on disk');
  assert.equal(
    tmux.state.killed.includes(`fd4711-${firstRow.callsign}`),
    true,
    'the (partial) window was killed by name',
  );

  const card = core.snapshot().sessions.find((s) => s.session_id === firstRow.session_id);
  assert.ok(card, 'the failed spawn left a card');
  assert.equal(card.col, 'offline', 'the card is tombstoned');
  assert.match(card.note ?? '', /spawn failed/);

  // git itself must no longer know about the worktree (prune ran).
  const list = git(['worktree', 'list', '--porcelain'], root);
  assert.equal(
    list.includes(wt),
    false,
    'git worktree list no longer references the cleaned worktree',
  );
});

test('H-R6: unverifiable compensation kill stays loud and nonterminal without removing its worktree', async (t) => {
  const { root, base } = initRepo(t, 'repo');
  const tmux = makeAdapter(4711, {
    ensureSession: () => Promise.resolve('fleetdeck-4711'),
    newWindow: () => Promise.reject(new Error('tmux new-window boom')),
    killWindowVerified: () => Promise.resolve({ ok: false, error: 'tmux window lookup failed' }),
  });
  const { db, core } = memoryCore(t, { tmux });

  const res = await (core.spawn({ cwd: root, worktree: true }) as Promise<SpawnResult>);
  assert.equal(res.status, 500);
  assert.match(res.body.reason ?? '', /cleanup unresolved: tmux window lookup failed/);

  const row = db.prepare<SpawnStarRow>('SELECT * FROM spawns').get();
  assert.ok(row, 'the provisional row exists');
  assert.equal(row.status, 'stalled', 'unknown cleanup retains a nonterminal owner row');
  const wt = path.join(base, `repo--fd-${row.callsign}`);
  assert.equal(
    existsSync(wt),
    true,
    'the worktree is retained while its pane may still be running',
  );
  assert.equal(
    git(['worktree', 'list', '--porcelain'], root).includes(wt),
    true,
    'git still knows about the retained worktree',
  );
  const card = core.snapshot().sessions.find((s) => s.session_id === row.session_id);
  assert.ok(card, 'the stalled spawn left a card');
  assert.equal(card.col, 'offline');
  assert.match(card.note ?? '', /cleanup unresolved/);
  await core.cleanup();
  const afterCleanup = db
    .prepare<StatusArchivedRow>(
      'SELECT spawns.status, sessions.archived_at FROM spawns JOIN sessions USING (session_id) WHERE spawn_id = ?',
    )
    .get(row.spawn_id);
  assert.ok(afterCleanup, 'the retained owner row is still joinable after cleanup');
  assert.equal(
    afterCleanup.status,
    'stalled',
    'manual cleanup never converts an UNKNOWN owner to gone',
  );
  assert.equal(
    afterCleanup.archived_at,
    null,
    'the unresolved card stays visible instead of being archived away',
  );
  assert.equal(
    (await (core.revive(row.spawn_id) as Promise<ReviveResult>)).status,
    409,
    'the retained owner cannot be duplicated by revive',
  );
});

test('H-R6: resume launch failure uses verified compensation and retains an UNKNOWN pane owner', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-resume-comp-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-resume-comp-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const tmux = makeAdapter(4711, {
    listScopedWindows: () => Promise.resolve([]),
    newWindow: () => Promise.reject(new Error('resume new-window timeout')),
    killWindowVerified: () => Promise.resolve({ ok: false, error: 'tmux window lookup failed' }),
  });
  const { db, core } = memoryCore(t, { tmux, env: { HOME: userHome } });
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('resume-s', 'otter', ?, 'offline', 'ended', 0, ?, ?, ?, 'spawned')`,
  ).run(cwd, now, now, now);
  db.prepare(
    `INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES ('resume-old', 'resume-s', 'otter', 'fleetdeck-4711', 'fd4711-otter', ?, ?, 'gone')`,
  ).run(cwd, now);
  const transcript = claudeTranscriptPath(cwd, 'resume-s', userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');

  const out = await (core.revive('resume-old') as Promise<ReviveResult>);
  assert.equal(out.status, 500);
  assert.match(out.body.reason ?? '', /cleanup unresolved: tmux window lookup failed/);
  const rows = db
    .prepare<SpawnIdStatusRow>(
      "SELECT spawn_id, status FROM spawns WHERE session_id='resume-s' ORDER BY requested_at, rowid",
    )
    .all();
  assert.equal(rows.length, 2);
  assert.equal(
    rows[1]?.status,
    'stalled',
    'the provisional resume row keeps ownership while cleanup is UNKNOWN',
  );
  const card = core.snapshot().sessions.find((s) => s.session_id === 'resume-s');
  assert.ok(card, 'the resume attempt left a card');
  assert.match(card.note ?? '', /cleanup unresolved/);
  assert.equal(
    (await (core.revive('resume-old') as Promise<ReviveResult>)).status,
    409,
    'a retry cannot duplicate the retained resume owner',
  );
});

// ---------------------------------------------------------------------------
// H-R7
// ---------------------------------------------------------------------------

test('H-R7: revive checks cwd/transcript BEFORE touching tmux, and refuses to kill a live non-claude pane', async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-hr7-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-hr7-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // A live pane running vim occupies the deterministic window name.
  const tmux = makeAdapter(4711, {
    listScopedWindows: () =>
      Promise.resolve([
        {
          session: 'fleetdeck-4711',
          window: 'fd4711-otter',
          window_id: '@9',
          pane_dead: false,
          pane_cmd: 'vim',
        },
      ]),
  });
  const { db, core, state } = memoryCore(t, { tmux, env: { HOME: userHome } });
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, source)
    VALUES ('s7', 'otter', ?, 'offline', 'ended', 0, ?, ?, ?, 'spawned')`,
  ).run(cwd, now, now, now);
  db.prepare(
    `INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES ('sp7', 's7', 'otter', 'fleetdeck-4711', 'fd4711-otter', ?, ?, 'gone')`,
  ).run(cwd, now);

  // Case 1: transcript missing → 410 WITHOUT having killed the vim pane.
  const missing = await (core.revive('sp7') as Promise<ReviveResult>);
  assert.equal(missing.status, 410, 'a missing transcript still refuses');
  assert.match(missing.body.reason ?? '', /transcript/);
  assert.deepEqual(
    state.killed,
    [],
    'eligibility is checked BEFORE tmux — the unrelated pane is not killed',
  );

  // Case 2: transcript present, but the window hosts a LIVE non-claude pane →
  // refuse (409) rather than destroying it.
  const transcript = claudeTranscriptPath(cwd, 's7', userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');
  const refuse = await (core.revive('sp7') as Promise<ReviveResult>);
  assert.equal(refuse.status, 409, 'a live non-claude pane is a refusal, not a kill');
  assert.match(refuse.body.reason ?? '', /live 'vim' pane/);
  assert.deepEqual(state.killed, [], 'the live non-claude pane is never killed');
});

// ---------------------------------------------------------------------------
// M-B2
// ---------------------------------------------------------------------------

test('M-B2: a PreToolUse conflict whisper declares hookEventName:"PreToolUse"', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb2-cwd-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { core } = memoryCore(t);
  const file = path.join(cwd, 'util.js');

  core.hookSessionStart({ session_id: 'A', cwd, source: 'startup' });
  core.hookSessionStart({ session_id: 'B', cwd, source: 'startup' });
  // A edits first (no rival yet).
  core.hookPostToolUse({
    session_id: 'A',
    hook_event_name: 'PostToolUse',
    cwd,
    tool_name: 'Edit',
    tool_input: { file_path: file },
  });
  // B edits the same file via a PreToolUse hook → whisper.
  const out = core.hookPostToolUse({
    session_id: 'B',
    hook_event_name: 'PreToolUse',
    cwd,
    tool_name: 'Edit',
    tool_input: { file_path: file },
  }) as HookOutput;
  assert.ok(out.hookSpecificOutput, 'the second editor gets a whisper');
  assert.equal(
    out.hookSpecificOutput.hookEventName,
    'PreToolUse',
    "the whisper must carry the caller's real event name so a PreToolUse client keeps it",
  );

  // A PostToolUse conflict still declares PostToolUse (no regression).
  core.hookSessionStart({ session_id: 'C', cwd, source: 'startup' });
  const post = core.hookPostToolUse({
    session_id: 'C',
    hook_event_name: 'PostToolUse',
    cwd,
    tool_name: 'Edit',
    tool_input: { file_path: file },
  }) as HookOutput;
  assert.equal(post.hookSpecificOutput?.hookEventName, 'PostToolUse');
});

// ---------------------------------------------------------------------------
// M-B4
// ---------------------------------------------------------------------------

test('M-B4: a corrupt conflicts row does not 500 /state — it is dropped, good rows survive', (t) => {
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const now = Date.now();
  db.prepare(
    'INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)',
  ).run(now, 'r', 'bad.js', 'warning', '{ this is not json');
  db.prepare(
    'INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)',
  ).run(now, 'r', 'good.js', 'warning', JSON.stringify(['sX', 'sY']));

  const core = createCore(db, {
    port: 4711,
    home: '/h',
    tmuxAdapter: makeAdapter().adapter as unknown as CoreTmuxAdapter,
  });
  let snap: Snapshot | undefined;
  assert.doesNotThrow(() => {
    snap = core.snapshot();
  }, 'a corrupt row must not throw out of snapshot()');
  assert.ok(snap, 'snapshot produced a value');
  assert.equal(snap.conflicts.length, 1, 'the corrupt conflict is dropped, the good one survives');
  const survivor = snap.conflicts[0];
  assert.ok(survivor, 'the well-formed conflict is the one that survives');
  assert.equal(survivor.rel_path, 'good.js');
  assert.deepEqual(survivor.sessions, ['sX', 'sY']);
});

// ---------------------------------------------------------------------------
// M-B5
// ---------------------------------------------------------------------------

for (const event of ['PreToolUse', 'PostToolUse']) {
  test(`M-B5: a resurrecting ${event} lifts a presumed-dead offline card out of the offline column`, (t) => {
    const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb5-cwd-'));
    t.after(() => {
      rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const { db, core } = memoryCore(t);

    core.applyEvent({ session_id: 's', hook_event_name: 'SessionStart', cwd, source: 'startup' });
    // Retention's silence tombstone is a GUESS (end_reason='presumed') — BUG-024
    // keeps ordinary activity able to reverse exactly this kind of tombstone.
    db.prepare(
      "UPDATE sessions SET col='offline', ended_at=?, end_reason='presumed' WHERE session_id='s'",
    ).run(Date.now());
    let card = db.prepare<SessionStarRow>("SELECT * FROM sessions WHERE session_id='s'").get();
    assert.ok(card, 'precondition: the card row exists');
    assert.equal(card.col, 'offline', 'precondition: the card is tombstoned offline');
    assert.ok(card.ended_at);

    // A late tool hook proves the process is alive again.
    core.applyEvent({ session_id: 's', hook_event_name: event, cwd, tool_name: 'Read' });
    card = db.prepare<SessionStarRow>("SELECT * FROM sessions WHERE session_id='s'").get();
    assert.ok(card, 'the card row survives the resurrection');
    assert.equal(card.ended_at, null, 'resurrection clears ended_at');
    assert.equal(
      card.col,
      'working',
      `a ${event} resurrection re-derives a live lane, not offline`,
    );
  });
}

// ---------------------------------------------------------------------------
// BUG-024
// ---------------------------------------------------------------------------

for (const event of ['FileChanged', 'Notification']) {
  test(`BUG-024: a late async ${event} does NOT resurrect a hook-proven dead session`, (t) => {
    const cwd = mkdtempSync(path.join(tmpdir(), 'fd-bug024-cwd-'));
    t.after(() => {
      rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const { db, core } = memoryCore(t);

    core.applyEvent({ session_id: 's', hook_event_name: 'SessionStart', cwd, source: 'startup' });
    core.applyEvent({ session_id: 's', hook_event_name: 'SessionEnd', cwd, reason: 'other' });
    let card = db.prepare<SessionStarRow>("SELECT * FROM sessions WHERE session_id='s'").get();
    assert.ok(card, 'precondition: the card row exists');
    assert.equal(card.col, 'offline', 'precondition: the card is tombstoned offline');
    assert.ok(card.ended_at);
    assert.equal(card.end_reason, 'other', 'precondition: the end is hook-PROVEN, not guessed');

    // The async hook was launched before exit but lands after SessionEnd.
    const ev =
      event === 'FileChanged'
        ? {
            session_id: 's',
            hook_event_name: 'FileChanged',
            cwd,
            file_path: path.join(cwd, 'a.js'),
          }
        : {
            session_id: 's',
            hook_event_name: 'Notification',
            cwd,
            notification_type: 'auth_success',
            message: 'signed in',
          };
    core.applyEvent(ev);
    card = db.prepare<SessionStarRow>("SELECT * FROM sessions WHERE session_id='s'").get();
    assert.ok(card, 'the card row survives the late event');
    assert.ok(
      card.ended_at != null,
      `a late ${event} must not clear a proven SessionEnd tombstone`,
    );
    assert.equal(
      card.col,
      'offline',
      `a late ${event} must not float a dead session back to a live lane`,
    );
    assert.equal(card.end_reason, 'other', 'the proven end reason survives');
  });
}

test('BUG-024: a fresh SessionStart DOES reverse a hook-proven SessionEnd (resume path)', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-bug024-start-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core } = memoryCore(t);

  core.applyEvent({ session_id: 's', hook_event_name: 'SessionStart', cwd, source: 'startup' });
  core.applyEvent({ session_id: 's', hook_event_name: 'SessionEnd', cwd, reason: 'logout' });

  // `claude --resume` starts a new run under the same id: SessionStart is the
  // bring-up proof that legitimately brings the card back.
  core.applyEvent({ session_id: 's', hook_event_name: 'SessionStart', cwd, source: 'resume' });
  const card = db.prepare<SessionStarRow>("SELECT * FROM sessions WHERE session_id='s'").get();
  assert.ok(card, 'the resumed session has a card');
  assert.equal(card.ended_at, null, 'SessionStart resurrects the card');
  assert.equal(card.col, 'queued', 'the resumed session re-derives a live lane');
  assert.equal(card.end_reason, null, 'the stale end reason is cleared');
});

// ---------------------------------------------------------------------------
// M-B6
// ---------------------------------------------------------------------------

test('M-B6: an ExitPlanMode plan-persist failure rolls the question row back and fails the hook open', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb6-cwd-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core } = memoryCore(t);

  // Force the plan insert to throw at runtime by removing its table AFTER the
  // prepared statements were compiled.
  db.exec('DROP TABLE plans');
  const before = db.prepare<CountRow>('SELECT COUNT(*) AS n FROM questions').get()?.n;

  const ev = {
    session_id: 's',
    cwd,
    tool_name: 'ExitPlanMode',
    tool_input: { plan: '# Plan\n\nstep 1\n' },
  };
  const row = core.hookHoldQuestion(ev, 'PermissionRequest');

  assert.equal(row, null, 'the hook fails OPEN (no relay) when the plan cannot be persisted');
  const after = db.prepare<CountRow>('SELECT COUNT(*) AS n FROM questions').get()?.n;
  assert.equal(
    after,
    before,
    'the question row is rolled back — no held question without its linked plan',
  );
  // BUG-112: the needs-you telemetry must not survive the rollback either —
  // the old order (applyEvent BEFORE the intake transaction) left a needs-you
  // card pointing at a question that no longer existed.
  assert.equal(
    db.prepare<CountRow>("SELECT COUNT(*) AS n FROM sessions WHERE session_id = 's'").get()?.n,
    0,
    'no card is dealt for an intake that rolled back',
  );
});

test('BUG-112: a plan-intake failure on an EXISTING card leaves the card exactly as it was', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-b112-cwd-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core } = memoryCore(t);

  // Bring the card up through the ordinary hook path, then park it in a known
  // pre-plan state.
  core.hookSessionStart({ session_id: 's', cwd, source: 'startup' });
  db.prepare(
    "UPDATE sessions SET col = 'idle', note = 'turn finished, waiting' WHERE session_id = 's'",
  ).run();
  const before = db
    .prepare<ColEventsNoteRow>("SELECT col, events, note FROM sessions WHERE session_id = 's'")
    .get();

  db.exec('DROP TABLE plans'); // plan insert now throws at runtime
  const ev = { session_id: 's', cwd, tool_name: 'ExitPlanMode', tool_input: { plan: '# Plan\n' } };
  const row = core.hookHoldQuestion(ev, 'PermissionRequest');

  assert.equal(row, null, 'the hook fails OPEN');
  const after = db
    .prepare<ColEventsNoteRow>("SELECT col, events, note FROM sessions WHERE session_id = 's'")
    .get();
  assert.deepEqual(
    after,
    before,
    'a failed plan intake leaves the card, its event count, and its note untouched — no orphan needs-you alert',
  );
  assert.equal(
    db.prepare<CountRow>('SELECT COUNT(*) AS n FROM questions').get()?.n,
    0,
    'no durable question was left behind',
  );
});

test('M-B6: on the happy path both the question row and its plan row persist and are linked', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb6ok-cwd-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core } = memoryCore(t);

  const planMd = '# Add caching\n\n1. do it\n';
  const ev = { session_id: 's', cwd, tool_name: 'ExitPlanMode', tool_input: { plan: planMd } };
  const row = core.hookHoldQuestion(ev, 'PermissionRequest');

  assert.ok(row?.id, 'the question row is created');
  const plan = db.prepare<PlanRow>('SELECT * FROM plans WHERE question_id = ?').get(row.id);
  assert.ok(plan, 'a plan row exists, linked by question_id');
  assert.equal(plan.plan_md, planMd, 'plan markdown is captured byte-identical');
  assert.equal(plan.status, 'proposed');
  // BUG-112: the telemetry now lands AFTER the durable intake — the board must
  // still show the needs-you card for the question it can actually answer.
  const card = db
    .prepare<ColEventsRow>("SELECT col, events FROM sessions WHERE session_id = 's'")
    .get();
  assert.ok(card, 'the intake dealt a card');
  assert.equal(card.col, 'needsyou', 'the card moves to needsyou once the intake committed');
  assert.equal(card.events, 1, 'the PermissionRequest event is counted exactly once');
});

// ---------------------------------------------------------------------------
// M-B8
// ---------------------------------------------------------------------------

test('M-B8: a remote harvest whose capture throws resolves cleanly (no unhandled rejection) via enableRemote', async (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mb8-cwd-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const tmux = makeAdapter(4711, {
    capturePane: () => Promise.reject(new Error('capture-pane exploded')),
  });
  const { db, core } = memoryCore(t, { tmux, env: { FLEETDECK_RC_HARVEST_MS: 0 } });

  const spawned = await (core.spawn({ cwd, remote_control: true }) as Promise<SpawnResult>);
  const { spawn_id, session_id } = spawned.body;
  core.hookSessionStart({ session_id, cwd, source: 'startup' }); // → live
  db.prepare("UPDATE sessions SET col='idle' WHERE session_id=?").run(session_id);

  // enableRemote awaits the (guarded) harvest; a throwing capturePane must not
  // reject the request.
  const res = await (core.enableRemote(spawn_id) as Promise<EnableRemoteResult>);
  assert.equal(
    res.status,
    200,
    `a throwing capture must not fail enableRemote: ${JSON.stringify(res.body)}`,
  );
  assert.equal(res.body.ok, true);
  assert.equal(res.body.url, null, 'no URL was harvested, but the flow completed cleanly');
});

// ---------------------------------------------------------------------------
// M-G1
// ---------------------------------------------------------------------------

test('M-G1: retentionSweep ages file_touches/commands/conflicts/settled-mail past the ledger window; pending mail is spared', (t) => {
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000 });
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const now = Date.now();
  const old = now - 48 * HOUR; // older than the default 24h ledger window
  const recent = now - 60_000;

  db.prepare(
    'INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)',
  ).run('r', 'old.js', '/x/old.js', 's', null, old);
  db.prepare(
    'INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)',
  ).run('r', 'new.js', '/x/new.js', 's', null, recent);
  db.prepare('INSERT INTO commands (at, text, parsed_json) VALUES (?, ?, ?)').run(
    old,
    'old cmd',
    '{}',
  );
  db.prepare('INSERT INTO commands (at, text, parsed_json) VALUES (?, ?, ?)').run(
    recent,
    'new cmd',
    '{}',
  );
  db.prepare(
    'INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?,?,?,?,?)',
  ).run(old, 'r', 'old.js', 'warning', '[]');
  db.prepare(
    'INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?,?,?,?,?)',
  ).run(recent, 'r', 'new.js', 'warning', '[]');
  db.prepare(
    'INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?,?,?,?,?)',
  ).run('nobody', 'ops', 'delivered old', old, old); // settled (delivered) → prunable
  db.prepare(
    'INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?,?,?,?,NULL)',
  ).run('nobody', 'ops', 'pending old', old); // pending → must survive

  // Boot runs retentionSweep once, which ages the old rows.
  createCore(db, {
    port: 4711,
    home: '/h',
    tmuxAdapter: makeAdapter().adapter as unknown as CoreTmuxAdapter,
  });

  assert.equal(
    db.prepare<CountRow>('SELECT COUNT(*) AS n FROM file_touches').get()?.n,
    1,
    'only the recent touch survives',
  );
  assert.equal(
    db.prepare<RelPathRow>('SELECT rel_path FROM file_touches').get()?.rel_path,
    'new.js',
  );
  assert.equal(
    db.prepare<CountRow>('SELECT COUNT(*) AS n FROM commands').get()?.n,
    1,
    'only the recent command survives',
  );
  assert.equal(
    db.prepare<CountRow>('SELECT COUNT(*) AS n FROM conflicts').get()?.n,
    1,
    'only the recent conflict survives',
  );
  assert.equal(
    db.prepare<CountRow>("SELECT COUNT(*) AS n FROM mail WHERE text='delivered old'").get()?.n,
    0,
    'settled old mail is pruned',
  );
  assert.equal(
    db.prepare<CountRow>("SELECT COUNT(*) AS n FROM mail WHERE text='pending old'").get()?.n,
    1,
    'pending mail is NEVER age-pruned',
  );
});

test('M-G1: snapshot windows the per-card file list to the ledger window', (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('sf', 'af', 'working', 'x', 1, ?, ?, 'hooks')`,
  ).run(now, now);
  // one recent touch, one ancient touch (older than 24h)
  db.prepare(
    'INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)',
  ).run('r', 'recent.js', '/x/recent.js', 'sf', null, now - 60_000);
  db.prepare(
    'INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)',
  ).run('r', 'ancient.js', '/x/ancient.js', 'sf', null, now - 48 * HOUR);

  const card = core.snapshot().sessions.find((s) => s.session_id === 'sf');
  assert.ok(card, 'the seeded session has a card');
  assert.deepEqual(
    card.files,
    ['/x/recent.js'],
    'the snapshot only shows touches inside the ledger window',
  );
});

// ---------------------------------------------------------------------------
// M-P8 (correctness of the cached-statement keying)
// ---------------------------------------------------------------------------

test('M-P8: cached updateSession statements apply the right columns across distinct shapes', (t) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mp8-cwd-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const { db, core } = memoryCore(t);

  // Drive a session through several event kinds — each builds a different
  // updateSession column shape. A mis-keyed cache would bind the wrong value
  // to the wrong column and corrupt the row.
  core.applyEvent({
    session_id: 'p',
    hook_event_name: 'SessionStart',
    cwd,
    source: 'startup',
    model: 'sonnet',
  });
  core.applyEvent({
    session_id: 'p',
    hook_event_name: 'UserPromptSubmit',
    cwd,
    prompt: 'do the thing',
  });
  core.applyEvent({
    session_id: 'p',
    hook_event_name: 'PostToolUse',
    cwd,
    tool_name: 'Bash',
    tool_input: { command: 'ls' },
  });
  core.applyEvent({ session_id: 'p', hook_event_name: 'Stop', cwd });

  const card = db.prepare<SessionStarRow>("SELECT * FROM sessions WHERE session_id='p'").get();
  assert.ok(card, 'the driven session has a card');
  assert.equal(card.col, 'idle', 'Stop → idle');
  assert.equal(card.model, 'sonnet', 'the launch model survived every later shape');
  assert.equal(card.last_tool, 'Bash');
  assert.equal(card.task, 'do the thing');
  assert.equal(card.source, 'hooks');
  assert.ok(card.events >= 4);
});

// ---------------------------------------------------------------------------
// R2-5
// ---------------------------------------------------------------------------

test("R2-5: a stale-id force-kill arriving during a revive's window creation is refused; the revived pane survives", async (t) => {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-r25-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-r25-cwd-'));
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // A newWindow we can PAUSE mid-flight. revive() inserts its provisional row,
  // then awaits newWindow — exactly the gap in which a stale kill used to slip
  // in and destroy the just-created pane. Gating newWindow lets us fire the
  // kill while the revive is parked there.
  const tmux = makeAdapter(4711);
  const { state } = tmux;
  let releaseNewWindow: (() => void) | undefined;
  const gate = new Promise<void>((resolve) => {
    releaseNewWindow = resolve;
  });
  let sawNewWindow: (() => void) | undefined;
  const started = new Promise<void>((resolve) => {
    sawNewWindow = resolve;
  });
  tmux.adapter.newWindow = async (spec: NewWindowSpec): Promise<NewWindowResult> => {
    sawNewWindow?.();
    await gate;
    const win: FakeWindow = {
      session: `fleetdeck-${spec.port}`,
      window: `fd${spec.port}-${spec.callsign}`,
      window_id: '@1',
      pane_dead: false,
      pane_cmd: 'claude',
    };
    state.windows.push(win);
    return { session: win.session, window: win.window, window_id: win.window_id };
  };

  const { db, core } = memoryCore(t, { tmux, env: { HOME: userHome } });

  // Seed a terminal (pane-dead) board spawn whose window name a revive reuses,
  // plus the resume transcript so revive is eligible. NO live remnant window is
  // registered, so revive skips its own remnant kill — state.killed then
  // reflects ONLY what the stale kill attempts.
  const sid = randomUUID();
  const oldId = randomUUID();
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, cwd, col, note, events, started_at, last_seen, ended_at, archived_at, source)
    VALUES (?, 'otter', ?, 'offline', 'ended', 0, ?, ?, ?, ?, 'spawned')`,
  ).run(sid, cwd, now, now, now, now);
  db.prepare(
    `INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status)
    VALUES (?, ?, 'otter', 'fleetdeck-4711', 'fd4711-otter', ?, ?, 'pane-dead')`,
  ).run(oldId, sid, cwd, now - 10_000);
  const transcript = claudeTranscriptPath(cwd, sid, userHome);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{}\n');

  // Start the revive but do NOT await it — it parks inside newWindow, AFTER the
  // provisional row exists and owns the reused window.
  const revivePromise = core.revive(oldId) as Promise<ReviveResult>;
  await started;

  // A forced kill via the OLD (historical) id lands mid-revive. It must be
  // refused: the reused window now belongs to the new provisional row.
  const stale = await (core.spawnKill(oldId, true) as Promise<SpawnKillResult>);
  assert.equal(
    stale.status,
    409,
    'a stale-id kill during the revive window creation must be refused',
  );
  assert.notEqual(
    stale.body.current_spawn_id,
    oldId,
    'the refusal must not name the stale id as owner',
  );
  assert.deepEqual(
    state.killed,
    [],
    'the stale kill killed no tmux window — the revived pane is untouched',
  );
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(oldId)?.status,
    'pane-dead',
    'the historical row is not flipped to killed by the refused request',
  );

  // Let the revive finish; the pane comes up and the row goes live-eligible.
  releaseNewWindow?.();
  const revived = await revivePromise;
  assert.equal(revived.status, 200, JSON.stringify(revived.body));
  const newId = revived.body.spawn_id;
  assert.ok(newId, 'a 200 revive minted a fresh spawn_id');
  assert.notEqual(newId, oldId);
  assert.equal(
    stale.body.current_spawn_id,
    newId,
    'the refusal named the new revive row as the current owner',
  );
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(newId)?.status,
    'spawning',
    'the revived row is spawning (live-eligible), not left provisioning or dead',
  );

  // The refused kill never reached its tombstone updateSession; revive's own
  // update is what stands (queued/reviving), not "pane killed from the board".
  const card = db.prepare<SessionStarRow>('SELECT * FROM sessions WHERE session_id=?').get(sid);
  assert.ok(card, 'the reviving session has a card');
  assert.equal(card.col, 'queued', 'the stale kill did not tombstone the reviving card offline');
  assert.match(card.note ?? '', /reviving/, "revive's state stands; the refused kill left no mark");

  // Sanity: the CURRENT id can still kill it.
  const good = await (core.spawnKill(newId, true) as Promise<SpawnKillResult>);
  assert.equal(good.status, 200, JSON.stringify(good.body));
  assert.deepEqual(state.killed, ['fd4711-otter'], 'only the current-id kill reaches tmux');
});

// ---------------------------------------------------------------------------
// R2-6
// ---------------------------------------------------------------------------

test('R2-6: a conflicts row that is valid JSON of the WRONG shape is dropped by snapshot() and cleanup(), never thrown on', async (t) => {
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const now = Date.now();
  const ins = db.prepare(
    'INSERT INTO conflicts (at, repo_id, rel_path, severity, sessions_json) VALUES (?, ?, ?, ?, ?)',
  );
  ins.run(now, 'r', 'obj.js', 'warning', '{}'); // an object, not an array
  ins.run(now, 'r', 'null.js', 'warning', 'null'); // null → null.map / null.length used to throw
  ins.run(now, 'r', 'str.js', 'warning', '"sX"'); // a bare string
  ins.run(now, 'r', 'num.js', 'warning', '42'); // a number
  ins.run(now, 'r', 'good.js', 'warning', JSON.stringify(['sX', 'sY'])); // the one well-formed row

  const core = createCore(db, {
    port: 4711,
    home: '/h',
    tmuxAdapter: makeAdapter().adapter as unknown as CoreTmuxAdapter,
  });

  let snap: Snapshot | undefined;
  assert.doesNotThrow(() => {
    snap = core.snapshot();
  }, 'wrong-shape rows must not throw out of snapshot()');
  assert.ok(snap, 'snapshot produced a value');
  assert.equal(snap.conflicts.length, 1, 'only the well-formed conflict survives the snapshot');
  const survivor = snap.conflicts[0];
  assert.ok(survivor, 'the well-formed conflict is the one that survives');
  assert.equal(survivor.rel_path, 'good.js');
  assert.deepEqual(survivor.sessions, ['sX', 'sY']);

  // cleanup() walks the same rows; its guard used to run `null.length` and throw.
  let res: CleanupResult | undefined;
  await assert.doesNotReject(async () => {
    res = await core.cleanup();
  }, 'wrong-shape rows must not throw out of cleanup()');
  assert.ok(res, 'cleanup produced a result');
  assert.ok(res.ok);
  assert.equal(
    db.prepare<CountRow>('SELECT COUNT(*) AS n FROM conflicts').get()?.n,
    0,
    'every conflict (wrong-shape + the dead-session good one) is cleared, none survives as a crash',
  );
});

// ---------------------------------------------------------------------------
// R2-7
// ---------------------------------------------------------------------------

test("R2-7: the per-card file cap keeps a card's NEWEST touches, not its oldest", (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('sn', 'an', 'working', 'x', 1, ?, ?, 'hooks')`,
  ).run(now, now);

  // 60 distinct files, f0 (oldest touch) … f59 (newest), all inside the ledger
  // window. The cap is 50, so the 10 OLDEST (f0…f9) must be the ones dropped.
  const ins = db.prepare(
    'INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)',
  );
  for (let i = 0; i < 60; i++) {
    ins.run('r', `f${i}.js`, `/x/f${i}.js`, 'sn', null, now - (60 - i) * 1000);
  }

  const card = core.snapshot().sessions.find((s) => s.session_id === 'sn');
  assert.ok(card, 'the seeded session has a card');
  assert.equal(card.files.length, 50, 'the per-card list is capped at 50');
  assert.equal(card.files[0], '/x/f59.js', 'the newest touch is listed first');
  assert.ok(
    card.files.includes('/x/f59.js') && card.files.includes('/x/f10.js'),
    'the newest 50 survive',
  );
  assert.ok(
    !card.files.includes('/x/f0.js') && !card.files.includes('/x/f9.js'),
    'the 10 OLDEST are dropped — the pre-fix code kept these and dropped the newest',
  );
});

// ---------------------------------------------------------------------------
// R2-8 — the FINAL git-status guard in removeWorktree (gate 2), reached only
// after `git worktree remove` fails. The existing H-R1 dirty case returns at
// gate 1 (inspect verdict 'has-work') and never reaches gate 2, so deleting the
// guard would still pass it. A tiny `git` shim on PATH lets an uncommitted
// write land BETWEEN the inspector's clean read and the actual removal — the
// exact TOCTOU the guard exists to catch — deterministically, with real git
// answering every other call.
// ---------------------------------------------------------------------------

function realGitPath(): string {
  // Resolved from the UNSHIMMED PATH (call before installing the shim).
  return execFileSync('sh', ['-c', 'command -v git'], { encoding: 'utf8' }).trim();
}

// A `git` that passes everything through to real git, except it runs one side
// effect right before `git worktree remove`:
//   FD_SHIM_MODE=dirty → drop an uncommitted file into the worktree, then let
//     real git run (it now refuses the dirty removal → gate 2 re-reads dirty).
//   FD_SHIM_MODE=break → remove the worktree's .git pointer and report failure,
//     so gate 2's own `git status` errors (the benign half-removed recovery).
function writeGitShim(t: TestContext): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-gitshim-'));
  const shim = path.join(dir, 'git');
  writeFileSync(
    shim,
    '#!/usr/bin/env bash\n' +
      'case " $* " in\n' +
      '  *" worktree remove "*)\n' +
      '    if [ "$FD_SHIM_MODE" = dirty ]; then\n' +
      '      printf \'UNCOMMITTED via TOCTOU\\n\' > "$FD_SHIM_TARGET/precious.txt"\n' +
      '    elif [ "$FD_SHIM_MODE" = break ]; then\n' +
      '      rm -f "$FD_SHIM_TARGET/.git"\n' +
      '      exit 1\n' +
      '    fi\n' +
      '    ;;\n' +
      'esac\n' +
      'exec "$FD_REAL_GIT" "$@"\n',
    { mode: 0o755 },
  );
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dir;
}

test('R2-8: a write landing between inspect and removal is refused by the final git-status guard — uncommitted work survives', async (t) => {
  const { db, core } = memoryCore(t);
  const { base, root } = initRepo(t, 'repo');

  // A CLEAN worktree: the inspector reads verdict 'safe' and passes gate 1.
  const wt = path.join(base, 'repo--fd-toctou');
  git(['worktree', 'add', '-q', '-b', 'fd/toctou', wt], root);
  ownWorktree(db, {
    sessionId: 's-toctou',
    callsign: 'toctou',
    spawnId: 'sp-toctou',
    cwd: root,
    worktreePath: wt,
  });

  const shimDir = writeGitShim(t);
  setEnv(t, {
    PATH: `${shimDir}:${process.env['PATH'] ?? ''}`,
    FD_REAL_GIT: realGitPath(),
    FD_SHIM_MODE: 'dirty',
    FD_SHIM_TARGET: wt,
  });

  const res = await (core.removeWorktree({ path: wt }) as Promise<RemoveWorktreeResult>); // no force
  assert.equal(
    res.status,
    409,
    `the final-status guard refuses the now-dirty tree: ${JSON.stringify(res.body)}`,
  );
  assert.equal(res.body.verdict, 'has-work');
  assert.equal(existsSync(wt), true, 'the worktree still exists — it was NOT rm -rf-ed');
  assert.equal(
    existsSync(path.join(wt, 'precious.txt')),
    true,
    'the uncommitted file the guard protected survives',
  );
});

test('R2-8: when git can no longer read the tree at the final status check, removal falls through to rmSync (half-removed recovery)', async (t) => {
  const { db, core } = memoryCore(t);
  const { base, root } = initRepo(t, 'repo');

  const wt = path.join(base, 'repo--fd-broken');
  git(['worktree', 'add', '-q', '-b', 'fd/broken', wt], root);
  ownWorktree(db, {
    sessionId: 's-broken',
    callsign: 'broken',
    spawnId: 'sp-broken',
    cwd: root,
    worktreePath: wt,
  });

  const shimDir = writeGitShim(t);
  setEnv(t, {
    PATH: `${shimDir}:${process.env['PATH'] ?? ''}`,
    FD_REAL_GIT: realGitPath(),
    FD_SHIM_MODE: 'break',
    FD_SHIM_TARGET: wt,
  });

  const res = await (core.removeWorktree({ path: wt }) as Promise<RemoveWorktreeResult>); // no force
  assert.equal(
    res.status,
    200,
    `an unreadable tree falls through to rmSync: ${JSON.stringify(res.body)}`,
  );
  assert.equal(res.body.removed, true);
  assert.equal(existsSync(wt), false, 'the daemon removed the directory itself when git could not');
});

// ---------------------------------------------------------------------------
// BUG-149
// ---------------------------------------------------------------------------

test('BUG-149: filesBySession enforces the per-session cap in SQL, not after materialization', (t) => {
  const { db, core } = memoryCore(t);
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES ('sn', 'an', 'working', 'x', 1, ?, ?, 'hooks')`,
  ).run(now, now);

  // 200 distinct files for one session (cap 50) — enough that an unbounded
  // statement would return 200 grouped rows while only 50 may cross the
  // SQL→JS boundary per frame. The R2-7 test only pins the final payload,
  // which was already capped in JS; this pins the STATEMENT's cardinality.
  const ins = db.prepare(
    'INSERT INTO file_touches (repo_id, rel_path, abs_path, session_id, worktree, at) VALUES (?,?,?,?,?,?)',
  );
  for (let i = 0; i < 200; i++) {
    ins.run('r', `f${i}.js`, `/x/f${i}.js`, 'sn', null, now - (200 - i) * 1000);
  }

  const { q } = createStatements(db);
  const rows = q.filesBySession.all(now - 24 * HOUR, 50);
  assert.equal(
    rows.length,
    50,
    'the statement returns at most the per-session cap, not every grouped row',
  );
  assert.equal(rows[0]?.abs_path, '/x/f199.js', 'newest touch first');
  assert.equal(rows.at(-1)?.abs_path, '/x/f150.js', 'the 51st-newest is the last row returned');
  assert.ok(!rows.some((r) => r.abs_path === '/x/f149.js'), 'older files never leave SQLite');

  // The full snapshot still renders the same capped, newest-first card list.
  const card = core.snapshot().sessions.find((s) => s.session_id === 'sn');
  assert.ok(card, 'the seeded session has a card');
  assert.equal(card.files.length, 50);
  assert.equal(card.files[0], '/x/f199.js');
});
