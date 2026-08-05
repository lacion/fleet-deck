#!/usr/bin/env node
// demo/run-with-timeout.mjs — portable replacement for `setsid timeout N cmd…`.
//
// GNU `timeout` and util-linux `setsid` do not ship with base macOS, so the
// smoke's worker launches used to abort on any non-Linux contributor machine.
// This launcher reproduces exactly the semantics demo/run-smoke.sh relies on:
//   - the command runs in its own process group (the setsid half), so teardown
//     can signal the whole tree at once;
//   - after N seconds the group is SIGTERMed (SIGKILLed on a short grace) and
//     the launcher exits 124, like GNU timeout;
//   - SIGTERM/SIGINT delivered to the launcher are forwarded into the child's
//     process group, because the smoke's stop_worker kills the launcher's
//     process group and the detached child is deliberately outside it;
//   - otherwise the launcher exits with the command's own exit status.
//
// Usage: node run-with-timeout.mjs <seconds> <command> [args...]

import { spawn } from 'node:child_process';
import os from 'node:os';

const [secondsRaw, command, ...args] = process.argv.slice(2);
const seconds = Number(secondsRaw);
if (!Number.isFinite(seconds) || seconds <= 0 || !command) {
  console.error('usage: node run-with-timeout.mjs <seconds> <command> [args...]');
  process.exit(2);
}

// detached: the child leads its own process group (pgid = child.pid), the
// portable POSIX equivalent of wrapping the command in setsid.
const child = spawn(command, args, { detached: true, stdio: 'inherit' });

let timedOut = false;
let settled = false;
let grace = null;

const killGroup = (signal) => {
  try { process.kill(-child.pid, signal); }
  catch { /* group already gone — nothing to escalate against */ }
};

const timer = setTimeout(() => {
  timedOut = true;
  killGroup('SIGTERM');
  // The smoke's teardown escalates a stuck group to SIGKILL after ~2 s; stay
  // inside that window so a TERM-ignoring worker can never be orphaned.
  grace = setTimeout(() => killGroup('SIGKILL'), 1500);
  grace.unref();
}, seconds * 1000);
timer.unref();

for (const signal of ['SIGTERM', 'SIGINT']) {
  process.on(signal, () => {
    killGroup('SIGTERM');
    if (!grace) {
      grace = setTimeout(() => killGroup('SIGKILL'), 1000);
      grace.unref();
    }
  });
}

child.on('error', (err) => {
  if (settled) return;
  settled = true;
  clearTimeout(timer);
  console.error(`run-with-timeout: could not launch ${command}: ${err.message}`);
  process.exit(127);
});

child.on('exit', (code, signal) => {
  if (settled) return;
  settled = true;
  clearTimeout(timer);
  if (grace) clearTimeout(grace);
  if (timedOut) process.exit(124);
  if (signal) process.exit(128 + (os.constants.signals[signal] ?? 0));
  process.exit(code ?? 1);
});
