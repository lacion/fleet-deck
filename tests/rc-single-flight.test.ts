// tests/rc-single-flight.test.ts
//
// Characterization (P9.1 Slice 4): pin the single-flight collapse of the
// enableRemote (`/rc`) control path — the exact gap the design doc names
// (docs/v1/evidence/effect/p9-1-design.md §3 Slice 4: "a dedicated single-flight
// test (two concurrent /rc collapse to one harvest) — appears thin today").
//
// BUG-052: enableRemote crosses several awaits (window lookup, the type+Enter
// keystroke, the harvest race) writing no DB state until the harvest lands, so
// two concurrent /rc requests for one spawn both passed every gate and typed
// `/rc` TWICE — real tmux rendered `/rc a/rc a`. The fix latches the enable per
// spawn: the first caller runs the body; a concurrent caller shares the SAME
// in-flight promise and never sends a second keystroke sequence.
//
// This suite proves that invariant with a keystroke + capture counter: two
// synchronous `core.enableRemote(id)` calls for one live/idle spawn must fire
// exactly ONE typeAndEnter and ONE capturePane, and both callers must observe
// the SAME resolved result (referential identity — the shared memoized promise).
// A second test pins the sync-prefix result shapes the existing spawn suites
// only thinly cover (404 / shell / not-live / not-idle / already-enabled).
//
// The core is wired with the ingress `runControlDetached` runner (as
// spawn-kill-cancel.test.ts / dismiss.test.ts do) so the SAME assertions
// exercise the legacy async body before the Slice-4 conversion and the Effect
// core after it — byte-identical across the flip.

import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
import test, { type TestContext } from './helpers/harness-test.ts';

type Db = ReturnType<typeof openDb>;
type Core = ReturnType<typeof createCore>;
type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

// The harvested URL the fake pane exposes in scrollback; RC_URL_RE
// (/https:\/\/claude\.ai\/\S+/) extracts it verbatim (the trailing " ready" is
// space-separated, so \S+ stops before it).
const RC_URL = 'https://claude.ai/code/session_singleflight';
const CAPTURE = `remote control ready at ${RC_URL} ready`;

interface FakeWindow {
  session: string;
  window: string;
  window_id: string;
  pane_dead: boolean;
  pane_cmd: string;
}
interface FakeState {
  windows: FakeWindow[];
  typeAndEnterCalls: number;
  capturePaneCalls: number;
  rcSent: string[];
}

interface SpawnResult {
  status: number;
  body: { spawn_id: string; session_id: string; reason?: string };
}
interface EnableRemoteResult {
  status: number;
  body: { ok: boolean; enabled?: boolean; url?: string | null; pending?: boolean; reason?: string };
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

// A reachable tmux with one live claude window created per newWindow(); the
// keystroke and capture counters are the single-flight observable.
function makeAdapter(): { state: FakeState; adapter: CoreTmuxAdapter } {
  const state: FakeState = { windows: [], typeAndEnterCalls: 0, capturePaneCalls: 0, rcSent: [] };
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
    paneCurrentCommand: (target: string) => {
      const w = state.windows.find((x) => x.window_id === target || x.window === target);
      return Promise.resolve(w ? { dead: w.pane_dead, cmd: w.pane_cmd } : null);
    },
    killWindowVerified: () => Promise.resolve({ ok: true, window_id: '@1' }),
    pasteText: () => Promise.resolve(true),
    sendEnter: () => Promise.resolve(true),
    typeKeys: () => Promise.resolve(true),
    typeAndEnter: (_target: string, text: string) => {
      state.typeAndEnterCalls += 1;
      state.rcSent.push(text);
      return Promise.resolve(true);
    },
    sendBringupEnter: () => Promise.resolve(true),
    capturePane: () => {
      state.capturePaneCalls += 1;
      return Promise.resolve(CAPTURE);
    },
    launchOverride: () => {
      /* unused */
    },
  };
  return { state, adapter: adapter as unknown as CoreTmuxAdapter };
}

// createCore with the maintenance-timer knobs pinned high (no unref timer
// perturbs a synchronous assertion) and the harvest set to next-microtask, and
// the ingress runner wired so the Effect core is the path under test.
function makeCore(t: TestContext, port = 4711): { db: Db; core: Core; state: FakeState } {
  setEnv(t, {
    FLEETDECK_NUDGE_MS: 1_000_000,
    FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000,
    FLEETDECK_RC_HARVEST_MS: 0,
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

// Drive a fresh spawn to a live, idle claude card (the enable precondition),
// exactly as derive-audit-reliability's M-B8 does.
async function liveIdleSpawn(
  core: Core,
  db: Db,
  t: TestContext,
): Promise<{ spawn_id: string; session_id: string }> {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-rc-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawned = await (core.spawn({ cwd }) as Promise<SpawnResult>);
  const { spawn_id, session_id } = spawned.body;
  assert.ok(spawn_id, `a spawn id is returned: ${JSON.stringify(spawned.body)}`);
  core.hookSessionStart({ session_id, cwd, source: 'startup' }); // → live
  db.prepare("UPDATE sessions SET col='idle' WHERE session_id=?").run(session_id);
  db.prepare("UPDATE spawns SET status='live' WHERE spawn_id=?").run(spawn_id);
  return { spawn_id, session_id };
}

// ---------------------------------------------------------------------------
// Single-flight: two concurrent enables collapse to ONE harvest + ONE keystroke.
// ---------------------------------------------------------------------------
test('rc single-flight: two concurrent enables collapse to one harvest, sharing the result', async (t) => {
  const { db, core, state } = makeCore(t);
  const { spawn_id } = await liveIdleSpawn(core, db, t);

  // Fire both enables on the SAME tick, before any await — the first caller sets
  // the latch synchronously (the sync prefix + the memo set both run before this
  // statement returns), so the second short-circuits on the in-flight promise.
  const p1 = core.enableRemote(spawn_id) as Promise<EnableRemoteResult>;
  const p2 = core.enableRemote(spawn_id) as Promise<EnableRemoteResult>;
  // The D7 object itself: both callers hold the SAME memoized in-flight Promise,
  // not merely two promises resolving to a shared value (slice-4 review NIT 1).
  assert.strictEqual(p1, p2, 'single-flight shares one in-flight promise');
  const [r1, r2] = await Promise.all([p1, p2]);

  assert.equal(state.typeAndEnterCalls, 1, 'BUG-052: the two enables typed `/rc` exactly ONCE');
  assert.equal(state.capturePaneCalls, 1, 'exactly one harvest ran for the collapsed enable');
  assert.equal(state.rcSent.length, 1, 'exactly one keystroke sequence reached the pane');
  assert.match(
    state.rcSent[0] ?? '',
    /^\/rc \S+$/,
    'the single keystroke is a `/rc <callsign>` submit',
  );

  assert.deepEqual(
    r1,
    { status: 200, body: { ok: true, enabled: true, url: RC_URL, pending: false } },
    'the enable harvested the URL and reported it (pending:false)',
  );
  assert.strictEqual(
    r1,
    r2,
    'both callers observe the SAME resolved result (shared memoized promise)',
  );
});

// ---------------------------------------------------------------------------
// Sync-prefix result shapes: the pre-await gates the existing suites cover only
// thinly. Each uses a distinct spawn so the per-spawn latch never interferes.
// ---------------------------------------------------------------------------
test('rc result shapes: 404 / shell / not-live / not-idle / already-enabled', async (t) => {
  const { db, core, state } = makeCore(t);

  // 404 — no such spawn.
  const missing = (await (core.enableRemote(
    'does-not-exist',
  ) as Promise<EnableRemoteResult>)) as EnableRemoteResult;
  assert.deepEqual(missing, { status: 404, body: { ok: false, reason: 'no such spawn' } });

  // 409 — a shell session has no remote control (kind gate, before status).
  const shell = await liveIdleSpawn(core, db, t);
  db.prepare("UPDATE spawns SET kind='shell' WHERE spawn_id=?").run(shell.spawn_id);
  const shellRes = await (core.enableRemote(shell.spawn_id) as Promise<EnableRemoteResult>);
  assert.deepEqual(shellRes, {
    status: 409,
    body: { ok: false, reason: 'remote control is unavailable for shell sessions' },
  });

  // 409 — spawn not live.
  const notLive = await liveIdleSpawn(core, db, t);
  db.prepare("UPDATE spawns SET status='stalled' WHERE spawn_id=?").run(notLive.spawn_id);
  const notLiveRes = await (core.enableRemote(notLive.spawn_id) as Promise<EnableRemoteResult>);
  assert.deepEqual(notLiveRes, {
    status: 409,
    body: { ok: false, reason: 'spawn is stalled, not live' },
  });

  // 409 — session neither queued nor idle (the TUI is mid-turn).
  const busy = await liveIdleSpawn(core, db, t);
  db.prepare("UPDATE sessions SET col='working' WHERE session_id=?").run(busy.session_id);
  const busyRes = await (core.enableRemote(busy.spawn_id) as Promise<EnableRemoteResult>);
  assert.deepEqual(busyRes, {
    status: 409,
    body: { ok: false, reason: 'session is working, not queued or idle' },
  });

  // 200 — already enabled: remote_control + a harvested URL short-circuits
  // idempotently, WITHOUT typing a second `/rc`.
  const already = await liveIdleSpawn(core, db, t);
  db.prepare('UPDATE spawns SET remote_control=1, remote_url=? WHERE spawn_id=?').run(
    'https://claude.ai/code/session_existing',
    already.spawn_id,
  );
  const before = state.typeAndEnterCalls;
  const alreadyRes = await (core.enableRemote(already.spawn_id) as Promise<EnableRemoteResult>);
  assert.deepEqual(alreadyRes, {
    status: 200,
    body: {
      ok: true,
      enabled: true,
      url: 'https://claude.ai/code/session_existing',
      pending: false,
    },
  });
  assert.equal(
    state.typeAndEnterCalls,
    before,
    'the idempotent already-enabled path types no keystroke',
  );
});
