#!/usr/bin/env node
// Plain-pipe tmux CONTROL MODE fixture for FLEETDECK_TERM_CMD. It answers the
// bridge's discovery/seed commands, emits one pane update, records every stdin
// command and records SIGTERM so integration tests can prove viewer teardown.

import { appendFileSync } from 'node:fs';
import readline from 'node:readline';

const record = process.env.FLEETDECK_TEST_TERM_RECORD;
let number = 0;
let streamed = false;

function note(value) {
  if (!record) return;
  try { appendFileSync(record, JSON.stringify({ pid: process.pid, ...value }) + '\n'); } catch { /* fixture reporting only */ }
}

function response(lines = [], ok = true) {
  const n = ++number;
  process.stdout.write(`%begin 100 ${n} 0\n`);
  for (const line of lines) process.stdout.write(line + '\n');
  process.stdout.write(`%${ok ? 'end' : 'error'} 100 ${n} 0\n`);
}

const input = readline.createInterface({ input: process.stdin });
process.stdin.resume();
input.on('line', line => {
  note({ type: 'line', line });
  if (line.startsWith('list-panes ')) response(['%1']);
  else if (line.startsWith('capture-pane ')) response([`seed \u001b[31mred\u001b[0m`]);
  else if (line.includes("'#{cursor_x} #{cursor_y}'")) {
    response(['2 3']);
    if (!streamed) {
      streamed = true;
      setTimeout(() => process.stdout.write('%output %1 live\\033[32m!\\033[0m\n'), 25);
    }
  } else response([]);
});

process.on('SIGTERM', () => {
  note({ type: 'signal', signal: 'SIGTERM' });
  process.exit(0);
});

note({ type: 'start' });
process.stdout.write('%begin 99 0 0\n%end 99 0 0\n'); // attach-session response: no stdin waiter yet
process.stdout.write('%session-changed $0 fleetdeck-test\n');
