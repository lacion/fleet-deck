// tests/tmux-adapter.test.ts
//
// Exercise the real tmux read/parse path. tmux's formatted-output printer
// escapes a literal unit separator as "\\037", which used to make every
// scoped window lookup return empty even though the pane existed.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { randomBytes, randomInt, randomUUID } from 'node:crypto';
import {
  chmodSync,
  existsSync,
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
  // Fixtures below emit canned tmux listings; they build them from the SHIPPED
  // separator so a change to it can never silently decouple fake tmux from the
  // parser under test.
  FIELD_SEP,
  ensureSession,
  exactWindowTarget,
  fleetServerAbsent,
  killWindowVerified,
  listScopedWindows,
  newWindow,
  paneCurrentCommand,
  sendEnter,
  sessionName,
} from '../src/daemon/spawn.ts';
import { waitUntil } from './helpers/wait.ts';

// The persisted generation claim and its death certificate — read back from
// disk via JSON.parse, whose return is `any`; funnel every read through a typed
// helper so member access is checked and no `any` escapes into the assertions.
interface GenerationRecord {
  generation: string;
  serverPid: number;
}
interface RetiredCertificate {
  retiredGeneration: string;
  retiredServerPid: number;
}
const readGeneration = (file: string): GenerationRecord =>
  JSON.parse(readFileSync(file, 'utf8')) as GenerationRecord;
const readCertificate = (file: string): RetiredCertificate =>
  JSON.parse(readFileSync(file, 'utf8')) as RetiredCertificate;

function tmuxOk(): boolean {
  const socket = `fleetdeck-adapter-probe-${process.pid}-${randomBytes(4).toString('hex')}`;
  try {
    execFileSync('tmux', ['-L', socket, '-f', '/dev/null', 'new-session', '-d', 'sleep 1'], {
      stdio: 'ignore',
    });
    execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' });
    return true;
  } catch {
    try {
      execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' });
    } catch {
      /* no server */
    }
    return false;
  }
}

// env: process.env on both — the socket name resolves against a runtime-mutated
// TMUX_TMPDIR, which Bun's default env inheritance (a startup snapshot) misses;
// a no-op under Node. See exec.ts.
function tmux(socket: string, args: readonly string[]): string {
  return execFileSync('tmux', ['-L', socket, ...args], {
    encoding: 'utf8',
    env: process.env,
  }).trim();
}

function tmuxStatus(socket: string, args: readonly string[]): number | null {
  return spawnSync('tmux', ['-L', socket, ...args], { stdio: 'ignore', env: process.env }).status;
}

function restoreEnv(previous: Map<string, string | undefined>): void {
  for (const [key, value] of previous) {
    if (value == null) Reflect.deleteProperty(process.env, key);
    else process.env[key] = value;
  }
}

function useLegacyGenerationMode(t: TestContext): void {
  const previousHome = process.env['FLEETDECK_HOME'];
  delete process.env['FLEETDECK_HOME'];
  t.after(() => {
    if (previousHome == null) delete process.env['FLEETDECK_HOME'];
    else process.env['FLEETDECK_HOME'] = previousHome;
  });
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function stopPid(pid: number | null): Promise<void> {
  if (pid == null || !Number.isSafeInteger(pid) || pid <= 1 || !pidAlive(pid)) return;
  try {
    process.kill(pid, 'SIGTERM');
  } catch {
    return;
  }
  for (let i = 0; i < 50 && pidAlive(pid); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  if (pidAlive(pid)) {
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      /* exited between probe and signal */
    }
  }
}

interface IsolatedTmuxEnv {
  home: string;
  socketRoot: string;
  socket: string;
  socketPath: string;
  previous: Map<string, string | undefined>;
}

// The prefix argument the .mjs accepted was never read — the temp dir has always
// used a fixed 'fd-tg-' prefix — so the dead parameter (and its dead call-site
// argument) is dropped rather than annotated; behaviour is identical.
function isolatedTmuxEnv(): IsolatedTmuxEnv {
  const home = mkdtempSync(path.join(tmpdir(), 'fd-tg-'));
  const socketRoot = path.join(home, 's');
  mkdirSync(socketRoot, { mode: 0o700 });
  const socket = `g-${process.pid}-${randomBytes(4).toString('hex')}`;
  const previous = new Map([
    ['FLEETDECK_HOME', process.env['FLEETDECK_HOME']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
    ['TMUX_TMPDIR', process.env['TMUX_TMPDIR']],
  ]);
  process.env['FLEETDECK_HOME'] = home;
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  process.env['TMUX_TMPDIR'] = socketRoot;
  const socketPath = path.join(socketRoot, `tmux-${process.getuid?.() ?? 0}`, socket);
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
    ['PATH', process.env['PATH']],
    ['FLEETDECK_HOME', process.env['FLEETDECK_HOME']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
    ['FLEETDECK_FAKE_TMUX_REACHABLE', process.env['FLEETDECK_FAKE_TMUX_REACHABLE']],
    ['FLEETDECK_FAKE_TMUX_GENERATION', process.env['FLEETDECK_FAKE_TMUX_GENERATION']],
    ['FLEETDECK_FAKE_TMUX_CREATIONS', process.env['FLEETDECK_FAKE_TMUX_CREATIONS']],
    ['FLEETDECK_FAKE_TMUX_PID', process.env['FLEETDECK_FAKE_TMUX_PID']],
  ]);
  writeFileSync(
    fakeTmux,
    `#!/bin/sh
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
      *" ; "*) printf '__fleetdeck_tmux_generation__=%s${FIELD_SEP}%s\n' "$value" "$FLEETDECK_FAKE_TMUX_PID" ;;
      *) printf '%s${FIELD_SEP}%s\n' "$value" "$FLEETDECK_FAKE_TMUX_PID" ;;
    esac
    exit 0
    ;;
esac
exit 1
`,
  );
  chmodSync(fakeTmux, 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_HOME'] = dir;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-generation-contract';
  process.env['FLEETDECK_FAKE_TMUX_REACHABLE'] = reachable;
  process.env['FLEETDECK_FAKE_TMUX_GENERATION'] = serverGeneration;
  process.env['FLEETDECK_FAKE_TMUX_CREATIONS'] = creations;
  process.env['FLEETDECK_FAKE_TMUX_PID'] = String(process.pid);
  t.after(() => {
    restoreEnv(previous);
    rmSync(dir, { recursive: true, force: true });
  });

  assert.equal(
    await ensureSession(port),
    sessionName(port),
    'first run may create and claim a server',
  );
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'claimed server empty listing is authoritative',
  );
  const expectedFile = path.join(dir, `tmux-generation-${port}`);
  const expected = readGeneration(expectedFile);
  assert.match(expected.generation, /^[0-9a-f-]{36}$/);
  assert.equal(expected.serverPid, process.pid);
  assert.deepEqual(Object.keys(expected).sort(), ['generation', 'serverPid']);
  assert.equal(statSync(expectedFile).mode & 0o777, 0o600);
  assert.equal(readFileSync(creations, 'utf8').trim().split('\n').length, 1);

  writeFileSync(expectedFile, `${expected.generation}\n`);
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'matching legacy UUID can be corroborated conservatively',
  );
  assert.deepEqual(
    readGeneration(expectedFile),
    expected,
    'matching legacy record upgrades to strict generation + PID JSON',
  );

  writeFileSync(serverGeneration, `${randomUUID()}\n`);
  assert.equal(
    await listScopedWindows(port),
    null,
    'different server generation makes empty UNKNOWN',
  );
  const killed = await killWindowVerified(`fd${port}-missing`);
  assert.ok(!killed.ok);
  assert.equal(killed.gone, undefined);
  assert.match(killed.error ?? '', /generation/i);
  await assert.rejects(ensureSession(port), /generation/i);
  assert.equal(
    readFileSync(creations, 'utf8').trim().split('\n').length,
    1,
    'expected generation forbids replacement creation',
  );
});

// Absence is a VERDICT, not a failed probe. Treating every unsuccessful tmux
// invocation as "the fleet is empty" would let one timed-out or shadowed tmux
// tell boot reconciliation to tombstone a board full of live panes, so only the
// two messages tmux itself uses for "nothing is listening here" may be
// authoritative. Everything else stays UNKNOWN and fails closed.
test("only tmux's own absence verdict is authoritative; a failed probe stays UNKNOWN", async (t) => {
  const port = 29_990;
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-absence-'));
  const fakeTmux = path.join(dir, 'tmux');
  const previous = new Map([
    ['PATH', process.env['PATH']],
    ['FLEETDECK_HOME', process.env['FLEETDECK_HOME']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
    ['FLEETDECK_FAKE_TMUX_STDERR', process.env['FLEETDECK_FAKE_TMUX_STDERR']],
  ]);
  // Fails every command, including new-session, with a caller-chosen stderr.
  writeFileSync(
    fakeTmux,
    `#!/bin/sh
printf '%s\\n' "$FLEETDECK_FAKE_TMUX_STDERR" >&2
exit 1
`,
  );
  chmodSync(fakeTmux, 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_HOME'] = dir;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-absence-contract';
  t.after(() => {
    restoreEnv(previous);
    rmSync(dir, { recursive: true, force: true });
  });

  const ABSENCE_VERDICTS = [
    'no server running on /tmp/tmux-1000/adapter-absence-contract',
    'error connecting to /tmp/tmux-1000/adapter-absence-contract (No such file or directory)',
  ];
  const TRANSPORT_FAULTS = [
    'connection timed out',
    'directory /tmp/tmux-1000 has unsafe permissions',
    'error connecting to /tmp/tmux-1000/x (File name too long)',
    'lost server',
    '',
  ];

  // With no death certificate on record, even tmux's own absence verdict proves
  // nothing about panes: this is exactly what a live server behind an unlinked
  // socket looks like.
  for (const stderr of [...ABSENCE_VERDICTS, ...TRANSPORT_FAULTS]) {
    process.env['FLEETDECK_FAKE_TMUX_STDERR'] = stderr;
    assert.equal(
      await listScopedWindows(port),
      null,
      `unproven absence stays UNKNOWN: ${stderr || '(silent failure)'}`,
    );
    // has-session is a predicate: absence must never be read as "it exists".
    await assert.rejects(
      ensureSession(port),
      /generation unavailable|could not create session/,
      'an unreachable server never fabricates a session that was never created',
    );
  }
  assert.equal(
    existsSync(path.join(dir, `tmux-generation-${port}`)),
    false,
    'reading claims nothing',
  );
  assert.equal(
    existsSync(path.join(dir, `tmux-generation-${port}.retired`)),
    false,
    'and invents no proof',
  );

  // Plant a death certificate: an owner we once claimed, proven gone by ESRCH.
  writeFileSync(
    path.join(dir, `tmux-generation-${port}.retired`),
    `${JSON.stringify({ retiredGeneration: randomUUID(), retiredServerPid: 424242 })}\n`,
  );
  for (const verdict of ABSENCE_VERDICTS) {
    process.env['FLEETDECK_FAKE_TMUX_STDERR'] = verdict;
    assert.deepEqual(
      await listScopedWindows(port),
      [],
      `proof + absence verdict is empty: ${verdict}`,
    );
  }
  // ...but the certificate never upgrades a probe that merely failed.
  for (const fault of TRANSPORT_FAULTS) {
    process.env['FLEETDECK_FAKE_TMUX_STDERR'] = fault;
    assert.equal(
      await listScopedWindows(port),
      null,
      `transport fault stays UNKNOWN even with proof: ${fault || '(silent failure)'}`,
    );
  }
});

test('first run creates and claims a tmux server with an owner-only generation file', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 24_000 + randomInt(500);
  const env = isolatedTmuxEnv();
  let serverPid: number | null = null;
  t.after(async () => {
    await stopPid(serverPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port));
  serverPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  assert.ok(pidAlive(serverPid), 'created tmux server is live');

  const file = path.join(env.home, `tmux-generation-${port}`);
  const record = readGeneration(file);
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

test('ensureSession retires a normally exited tmux owner and claims a new generation', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 24_500 + randomInt(400);
  const env = isolatedTmuxEnv();
  let newPid: number | null = null;
  t.after(async () => {
    await stopPid(newPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port));
  const file = path.join(env.home, `tmux-generation-${port}`);
  const first = readGeneration(file);
  assert.ok(pidAlive(first.serverPid));

  tmux(env.socket, ['kill-server']);
  for (let i = 0; i < 50 && pidAlive(first.serverPid); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.equal(
    pidAlive(first.serverPid),
    false,
    'normal kill makes the persisted owner definitively dead',
  );
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'a proven-dead owner makes its old fleet authoritatively empty',
  );

  assert.equal(await ensureSession(port), sessionName(port));
  const second = readGeneration(file);
  newPid = second.serverPid;
  assert.notEqual(second.serverPid, first.serverPid);
  assert.notEqual(second.generation, first.generation);
  assert.ok(pidAlive(second.serverPid));
});

// REGRESSION (observed 2026-07-22 on a live fleet): retiring a proven-dead
// owner used to answer authoritativeEmpty exactly ONCE — from the very call
// that unlinked the file. Every later lookup saw no claim and no server and
// answered UNKNOWN, so revive/adopt/rc returned "tmux window lookup failed;
// revive held to avoid a duplicate session" forever. The liveness tick consumed
// the single recovery seconds after the crash, long before a human could click
// Revive, and nothing could heal it because those callers ask their question
// BEFORE ensureSession — the only code that creates a server. One dead tmux
// server therefore wedged the whole board permanently. Lookups must stay
// authoritatively empty for as long as the owner is gone.
test('a dead tmux owner keeps its old fleet authoritatively empty across repeated lookups', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 24_900 + randomInt(400);
  const env = isolatedTmuxEnv();
  let newPid: number | null = null;
  t.after(async () => {
    await stopPid(newPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port));
  const file = path.join(env.home, `tmux-generation-${port}`);
  const first = readGeneration(file);

  tmux(env.socket, ['kill-server']);
  for (let i = 0; i < 50 && pidAlive(first.serverPid); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.equal(pidAlive(first.serverPid), false, 'the persisted owner is definitively dead');

  // The first call retires the dead claim; the ones after it hold no claim at
  // all. All three answer the same, or a human clicking Revive after the
  // liveness tick has already run gets a permanent refusal.
  assert.deepEqual(await listScopedWindows(port), [], 'retiring lookup is authoritatively empty');
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'second lookup is still authoritatively empty',
  );
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'third lookup is still authoritatively empty',
  );
  assert.equal(existsSync(file), false, 'the dead claim stays retired');
  assert.equal(
    existsSync(`${file}.retired`),
    true,
    'the death certificate outlives the claim it replaced',
  );
  const certificate = readCertificate(`${file}.retired`);
  assert.equal(
    certificate.retiredServerPid,
    first.serverPid,
    'the certificate names the PID it proved dead',
  );
  assert.equal(certificate.retiredGeneration, first.generation);
  assert.equal(statSync(`${file}.retired`).mode & 0o777, 0o600, 'certificate is owner-only');

  // killWindowVerified answers the same question for revive's remnant cleanup:
  // with the owner gone the window is authoritatively gone, never an error.
  const killed = await killWindowVerified(`fd${port}-heron`);
  assert.ok(!killed.ok);
  assert.equal(killed.gone, true, 'a window on a dead owner is gone, not UNKNOWN');
  assert.equal(killed.error, undefined);

  // And the fleet must still be able to come back up afterwards.
  assert.equal(await ensureSession(port), sessionName(port));
  const second = readGeneration(file);
  newPid = second.serverPid;
  assert.notEqual(second.serverPid, first.serverPid);
  assert.ok(pidAlive(second.serverPid));
  assert.equal(
    existsSync(`${file}.retired`),
    false,
    'a live claim supersedes the certificate — the old proof must not outlive this server',
  );
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'the reclaimed server answers from its own identity',
  );
});

// REGRESSION (BUG-049): when the claimed server died and an UNRELATED server
// bound the same socket label before the next probe, the retiring call used to
// immediately adopt the replacement — claim it, delete the death certificate,
// and hand the liveness tick a verified EMPTY listing. The tick saw
// fleetServerAbsent === false and never settled the old generation's rows, so
// every pane that died with the old server kept reading 'live' forever and
// Revive answered 409 "not revivable". The retiring call must report the old
// generation's loss, and must never claim the replacement; the death signal
// must survive long enough for settlement.
test('retiring a dead owner never adopts a replacement and keeps the fleet-death signal', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 25_300 + randomInt(400);
  const env = isolatedTmuxEnv();
  const replacementSession = `orphan-${port}`;
  let replacementPid: number | null = null;
  t.after(async () => {
    await stopPid(replacementPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), sessionName(port));
  const file = path.join(env.home, `tmux-generation-${port}`);
  const first = readGeneration(file);

  tmux(env.socket, ['kill-server']);
  for (let i = 0; i < 50 && pidAlive(first.serverPid); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.equal(pidAlive(first.serverPid), false, 'the persisted owner is definitively dead');

  // Someone starts an unrelated server on the same label before the next probe.
  tmux(env.socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    replacementSession,
    'sleep 3600',
  ]);
  replacementPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  assert.notEqual(
    replacementPid,
    first.serverPid,
    'same label now reaches a different tmux server',
  );

  // The call that retires the dead owner proves the whole old generation lost.
  // A replacement answering the socket must not mute that signal: nothing on
  // the new server can resurrect panes that died with the old one.
  assert.equal(
    await fleetServerAbsent(port),
    true,
    'the retiring call reports the old generation lost',
  );

  // And the retirement must not have adopted the replacement: no new claim may
  // be written and the death certificate must survive, or the fleet-death
  // signal is gone for every caller after this one.
  assert.equal(
    existsSync(file),
    false,
    'the replacement server is never claimed by the retiring call',
  );
  assert.equal(
    existsSync(`${file}.retired`),
    true,
    'the death certificate survives the reachable replacement',
  );
  const certificate = readCertificate(`${file}.retired`);
  assert.equal(
    certificate.retiredGeneration,
    first.generation,
    'the certificate still names the lost generation',
  );

  // Later calls must not quietly adopt it either: while the death certificate
  // covers this label, a reachable server on it is an interloper — the label
  // was just vacated by a server we proved dead — so readers still answer the
  // old fleet's emptiness (its panes are provably gone with it) and no claim
  // or mutation ever lands on the replacement.
  assert.deepEqual(
    await listScopedWindows(port),
    [],
    'the interloper never reads as the old fleet — the old fleet stays authoritatively empty',
  );
  assert.equal(
    await fleetServerAbsent(port),
    true,
    'the fleet-death signal survives the reachable replacement',
  );
  assert.equal(existsSync(file), false, 'no later call claims the replacement either');
  assert.equal(existsSync(`${file}.retired`), true, 'the death certificate still covers the label');
  assert.ok(pidAlive(replacementPid), 'read paths never kill the interloper');

  // Recovery is ensureSession's job, and it is the one caller that may evict:
  // it proves the interloper foreign with the certificate, stops exactly that
  // PID, and stands up its own claimed server in its place.
  assert.equal(await ensureSession(port), sessionName(port));
  const second = readGeneration(file);
  assert.notEqual(second.generation, first.generation);
  assert.ok(pidAlive(second.serverPid), 'the recovered server is a live, claimed generation');
  await stopPid(second.serverPid);
  assert.equal(
    pidAlive(replacementPid),
    false,
    'ensureSession stopped the foreign interloper to recover',
  );
  assert.equal(
    existsSync(`${file}.retired`),
    false,
    'a live claim supersedes the certificate — the old proof must not outlive this server',
  );
});

test('a home that never claimed a server stays UNKNOWN about an absent fleet', async (t) => {
  // The other side of the coin: absence is only ever licensed by a death
  // certificate. A home that never claimed a server has no evidence at all, and
  // "nothing answering the socket" is precisely what a LIVE server whose socket
  // was unlinked looks like — its panes would still be running. Fail closed.
  // tests/spawn.test.mjs pins the same contract from the kill side.
  const port = 25_500 + randomInt(400);
  const env = isolatedTmuxEnv();
  t.after(() => {
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await listScopedWindows(port), null);
  assert.equal(
    existsSync(path.join(env.home, `tmux-generation-${port}`)),
    false,
    'reading an absent fleet claims nothing',
  );
  assert.equal(
    existsSync(path.join(env.home, `tmux-generation-${port}.retired`)),
    false,
    'and never invents a death certificate it did not earn',
  );
});

// The boundary the fix must NOT cross: a claimed server that is still ALIVE but
// whose socket was unlinked out from under us. Its panes are running and
// unreachable, so an empty listing is a lie and must stay UNKNOWN — this is the
// case the whole generation identity exists to catch, and it is distinguished
// from the retired-owner case above only by the recorded PID still being alive.
test('an unreachable but still-live claimed server keeps lookups UNKNOWN', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 25_900 + randomInt(400);
  const env = isolatedTmuxEnv();
  const fleetWindow = `fd${port}-orca`;
  let originalPid: number | null = null;
  t.after(async () => {
    await stopPid(originalPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  tmux(env.socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    sessionName(port),
    '-n',
    fleetWindow,
    'sleep 3600',
  ]);
  originalPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  assert.equal(
    (await listScopedWindows(port))?.[0]?.window,
    fleetWindow,
    'the live fleet window is claimed',
  );

  unlinkSync(env.socketPath); // server still running, now unreachable by label
  assert.equal(
    await listScopedWindows(port),
    null,
    'a live owner we cannot reach is UNKNOWN, never empty',
  );
  assert.equal(
    existsSync(path.join(env.home, `tmux-generation-${port}`)),
    true,
    'a claim whose PID is alive is never retired',
  );
  const killed = await killWindowVerified(fleetWindow);
  assert.ok(!killed.ok);
  assert.equal(killed.gone, undefined, 'an unreachable live window is never authoritatively gone');
  assert.match(killed.error ?? '', /generation/i);
  assert.ok(pidAlive(originalPid), 'the original server and its panes are left alone');
});

test('an unlinked socket replacement cannot impersonate the claimed tmux server', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 24_500 + randomInt(500);
  const env = isolatedTmuxEnv();
  const fleetSession = sessionName(port);
  const fleetWindow = `fd${port}-original`;
  const replacementSession = `replacement-${port}`;
  let originalPid: number | null = null;
  let replacementPid: number | null = null;
  t.after(async () => {
    // The socket names only the replacement now, so clean both exact recorded
    // server PIDs rather than relying on a label that cannot reach the original.
    await stopPid(replacementPid);
    await stopPid(originalPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  tmux(env.socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    fleetSession,
    '-n',
    fleetWindow,
    'sleep 3600',
  ]);
  originalPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  const claimed = await listScopedWindows(port);
  assert.equal(
    claimed?.[0]?.window,
    fleetWindow,
    'pre-feature server and fleet window are claimed',
  );
  assert.ok(pidAlive(originalPid));

  unlinkSync(env.socketPath);
  tmux(env.socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    replacementSession,
    'sleep 3600',
  ]);
  replacementPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  assert.notEqual(replacementPid, originalPid, 'same label now reaches a different tmux server');

  assert.equal(await listScopedWindows(port), null, 'replacement empty listing is UNKNOWN');
  const killed = await killWindowVerified(fleetWindow);
  assert.ok(!killed.ok);
  assert.equal(killed.gone, undefined, 'replacement is never authoritative gone');
  assert.match(killed.error ?? '', /generation/i);
  await assert.rejects(ensureSession(port), /generation/i, 'ensureSession refuses the replacement');
  assert.equal(
    tmuxStatus(env.socket, ['has-session', '-t', `=${fleetSession}`]),
    1,
    'ensureSession did not create or accept the fleet session on the replacement',
  );
  assert.ok(pidAlive(originalPid), 'inaccessible original tmux server and panes remain alive');
  assert.ok(
    pidAlive(replacementPid),
    'generation checks do not kill the replacement server either',
  );
});

test('exact fleet target blocks same-number pane mutation and new-window launch after socket replacement', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  const port = 25_000 + randomInt(500);
  const env = isolatedTmuxEnv();
  const fleetSession = sessionName(port);
  const fleetWindow = `fd${port}-bound`;
  const replacementSession = `replacement-${port}`;
  const launched = path.join(env.home, 'replacement-launched');
  let originalPid: number | null = null;
  let replacementPid: number | null = null;
  t.after(async () => {
    await stopPid(replacementPid);
    await stopPid(originalPid);
    restoreEnv(env.previous);
    rmSync(env.home, { recursive: true, force: true });
  });

  assert.equal(await ensureSession(port), fleetSession);
  tmux(env.socket, ['rename-window', '-t', `=${fleetSession}:`, fleetWindow]);
  originalPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  const originalId = tmux(env.socket, [
    'display-message',
    '-p',
    '-t',
    `=${fleetSession}:=${fleetWindow}`,
    '#{window_id}',
  ]);
  const target = exactWindowTarget(port, fleetWindow);

  unlinkSync(env.socketPath);
  tmux(env.socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    replacementSession,
    '-n',
    'decoy',
    'sleep 3600',
  ]);
  replacementPid = Number(tmux(env.socket, ['display-message', '-p', '#{pid}']));
  const replacementId = tmux(env.socket, [
    'display-message',
    '-p',
    '-t',
    `=${replacementSession}:=decoy`,
    '#{window_id}',
  ]);
  assert.equal(replacementId, originalId, 'replacement reused the same numeric window id');

  assert.equal(
    await sendEnter(target),
    false,
    'exact fleet target cannot redirect Enter to same-number decoy',
  );
  await assert.rejects(
    newWindow({
      port,
      callsign: 'never-launched',
      cwd: env.home,
      argv: ['sh', '-c', `touch ${launched}`],
    }),
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
    ['PATH', process.env['PATH']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
    ['FLEETDECK_FAKE_TMUX_MODE', process.env['FLEETDECK_FAKE_TMUX_MODE']],
  ]);
  writeFileSync(
    fakeTmux,
    `#!/bin/sh
case "$FLEETDECK_FAKE_TMUX_MODE" in
  fail) exit 1 ;;
  empty) exit 0 ;;
  malformed) printf '%s\n' 'fleetdeck-29999\tfd29999-agent\tnot-an-id\t0\tclaude'; exit 0 ;;
esac
exit 1
`,
  );
  chmodSync(fakeTmux, 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-list-contract';
  t.after(() => {
    restoreEnv(previous);
    rmSync(dir, { recursive: true, force: true });
  });

  process.env['FLEETDECK_FAKE_TMUX_MODE'] = 'fail';
  assert.equal(await listScopedWindows(29_999), null, 'transport failure is UNKNOWN');
  process.env['FLEETDECK_FAKE_TMUX_MODE'] = 'empty';
  assert.deepEqual(await listScopedWindows(29_999), [], 'successful empty output is authoritative');
  process.env['FLEETDECK_FAKE_TMUX_MODE'] = 'malformed';
  assert.equal(await listScopedWindows(29_999), null, 'malformed successful output is UNKNOWN');
});

test('tmux adapter parses scoped panes and kills only the exact fleet session window', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 22_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-${process.pid}-${randomBytes(4).toString('hex')}`;
  const fleetSession = sessionName(port);
  const decoySession = `decoy-${port}`;
  const window = `fd${port}-adapter`;
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;

  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  // Create the decoy first so an all-server scan encounters the wrong, same-name
  // window before the daemon-owned one. Exact session corroboration must exclude it.
  // Direct argv after `--`, exactly like production newWindow (spawn.mjs passes
  // '--', ...argv): tmux execs sleep itself, so pane_current_command is 'sleep'
  // immediately. The shell-string form ('sleep 3600') leaves the pane reporting
  // its login shell (zsh, bash, …) until exec completes, which raced the
  // pane_cmd assertions below on zsh/macOS hosts.
  tmux(socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    decoySession,
    '-n',
    window,
    '--',
    'sleep',
    '3600',
  ]);
  tmux(socket, ['new-session', '-d', '-s', fleetSession, '-n', window, '--', 'sleep', '3600']);
  tmux(socket, ['split-window', '-d', '-t', `${fleetSession}:${window}`, '--', 'sleep', '3600']);

  const fleetWindowId = tmux(socket, [
    'display-message',
    '-p',
    '-t',
    `${fleetSession}:${window}`,
    '#{window_id}',
  ]);
  const decoyWindowId = tmux(socket, [
    'display-message',
    '-p',
    '-t',
    `${decoySession}:${window}`,
    '#{window_id}',
  ]);
  assert.notEqual(fleetWindowId, decoyWindowId);

  // Bounded readiness poll (not a fixed sleep): even with direct argv, tmux
  // does not promise the pane has exec'd by the time new-session/split-window
  // return — pane_current_command can still report the shell (zsh, bash, …) at
  // this point. Poll until the fleet pane has execed before asserting its
  // command, scaled like every other wait.
  const windows = await waitUntil(
    async () => {
      const listed = await listScopedWindows(port);
      return listed !== null && listed.length === 1 && listed[0]?.pane_cmd === 'sleep'
        ? listed
        : null;
    },
    {
      timeoutMs: 5_000,
      intervalMs: 25,
      label: `pane of ${fleetSession}:${window} (${fleetWindowId}) to exec sleep`,
    },
  );
  assert.deepEqual(windows, [
    {
      session: fleetSession,
      window,
      window_id: fleetWindowId,
      pane_dead: false,
      pane_cmd: 'sleep',
    },
  ]);
  const pane = await waitUntil(
    async () => {
      const found = await paneCurrentCommand(fleetWindowId);
      return found?.cmd === 'sleep' ? found : null;
    },
    { label: `paneCurrentCommand(${fleetWindowId}) reports sleep` },
  );
  assert.deepEqual(pane, { dead: false, cmd: 'sleep' });

  const killed = await killWindowVerified(window);
  assert.deepEqual(killed, { ok: true, window_id: fleetWindowId });
  assert.equal(
    tmuxStatus(socket, ['has-session', '-t', `=${decoySession}`]),
    0,
    'same-name decoy session survives',
  );
  assert.equal(
    tmuxStatus(socket, ['has-session', '-t', `=${fleetSession}`]),
    1,
    'fleet session exits after its only window is killed',
  );
});

test('duplicate scoped window names are ambiguous and never selected or killed', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 23_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-duplicate-${process.pid}-${randomBytes(4).toString('hex')}`;
  const session = sessionName(port);
  const window = `fd${port}-duplicate`;
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  tmux(socket, ['-f', '/dev/null', 'new-session', '-d', '-s', session, '-n', window, 'sleep 3600']);
  tmux(socket, ['new-window', '-d', '-t', `${session}:`, '-n', window, 'sleep 3600']);

  assert.equal(
    await listScopedWindows(port),
    null,
    'duplicate name makes the fleet listing UNKNOWN',
  );
  assert.deepEqual(await killWindowVerified(window), {
    ok: false,
    error: 'ambiguous scoped tmux window name',
  });
  const names = tmux(socket, ['list-windows', '-t', `=${session}`, '-F', '#{window_name}']).split(
    '\n',
  );
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
    ['PATH', process.env['PATH']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
    ['FLEETDECK_FAKE_TMUX_STATE', process.env['FLEETDECK_FAKE_TMUX_STATE']],
    ['FLEETDECK_FAKE_FLEET_SESSION', process.env['FLEETDECK_FAKE_FLEET_SESSION']],
    ['FLEETDECK_FAKE_DECOY_SESSION', process.env['FLEETDECK_FAKE_DECOY_SESSION']],
    ['FLEETDECK_FAKE_WINDOW', process.env['FLEETDECK_FAKE_WINDOW']],
  ]);

  writeFileSync(
    fakeTmux,
    `#!/bin/sh
case " $* " in
  *" list-panes "*)
    case " $* " in
      *'#{session_name}'*)
        count=0
        if [ -f "$FLEETDECK_FAKE_TMUX_STATE" ]; then count=$(cat "$FLEETDECK_FAKE_TMUX_STATE"); fi
        if [ "$count" -eq 0 ]; then
          printf '%s${FIELD_SEP}%s${FIELD_SEP}%s\\n' "$FLEETDECK_FAKE_FLEET_SESSION" "$FLEETDECK_FAKE_WINDOW" '@1'
        fi
        printf '%s${FIELD_SEP}%s${FIELD_SEP}%s\\n' "$FLEETDECK_FAKE_DECOY_SESSION" "$FLEETDECK_FAKE_WINDOW" '@2'
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
`,
  );
  chmodSync(fakeTmux, 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-race';
  process.env['FLEETDECK_FAKE_TMUX_STATE'] = stateFile;
  process.env['FLEETDECK_FAKE_FLEET_SESSION'] = fleetSession;
  process.env['FLEETDECK_FAKE_DECOY_SESSION'] = decoySession;
  process.env['FLEETDECK_FAKE_WINDOW'] = window;

  t.after(() => {
    restoreEnv(previous);
    rmSync(dir, { recursive: true, force: true });
  });

  assert.deepEqual(await killWindowVerified(window), { ok: false, gone: true });
});

// BUG-046: a scoped window NAME is reusable. Dismiss/Clear verify DB ownership
// and then await a name-based kill; a revive landing inside that await recreates
// the same deterministic name with a fresh live pane, and the kill's re-resolve
// happily destroys the replacement. killWindowVerified therefore accepts
// {expectWindowId, expect} and refuses — {ok:false, stale:true} — when the
// kill-time generation no longer matches what the caller verified.
test('killWindowVerified refuses a recycled window name (expectWindowId mismatch)', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 21_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-recycle-${process.pid}-${randomBytes(4).toString('hex')}`;
  const session = sessionName(port);
  const window = `fd${port}-recycled`;
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  // The dead remnant the caller listed (a keeper window keeps the session — and
  // therefore the server — alive when the remnant is killed below).
  tmux(socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    session,
    '-n',
    'keeper',
    'sleep 3600',
  ]);
  tmux(socket, ['new-window', '-d', '-t', `${session}:`, '-n', window, 'sleep 3600']);
  const remnantId = tmux(socket, [
    'display-message',
    '-p',
    '-t',
    `=${session}:=${window}`,
    '#{window_id}',
  ]);
  // The race: a revive kills the remnant and recreates the SAME name — a new
  // window_id with a live replacement pane.
  tmux(socket, ['kill-window', '-t', `=${session}:=${window}`]);
  tmux(socket, ['new-window', '-d', '-t', `${session}:`, '-n', window, 'sleep 3600']);
  const replacementId = tmux(socket, [
    'display-message',
    '-p',
    '-t',
    `=${session}:=${window}`,
    '#{window_id}',
  ]);
  assert.notEqual(replacementId, remnantId, 'the recreated name is a different window generation');

  const refused = await killWindowVerified(window, { expectWindowId: remnantId });
  assert.ok(!refused.ok);
  assert.equal(refused.stale, true, 'a generation swap is a stale no-op, never a kill');
  assert.equal(
    tmuxStatus(socket, ['has-session', '-t', `=${session}`]),
    0,
    'the replacement pane survived',
  );

  // The up-to-date id still kills — the option refuses only STALE expectations.
  const killed = await killWindowVerified(window, { expectWindowId: replacementId });
  assert.deepEqual(killed, { ok: true, window_id: replacementId });
});

test('killWindowVerified treats a failing expect predicate as a stale no-op', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 20_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-expect-${process.pid}-${randomBytes(4).toString('hex')}`;
  const session = sessionName(port);
  const window = `fd${port}-expected`;
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  tmux(socket, ['-f', '/dev/null', 'new-session', '-d', '-s', session, '-n', window, 'sleep 3600']);

  let verdict = false; // the DB owner flipped live-eligible mid-kill
  let calls = 0;
  const refused = await killWindowVerified(window, {
    expect: () => {
      calls += 1;
      return verdict;
    },
  });
  assert.ok(!refused.ok);
  assert.equal(refused.stale, true);
  assert.ok(calls >= 1, 'the predicate was consulted');
  assert.equal(tmuxStatus(socket, ['has-session', '-t', `=${session}`]), 0, 'nothing was killed');

  verdict = true;
  const killed = await killWindowVerified(window, { expect: () => verdict });
  assert.equal(killed.ok, true, 'a passing expectation kills as before');
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
    ['PATH', process.env['PATH']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
    ['FLEETDECK_FAKE_TMUX_STATE', process.env['FLEETDECK_FAKE_TMUX_STATE']],
    ['FLEETDECK_FAKE_TMUX_MODE', process.env['FLEETDECK_FAKE_TMUX_MODE']],
    ['FLEETDECK_FAKE_FLEET_SESSION', process.env['FLEETDECK_FAKE_FLEET_SESSION']],
    ['FLEETDECK_FAKE_WINDOW', process.env['FLEETDECK_FAKE_WINDOW']],
  ]);

  writeFileSync(
    fakeTmux,
    `#!/bin/sh
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
      printf '%s${FIELD_SEP}%s${FIELD_SEP}%s\\n' "$FLEETDECK_FAKE_FLEET_SESSION" "$FLEETDECK_FAKE_WINDOW" '@1'
      printf '1\\n' > "$FLEETDECK_FAKE_TMUX_STATE"
      exit 0
    fi
    exit 1
    ;;
  *" kill-window "*) exit 1 ;;
esac
exit 1
`,
  );
  chmodSync(fakeTmux, 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-outage';
  process.env['FLEETDECK_FAKE_TMUX_STATE'] = stateFile;
  process.env['FLEETDECK_FAKE_FLEET_SESSION'] = fleetSession;
  process.env['FLEETDECK_FAKE_WINDOW'] = window;

  t.after(() => {
    restoreEnv(previous);
    rmSync(dir, { recursive: true, force: true });
  });

  process.env['FLEETDECK_FAKE_TMUX_MODE'] = 'no-server';
  assert.deepEqual(await killWindowVerified(window), {
    ok: false,
    error: 'tmux window lookup failed',
  });

  process.env['FLEETDECK_FAKE_TMUX_MODE'] = 'initial';
  assert.deepEqual(await killWindowVerified(window), {
    ok: false,
    error: 'tmux window lookup failed',
  });

  process.env['FLEETDECK_FAKE_TMUX_MODE'] = 'recheck';
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
    ['PATH', process.env['PATH']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
  ]);
  writeFileSync(
    fakeTmux,
    `#!/bin/sh
case " $* " in
  *" list-panes "*) printf '%s\n' 'fleetdeck-29996\tfd29996-victim\tbad-id'; exit 0 ;;
esac
exit 1
`,
  );
  chmodSync(fakeTmux, 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-malformed-kill';
  t.after(() => {
    restoreEnv(previous);
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

// Adversarial-review MAJOR-1 regression pin: arming remain-on-exit for fleet
// windows must be SESSION-scoped, never server-global. The rejected first cut
// used `set-option -w -g -t =<session>` — which, despite the -t, writes the
// SERVER-GLOBAL window default (verified on tmux 3.7b) and would leak
// remain-on-exit onto every window of the user's shared default socket. The
// shipped mechanism is a session-scoped after-new-window hook; this pins BOTH
// halves: a fast-dying fleet window keeps its dead pane (the setup-failure
// screen survives), and a foreign session on the same server stays untouched.
test('newWindow arms remain-on-exit for the fleet session only — never server-global', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 22_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-${process.pid}-${randomBytes(4).toString('hex')}`;
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  const userSession = `user-${port}`;
  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  // A "user's own" session shares the server, exactly like the default socket.
  tmux(socket, ['-f', '/dev/null', 'new-session', '-d', '-s', userSession, 'sleep 3600']);
  await ensureSession(port);

  // A fleet window whose command dies immediately: the dead pane must survive
  // (remain-on-exit armed BEFORE the command could exit — the hook closes the
  // new-window→set-option race a plain per-window set loses).
  const win = await newWindow({
    port,
    callsign: 'roe',
    cwd: tmpdir(),
    argv: ['sh', '-c', 'echo SETUPFAIL; exit 7'],
  });
  await new Promise((resolve) => setTimeout(resolve, 500));
  const dead = tmux(socket, ['display-message', '-p', '-t', win.window_id, '#{pane_dead}']);
  assert.equal(dead, '1', 'fast-exit fleet pane is kept dead, not deleted');

  // The server-global default is untouched…
  const globalOpt = spawnSync(
    'tmux',
    ['-L', socket, 'show-options', '-g', '-w', 'remain-on-exit'],
    { encoding: 'utf8' },
  ).stdout.trim();
  assert.ok(
    !/\bon$/.test(globalOpt),
    `server-global remain-on-exit must stay off/unset — got "${globalOpt}"`,
  );

  // …and a window dying in the USER's session still closes normally.
  tmux(socket, ['new-window', '-d', '-t', `=${userSession}:`, '-n', 'udie', 'true']);
  await new Promise((resolve) => setTimeout(resolve, 500));
  const userWindows = tmux(socket, [
    'list-windows',
    '-t',
    `=${userSession}`,
    '-F',
    '#{window_name}',
  ]);
  assert.ok(!userWindows.includes('udie'), 'a user window must still close on exit');
});

// BUG-050 regression pin: an orphan window already holding the deterministic
// fleet name must make newWindow REFUSE — without launching a second agent.
// The old order (create under the final name, postcondition afterwards) let
// tmux accept a duplicate name, started the billed command, and only then
// failed the exact-name check — while name-based compensation refused the
// now-ambiguous duplicate set, orphaning a live agent. The adapter must
// launch under a unique temporary name, verify the final name is free, and
// claim it by id; failure rolls back by that id, so no agent survives.
test('newWindow refuses an occupied scoped name before any agent starts', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 22_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-${process.pid}-${randomBytes(4).toString('hex')}`;
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  const session = sessionName(port);
  const callsign = 'occupied';
  const window = `fd${port}-${callsign}`;
  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  await ensureSession(port);
  // An orphan (or manually created) window already owns the deterministic name.
  tmux(socket, ['new-window', '-d', '-t', `=${session}:`, '-n', window, 'sleep 3600']);
  const orphanId = tmux(socket, [
    'display-message',
    '-p',
    '-t',
    `=${session}:=${window}`,
    '#{window_id}',
  ]);

  await assert.rejects(
    newWindow({ port, callsign, cwd: tmpdir(), argv: ['sleep', '3600'] }),
    /already exists/,
    'a taken scoped name rejects the spawn',
  );
  const names = tmux(socket, ['list-windows', '-t', `=${session}`, '-F', '#{window_name}']).split(
    '\n',
  );
  assert.deepEqual(
    names.filter((n) => n === window),
    [window],
    'no duplicate same-name window was created',
  );
  assert.deepEqual(
    names.filter((n) => n.startsWith(`${window}~`)),
    [],
    'the provisional window was rolled back, not left running',
  );
  const ids = tmux(socket, ['list-windows', '-a', '-F', '#{window_id}']).split('\n');
  assert.ok(ids.includes(orphanId), 'the orphan window itself is untouched');
  const panes = tmux(socket, ['list-panes', '-a', '-F', '#{pane_current_command}']).split('\n');
  assert.equal(
    panes.filter((c) => c === 'sleep').length,
    1,
    'no second agent process was launched or leaked',
  );
});

// BUG-051 regression: the kill must be re-targeted BY EXACT NAME at the moment
// it executes. A real concurrent rename between the lookup and the kill cannot
// be timed deterministically in a test, so a fake tmux performs the
// BUG-051 race inline: the list answers "the name lives at @0", the wrapper
// renames the name onto @1 and puts a repurposed window at @0, and the next
// command is the kill. Kill-by-@id destroys the repurposed window and reports
// success; kill-by-exact-name re-resolves and kills the window that still
// carries the name.
test('a renamed, repurposed window id is never killed by a stale @id', async (t) => {
  useLegacyGenerationMode(t);
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-tmux-staleid-'));
  const port = 29_996;
  const fleetSession = sessionName(port);
  const window = `fd${port}-staleid`;
  const repurposed = 'repurposed-by-human';
  const kills = path.join(dir, 'kills');
  const previous = new Map([
    ['PATH', process.env['PATH']],
    ['FLEETDECK_TMUX_SOCKET', process.env['FLEETDECK_TMUX_SOCKET']],
  ]);

  writeFileSync(
    path.join(dir, 'tmux'),
    `#!/bin/sh
case " $* " in
  *" list-panes "*)
    printf '%s${FIELD_SEP}%s${FIELD_SEP}%s\\n' '${fleetSession}' '${window}' '@0'
    printf '%s${FIELD_SEP}%s${FIELD_SEP}%s\\n' '${fleetSession}' '${repurposed}' '@1'
    exit 0
    ;;
  *" kill-window "*)
    # The race, resolved at kill time: '@0' is now the repurposed window and
    # the scoped name moved to @1. An exact-name target =session:=name must
    # therefore act on @1; a stale numeric @id acts on the repurposed @0.
    case " $* " in
      *" -t @0 "*) printf 'KILLED-REPURPOSED\\n' >> "$FLEETDECK_FAKE_KILLS" ;;
      *" -t @1 "*) printf 'KILLED-MOVED\\n' >> "$FLEETDECK_FAKE_KILLS" ;;
      *" =${fleetSession}:=${window} "*) printf 'KILLED-MOVED\\n' >> "$FLEETDECK_FAKE_KILLS" ;;
      *) printf 'KILLED-UNKNOWN\\n' >> "$FLEETDECK_FAKE_KILLS" ;;
    esac
    # Post-kill recheck, when reached: the name is gone, the repurposed @0 lives.
    printf '%s${FIELD_SEP}%s${FIELD_SEP}%s\\n' '${fleetSession}' '${repurposed}' '@0'
    exit 0
    ;;
esac
exit 1
`,
  );
  chmodSync(path.join(dir, 'tmux'), 0o700);
  process.env['PATH'] = `${dir}:${process.env['PATH']}`;
  process.env['FLEETDECK_TMUX_SOCKET'] = 'adapter-staleid';
  process.env['FLEETDECK_FAKE_KILLS'] = kills;
  t.after(() => {
    restoreEnv(previous);
    delete process.env['FLEETDECK_FAKE_KILLS'];
    rmSync(dir, { recursive: true, force: true });
  });

  const killed = await killWindowVerified(window);
  assert.deepEqual(
    killed,
    { ok: true, window_id: '@0' },
    'the kill reports the id the lookup selected',
  );
  assert.equal(
    readFileSync(kills, 'utf8').trim(),
    'KILLED-MOVED',
    'the kill landed on the window still carrying the scoped name, never the repurposed @id',
  );
});

// A scoped window name containing ':' passes the caller's loose scoped-name
// regex but, if pasted into `=<session>:=<name>`, parses as target syntax
// ("kill the window named <innocent>") instead of one literal name.
test('tmux-target-syntax characters in a scoped kill name are rejected, never parsed as targets', {
  skip: !tmuxOk() && 'tmux server unavailable',
}, async (t) => {
  useLegacyGenerationMode(t);
  const port = 27_000 + randomInt(1_000);
  const socket = `fleetdeck-adapter-trap-${process.pid}-${randomBytes(4).toString('hex')}`;
  const fleetSession = sessionName(port);
  const innocent = 'innocent';
  const previousSocket = process.env['FLEETDECK_TMUX_SOCKET'];
  process.env['FLEETDECK_TMUX_SOCKET'] = socket;
  t.after(() => {
    try {
      tmux(socket, ['kill-server']);
    } catch {
      /* already gone */
    }
    if (previousSocket == null) delete process.env['FLEETDECK_TMUX_SOCKET'];
    else process.env['FLEETDECK_TMUX_SOCKET'] = previousSocket;
  });

  tmux(socket, [
    '-f',
    '/dev/null',
    'new-session',
    '-d',
    '-s',
    fleetSession,
    '-n',
    innocent,
    'sleep 3600',
  ]);
  tmux(socket, [
    'new-window',
    '-d',
    '-t',
    `=${fleetSession}:`,
    '-n',
    `fd${port}-a:${innocent}`,
    'sleep 3600',
  ]);

  assert.deepEqual(await killWindowVerified(`fd${port}-a:${innocent}`), {
    ok: false,
    error: 'invalid scoped tmux window name',
  });
  assert.equal(tmuxStatus(socket, ['has-session', '-t', `=${fleetSession}`]), 0);
  const names = tmux(socket, [
    'list-windows',
    '-t',
    `=${fleetSession}`,
    '-F',
    '#{window_name}',
  ]).split('\n');
  assert.deepEqual(
    names,
    [innocent, `fd${port}-a:${innocent}`],
    'nothing was killed by target-syntax injection',
  );
});
