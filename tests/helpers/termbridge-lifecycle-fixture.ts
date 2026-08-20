#!/usr/bin/env node
// Minimal tmux control-mode peer for the bridge-wide lifecycle tests. It keeps
// one real child and real pipes alive, records every command/signal, and can
// deliberately ignore SIGTERM or wedge send-keys so close() must reject the
// waiter, settle its input chain, and exercise the bounded SIGKILL backstop.

import { appendFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import readline from 'node:readline';

const record = process.env['FLEETDECK_TEST_TERM_LIFECYCLE_RECORD'];
const ignoreSigterm = process.env['FLEETDECK_TEST_TERM_LIFECYCLE_IGNORE_SIGTERM'] === '1';
const hangSend = process.env['FLEETDECK_TEST_TERM_LIFECYCLE_HANG_SEND'] === '1';
const outputMs = Number(process.env['FLEETDECK_TEST_TERM_LIFECYCLE_OUTPUT_MS'] ?? 0);
const triggerWindowClose = process.env['FLEETDECK_TEST_TERM_LIFECYCLE_WINDOW_CLOSE'] === '1';
const inheritPipes = process.env['FLEETDECK_TEST_TERM_LIFECYCLE_INHERIT_PIPES'] === '1';
let responseNumber = 0;
let failNextCloseProbe = triggerWindowClose;

function note(value: Record<string, unknown>): void {
  if (!record) return;
  appendFileSync(record, `${JSON.stringify({ pid: process.pid, at: Date.now(), ...value })}\n`);
}

function respond(lines: string[] = [], ok = true): void {
  responseNumber += 1;
  const key = String(responseNumber);
  process.stdout.write(`%begin ${key} ${key} 0\n`);
  for (const line of lines) process.stdout.write(`${line}\n`);
  process.stdout.write(`%${ok ? 'end' : 'error'} ${key} ${key} 0\n`);
}

const input = readline.createInterface({ input: process.stdin });
input.on('line', (line) => {
  note({ type: 'command', line });
  if (hangSend && line.startsWith('send-keys ')) {
    note({ type: 'send-hung' });
    return;
  }
  if (line.startsWith('list-panes -a ')) {
    if (failNextCloseProbe) {
      failNextCloseProbe = false;
      note({ type: 'close-probe-error' });
      respond([], false);
      return;
    }
    respond([line.includes('#{pane_dead}') ? '%1 0' : '%1']);
    return;
  }
  if (line.startsWith('list-panes -t ')) {
    respond(['%1']);
    return;
  }
  if (line.startsWith('capture-pane ')) {
    respond(['lifecycle seed']);
    return;
  }
  if (line.includes('#{cursor_x}')) {
    respond(['2 3 1']);
    return;
  }
  if (line.includes('#{bracket_paste_flag}')) {
    respond(['1']);
    return;
  }
  respond();
});

process.on('SIGTERM', () => {
  note({ type: 'signal', signal: 'SIGTERM' });
  if (!ignoreSigterm) process.exit(0);
});

note({ type: 'start' });
if (inheritPipes) {
  // Keep the direct child's stdout/stderr pipe write ends open after that child
  // exits. The descendant ignores TERM too: bridge close must own the POSIX
  // control process group, escalate the whole tree, and prove it is gone rather
  // than settling on the direct ChildProcess `exit` event.
  const pipeHolder = spawn(
    process.execPath,
    ['-e', "process.on('SIGTERM', () => {}); setInterval(() => {}, 60_000)"],
    { stdio: ['ignore', 'inherit', 'inherit'] },
  );
  note({ type: 'pipe-holder', pid: pipeHolder.pid });
}
process.stdout.write('%begin 99 0 0\n%end 99 0 0\n');
process.stdout.write('%session-changed $0 fleetdeck-lifecycle\n');

if (Number.isFinite(outputMs) && outputMs > 0) {
  setInterval(() => {
    process.stdout.write('%output %1 lifecycle-output\n');
    process.stderr.write('lifecycle-stderr\n');
  }, outputMs);
}

if (triggerWindowClose) {
  setTimeout(() => {
    note({ type: 'window-close' });
    process.stdout.write('%window-close @1\n');
  }, 200);
}
