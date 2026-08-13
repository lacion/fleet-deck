import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmodSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { startDaemon } from './helpers/daemon.ts';
import { getJson, postJson } from './helpers/http.ts';
import { waitForSpecRecords, waitUntil } from './helpers/wait.ts';
import type { StateResponse, SessionEntry } from '../contracts/state.ts';

// createCore's tmuxAdapter option is a full TmuxAdapter; the fakes/null below
// ride in via `as unknown as CoreTmuxAdapter`.
type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

// core.spawn is typed `(...args: unknown[]) => unknown`; this is the facet the
// tests read off its resolved value.
interface CoreSpawnResult {
  status: number;
  body: { session_id: string; spawn_id: string };
}

// The window spec the injected adapter's newWindow captures (only the fields
// the test reads). env is a real map, so FLEETDECK_SETUP_CMD is a named
// optional property (dot access) rather than pure index access.
interface NewWindowSpec {
  callsign: string;
  env: { FLEETDECK_SETUP_CMD?: string; [key: string]: string | undefined };
  argv: string[];
}
interface FakeWindow {
  session: string;
  window: string;
  window_id: string;
  pane_dead: boolean;
  pane_cmd: string;
}

// The FLEETDECK_TEST_SPAWN_RECORD spec-record facet (the shell argv the daemon
// would have handed the pane).
interface ShellArgvSpec {
  setup_cmd: string;
  env: { FLEETDECK_SETUP_CMD?: string; [key: string]: string | undefined };
  gateway_env: unknown;
  argv: string[];
}
interface SpecRecord {
  parsed: ShellArgvSpec;
}

// POST /api/spawn HTTP acknowledgement facet.
interface SpawnHttpAck {
  session_id: string;
  tmux: { session: string; window: string };
}

// GET/POST /api/settings and /state settings-carrying facet.
interface SettingsResponse {
  settings: { repo_setup: Record<string, unknown> };
}

// 400 refusal body facet.
interface RefusalBody {
  reason?: string;
}

// db.prepare row facets.
interface StatusRow {
  status: string;
}
interface NoteRow {
  note: string | null;
}

const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.ts');
try {
  chmodSync(FIXTURE, 0o755);
} catch {
  /* best effort */
}

const EXPECTED_WRAPPER = [
  'cmd=$FLEETDECK_SETUP_CMD; unset FLEETDECK_SETUP_CMD',
  'printf \'▶ fleetdeck setup: %s\\n\' "$cmd"',
  'sh -c "$cmd"; rc=$?',
  'if [ "$rc" -ne 0 ]; then printf \'✗ setup failed (exit %s) — claude not started\\n\' "$rc"; exit "$rc"; fi',
  'exec "$@"',
].join('\n');

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
const overrideEnv = (record: string): Record<string, string> => ({
  FLEETDECK_SPAWN_CMD: FIXTURE,
  FLEETDECK_TEST_SPAWN_RECORD: record,
});

test('injected adapter receives setup through env and the fixed wrapper argv', async (t) => {
  const cwd = scratch('fd-setup-core-');
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  let launched: NewWindowSpec | undefined;
  let window: FakeWindow | undefined;
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: (p: number) => `fleetdeck-${p}`,
    windowName: (p: number, callsign: string) => `fd${p}-${callsign}`,
    ensureSession: (): Promise<string> => Promise.resolve('fleetdeck-4711'),
    newWindow: (spec: NewWindowSpec): Promise<FakeWindow> => {
      launched = spec;
      window = {
        session: 'fleetdeck-4711',
        window: `fd4711-${spec.callsign}`,
        window_id: '@1',
        pane_dead: false,
        pane_cmd: 'sh',
      };
      return Promise.resolve(window);
    },
    listScopedWindows: (): Promise<FakeWindow[]> => Promise.resolve(window ? [window] : []),
    paneCurrentCommand: (): Promise<{ dead: boolean; cmd: string } | null> =>
      Promise.resolve(window ? { dead: window.pane_dead, cmd: window.pane_cmd } : null),
    killWindowVerified: (): Promise<{ ok: boolean }> => Promise.resolve({ ok: true }),
    capturePane: (): Promise<string> => Promise.resolve(''),
    sendBringupEnter: (): Promise<boolean> => Promise.resolve(true),
    fleetServerAbsent: (): Promise<boolean> => Promise.resolve(false),
  };
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const core = createCore(db, {
    port: 4711,
    home: '/tmp/fd-setup-core-home',
    tmuxAdapter: adapter as unknown as CoreTmuxAdapter,
  });
  const setup = 'printf "%s" "$(id)"';
  const spawned = (await core.spawn({
    cwd,
    setup_cmd: setup,
    prompt: '--literal',
  })) as CoreSpawnResult;
  assert.equal(spawned.status, 200);
  assert.ok(launched, 'adapter.newWindow should have captured the launch spec');
  assert.ok(window, 'adapter.newWindow should have created a window record');
  assert.equal(launched.env.FLEETDECK_SETUP_CMD, setup);
  assert.equal(launched.argv.includes(setup), false);
  const sh = launched.argv.indexOf('sh');
  assert.deepEqual(launched.argv.slice(sh), [
    'sh',
    '-c',
    EXPECTED_WRAPPER,
    'fleetdeck-setup',
    'claude',
    '--session-id',
    spawned.body.session_id,
    '--',
    '--literal',
  ]);
  const settings = core.setSettings({ repo_setup: { fleetdeck: setup } });
  assert.equal(settings.status, 200);
  assert.deepEqual(core.snapshot().settings['repo_setup'], { fleetdeck: setup });
  assert.equal(core.setSettings({ repo_setup: { fleetdeck: 'x'.repeat(2001) } }).status, 400);

  await core.spawnLivenessTick();
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(spawned.body.spawn_id)
      ?.status,
    'spawning',
    'a live sh setup phase is not mistaken for Claude having exited',
  );
  window.pane_dead = true;
  await core.spawnLivenessTick();
  assert.equal(
    db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id=?').get(spawned.body.spawn_id)
      ?.status,
    'pane-dead',
    'pane_dead during setup condemns immediately without hysteresis',
  );
  assert.equal(
    db.prepare<NoteRow>('SELECT note FROM sessions WHERE session_id=?').get(spawned.body.session_id)
      ?.note,
    'pane exited during setup/bring-up — open the terminal for the error',
  );
});

test('setup_cmd body validation rejects bad types, size, controls, and shell combination', async (t) => {
  const cwd = scratch('fd-setup-validation-');
  const daemon = await startDaemon({ env: overrideEnv(path.join(cwd, 'records.jsonl')) });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const cases: [Record<string, unknown>, RegExp][] = [
    [{ cwd, setup_cmd: 42 }, /must be a string/],
    [{ cwd, setup_cmd: 'x'.repeat(2001) }, /2000/],
    [{ cwd, setup_cmd: 'ok\u0000bad' }, /control/],
    [{ cwd, setup_cmd: 'ok\tbad' }, /control/],
    [{ kind: 'shell', cwd, setup_cmd: 'echo no' }, /Claude-only/],
  ];
  for (const [body, reason] of cases) {
    const res = await postJson(`${daemon.baseUrl}/api/spawn`, body);
    assert.equal(res.status, 400);
    assert.match((res.json as RefusalBody | null)?.reason ?? '', reason);
  }
});

test('setup wrapper is fixed, Claude argv stays positional, and setup text rides env only', async (t) => {
  const cwd = scratch('fd-setup-override-');
  const record = path.join(cwd, 'records.jsonl');
  const daemon = await startDaemon({ env: overrideEnv(record) });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const hostile = 'printf \'"quoted"; $HOME $(id) `whoami`\\n\'';
  const prompt = '--dangerously-skip-permissions; $(touch never)';
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    setup_cmd: hostile,
    prompt,
    model: 'sonnet',
    permission_mode: 'acceptEdits',
  });
  assert.equal(spawned.status, 200);
  const ack = spawned.json as SpawnHttpAck;
  const last = (await waitForSpecRecords(record, 1)).at(-1) as SpecRecord | undefined;
  assert.ok(last, 'a spawn spec record should have been captured');
  const spec = last.parsed;
  assert.equal(spec.setup_cmd, hostile);
  assert.equal(spec.env.FLEETDECK_SETUP_CMD, hostile);
  assert.equal(spec.gateway_env, null);
  assert.equal(
    spec.argv.some((arg) => arg === hostile || arg.includes(hostile)),
    false,
    'user setup text must never enter pane argv or shell source',
  );

  const sh = spec.argv.indexOf('sh');
  assert.ok(sh > 0);
  assert.deepEqual(spec.argv.slice(sh), [
    'sh',
    '-c',
    EXPECTED_WRAPPER,
    'fleetdeck-setup',
    'claude',
    '--session-id',
    ack.session_id,
    '--model',
    'sonnet',
    '--permission-mode',
    'acceptEdits',
    '--',
    prompt,
  ]);
  assert.equal(spec.argv.at(-2), '--');
  assert.equal(spec.argv.at(-1), prompt);
  assert.equal(
    spec.argv.includes('-u') &&
      spec.argv.some((arg, i) => arg === 'FLEETDECK_SETUP_CMD' && spec.argv[i - 1] === '-u'),
    false,
    'the launch keeps its deliberate setup env until the fixed wrapper unsets it',
  );

  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  const card = state.sessions.find((s) => s.session_id === ack.session_id);
  assert.ok(card, 'the spawned session should appear in /state');
  assert.equal(card.spawn?.setup_cmd, hostile);
});

test('repo_setup validates, persists, and rides settings broadcasts', async (t) => {
  const home = scratch('fd-setup-settings-home-');
  const first = await startDaemon({ home });
  t.after(async () => {
    if (first.proc.exitCode === null) await first.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true });
  });

  const value = { fleetdeck: 'super code\npython -m venv .venv' };
  const saved = await postJson(`${first.baseUrl}/api/settings`, { repo_setup: value });
  assert.equal(saved.status, 200);
  assert.deepEqual((saved.json as SettingsResponse).settings.repo_setup, value);
  const state = await getJson(`${first.baseUrl}/state`);
  assert.deepEqual((state.json as SettingsResponse).settings.repo_setup, value);

  for (const body of [
    { repo_setup: [] },
    { repo_setup: { fleetdeck: 7 } },
    { repo_setup: { fleetdeck: 'x'.repeat(2001) } },
    {
      repo_setup: Object.fromEntries(
        Array.from({ length: 51 }, (_, i): [string, string] => [`repo-${i}`, 'true']),
      ),
    },
  ]) {
    const rejected = await postJson(`${first.baseUrl}/api/settings`, body);
    assert.equal(rejected.status, 400);
  }

  await first.stop({ keepHome: true });
  const second = await startDaemon({ port: first.port, home });
  t.after(async () => {
    await second.stop({ keepHome: true });
  });
  const restored = await getJson(`${second.baseUrl}/api/settings`);
  assert.deepEqual((restored.json as SettingsResponse).settings.repo_setup, value);
});

test('repo_setup keeps a legitimate __proto__ repo entry through a raw JSON body', async (t) => {
  const home = scratch('fd-setup-proto-home-');
  const daemon = await startDaemon({ home });
  t.after(async () => {
    await daemon.stop();
    rmSync(home, { recursive: true, force: true });
  });

  // Raw JSON: object-literal syntax { __proto__: ... } would never create an
  // own property, so the body must arrive as text.
  const res = await fetch(`${daemon.baseUrl}/api/settings`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: '{"repo_setup":{"__proto__":"echo setup"}}',
    signal: AbortSignal.timeout(5000),
  });
  assert.equal(res.status, 200);
  const saved = JSON.parse(await res.text()) as SettingsResponse;
  assert.equal(Object.keys(saved.settings.repo_setup)[0], '__proto__');
  assert.equal(saved.settings.repo_setup['__proto__'], 'echo setup');

  const state = await getJson(`${daemon.baseUrl}/state`);
  assert.equal((state.json as SettingsResponse).settings.repo_setup['__proto__'], 'echo setup');

  const restored = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(
    (restored.json as SettingsResponse).settings.repo_setup['__proto__'],
    'echo setup',
    'the stored row must serialize the entry back, not an empty object',
  );
});

test('repo_setup_patch merges per-repository saves, deletes on the sentinel, and validates like repo_setup', async (t) => {
  const home = scratch('fd-setup-patch-home-');
  const daemon = await startDaemon({ home });
  t.after(async () => {
    await daemon.stop();
    rmSync(home, { recursive: true, force: true });
  });

  const post = (body: unknown) => postJson(`${daemon.baseUrl}/api/settings`, body);
  const current = async (): Promise<Record<string, unknown>> =>
    ((await getJson(`${daemon.baseUrl}/api/settings`)).json as SettingsResponse).settings
      .repo_setup;

  // Two boards holding the SAME stale snapshot each save their own repository;
  // the merge must keep both — a whole-object replace would keep only the last.
  const boardA = await post({ repo_setup_patch: { repoA: 'echo A' } });
  assert.equal(boardA.status, 200);
  assert.deepEqual((boardA.json as SettingsResponse).settings.repo_setup, { repoA: 'echo A' });
  const boardB = await post({ repo_setup_patch: { repoB: 'echo B' } });
  assert.equal(boardB.status, 200);
  assert.deepEqual((boardB.json as SettingsResponse).settings.repo_setup, {
    repoA: 'echo A',
    repoB: 'echo B',
  });

  // A patch OVERWRITES one entry in place without touching the other.
  const updated = await post({ repo_setup_patch: { repoA: 'echo A2' } });
  assert.deepEqual((updated.json as SettingsResponse).settings.repo_setup, {
    repoA: 'echo A2',
    repoB: 'echo B',
  });

  // A stale whole-object writer (the old board shape) still works and CANNOT
  // resurrect a deleted entry — its own snapshot simply never had it.
  const whole = await post({ repo_setup: { repoC: 'echo C' } });
  assert.deepEqual((whole.json as SettingsResponse).settings.repo_setup, { repoC: 'echo C' });

  // Patches apply onto the whole-object state, and the tombstone deletes.
  const merged = await post({ repo_setup_patch: { repoD: 'echo D', repoC: '__delete' } });
  assert.equal(merged.status, 200);
  assert.deepEqual(await current(), { repoD: 'echo D' });

  // Deleting the last entry clears the row, and a null patch clears everything.
  await post({ repo_setup_patch: { repoD: '__delete' } });
  assert.deepEqual(await current(), {});
  await post({ repo_setup_patch: { repoA: 'echo A' } });
  await post({ repo_setup_patch: null });
  assert.deepEqual(await current(), {});

  // The same gates as repo_setup, entry by entry, and nothing half-applies.
  for (const body of [
    { repo_setup_patch: [] },
    { repo_setup_patch: { repoA: 7 } },
    { repo_setup_patch: { repoA: 'x'.repeat(2001) } },
    { repo_setup_patch: { 'bad\tname': 'true' } },
    { repo_setup_patch: { repoA: 'ok', repoB: 7 } },
  ]) {
    const rejected = await post(body);
    assert.equal(rejected.status, 400, JSON.stringify(body));
  }
  assert.deepEqual(await current(), {});
});

test('setRepoSetupEntry writes one repo without a broadcast and drops junk', (t) => {
  const db = openDb(':memory:');
  t.after(() => {
    db.close();
  });
  const core = createCore(db, {
    port: 4712,
    home: '/tmp/fd-setup-entry-home',
    // No tmuxAdapter: this test exercises only repo_setup, so let createCore fall
    // back to defaultTmuxAdapter. Passing `null as unknown as CoreTmuxAdapter`
    // (a cast past a NonNullable type) used to defeat that default — the boot
    // retentionSweep then read `.spawnOverrideCmd` off null and threw, swallowed
    // by derive.ts's fire-and-forget .catch. See ts-migration-bugs.md BUG-TMUXNULL.
  });
  const repoSetup = (): Record<string, string> => core.resolveSettings().repo_setup;

  core.setRepoSetupEntry('fleetdeck', 'super code');
  core.setRepoSetupEntry('other', 'echo hi');
  assert.deepEqual(repoSetup(), { fleetdeck: 'super code', other: 'echo hi' });
  core.setRepoSetupEntry('fleetdeck', null); // null deletes its entry
  assert.deepEqual(repoSetup(), { other: 'echo hi' });
  core.setRepoSetupEntry('bad\tname', 'true'); // invalid: silently no-ops
  core.setRepoSetupEntry('other', 'x'.repeat(2001));
  assert.deepEqual(repoSetup(), { other: 'echo hi' });
});

test('real tmux setup failure stays visible, condemns immediately, and never starts Claude', {
  skip: !tmuxOk() && 'tmux unavailable',
}, async (t) => {
  const cwd = scratch('fd-setup-fail-');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_NUDGE_MS: '60000',
      FLEETDECK_SETUP_REGISTER_MS: '1000',
    },
  });
  const socket = `fleetdeck-test-${daemon.port}`;
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, setup_cmd: 'exit 7' });
  assert.equal(spawned.status, 200);
  const ack = spawned.json as SpawnHttpAck;
  const card = await waitUntil(
    async (): Promise<SessionEntry | null> => {
      const s = ((await getJson(`${daemon.baseUrl}/state`)).json as StateResponse).sessions.find(
        (row) => row.session_id === ack.session_id,
      );
      return s?.spawn?.status === 'pane-dead' ? s : null;
    },
    { timeoutMs: 5000, label: 'setup failure condemnation' },
  );
  assert.equal(card.note, 'pane exited during setup/bring-up — open the terminal for the error');

  const target = `=${ack.tmux.session}:=${ack.tmux.window}`;
  // -S - : include scrollback. tmux moves earlier lines (the ▶ banner) into
  // history on the detached pane by the time it dies; the visible screen keeps
  // the ✗ failure line, which is the human-facing guarantee — the banner is
  // asserted from history.
  const screen = tmux(socket, ['capture-pane', '-p', '-S', '-', '-t', target]);
  assert.match(screen, /fleetdeck setup: exit 7/);
  assert.match(screen, /setup failed \(exit 7\) — claude not started/);
  assert.doesNotMatch(screen, /claude: .*not found/);
});

test('real tmux long-running setup is not condemned while sh/setup binary runs', {
  skip: !tmuxOk() && 'tmux unavailable',
}, async (t) => {
  const cwd = scratch('fd-setup-sleep-');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_AGENTS_POLL_MS: '100',
      FLEETDECK_NUDGE_MS: '60000',
      FLEETDECK_SETUP_REGISTER_MS: '250',
    },
  });
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, setup_cmd: 'sleep 30' });
  assert.equal(spawned.status, 200);
  const ack = spawned.json as SpawnHttpAck;
  // Require the liveness poller to actually run and classify the still-running
  // setup: a row observed still in its initial 'spawning' state proves nothing
  // about the scheduler (BUG-178).
  const card = await waitUntil(
    async (): Promise<SessionEntry | null> => {
      const s = ((await getJson(`${daemon.baseUrl}/state`)).json as StateResponse).sessions.find(
        (row) => row.session_id === ack.session_id,
      );
      return s?.spawn?.status === 'stalled' ? s : null;
    },
    { timeoutMs: 5000, label: 'long-running setup stall classification' },
  );
  assert.match(card.note ?? '', /setup may still be running/);
});
