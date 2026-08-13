import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.ts';
import { createCore } from '../scripts/fleetd/derive.ts';
import { startDaemon } from './helpers/daemon.ts';
import { getJson, postJson } from './helpers/http.ts';
import { waitForSpecRecords, waitUntil } from './helpers/wait.ts';
import type { SqliteHandle } from '../scripts/fleetd/sqlite.ts';
import type { StateResponse } from '../contracts/state.ts';

// The daemon's tmuxAdapter option is typed against derive.ts's unexported
// `TmuxAdapter`; the fake below is deliberately narrow (it omits methods these
// paths never call and takes a narrowed newWindow spec), so it rides the
// untyped CoreOpts seam via `as unknown as CoreTmuxAdapter`.
type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

// ── Facets read off the daemon's `unknown` JSON bodies / seam returns ─────────
interface SpawnHttpAck {
  session_id: string;
  spawn_id: string;
  tmux: { session: string; window: string };
}
interface RefusalBody {
  reason: string;
}
interface MailDelivery {
  delivered: number;
}
interface ShellSpec {
  kind: string;
  argv: string[];
}
interface SpecRecord {
  parsed: ShellSpec;
}
// core.spawn()/spawnKill() are `unknown` at the createCore seam; these are the
// body facets the in-memory tests read from them.
interface CoreSpawnResult {
  status: number;
  body: { session_id: string; spawn_id: string };
}
interface SpawnKillResult {
  status: number;
  body: { status: string };
}
// core.command()/postMail() return narrow declared unions; these widening facets
// (reached via `as unknown as`) expose only the fields the tests assert on.
interface CommandResult {
  ok: boolean;
  unrouted: boolean;
}
interface PostMailSuccess {
  ok: boolean;
  delivered: number;
}
interface PostMailRefusal {
  status: number;
  body: { reason: string };
}

// Row shapes for the .get() reads. Under noUncheckedIndexedAccess and the sqlite
// seam, prepare<R>().get() is `R | undefined`.
interface StatusRow {
  status: string;
}
interface ColRow {
  col: string;
}
interface CountRow {
  n: number;
}
interface CallsignRow {
  callsign: string;
}
interface PrevRow {
  prev_callsign: string | null;
}
interface SessionRow {
  session_id: string;
  callsign: string;
  prev_callsign: string | null;
}

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.ts');
try {
  chmodSync(FIXTURE, 0o755);
} catch {
  /* best effort */
}

const scratch = (prefix: string): string => mkdtempSync(path.join(tmpdir(), prefix));
const tmuxOk = (): boolean => {
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
};
const tmux = (socket: string, args: string[]): string =>
  execFileSync('tmux', ['-L', socket, ...args], { encoding: 'utf8' }).trim();

function overrideEnv(record: string): Record<string, string> {
  return {
    FLEETDECK_SPAWN_CMD: FIXTURE,
    FLEETDECK_TEST_SPAWN_RECORD: record,
  };
}

// ── fake tmux ────────────────────────────────────────────────────────────────
interface FakeWindow {
  session: string;
  window: string;
  window_id: string;
  pane_dead: boolean;
  pane_cmd: string | undefined;
}
interface NewWindowSpec {
  argv: string[];
  port: number;
  callsign: string;
}
interface FakeState {
  windows: FakeWindow[];
  launches: NewWindowSpec[];
}

function fakeAdapter(): { state: FakeState; adapter: CoreTmuxAdapter } {
  const state: FakeState = { windows: [], launches: [] };
  const adapter = {
    spawnOverrideCmd: (): string | null => null,
    hasTmux: (): boolean => true,
    sessionName: (p: number): string => `fleetdeck-${p}`,
    windowName: (p: number, callsign: string): string => `fd${p}-${callsign}`,
    ensureSession: (p: number): Promise<string> => Promise.resolve(`fleetdeck-${p}`),
    newWindow: (spec: NewWindowSpec): Promise<FakeWindow> => {
      state.launches.push(spec);
      const shell = spec.argv.at(-1);
      const win: FakeWindow = {
        session: `fleetdeck-${spec.port}`,
        window: `fd${spec.port}-${spec.callsign}`,
        window_id: '@1',
        pane_dead: false,
        pane_cmd: shell,
      };
      state.windows.push(win);
      return Promise.resolve(win);
    },
    listScopedWindows: (): Promise<FakeWindow[]> => Promise.resolve(state.windows),
    paneCurrentCommand: (
      target: string,
    ): Promise<{ dead: boolean; cmd: string | undefined } | null> => {
      const win = state.windows.find((w) => w.window_id === target || w.window === target);
      return Promise.resolve(win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null);
    },
    exactWindowTarget: (_p: number, window: string): string => window,
    killWindowVerified: (
      name: string,
    ): Promise<{ gone: boolean } | { ok: boolean; window_id: string }> => {
      const win = state.windows.find((w) => w.window === name);
      if (!win) return Promise.resolve({ gone: true });
      state.windows = state.windows.filter((w) => w !== win);
      return Promise.resolve({ ok: true, window_id: win.window_id });
    },
    capturePane: (): Promise<string> => Promise.resolve(''),
    sendBringupEnter: (): Promise<boolean> => Promise.resolve(true),
    typeKeys: (): Promise<boolean> => Promise.resolve(true),
    sendEnter: (): Promise<boolean> => Promise.resolve(true),
    pasteText: (): Promise<boolean> => Promise.resolve(true),
    fleetServerAbsent: (): Promise<boolean> => Promise.resolve(false),
  };
  return { state, adapter: adapter as unknown as CoreTmuxAdapter };
}

test('shell spawn validation rejects Claude/repo fields loudly', async (t) => {
  const cwd = scratch('fd-shell-validation-');
  const record = path.join(cwd, 'records.jsonl');
  const daemon = await startDaemon({ env: overrideEnv(record) });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const cases: [Record<string, unknown>, RegExp][] = [
    [{ kind: 'other', cwd }, /kind/],
    [{ kind: 'shell', cwd, repo: 'org/repo' }, /repo/],
    [{ kind: 'shell', cwd, prompt: 'do it' }, /prompt/],
    [{ kind: 'shell', cwd, model: 'sonnet' }, /model/],
    [{ kind: 'shell', cwd, dangerously_skip_permissions: true }, /dangerously_skip_permissions/],
  ];
  for (const [body, reason] of cases) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, body);
    assert.equal(res.status, 400);
    assert.match((res.json as RefusalBody | null)?.reason ?? '', reason);
  }
});

test('override shell is live immediately, source shell, snapshot kind shell, and mail-safe', async (t) => {
  const cwd = scratch('fd-shell-override-');
  const record = path.join(cwd, 'records.jsonl');
  const daemon = await startDaemon({ env: overrideEnv(record) });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { kind: 'shell', cwd });
  assert.equal(spawned.status, 200);
  const spawnedBody = spawned.json as SpawnHttpAck;
  const spec = ((await waitForSpecRecords(record, 1)).at(-1) as SpecRecord).parsed;
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = state.sessions.find((s) => s.session_id === spawnedBody.session_id);
  assert.ok(card);
  assert.equal(card.source, 'shell');
  assert.equal(card.col, 'idle');
  assert.equal(card.note, 'shell');
  assert.equal(card.spawn?.kind, 'shell');
  assert.equal(card.spawn.status, 'live');
  assert.equal(spec.kind, 'shell');
  assert.equal(spec.argv.includes('--session-id'), false);
  assert.equal(
    spec.argv.at(-1),
    (process.env['SHELL'] ?? '').trim() || (existsSync('/bin/bash') ? 'bash' : 'sh'),
  );

  const all = await postJson(
    `${daemon.baseUrl}/mail`,
    { to: 'all', from: 'ops', text: 'safe fanout' },
    { token: daemon.token },
  );
  assert.equal(all.status, 200);
  assert.equal((all.json as MailDelivery).delivered, 0);
  const repo = await postJson(
    `${daemon.baseUrl}/mail`,
    {
      to: `repo:${String(card.repo_name)}`,
      from: 'ops',
      text: 'safe repo fanout',
    },
    { token: daemon.token },
  );
  assert.equal(repo.status, 200);
  assert.equal((repo.json as MailDelivery).delivered, 0);
  const direct = await postJson(
    `${daemon.baseUrl}/mail`,
    { to: card.callsign, from: 'ops', text: 'unsafe direct' },
    { token: daemon.token },
  );
  assert.equal(direct.status, 409);
  assert.equal(
    (direct.json as RefusalBody).reason,
    `${card.callsign} is a shell pane — mail would be typed into a shell`,
  );

  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnedBody.spawn_id}/revive`, {});
  assert.equal(revive.status, 410);
  assert.match((revive.json as RefusalBody).reason, /no conversation to resume/);
  const remote = await postJson(`${daemon.baseUrl}/api/spawn/${spawnedBody.spawn_id}/rc`, {});
  assert.equal(remote.status, 409);
  assert.match((remote.json as RefusalBody).reason, /unavailable for shell/);
});

test('shell kill needs no force; liveness accepts any running command; retention ignores silence', async (t) => {
  const cwd = scratch('fd-shell-core-');
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const tmuxState = fakeAdapter();
  const core = createCore(db, {
    port: 4711,
    home: '/tmp/fd-shell-core-home',
    tmuxAdapter: tmuxState.adapter,
  });

  const first = (await core.spawn({ kind: 'shell', cwd })) as CoreSpawnResult;
  assert.equal(first.status, 200);
  const sid = first.body.session_id;
  const spawnId = first.body.spawn_id;
  const win0 = tmuxState.state.windows[0];
  assert.ok(win0);
  win0.pane_cmd = 'vim';
  // Production requires CONDEMN_DEAD_READS (=2 in spawns.mjs) consecutive dead
  // reads before condemning a live spawn, so one tick can only ever record a
  // dead streak of 1 and still observe 'live' — a liveness regression against
  // arbitrary commands would stay hidden behind a green test. Drive one full
  // condemnation cycle's worth of completed ticks; the shell must remain live
  // through every one of them.
  for (let tick = 0; tick < 2; tick++) await core.spawnLivenessTick();
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(spawnId)?.status,
    'live',
  );

  db.prepare('UPDATE sessions SET last_seen=? WHERE session_id=?').run(1, sid);
  await core.retentionSweep(Date.now() + 24 * 3_600_000);
  assert.equal(
    db.prepare<ColRow>('SELECT col FROM sessions WHERE session_id=?').get(sid)?.col,
    'idle',
  );

  const killed = (await core.spawnKill(spawnId, false)) as SpawnKillResult;
  assert.equal(killed.status, 200);
  assert.equal(killed.body.status, 'killed');
});

test(
  'real tmux keeps a healthy bash shell and condemns it after exit',
  {
    skip: !tmuxOk() && 'tmux unavailable',
  },
  async (t) => {
    const cwd = scratch('fd-shell-real-');
    const daemon = await startDaemon({
      env: {
        SHELL: '/bin/bash',
        FLEETDECK_AGENTS_POLL_MS: '100',
        FLEETDECK_NUDGE_MS: '60000',
      },
    });
    const socket = `fleetdeck-test-${daemon.port}`;
    t.after(async () => {
      await daemon.stop();
      rmSync(cwd, { recursive: true, force: true });
    });

    const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { kind: 'shell', cwd });
    assert.equal(spawned.status, 200);
    const spawnedBody = spawned.json as SpawnHttpAck;
    await new Promise((resolve) => setTimeout(resolve, 450));
    let card = ((await getJson(`${daemon.baseUrl}/state`)).json as StateResponse).sessions.find(
      (s) => s.session_id === spawnedBody.session_id,
    );
    assert.ok(card);
    assert.equal(card.spawn?.status, 'live', 'a bare bash shell is healthy for kind=shell');

    const target = `=${spawnedBody.tmux.session}:=${spawnedBody.tmux.window}`;
    tmux(socket, ['send-keys', '-t', target, 'exit', 'Enter']);
    card = await waitUntil(
      async () => {
        const s = ((await getJson(`${daemon.baseUrl}/state`)).json as StateResponse).sessions.find(
          (row) => row.session_id === spawnedBody.session_id,
        );
        return s?.spawn?.status === 'pane-dead' ? s : null;
      },
      { timeoutMs: 5000, label: 'shell pane condemnation after exit' },
    );
    assert.equal(card.col, 'offline');
    assert.match(card.note ?? '', /shell pane exited/);
  },
);

// --- adversarial-review MAJOR-2: the ORCHESTRATOR path must not route into a
// shell. /mail's walls (fan-out exclusion + direct 409) do not cover /command:
// `assign` resolves through resolveTargets and delivers via the raw mail()
// insert, and `assign auto`'s autoCandidate ranked an idle shell FIRST — a
// task the orchestrator reports as delivered would sit undeliverable forever
// (ownedPaneRow refuses to type into a shell). These pin all three walls.
test('orchestrator assign/assign-auto never route a task into a shell pane', async (t) => {
  const cwd = scratch('fd-shell-orch-');
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const tmuxState = fakeAdapter();
  const core = createCore(db, {
    port: 4711,
    home: '/tmp/fd-shell-orch-home',
    tmuxAdapter: tmuxState.adapter,
  });

  const spawned = (await core.spawn({ kind: 'shell', cwd })) as CoreSpawnResult;
  assert.equal(spawned.status, 200);
  const shellCard = db
    .prepare<SessionRow>('SELECT * FROM sessions WHERE session_id = ?')
    .get(spawned.body.session_id);
  assert.ok(shellCard);

  // assign auto with ONLY an idle shell on the board: unrouted, never the shell
  const auto = core.command('assign auto do the thing') as unknown as CommandResult;
  assert.equal(auto.ok, false);
  assert.equal(auto.unrouted, true);

  // direct assign naming the shell's callsign: resolves to nothing
  core.command(`assign ${shellCard.callsign} do the thing`);
  assert.equal(
    db
      .prepare<CountRow>('SELECT COUNT(*) AS n FROM mail WHERE to_session = ?')
      .get(shellCard.session_id)?.n,
    0,
    'no mail row may ever target a shell session',
  );

  // and the shell's prev_callsign (after a rename) must not route either
  core.applyCustomName(shellCard.session_id, 'renamed');
  const renamed = db
    .prepare<SessionRow>('SELECT * FROM sessions WHERE session_id = ?')
    .get(shellCard.session_id);
  assert.ok(renamed);
  assert.ok(renamed.prev_callsign, 'rename must record the birth name');
  core.command(`assign ${renamed.prev_callsign} do the thing`);
  core.command(`assign ${renamed.callsign} do the thing`);
  assert.equal(
    db
      .prepare<CountRow>('SELECT COUNT(*) AS n FROM mail WHERE to_session = ?')
      .get(shellCard.session_id)?.n,
    0,
    "neither of a shell's names may route orchestrator mail",
  );
});

// --- adversarial-review MINOR-1: a shell's ABANDONED birth name must not block
// mail to a live claude that now wears it. Current-name-wins, exactly like
// resolveTargets: the 409 fires only when everything the name resolves to is a
// shell.
test('a shell prev_callsign does not block mail to the claude now wearing the name', async (t) => {
  const cwd = scratch('fd-shell-prevname-');
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const tmuxState = fakeAdapter();
  const core = createCore(db, {
    port: 4711,
    home: '/tmp/fd-shell-prevname-home',
    tmuxAdapter: tmuxState.adapter,
  });

  const spawned = (await core.spawn({ kind: 'shell', cwd })) as CoreSpawnResult;
  const shellCard = db
    .prepare<SessionRow>('SELECT * FROM sessions WHERE session_id = ?')
    .get(spawned.body.session_id);
  assert.ok(shellCard);
  // rename the shell away from its birth name, then hand that name to a claude
  core.applyCustomName(shellCard.session_id, 'moved');
  const birth = db
    .prepare<PrevRow>('SELECT prev_callsign FROM sessions WHERE session_id = ?')
    .get(shellCard.session_id)?.prev_callsign;
  assert.ok(birth);
  db.prepare('UPDATE sessions SET callsign = ? WHERE session_id = ?').run(
    birth,
    seedClaude(db, cwd),
  );

  // postMail returns the plain body on success and {status, body} on refusal
  const res = (await core.postMail({
    to: birth,
    from: 'tester',
    text: 'for the claude',
  })) as unknown as PostMailSuccess;
  assert.equal(
    res.ok,
    true,
    `mail to the reissued name must reach the claude (got ${JSON.stringify(res)})`,
  );
  assert.equal(res.delivered, 1, 'exactly the claude receives it');

  // while a name that STILL resolves only to the shell keeps the loud 409
  const stillShell = db
    .prepare<CallsignRow>('SELECT callsign FROM sessions WHERE session_id = ?')
    .get(shellCard.session_id)?.callsign;
  assert.ok(stillShell);
  const refused = (await core.postMail({
    to: stillShell,
    from: 'tester',
    text: 'nope',
  })) as unknown as PostMailRefusal;
  assert.equal(refused.status, 409);
  assert.match(refused.body.reason, /shell pane/);
});

// A hook-registered claude session row, minimal columns, for mail-routing tests.
function seedClaude(db: SqliteHandle, cwd: string): string {
  const sid = 'claude-' + Math.random().toString(16).slice(2, 10);
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, cwd, col, source, started_at, last_seen)
    VALUES (?, ?, ?, 'idle', 'hooks', ?, ?)`,
  ).run(sid, 'temp-' + sid.slice(-4), cwd, Date.now(), Date.now());
  return sid;
}
