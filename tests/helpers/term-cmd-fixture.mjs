#!/usr/bin/env node
// Plain-pipe tmux CONTROL MODE fixture for FLEETDECK_TERM_CMD. It answers the
// bridge's discovery/seed commands, emits pane updates, records every stdin
// command and records SIGTERM so integration tests can prove viewer teardown.
//
// Since v1.9 the daemon keeps ONE control client for the whole fleet and demuxes
// %output by pane id, so this fixture must model several panes rather than one:
// `list-panes -t =<session>:=<window>` hands out a STABLE pane id per window
// (%1, %2, …, in first-seen order), and each pane streams output tagged with its
// own id. A test can therefore prove that two viewers on two windows see two
// different streams through a single fixture process.

import { appendFileSync } from 'node:fs';
import readline from 'node:readline';

const record = process.env.FLEETDECK_TEST_TERM_RECORD;
let number = 0;

// Fault injection (Item 6): make the viewer's per-window pane lookup report a
// VANISHED window so the bridge takes its "pane gone" path. The knob value
// selects the failure MODE:
//   'error' | '*'  → `list-panes -t` itself fails (%error)  → bridge sees !ok
//   'empty'        → the command succeeds but hands back no pane id
//   <substring>    → fail (%error) only windows whose name contains the value
// Only the per-window `list-panes -t` lookup is affected; the `list-panes -a`
// window-close probe still answers truthfully so viewer teardown is unaffected.
const noPaneKnob = process.env.FLEETDECK_TEST_TERM_NO_PANE;
function noPaneModeFor(window) {
  if (!noPaneKnob) return null;
  if (noPaneKnob === 'empty') return 'empty';
  if (noPaneKnob === 'error' || noPaneKnob === '*') return 'error';
  return window.includes(noPaneKnob) ? 'error' : null;
}

// window name -> pane id, assigned on first sight and stable thereafter
const panes = new Map();
const streamed = new Set();

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

/** `list-panes -t =fleetdeck-21777:=fd21777-viper-c7a7 -F '#{pane_id}'` → %1
 *
 * Pane ids are handed out in first-seen order, which — with several viewers
 * connecting at once — is NOT the order the test opened its tiles in. So record
 * the mapping: a test that wants to know which pane a window got must read it
 * here rather than assume it. */
function paneForListPanes(line) {
  const target = /-t\s+=\S*?:(\S+)/.exec(line);
  const window = target?.[1]?.replace(/^=/, '') ?? 'default';
  if (!panes.has(window)) {
    panes.set(window, `%${panes.size + 1}`);
    note({ type: 'pane', window, pane: panes.get(window) });
  }
  return panes.get(window);
}

/** The pane a `-t %N`-style command is aimed at. */
function paneForTarget(line) {
  return /-t\s+(%\d+)/.exec(line)?.[1] ?? null;
}

const input = readline.createInterface({ input: process.stdin });
process.stdin.resume();
input.on('line', line => {
  note({ type: 'line', line });
  if (line.startsWith('list-panes -t ')) {
    const target = /-t\s+=\S*?:(\S+)/.exec(line);
    const window = target?.[1]?.replace(/^=/, '') ?? 'default';
    const mode = noPaneModeFor(window);
    if (mode === 'error') response([], false);      // window gone: list-panes fails
    else if (mode === 'empty') response([]);         // window gone: no pane id comes back
    else response([paneForListPanes(line)]);
  } else if (line.startsWith('list-panes -a')) {
    response([...panes.values()]); // window-close probe: every pane still alive
  } else if (line.startsWith('capture-pane ')) {
    const pane = paneForTarget(line) || '%1';
    response([`seed ${pane} \u001b[31mred\u001b[0m`]);
  } else if (line.includes("'#{cursor_x} #{cursor_y}'")) {
    const pane = paneForTarget(line) || '%1';
    response(['2 3']);
    // One output burst per pane, tagged with that pane's id, so a grid test can
    // prove each viewer received ITS stream and not its neighbour's.
    if (!streamed.has(pane)) {
      streamed.add(pane);
      setTimeout(() => process.stdout.write(`%output ${pane} live ${pane}\\033[32m!\\033[0m\n`), 25);
    }
  } else {
    response([]);
  }
});

process.on('SIGTERM', () => {
  note({ type: 'signal', signal: 'SIGTERM' });
  process.exit(0);
});

note({ type: 'start' });
process.stdout.write('%begin 99 0 0\n%end 99 0 0\n'); // attach-session response: no stdin waiter yet
process.stdout.write('%session-changed $0 fleetdeck-test\n');
