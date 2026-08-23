// tests/revive-resurrect-mixed-caller.test.ts
//
// Characterization (P9.1 Slice 5) — the design doc's named gap
// (docs/v1/evidence/effect/p9-1-design.md §3 Slice 5): "a test asserting
// resurrectSpawn's path into launchResume produces byte-identical provisional-row
// insert + R2-5 owner re-check behavior as the request path".
//
// MATERIAL FACTUAL CORRECTION (the conversion rests on it): the design's phrase
// "resurrectSpawn's path INTO launchResume" is imprecise. `resurrectSpawn`
// (src/daemon/spawns.ts) NEVER calls launchResume — it is a standalone sync
// compare-and-set. It is the MIXED CALLER:
//   • request revive's BUG-3 adopt branch → resurrectSpawn(row)
//   • the root liveness tick's resurrect loop (after the BUG-152 re-read) →
//     resurrectSpawn(fresh)
// `launchResume` has ONLY request callers — reviveEffect and adoptSessionEffect
// (plus their Legacy twins); no root leg ever reaches it. That is exactly why
// D5 option (a)
// holds: launchResume stays a plain async function, body UNTOUCHED, awaited inside
// reviveEffect's / adoptSessionEffect's coarse Effect.promise tails; resurrectSpawn
// (a distinct sync function) is untouched too. Neither is an Effect.
//
// PART A pins the mixed-caller invariant: a request revive (the BUG-3 live-pane
// adopt) and a root liveness-tick resurrection, driven against IDENTICAL fixtures,
// produce the SAME observable spawn-row + card transition — because resurrectSpawn
// is the single source of that transition for both callers.
//
// PART B pins launchResume's request path through revive: a genuinely-dead revive
// inserts a fresh PROVISIONAL owner row ('provisioning' → 'spawning') and the R2-5
// pre-launch owner re-check EXCLUDES the revive's own terminal ('pane-dead') row
// via excludeSpawnId, so the launch proceeds and the NEW row becomes the window's
// current owner while the old row is left untouched.
//
// The core is wired with the ingress runControlDetached runner (as
// rc-single-flight.test.ts / spawn-kill-cancel.test.ts do) so the SAME assertions
// exercise the legacy async bodies BEFORE the Slice-5 conversion and the Effect
// cores AFTER — byte-identical across the flip. Author legacy-first (3x), convert,
// run 3x unchanged.

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { openDb } from '../src/daemon/db.ts';
import { claudeTranscriptPath, createCore } from '../src/daemon/derive.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
import test, { type TestContext } from './helpers/harness-test.ts';

type Db = ReturnType<typeof openDb>;
type Core = ReturnType<typeof createCore>;
type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

interface FakeWindow {
  session: string;
  window: string;
  window_id: string;
  pane_dead: boolean;
  pane_cmd: string;
}
interface FakeState {
  windows: FakeWindow[];
}

interface SpawnResult {
  status: number;
  body: { spawn_id: string; session_id: string; reason?: string };
}
interface ReviveResult {
  status: number;
  body: {
    ok: boolean;
    adopted?: boolean;
    spawn_id?: string;
    session_id?: string;
    callsign?: string;
    reason?: string;
  };
}

// The resurrect transition resurrectSpawn writes, split into a caller-independent
// part (deep-compared across the two callers) and last_seen (time-based, asserted
// recent on both). spawn_id / session_id / callsign / tmux_window are identity and
// never enter the compare.
interface Transition {
  spawn: { status: string; fail_detail: string | null };
  card: {
    col: string | null;
    ended_at: number | null;
    archived_at: number | null;
    notification_type: string | null;
    note: string | null;
  };
}

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

// A reachable tmux whose newWindow() creates a live claude window. Windows are
// mutable in `state.windows` so a test can flip one pane_dead (a dead remnant).
function makeAdapter(): { state: FakeState; adapter: CoreTmuxAdapter } {
  const state: FakeState = { windows: [] };
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: (p: number) => `fleetdeck-${p}`,
    windowName: (p: number, callsign: string) => `fd${p}-${callsign}`,
    ensureSession: (p: number) => Promise.resolve(`fleetdeck-${p}`),
    newWindow: (spec: { port: number; callsign: string }) => {
      const win: FakeWindow = {
        session: `fleetdeck-${spec.port}`,
        window: `fd${spec.port}-${spec.callsign}`,
        window_id: `@${state.windows.length + 1}`,
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
    paneCurrentCommand: (target: string) => {
      const w = state.windows.find((x) => x.window_id === target || x.window === target);
      return Promise.resolve(w ? { dead: w.pane_dead, cmd: w.pane_cmd } : null);
    },
    killWindowVerified: () => Promise.resolve({ ok: true, window_id: '@1' }),
    pasteText: () => Promise.resolve(true),
    sendEnter: () => Promise.resolve(true),
    typeKeys: () => Promise.resolve(true),
    typeAndEnter: () => Promise.resolve(true),
    sendBringupEnter: () => Promise.resolve(true),
    capturePane: () => Promise.resolve(''),
    launchOverride: () => {
      /* unused — spawnOverrideCmd returns null, so launch goes via newWindow */
    },
  };
  return { state, adapter: adapter as unknown as CoreTmuxAdapter };
}

// createCore with the maintenance timers pinned high (no unref nudge/mail timer
// perturbs a synchronous assertion), an in-process user HOME so
// claudeTranscriptPath resolves under a scratch dir, and the ingress runner wired
// so the Effect core is the path under test.
function makeCore(t: TestContext, port = 4711): { db: Db; core: Core; state: FakeState } {
  const userHome = mkdtempSync(path.join(tmpdir(), 'fd-mixed-home-'));
  t.after(() => rmSync(userHome, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  setEnv(t, {
    HOME: userHome,
    FLEETDECK_NUDGE_MS: 1_000_000,
    FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000,
  });
  const { state, adapter } = makeAdapter();
  const db = openDb(':memory:');
  const core = createCore(db, {
    port,
    home: '/daemon-home',
    tmuxAdapter: adapter,
    runControlDetached,
  });
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });
  return { db, core, state };
}

// The condemned pre-state resurrectSpawn is expected to reverse: a 'pane-dead'
// spawn carrying a stale fail_detail whose card is offline + archived with a
// needs-you chip. Both callers must reverse it identically.
const CONDEMNED_AT = 1_700_000_000_000;

// Drive a fresh spawn to 'live' with a live claude window, write its resume
// transcript, then condemn it: spawn row 'pane-dead' + fail_detail, card offline /
// ended / archived / spawn_stalled chip. With livePane=false the window's pane is
// flipped dead (a remnant revive must kill before it can launchResume).
async function condemnedSpawn(
  core: Core,
  db: Db,
  state: FakeState,
  t: TestContext,
  { livePane }: { livePane: boolean },
): Promise<{ spawn_id: string; session_id: string; cwd: string; tmux_window: string }> {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mixed-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawned = await (core.spawn({ cwd }) as Promise<SpawnResult>);
  const { spawn_id, session_id } = spawned.body;
  assert.ok(spawn_id, `a spawn id is returned: ${JSON.stringify(spawned.body)}`);
  core.hookSessionStart({ session_id, cwd, source: 'startup' }); // → live

  const transcript = claudeTranscriptPath(cwd, session_id, process.env['HOME']);
  mkdirSync(path.dirname(transcript), { recursive: true });
  writeFileSync(transcript, '{"type":"summary"}\n');

  const tmux_window = (
    db.prepare('SELECT tmux_window FROM spawns WHERE spawn_id=?').get(spawn_id) as {
      tmux_window: string;
    }
  ).tmux_window;
  if (!livePane) {
    const w = state.windows.find((x) => x.window === tmux_window);
    assert.ok(w, 'the spawn created a scoped window');
    w.pane_dead = true;
  }

  db.prepare("UPDATE spawns SET status='pane-dead', fail_detail='clone boom' WHERE spawn_id=?").run(
    spawn_id,
  );
  db.prepare(
    "UPDATE sessions SET col='offline', ended_at=?, archived_at=?, notification_type='spawn_stalled', note='pane idle — resume with claude --resume' WHERE session_id=?",
  ).run(CONDEMNED_AT, CONDEMNED_AT, session_id);
  return { spawn_id, session_id, cwd, tmux_window };
}

function transitionOf(db: Db, spawn_id: string, session_id: string): Transition {
  const s = db
    .prepare('SELECT status, fail_detail FROM spawns WHERE spawn_id=?')
    .get(spawn_id) as Transition['spawn'];
  const c = db
    .prepare(
      'SELECT col, ended_at, archived_at, notification_type, note FROM sessions WHERE session_id=?',
    )
    .get(session_id) as Transition['card'];
  return { spawn: s, card: c };
}

function lastSeen(db: Db, session_id: string): number | null {
  return (
    db.prepare('SELECT last_seen FROM sessions WHERE session_id=?').get(session_id) as {
      last_seen: number | null;
    }
  ).last_seen;
}

// ---------------------------------------------------------------------------
// PART A — resurrectSpawn is the MIXED caller: request revive (BUG-3) and the root
// liveness tick drive the identical spawn-row + card transition on identical
// fixtures. This is the invariant the D5 (a) disposition preserves: whatever
// converts revive must leave resurrectSpawn's transition byte-identical to the
// untouched root leg's.
// ---------------------------------------------------------------------------
test('resurrectSpawn mixed caller: request revive (BUG-3) and root liveness tick drive the identical transition', async (t) => {
  const { db, core, state } = makeCore(t);

  // Two identical condemned spawns, each with a live claude pane on its window.
  const a = await condemnedSpawn(core, db, state, t, { livePane: true }); // request-revive path
  const b = await condemnedSpawn(core, db, state, t, { livePane: true }); // root-tick path

  // Identical PRE-states (modulo identity) — the fixtures really are apples-to-apples.
  assert.deepEqual(
    transitionOf(db, a.spawn_id, a.session_id),
    transitionOf(db, b.spawn_id, b.session_id),
    'both condemned spawns start in the identical observable state',
  );

  const t0 = Date.now();

  // Path 1 — request revive. The window hosts a live claude, so revive takes the
  // BUG-3 adopt branch (spawns.ts:2411) → resurrectSpawn(row) → 200 {adopted}.
  const reviveWire = (await (core.revive(a.spawn_id) as Promise<ReviveResult>)) as ReviveResult;
  assert.equal(
    reviveWire.status,
    200,
    `request revive adopts the live pane: ${JSON.stringify(reviveWire)}`,
  );
  assert.equal(
    reviveWire.body.adopted,
    true,
    'the request-revive BUG-3 branch reports adopted:true',
  );
  assert.equal(
    reviveWire.body.spawn_id,
    a.spawn_id,
    'BUG-3 adoption resurrects the SAME row (no new spawn)',
  );

  // Path 2 — root liveness tick. b is 'pane-dead' with a live claude pane, so the
  // resurrect loop (spawns.ts:3886) → resurrectSpawn(fresh). No request, no wire.
  await core.spawnLivenessTick();

  // Both callers went through the single resurrectSpawn body: identical transition.
  const ta = transitionOf(db, a.spawn_id, a.session_id);
  const tb = transitionOf(db, b.spawn_id, b.session_id);
  assert.deepEqual(
    ta,
    {
      spawn: { status: 'live', fail_detail: null },
      card: {
        col: 'idle',
        ended_at: null,
        archived_at: null,
        notification_type: null,
        note: 'pane is a live claude — restored to the board',
      },
    },
    'request revive resurrected the row + card exactly as resurrectSpawn specifies',
  );
  assert.deepEqual(tb, ta, 'the root liveness tick drove the byte-identical transition');

  // last_seen refreshed on both (resurrectSpawn stamps Date.now()); it is the one
  // field excluded from the deep compare because it is time-based.
  assert.ok((lastSeen(db, a.session_id) ?? 0) >= t0, 'request revive refreshed last_seen');
  assert.ok((lastSeen(db, b.session_id) ?? 0) >= t0, 'root tick refreshed last_seen');
});

// ---------------------------------------------------------------------------
// PART B — launchResume's request path (through revive of a genuinely-dead pane):
// the provisional-row insert + the R2-5 pre-launch owner re-check that excludes the
// revive's own terminal row. This is the launchResume behavior the D5 (a)
// disposition keeps byte-identical (body untouched, awaited inside the coarse tail).
// ---------------------------------------------------------------------------
test('launchResume via revive: inserts a fresh provisional owner and excludes its own terminal row (R2-5)', async (t) => {
  const { db, core, state } = makeCore(t);

  // A condemned spawn whose window pane is DEAD — revive kills the remnant, then
  // reaches launchResume (no BUG-3 adopt: the pane is not a live claude).
  const c = await condemnedSpawn(core, db, state, t, { livePane: false });

  // Before: the terminal 'pane-dead' row is the window's current owner.
  const ownerBefore = db
    .prepare(
      "SELECT spawn_id FROM spawns WHERE tmux_window=? AND status IN ('provisioning','spawning','stalled','live','pane-dead') ORDER BY requested_at DESC, rowid DESC LIMIT 1",
    )
    .get(c.tmux_window) as { spawn_id: string };
  assert.equal(ownerBefore.spawn_id, c.spawn_id, 'the dead row owns its window before revive');

  const wire = (await (core.revive(c.spawn_id) as Promise<ReviveResult>)) as ReviveResult;

  // launchResume returned 200 with a NEW spawn id — the R2-5 owner re-check saw the
  // window "owned" by c but c.spawn_id === excludeSpawnId, so it did NOT refuse.
  assert.equal(wire.status, 200, `revive launched a resume: ${JSON.stringify(wire)}`);
  assert.equal(wire.body.ok, true, 'revive succeeded');
  const newId = wire.body.spawn_id;
  assert.ok(newId && newId !== c.spawn_id, 'a fresh spawn row was minted for the resume');
  assert.equal(wire.body.session_id, c.session_id, 'the resume keeps the same session');

  // The fresh provisional row was inserted and flipped live-eligible ('spawning'),
  // carrying the SAME session + window as the dead row it resumes.
  const fresh = db
    .prepare('SELECT status, session_id, tmux_window FROM spawns WHERE spawn_id=?')
    .get(newId) as { status: string; session_id: string; tmux_window: string };
  assert.equal(fresh.status, 'spawning', 'the new row is live-eligible after the pane came up');
  assert.equal(fresh.session_id, c.session_id, 'the new row belongs to the resumed session');
  assert.equal(fresh.tmux_window, c.tmux_window, 'the new row reuses the dead row’s window name');

  // The old terminal row is untouched — launchResume never mutates excludeSpawnId.
  const old = db.prepare('SELECT status FROM spawns WHERE spawn_id=?').get(c.spawn_id) as {
    status: string;
  };
  assert.equal(old.status, 'pane-dead', 'the excluded terminal row is left exactly as it was');

  // After: the new provisional/spawning row now OWNS the window (R2-5 — the
  // provisional owner outranks the stale 'pane-dead' sibling).
  const ownerAfter = db
    .prepare(
      "SELECT spawn_id FROM spawns WHERE tmux_window=? AND status IN ('provisioning','spawning','stalled','live','pane-dead') ORDER BY requested_at DESC, rowid DESC LIMIT 1",
    )
    .get(c.tmux_window) as { spawn_id: string };
  assert.equal(ownerAfter.spawn_id, newId, 'the fresh row is now the window owner');
});
