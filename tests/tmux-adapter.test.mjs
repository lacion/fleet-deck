// tests/tmux-adapter.test.mjs
//
// Exercise the real tmux read/parse path. tmux's formatted-output printer
// escapes a literal unit separator as "\\037", which used to make every
// scoped window lookup return empty even though the pane existed.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { randomBytes, randomInt, randomUUID } from 'node:crypto';
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  ensureSession,
  exactWindowTarget,
  killWindowVerified,
  listScopedWindows,
  newWindow,
  paneCurrentCommand,
  sendEnter,
  sessionName,
} from '../scripts/fleetd/spawn.mjs';

function tmuxOk() {
  const socket = `fleetdeck-adapter-probe-${process.pid}-${randomBytes(4).toString('hex')}`;
  try {
    execFileSync('tmux', ['-L', socket, '-f', '/dev/null', 'new-session', '-d', 'sleep 1'], { stdio: 'ignore' });
    execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' });
    return true;
  } catch {
    try { execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' }); } catch { /* no server */ }
    return false;
  }
}

function tmux(socket, args) {
  return execFileSync('tmux', ['-L', socket, ...args], { encoding: 'utf8' }).trim();
}

function tmuxStatus(socket, args) {
  return spawnSync('tmux', ['-L', socket, ...args], { stdio: 'ignore' }).status;
}

function restoreEnv(previous) {
  for (const [key, value] of previous) {
    if (value == null) delete process.env[key];
    else process.env[key] = value;
  }
}

function useLegacyGenerationMode(t) {
  const previousHome = process.env.FLEETDECK_HOME;
  delete process.env.FLEETDECK_HOME;
  t.after(() => {
    if (previousHome == null) delete process.env.FLEETDECK_HOME;
    else process.env.FLEETDECK_HOME = previousHome;
  });
}

function pidAlive(pid) {
  try { process.kill(pid, 0); return true; } catch { return false; }
}

async function stopPid(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 1 || !pidAlive(pid)) return;
  try { process.kill(pid, 'SIGTERM'); } catch { return; }
  for (let i = 0; i < 50 && pidAlive(pid); i += 1) {
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  if (pidAlive(pid)) {
    try { process.kill(pid, 'SIGKILL'); } catch { /* exited between probe and signal */ }
  }
}

function isolatedTmuxEnv(_prefix) {
  const home = mkdtempSync(path.join(tmpdir(), 'fd-tg-'));
  const socketRoot = path.join(home, 's');
  mkdirSync(socketRoot, { mode: 0o700 });
  const socket = `g-${process.pid}-${randomBytes(4).toString('hex')}`;
  const previous = new Map([
    ['FLEETDECK_HOME', process.env.FLEETDECK_HOME],
    ['FLEETDECK_TMUX_SOCKET', process.env.FLEETDECK_TMUX_SOCKET],
    ['TMUX_TMPDIR', process.env.TMUX_TMPDIR],
  ]);
  process.env.FLEETDECK_HOME = home;
  process.env.FLEETDECK_TMUX_SOCKET = socket;
  process.env.TMUX_TMPDIR = socketRoot;
  const socketPath = path.join(socketRoot, `tmux-${process.getuid()}`, socket);
  return { home, socketRoot, socket, socketPath, previous };
}

test('persisted generation gates successful empty listings and replacement creation', async (t) => {
  const port = 29_995;
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-generation-fake-'));
  const fakeTmux = path.join(dir, 'tmux');
  const reachable = path.join(dir, 'reachable');
  const serverGeneration = path.join(dir, 'server-generation');
  const creations = path.join(dir, 'creations');
  const previous = new Map([
    ['PATH', process.env.PATH],
    ['FLEETDECK_HOME', process.env.FLEETDECK_HOME],
    ['FLEETDECK_TMUX_SOCKET', process.env.FLEETDECK_TMUX_SOCKET],
    ['FLEETDECK_FAKE_TMUX_REACHABLE', process.env.FLEETDECK_FAKE_TMUX_REACHABLE],
    ['FLEETDECK_FAKE_TMUX_GENERATION', process.env.FLEETDECK_FAKE_TMUX_GENERATION],
    ['FLEETDECK_FAKE_TMUX_CREATIONS', process.env.FLEETDECK_FAKE_TMUX_CREATIONS],
    ['FLEETDECK_FAKE_TMUX_PID', process.env.FLEETDECK_FAKE_TMUX_PID],
  ]);
  writeFileSync(fakeTmux, `#!/bin/sh
case " $* " in
  *" new-session "*)
    : > "$FLEETDECK_FAKE_TMUX_REACHABLE"
    printf 'created\n' >> "$FLEETDECK_FAKE_TMUX_CREATIONS"
    exit 0
    ;;
  *" show-options "*)
    [ -f "$FLEETDECK_FAKE_TMUX_REACHABLE" ] || exit 1
    [ -f "$FLEETDECK_FAKE_TMUX_GENERATION" ] && cat "$FLEETDECK_FAKE_TMUX_GENERATION"
    exit 0
    ;;
  *" set-option "*)
    [ -f "$FLEETDECK_FAKE_TMUX_REACHABLE" ] || exit 1
    for value do :; done
    printf '%s\n' "$value" > "$FLEETDECK_FAKE_TMUX_GENERATION"
    exit 0
    ;;
  *" display-message "*)
    [ -f "$FLEETDECK_FAKE_TMUX_REACHABLE" ] || exit 1
    value=''
    [ -f "$FLEETDECK_FAKE_TMUX_GENERATION" ] && value=$(cat "$FLEETDECK_FAKE_TMUX_GENERATION")
    case " $* " in
      *" ; "*) printf '__fleetdeck_tmux_generation__=%s\t%s\n' "$value" "$FLEETDECK_FAKE_TMUX_PID" ;;
      *) printf '%s\t%s\n' "$value" "$FLEETDECK_FAKE_TMUX_PID" ;;
    esac
    exit 0
    ;;
esac
exit 1
`);
  chmodSync(fakeTmux, 0o700);
  process.env.PATH = `${dir}:${process.env.PATH}`;
  process.env.FLEETDECK_HOME = dir;
  process.env.FLEETDECK_TMUX_SOCKET = 'adapter-generation-contract';
  process.env.FLEETDECK_FAKE_TMUX_REACHABLE = reachable;
  process.env.FLEETDECK_FAKE_TMUX_GENERATION = serverGeneration;
  process.env.FLEETDECK_FAKE_TMUX_CREATIONS = creations;
  process.env.FLEETDECK_FAKE_TMUX_PID = String(process.pid);
  t.after(() => {
    restoreEnv(previous);
    rmSync(dir, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port), 'first run may create and claim a server');
  assert.deepEqual(await listScopedWindows(port), [], 'claimed server empty listing is authoritative');
  const expectedFile = path.join(dir, `tmux-generation-${port}`);
  const expected = JSON.parse(readFileSync(expectedFile, 'utf8'));
  assert.match(expected.generation, /^[0-9a-f-]{36}$/);
  assert.equal(expected.serverPid, process.pid);
  assert.deepEqual(Object.keys(expected).sort(), ['generation', 'serverPid']);
  assert.equal(statSync(expectedFile).mode & 0o777, 0o600);
  assert.equal(readFileSync(creations, 'utf8').trim().split('\n').length, 1);

  writeFileSync(expectedFile, `${expected.generation}\n`);
  assert.deepEqual(await listScopedWindows(port), [], 'matching legacy UUID can be corroborated conservatively');
  assert.deepEqual(JSON.parse(readFileSync(expectedFile, 'utf8')), expected, 'matching legacy record upgrades to strict generation + PID JSON');

  writeFileSync(serverGeneration, `${randomUUID()}\n`);
  assert.equal(await listScopedWindows(port), null, 'different server generation makes empty UNKNOWN');
  const killed = await killWindowVerified(`fd${port}-missing`);
  assert.equal(killed.gone, undefined);
  assert.match(killed.error, /generation/i);
  await assert.rejects(ensureSession(port), /generation/i);
  assert.equal(readFileSync(creations, 'utf8').trim().split('\n').length, 1, 'expected generation forbids replacement creation');
});

test('first run creates and claims a tmux server with an owner-only generation file', { skip: !tmuxOk() && 'tmux server unavailable' }, async (t) => {
  const port = 24_000 + randomInt(500);
  const env = isolatedTmuxEnv('fleetdeck-tmux-generation-first-');
  let serverPid = null;
  t.after(async () => {
    await stopPid(serverPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port));
  serverPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  assert.ok(pidAlive(serverPid), 'created tmux server is live');

  const file = path.join(env.home, `tmux-generation-${port}`);
  const record = JSON.parse(readFileSync(file, 'utf8'));
  assert.match(record.generation, /^[0-9a-f-]{36}$/);
  assert.equal(record.serverPid, serverPid);
  assert.deepEqual(Object.keys(record).sort(), ['generation', 'serverPid']);
  assert.equal(statSync(file).mode & 0o777, 0o600, 'generation file is owner-only');
  assert.equal(
    tmux(env.socket, ['show-options', '-gqv', `@fleetdeck_generation_${port}`]),
    record.generation,
    'persisted and server-side generations match',
  );
});

test('ensureSession retires a normally exited tmux owner and claims a new generation', { skip: !tmuxOk() && 'tmux server unavailable' }, async (t) => {
  const port = 24_500 + randomInt(400);
  const env = isolatedTmuxEnv('fleetdeck-tmux-generation-recovery-');
  let newPid = null;
  t.after(async () => {
    await stopPid(newPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port));
  const file = path.join(env.home, `tmux-generation-${port}`);
  const first = JSON.parse(readFileSync(file, 'utf8'));
  assert.ok(pidAlive(first.serverPid));

  tmux(env.socket, ['kill-server']);
  for (let i = 0; i < 50 && pidAlive(first.serverPid); i += 1) {
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.equal(pidAlive(first.serverPid), false, 'normal kill makes the persisted owner definitively dead');
  assert.deepEqual(await listScopedWindows(port), [], 'a proven-dead owner makes its old fleet authoritatively empty');

  assert.equal(await ensureSession(port), sessionName(port));
  const second = JSON.parse(readFileSync(file, 'utf8'));
  newPid = second.serverPid;
  assert.notEqual(second.serverPid, first.serverPid);
  assert.notEqual(second.generation, first.generation);
  assert.ok(pidAlive(second.serverPid));
});

test('an unlinked socket replacement cannot impersonate the claimed tmux server', { skip: !tmuxOk() && 'tmux server unavailable' }, async (t) => {
  const port = 24_500 + randomInt(500);
  const env = isolatedTmuxEnv('fleetdeck-tmux-generation-replacement-');
  const fleetSession = sessionName(port);
  const fleetWindow = `fd${port}-original`;
  const replacementSession = `replacement-${port}`;
  let originalPid = null;
  let replacementPid = null;
  t.after(async () => {
    // The socket names only the replacement now, so clean both exact recorded
    // server PIDs rather than relying on a label that cannot reach the original.
    await stopPid(replacementPid);
    await stopPid(originalPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  tmux(env.socket, ['-f', '/dev/null', 'new-session', '-d', '-s', fleetSession, '-n', fleetWindow, 'sleep 3600']);
  originalPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  const claimed = await listScopedWindows(port);
  assert.equal(claimed?.[0]?.window, fleetWindow, 'pre-feature server and fleet window are claimed');
  assert.ok(pidAlive(originalPid));

  unlinkSync(env.socketPath);
  tmux(env.socket, ['-f', '/dev/null', 'new-session', '-d', '-s', replacementSession, 'sleep 3600']);
  replacementPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  assert.notEqual(replacementPid, originalPid, 'same label now reaches a different tmux server');

  assert.equal(await listScopedWindows(port), null, 'replacement empty listing is UNKNOWN');
  const killed = await killWindowVerified(fleetWindow);
  assert.equal(killed.ok, false);
  assert.equal(killed.gone, undefined, 'replacement is never authoritative gone');
  assert.match(killed.error, /generation/i);
  await assert.rejects(ensureSession(port), /generation/i, 'ensureSession refuses the replacement');
  assert.equal(tmuxStatus(env.socket, ['has-session', '-t', `=${fleetSession}`]), 1, 'ensureSession did not create or accept the fleet session on the replacement');
  assert.ok(pidAlive(originalPid), 'inaccessible original tmux server and panes remain alive');
  assert.ok(pidAlive(replacementPid), 'generation checks do not kill the replacement server either');
});

test('exact fleet target blocks same-number pane mutation and new-window launch after socket replacement', { skip: !tmuxOk() && 'tmux server unavailable' }, async (t) => {
  const port = 25_000 + randomInt(500);
  const env = isolatedTmuxEnv('fleetdeck-tmux-target-replacement-');
  const fleetSession = sessionName(port);
  const fleetWindow = `fd${port}-bound`;
  const replacementSession = `replacement-${port}`;
  const launched = path.join(env.home, 'replacement-launched');
  let originalPid = null;
  let replacementPid = null;
  t.after(async () => {
    await stopPid(replacementPid);
    await stopPid(originalPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), fleetSession);
  tmux(env.socket, ['rename-window', '-t', `=${fleetSession}:`, fleetWindow]);
  originalPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  const originalId = tmux(env.socket, ['display-message', '-p', '-t', `=${fleetSession}:=${fleetWindow}`, '#{window_id}']);
  const target = exactWindowTarget(port, fleetWindow);

  unlinkSync(env.socketPath);
  tmux(env.socket, ['-f', '/dev/null', 'new-session', '-d', '-s', replacementSession, '-n', 'decoy', 'sleep 3600']);
  replacementPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  const replacementId = tmux(env.socket, ['display-message', '-p', '-t', `=${replacementSession}:=decoy`, '#{window_id}']);
  assert.equal(replacementId, originalId, 'replacement reused the same numeric window id');

  assert.equal(await sendEnter(target), false, 'exact fleet target cannot redirect Enter to same-number decoy');
  await assert.rejects(
    newWindow({ port, callsign: 'never-launched', cwd: env.home, argv: ['sh', '-c', `touch ${launched}`] }),
    /tmux (?:new-window failed|server generation unavailable or changed)/,
  );
  assert.equal(tmuxStatus(env.socket, ['has-session', '-t', `=${fleetSession}`]), 1);
  assert.equal(tmuxStatus(env.socket, ['has-session', '-t', `=${replacementSession}`]), 0);
  assert.equal(spawnSync('test', ['-e', launched]).status, 1, 'replacement received no launch');
});

test('scoped listing distinguishes failure, validated empty, and malformed success', async (t) => {
  useLegacyGenerationMode(t);
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-list-'));
  const fakeTmux = path.join(dir, 'tmux');
  const previous = new Map([
    ['PATH', process.env.PATH],
    ['FLEETDECK_TMUX_SOCKET', process.env.FLEETDECK_TMUX_SOCKET],
    ['FLEETDECK_FAKE_TMUX_MODE', process.env.FLEETDECK_FAKE_TMUX_MODE],
  ]);
  writeFileSync(fakeTmux, `#!/bin/sh
case "$FLEETDECK_FAKE_TMUX_MODE" in
  fail) exit 1 ;;
  empty) exit 0 ;;
  malformed) printf '%s\n' 'fleetdeck-29999\tfd29999-agent\tnot-an-id\t0\tclaude'; exit 0 ;;
esac
exit 1
`);
  chmodSync(fakeTmux, 0o700);
  process.env.PATH = `${dir}:${process.env.PATH}`;
  process.env.FLEETDECK_TMUX_SOCKET = 'adapter-list-contract';
  t.after(() => {
    for (const [key, value] of previous) {
      if (value == null) delete process.env[key];
      else process.env[key] = value;
    }
    rmSync(dir, { recursive: true, force: true });
  });

  process.env.FLEETDECK_FAKE_TMUX_MODE = 'fail';
  assert.equal(await listScopedWindows(29_999), null, 'transport failure is UNKNOWN');
  process.env.FLEETDECK_FAKE_TMUX_MODE = 'empty';
  assert.deepEqual(await listScopedWindows(29_999), [], 'successful empty output is authoritative');
  process.env.FLEETDECK_FAKE_TMUX_MODE = 'malformed';
  assert.equal(await listScopedWindows(29_999), null, 'malformed successful output is UNKNOWN');
});

test('tmux adapter parses scoped panes and kills only the exact fleet session window', { skip: !tmuxOk() && 'tmux server unavailable' }, async (t) => {
  useLegacyGenerationMode(t);
  const port = 22_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-${process.pid}-${randomBytes(4).toString('hex')}`;
  const fleetSession = sessionName(port);
  const decoySession = `decoy-${port}`;
  const window = `fd${port}-adapter`;
  const previousSocket = process.env.FLEETDECK_TMUX_SOCKET;
  process.env.FLEETDECK_TMUX_SOCKET = socket;

  t.after(() => {
    try { tmux(socket, ['kill-server']); } catch { /* already gone */ }
    if (previousSocket == null) delete process.env.FLEETDECK_TMUX_SOCKET;
    else process.env.FLEETDECK_TMUX_SOCKET = previousSocket;
  });

  // Create the decoy first so an all-server scan encounters the wrong, same-name
  // window before the daemon-owned one. Exact session corroboration must exclude it.
  tmux(socket, ['-f', '/dev/null', 'new-session', '-d', '-s', decoySession, '-n', window, 'sleep 3600']);
  tmux(socket, ['new-session', '-d', '-s', fleetSession, '-n', window, 'sleep 3600']);
  tmux(socket, ['split-window', '-d', '-t', `${fleetSession}:${window}`, 'sleep 3600']);

  const fleetWindowId = tmux(socket, ['display-message', '-p', '-t', `${fleetSession}:${window}`, '#{window_id}']);
  const decoyWindowId = tmux(socket, ['display-message', '-p', '-t', `${decoySession}:${window}`, '#{window_id}']);
  assert.notEqual(fleetWindowId, decoyWindowId);

  const windows = await listScopedWindows(port);
  assert.deepEqual(windows, [{
    session: fleetSession,
    window,
    window_id: fleetWindowId,
    pane_dead: false,
    pane_cmd: 'sleep',
  }]);
  assert.deepEqual(await paneCurrentCommand(fleetWindowId), { dead: false, cmd: 'sleep' });

  const killed = await killWindowVerified(window);
  assert.deepEqual(killed, { ok: true, window_id: fleetWindowId });
  assert.equal(tmuxStatus(socket, ['has-session', '-t', `=${decoySession}`]), 0, 'same-name decoy session survives');
  assert.equal(tmuxStatus(socket, ['has-session', '-t', `=${fleetSession}`]), 1, 'fleet session exits after its only window is killed');
});

test('duplicate scoped window names are ambiguous and never selected or killed', { skip: !tmuxOk() && 'tmux server unavailable' }, async (t) => {
  useLegacyGenerationMode(t);
  const port = 23_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-duplicate-${process.pid}-${randomBytes(4).toString('hex')}`;
  const session = sessionName(port);
  const window = `fd${port}-duplicate`;
  const previousSocket = process.env.FLEETDECK_TMUX_SOCKET;
  process.env.FLEETDECK_TMUX_SOCKET = socket;
  t.after(() => {
    try { tmux(socket, ['kill-server']); } catch { /* already gone */ }
    if (previousSocket == null) delete process.env.FLEETDECK_TMUX_SOCKET;
    else process.env.FLEETDECK_TMUX_SOCKET = previousSocket;
  });

  tmux(socket, ['-f', '/dev/null', 'new-session', '-d', '-s', session, '-n', window, 'sleep 3600']);
  tmux(socket, ['new-window', '-d', '-t', `${session}:`, '-n', window, 'sleep 3600']);

  assert.equal(await listScopedWindows(port), null, 'duplicate name makes the fleet listing UNKNOWN');
  assert.deepEqual(await killWindowVerified(window), { ok: false, error: 'ambiguous scoped tmux window name' });
  const names = tmux(socket, ['list-windows', '-t', `=${session}`, '-F', '#{window_name}']).split('\n');
  assert.deepEqual(names, [window, window], 'neither duplicate was killed');
});

test('kill failure recheck treats a vanished fleet window as gone despite a same-name decoy', async (t) => {
  useLegacyGenerationMode(t);
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-fake-'));
  const fakeTmux = path.join(dir, 'tmux');
  const stateFile = path.join(dir, 'list-count');
  const port = 29_998;
  const fleetSession = sessionName(port);
  const decoySession = 'decoy-race';
  const window = `fd${port}-victim`;
  const previous = new Map([
    ['PATH', process.env.PATH],
    ['FLEETDECK_TMUX_SOCKET', process.env.FLEETDECK_TMUX_SOCKET],
    ['FLEETDECK_FAKE_TMUX_STATE', process.env.FLEETDECK_FAKE_TMUX_STATE],
    ['FLEETDECK_FAKE_FLEET_SESSION', process.env.FLEETDECK_FAKE_FLEET_SESSION],
    ['FLEETDECK_FAKE_DECOY_SESSION', process.env.FLEETDECK_FAKE_DECOY_SESSION],
    ['FLEETDECK_FAKE_WINDOW', process.env.FLEETDECK_FAKE_WINDOW],
  ]);

  writeFileSync(fakeTmux, `#!/bin/sh
case " $* " in
  *" list-panes "*)
    case " $* " in
      *'#{session_name}'*)
        count=0
        if [ -f "$FLEETDECK_FAKE_TMUX_STATE" ]; then count=$(cat "$FLEETDECK_FAKE_TMUX_STATE"); fi
        if [ "$count" -eq 0 ]; then
          printf '%s\\t%s\\t%s\\n' "$FLEETDECK_FAKE_FLEET_SESSION" "$FLEETDECK_FAKE_WINDOW" '@1'
        fi
        printf '%s\\t%s\\t%s\\n' "$FLEETDECK_FAKE_DECOY_SESSION" "$FLEETDECK_FAKE_WINDOW" '@2'
        printf '%s\\n' "$((count + 1))" > "$FLEETDECK_FAKE_TMUX_STATE"
        exit 0
        ;;
      *)
        printf '%s\\n' "$FLEETDECK_FAKE_WINDOW"
        exit 0
        ;;
    esac
    ;;
  *" kill-window "*) exit 1 ;;
esac
exit 1
`);
  chmodSync(fakeTmux, 0o700);
  process.env.PATH = `${dir}:${process.env.PATH}`;
  process.env.FLEETDECK_TMUX_SOCKET = 'adapter-race';
  process.env.FLEETDECK_FAKE_TMUX_STATE = stateFile;
  process.env.FLEETDECK_FAKE_FLEET_SESSION = fleetSession;
  process.env.FLEETDECK_FAKE_DECOY_SESSION = decoySession;
  process.env.FLEETDECK_FAKE_WINDOW = window;

  t.after(() => {
    for (const [key, value] of previous) {
      if (value == null) delete process.env[key];
      else process.env[key] = value;
    }
    rmSync(dir, { recursive: true, force: true });
  });

  assert.deepEqual(await killWindowVerified(window), { ok: false, gone: true });
});

test('tmux outages during kill lookup and recheck stay errors, never become gone', async (t) => {
  useLegacyGenerationMode(t);
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-fail-'));
  const fakeTmux = path.join(dir, 'tmux');
  const stateFile = path.join(dir, 'list-count');
  const port = 29_997;
  const fleetSession = sessionName(port);
  const window = `fd${port}-outage`;
  const previous = new Map([
    ['PATH', process.env.PATH],
    ['FLEETDECK_TMUX_SOCKET', process.env.FLEETDECK_TMUX_SOCKET],
    ['FLEETDECK_FAKE_TMUX_STATE', process.env.FLEETDECK_FAKE_TMUX_STATE],
    ['FLEETDECK_FAKE_TMUX_MODE', process.env.FLEETDECK_FAKE_TMUX_MODE],
    ['FLEETDECK_FAKE_FLEET_SESSION', process.env.FLEETDECK_FAKE_FLEET_SESSION],
    ['FLEETDECK_FAKE_WINDOW', process.env.FLEETDECK_FAKE_WINDOW],
  ]);

  writeFileSync(fakeTmux, `#!/bin/sh
case " $* " in
  *" list-panes "*)
    if [ "$FLEETDECK_FAKE_TMUX_MODE" = no-server ]; then
      printf '%s\n' 'no server running on /tmp/fake-tmux' >&2
      exit 1
    fi
    if [ "$FLEETDECK_FAKE_TMUX_MODE" = initial ]; then exit 1; fi
    count=0
    if [ -f "$FLEETDECK_FAKE_TMUX_STATE" ]; then count=$(cat "$FLEETDECK_FAKE_TMUX_STATE"); fi
    if [ "$count" -eq 0 ]; then
      printf '%s\\t%s\\t%s\\n' "$FLEETDECK_FAKE_FLEET_SESSION" "$FLEETDECK_FAKE_WINDOW" '@1'
      printf '1\\n' > "$FLEETDECK_FAKE_TMUX_STATE"
      exit 0
    fi
    exit 1
    ;;
  *" kill-window "*) exit 1 ;;
esac
exit 1
`);
  chmodSync(fakeTmux, 0o700);
  process.env.PATH = `${dir}:${process.env.PATH}`;
  process.env.FLEETDECK_TMUX_SOCKET = 'adapter-outage';
  process.env.FLEETDECK_FAKE_TMUX_STATE = stateFile;
  process.env.FLEETDECK_FAKE_FLEET_SESSION = fleetSession;
  process.env.FLEETDECK_FAKE_WINDOW = window;

  t.after(() => {
    for (const [key, value] of previous) {
      if (value == null) delete process.env[key];
      else process.env[key] = value;
    }
    rmSync(dir, { recursive: true, force: true });
  });

  process.env.FLEETDECK_FAKE_TMUX_MODE = 'no-server';
  assert.deepEqual(await killWindowVerified(window), { ok: false, error: 'tmux window lookup failed' });

  process.env.FLEETDECK_FAKE_TMUX_MODE = 'initial';
  assert.deepEqual(await killWindowVerified(window), { ok: false, error: 'tmux window lookup failed' });

  process.env.FLEETDECK_FAKE_TMUX_MODE = 'recheck';
  rmSync(stateFile, { force: true });
  assert.deepEqual(await killWindowVerified(window), {
    ok: false,
    error: 'tmux window recheck failed after kill error',
  });
});

test('invalid names and malformed successful kill listings are errors, never gone', async (t) => {
  useLegacyGenerationMode(t);
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-malformed-kill-'));
  const fakeTmux = path.join(dir, 'tmux');
  const previous = new Map([
    ['PATH', process.env.PATH],
    ['FLEETDECK_TMUX_SOCKET', process.env.FLEETDECK_TMUX_SOCKET],
  ]);
  writeFileSync(fakeTmux, `#!/bin/sh
case " $* " in
  *" list-panes "*) printf '%s\n' 'fleetdeck-29996\tfd29996-victim\tbad-id'; exit 0 ;;
esac
exit 1
`);
  chmodSync(fakeTmux, 0o700);
  process.env.PATH = `${dir}:${process.env.PATH}`;
  process.env.FLEETDECK_TMUX_SOCKET = 'adapter-malformed-kill';
  t.after(() => {
    for (const [key, value] of previous) {
      if (value == null) delete process.env[key];
      else process.env[key] = value;
    }
    rmSync(dir, { recursive: true, force: true });
  });

  assert.deepEqual(await killWindowVerified('not-scoped'), {
    ok: false,
    error: 'invalid scoped tmux window name',
  });
  assert.deepEqual(await killWindowVerified('fd29996-victim'), {
    ok: false,
    error: 'malformed tmux window listing',
  });
});
