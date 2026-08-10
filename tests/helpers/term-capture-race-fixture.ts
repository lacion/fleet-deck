#!/usr/bin/env node
// Plain-pipe tmux CONTROL MODE fixture for FLEETDECK_TERM_CMD, reproducing
// BUG-056's same-chunk flush: real tmux writes `%end …\n%output %N <bytes>\n`
// for a capture-pane in ONE stdout write, and the bridge's stdout handler
// resolves the capture waiter and still processes that trailing %output in the
// same feed() loop — before any Promise continuation can run. A viewer that
// subscribed only after its `await capture-pane` returned therefore never saw
// these bytes. The regression test asserts they are now buffered at subscribe
// time and replayed after the init frame.
import readline from 'node:readline';

let number = 0;

function response(lines: string[] = [], ok = true, extra = ''): void {
  const n = ++number;
  process.stdout.write(`%begin 100 ${n} 0\n`);
  for (const line of lines) process.stdout.write(line + '\n');
  process.stdout.write(`%${ok ? 'end' : 'error'} 100 ${n} 0\n`);
  // Same stdout write as the response block — the chunk shape that exposed
  // the capture → subscribe gap.
  if (extra) process.stdout.write(extra);
}

const input = readline.createInterface({ input: process.stdin });
process.stdin.resume();
input.on('line', (line: string) => {
  if (line.startsWith('list-panes -t ')) response(['%1']);
  else if (line.startsWith('capture-pane '))
    response(['seed %1'], true, '%output %1 after-capture\n');
  else if (line.includes("'#{cursor_x} #{cursor_y}'")) response(['2 3']);
  else response([]);
});

process.stdout.write('%begin 99 0 0\n%end 99 0 0\n'); // attach-session response: no stdin waiter yet
process.stdout.write('%session-changed $0 fleetdeck-test\n');
