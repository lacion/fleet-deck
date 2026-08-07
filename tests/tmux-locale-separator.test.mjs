// tests/tmux-locale-separator.test.mjs
//
// THE UNTESTED AXIS: the tmux SERVER's locale.
//
// tmux sanitizes its own formatted output according to the locale of the
// SERVER process, and under the C/POSIX locale it rewrites a literal TAB to
// "_" — in `display-message -p` and in `list-* -F` alike. It is not a version
// difference; tmux 3.4 and 3.7b behave identically. When the daemon's tmux
// field separator was a TAB, every format round-trip in spawn.mjs collapsed to
// a single field on such a server: the generation read returned no pid, the
// UUID match failed, the server generation could never be claimed, and EVERY
// spawn failed with "tmux server generation could not be claimed".
//
// A C locale is the DEFAULT in minimal containers and is common under systemd
// and cron, so this is a mainstream deployment — yet a UTF-8 developer machine
// and a UTF-8 CI runner both pass happily. Nothing in the suite pinned the
// locale, so nothing caught it. These tests do.
//
// They deliberately assert on the SHIPPED separator rather than a hard-coded
// character, so the contract survives a future separator change: whatever
// spawn.mjs uses must round-trip under a C-locale server.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
// The separator the daemon actually ships — imported, never duplicated, so
// these tests follow a future change instead of pinning a stale character.
import { FIELD_SEP } from '../scripts/fleetd/spawn.mjs';

function shippedSeparator() {
  assert.equal(typeof FIELD_SEP, 'string');
  assert.ok(FIELD_SEP.length > 0, 'spawn.mjs must export a non-empty FIELD_SEP');
  return FIELD_SEP;
}

function tmuxOk() {
  const socket = `fdlocale-probe-${process.pid}-${randomBytes(4).toString('hex')}`;
  try {
    execFileSync('tmux', ['-L', socket, '-f', '/dev/null', 'new-session', '-d', 'sleep 1'], { stdio: 'ignore' });
    execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' });
    return true;
  } catch { return false; }
}

// Stand up an isolated tmux server whose own environment carries `env`, so the
// SERVER (not this test process) runs under the locale we are pinning.
function serverUnder(t, env) {
  const socket = `fdlocale-${process.pid}-${randomBytes(4).toString('hex')}`;
  const scratch = mkdtempSync(path.join(tmpdir(), 'fd-locale-'));
  execFileSync('tmux', ['-L', socket, '-f', '/dev/null', 'new-session', '-d', 'sleep 600'], {
    stdio: 'ignore',
    env: { ...process.env, ...env },
  });
  t.after(() => {
    try { execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' }); } catch { /* already gone */ }
    rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const ask = (format) => execFileSync('tmux', ['-L', socket, 'display-message', '-p', format], {
    encoding: 'utf8',
    env: { ...process.env, ...env },
  }).replace(/\n$/, '');
  return { socket, ask };
}

const C_LOCALE = { LC_ALL: 'C', LANG: 'C', LC_CTYPE: 'C' };

test('REGRESSION: the shipped tmux field separator survives a C-locale server', { skip: !tmuxOk() && 'tmux unavailable' }, (t) => {
  const sep = shippedSeparator();
  const { ask } = serverUnder(t, C_LOCALE);

  const round = ask(`A${sep}B`);
  assert.equal(round, `A${sep}B`,
    `the field separator ${JSON.stringify(sep)} must survive display-message under a C-locale tmux server; `
    + `got ${JSON.stringify(round)}. A separator tmux rewrites here makes every spawn fail on any C-locale host.`);
});

test('REGRESSION: a two-field generation-shaped read parses under a C-locale server', { skip: !tmuxOk() && 'tmux unavailable' }, (t) => {
  const sep = shippedSeparator();
  const { socket, ask } = serverUnder(t, C_LOCALE);
  const uuid = '792042d8-0d4d-4eb5-85d7-6c5372f63585';
  execFileSync('tmux', ['-L', socket, 'set', '-g', '@fleetdeck_generation_4711', uuid], {
    stdio: 'ignore', env: { ...process.env, ...C_LOCALE },
  });

  // The exact shape readServerGeneration() sends, and the exact parse it does.
  const value = ask(`#{@fleetdeck_generation_4711}${sep}#{pid}`);
  const [generation, pidText, ...extra] = value.split(sep);
  assert.equal(extra.length, 0, `expected exactly two fields, got ${JSON.stringify(value)}`);
  assert.equal(generation, uuid, 'the generation UUID must come back whole and matchable');
  assert.ok(Number.isInteger(Number(pidText)) && Number(pidText) > 0,
    `the server pid must parse; got ${JSON.stringify(pidText)} from ${JSON.stringify(value)}`);
});

test('CONTROL: a literal TAB really is rewritten by a C-locale server (the bug this pins)', { skip: !tmuxOk() && 'tmux unavailable' }, (t) => {
  const { ask } = serverUnder(t, C_LOCALE);
  const round = ask('A\tB');
  // If a future tmux stops sanitizing TAB this control goes stale rather than
  // wrong — the guarantee we depend on is the assertion above, not this one.
  assert.notEqual(round, 'A\tB',
    'control: a C-locale tmux server is expected to rewrite a literal TAB — if this now survives, '
    + 'tmux behaviour changed and the separator rationale in spawn.mjs should be revisited');
  assert.equal(round, 'A_B', `expected the documented "_" rewrite, got ${JSON.stringify(round)}`);
});

test('the separator cannot collide with the values it delimits', () => {
  const sep = shippedSeparator();
  // Fields carried across the format round-trips in spawn.mjs.
  const samples = [
    '792042d8-0d4d-4eb5-85d7-6c5372f63585', // generation uuid
    '12345',                                 // pid
    'fleetdeck-4711',                        // session_name
    'fd4711-orca-b3e3',                      // window_name
    '@8',                                    // window_id
    '0', '1',                                // pane_dead
    '__fleetdeck_tmux_generation__=',        // the verification header
  ];
  for (const s of samples) {
    assert.ok(!s.includes(sep),
      `separator ${JSON.stringify(sep)} must not occur in a delimited value (${JSON.stringify(s)})`);
  }
});
