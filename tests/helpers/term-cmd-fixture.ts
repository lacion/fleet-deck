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
//
// FLEETDECK_TERM_CMD is spawned DIRECTLY by the bridge (spawn(override, []),
// no shell), so this file carries a shebang and must stay chmod +x (git mode
// 100755) — the OS reads the shebang to launch it under node, which type-strips
// the .ts by extension.

import { appendFileSync } from 'node:fs';
import readline from 'node:readline';

const record = process.env['FLEETDECK_TEST_TERM_RECORD'];
let number = 0;

// Fault injection (Item 6): make the viewer's per-window pane lookup report a
// VANISHED window so the bridge takes its "pane gone" path. The knob value
// selects the failure MODE:
//   'error' | '*'  → `list-panes -t` itself fails (%error)  → bridge sees !ok
//   'empty'        → the command succeeds but hands back no pane id
//   <substring>    → fail (%error) only windows whose name contains the value
// Only the per-window `list-panes -t` lookup is affected; the `list-panes -a`
// window-close probe still answers truthfully (unless PROBE_FAILS below says
// otherwise) so viewer teardown is unaffected.
const noPaneKnob = process.env['FLEETDECK_TEST_TERM_NO_PANE'];
function noPaneModeFor(window: string): 'empty' | 'error' | null {
  if (!noPaneKnob) return null;
  if (noPaneKnob === 'empty') return 'empty';
  if (noPaneKnob === 'error' || noPaneKnob === '*') return 'error';
  return window.includes(noPaneKnob) ? 'error' : null;
}

// Fault injection (BUG-159): fail the openViewer SIGWINCH jiggle's resize steps.
// The open sequence is size(rows), size(rows-1), size(rows); the knob value
// picks which step(s) answer %error:
//   'mid'     → only the rows-1 step fails
//   'restore' → only the final restore-to-rows step fails
// Real resize traffic (a client's later {t:'resize'} frame) is unaffected —
// the bridge does not re-run the jiggle sequence there. ('mid'/'restore' are
// jiggle-step selectors on FLEETDECK_TEST_TERM_FAIL_RESIZE; the BUG-165 values
// below — '1', '*', or a command prefix — select failure by command instead.
// The two schemes never collide: a resize-window line never starts with 'mid'
// or 'restore'.)
//
// Fault injection (BUG-165): the default branch below used to answer EVERY
// non-list/capture/cursor command with a bare success, so no test could reach
// the bridge's lifecycle edges — resize failure, dead-pane send-keys policy,
// command-timeout teardown, or %window-close. These knobs open those edges:
//
//   FLEETDECK_TEST_TERM_HANG_MS     — delay every reply by N ms ('800'), or
//                                     only replies to commands starting with a
//                                     prefix ('send-keys:60000'). NOTE: a reply
//                                     past FLEETDECK_TERM_CMD_TIMEOUT_MS does
//                                     NOT unblock the waiter — the deadline
//                                     teardown kills the whole control client
//                                     (a late reply would resolve the wrong
//                                     FIFO waiter), and SIGTERM here exits the
//                                     fixture, so the child 'exit' event is
//                                     what finally rejects the wedged command.
//                                     Scope with care: delaying only SOME
//                                     replies lets a later command's reply
//                                     overtake the wedged one in FIFO order.
//   FLEETDECK_TEST_TERM_FAIL_RESIZE — resize-window answers %error instead of
//                                     %end. The open path takes its 'terminal
//                                     resize failed' throw; an established
//                                     viewer's resize takes viewer.finish.
//   FLEETDECK_TEST_TERM_DEAD_PANE   — send-keys answers %error (the pane died
//                                     under an established viewer → input()
//                                     must finish that viewer) and list-panes -a
//                                     reports NO panes, so a %window-close probe
//                                     (FLEETDECK_TEST_TERM_CLOSE_WINDOW, emitted
//                                     once the first pane streams) condemns it.
//   FLEETDECK_TEST_BRACKET_PASTE    — cursor reply carries #{bracket_paste_flag}=1
//                                     ('1'), so the init seed replays the pane's
//                                     bracketed-paste mode-set and the board
//                                     unblocks multi-line paste. Default 0 (off)
//                                     keeps the seed cursor-only.
// <cmd> on a knob's value scopes the fault to commands starting with that word.
const hangKnob = process.env['FLEETDECK_TEST_TERM_HANG_MS'];
function hangFor(cmd: string): number {
  if (!hangKnob) return 0;
  const [prefix = '', ms] = hangKnob.split(':');
  if (ms !== undefined) return cmd.startsWith(prefix) ? Number(ms) || 0 : 0;
  return Number(prefix) || 0;
}
const failResizeKnob = process.env['FLEETDECK_TEST_TERM_FAIL_RESIZE'];
const deadPaneKnob = process.env['FLEETDECK_TEST_TERM_DEAD_PANE'];
const closeWindowKnob = process.env['FLEETDECK_TEST_TERM_CLOSE_WINDOW'];
const bracketPasteKnob = process.env['FLEETDECK_TEST_BRACKET_PASTE'];
let closeWindowSent = false;
function knobHits(knob: string | undefined, cmd: string): boolean {
  if (!knob) return false;
  return knob === '1' || knob === '*' || cmd.startsWith(knob);
}

// window name -> pane id, assigned on first sight and stable thereafter
const panes = new Map<string, string>();
const streamed = new Set<string>();
// Fault injection (BUG-157): make the window-close probe (`list-panes -a`)
// FAIL so the bridge's failed-probe path is exercised. Without the recheck an
// idle viewer whose pane just died would never finish; with it the probe is
// retried after a settle delay and the second, healthy answer condemns the
// dead pane.
const probeFails = new Set(
  (process.env['FLEETDECK_TEST_TERM_PROBE_FAILS'] ?? '0')
    .split(',')
    .map(Number)
    .filter(Number.isInteger),
);
let probeCalls = 0;

// Fault injection (BUG-158): make a pane FLOOD %output while the bridge is
// still in its pre-init window (the cursor round-trip that follows
// capture-pane — the response is withheld until the flood lands). Value:
//   '<n>'            → n bytes of flood, aimed at the FIRST pane seen
//   '<n>@<window>'   → n bytes, aimed only at that window's pane
const preInitFloodKnob = process.env['FLEETDECK_TEST_TERM_PREINIT_FLOOD'];
const flood = ((): { bytes: number; window: string | null } | null => {
  const m = /^(\d+)(?:@(.+))?$/.exec(preInitFloodKnob ?? '');
  return m ? { bytes: Number(m[1]), window: m[2] ?? null } : null;
})();
const flooded = new Set<string>();

// window name -> ordered rows of each resize-window seen (jiggle detection)
const resizeSeqs = new Map<string, number[]>();

function note(value: Record<string, unknown>): void {
  if (!record) return;
  try {
    appendFileSync(record, JSON.stringify({ pid: process.pid, ...value }) + '\n');
  } catch {
    /* fixture reporting only */
  }
}

function response(lines: string[] = [], ok = true, cmd = ''): void {
  const n = ++number;
  const write = (): void => {
    process.stdout.write(`%begin 100 ${n} 0\n`);
    for (const line of lines) process.stdout.write(line + '\n');
    process.stdout.write(`%${ok ? 'end' : 'error'} 100 ${n} 0\n`);
  };
  // Replies are matched to commands by FIFO order, so a wedged reply stalls
  // the shared control stream — exactly what the bridge's COMMAND_TIMEOUT_MS
  // teardown exists for.
  const delay = hangFor(cmd);
  if (delay > 0) setTimeout(write, delay);
  else write();
}

/** `list-panes -t =fleetdeck-21777:=fd21777-viper-c7a7 -F '#{pane_id}'` → %1
 *
 * Pane ids are handed out in first-seen order, which — with several viewers
 * connecting at once — is NOT the order the test opened its tiles in. So record
 * the mapping: a test that wants to know which pane a window got must read it
 * here rather than assume it. */
function paneForListPanes(line: string): string {
  const target = /-t\s+=\S*?:(\S+)/.exec(line);
  const window = target?.[1]?.replace(/^=/, '') ?? 'default';
  let pane = panes.get(window);
  if (pane === undefined) {
    pane = `%${panes.size + 1}`;
    panes.set(window, pane);
    note({ type: 'pane', window, pane });
  }
  return pane;
}

/** The pane a `-t %N`-style command is aimed at. */
function paneForTarget(line: string): string | null {
  return /-t\s+(%\d+)/.exec(line)?.[1] ?? null;
}

const input = readline.createInterface({ input: process.stdin });
process.stdin.resume();
input.on('line', (line: string) => {
  note({ type: 'line', line });
  if (line.startsWith('list-panes -t ')) {
    const target = /-t\s+=\S*?:(\S+)/.exec(line);
    const window = target?.[1]?.replace(/^=/, '') ?? 'default';
    const mode = noPaneModeFor(window);
    if (mode === 'error')
      response([], false, line); // window gone: list-panes fails
    else if (mode === 'empty')
      response([], true, line); // window gone: no pane id comes back
    else response([paneForListPanes(line)], true, line);
  } else if (line.startsWith('list-panes -a')) {
    // window-close probe + BUG-055 pane_dead poll: '%N [dead]' per pane. The
    // plain-id form used by the close probe and the id+flag form used by the
    // dead poll both parse the same way, so answer both shapes. The dead knob
    // is a substring matched against window names ('*' = every pane) — its
    // panes report pane_dead=1, modelling a remain-on-exit pane whose process
    // has exited.
    //
    // BUG-157: the probe itself can be made to FAIL (PROBE_FAILS call numbers)
    // so the bridge's failed-probe path is exercised — a failed probe is not
    // proof a pane died, so the bridge must retry after a settle delay and let
    // the second, healthy answer condemn the dead pane.
    probeCalls += 1;
    if (probeFails.has(probeCalls)) {
      response([], false, line); // the probe itself fails: not proof a pane died
    } else {
      const withFlags = line.includes('pane_dead');
      const isDead = (w: string): boolean =>
        deadPaneKnob === '*' ||
        (deadPaneKnob != null && deadPaneKnob !== '' && w.includes(deadPaneKnob));
      if (withFlags) {
        response(
          [...panes.entries()].map(([w, p]) => `${p} ${isDead(w) ? 1 : 0}`),
          true,
          line,
        );
      } else {
        // window-close probe: every pane still alive — unless DEAD_PANE says
        // the pane died, in which case the probe must answer truthfully (none
        // alive) so the bridge finishes the viewers watching it.
        response(deadPaneKnob ? [] : [...panes.values()], true, line);
      }
    }
  } else if (line.startsWith('send-keys ')) {
    // A dead pane refuses input. The bridge's input() must treat !ok as
    // 'terminal pane closed' and finish the viewer — bare-success here hid it.
    response([], !knobHits(deadPaneKnob, line), line);
  } else if (line.startsWith('resize-window ') || line.startsWith('refresh-client ')) {
    // resize-window unsupported/failed. refresh-client is grouped in because it
    // is the bridge's documented FALLBACK for a failed resize-window — failing
    // only resize-window would exercise the fallback, not the failure. With
    // both refusing, the open path throws 'terminal resize failed' and an
    // established viewer's resize finishes it with the same reason.
    //
    // BUG-159: 'mid'/'restore' knob values fail a single SIGWINCH jiggle step,
    // identified by the per-window resize sequence (rows, rows-1, rows).
    let fail = knobHits(failResizeKnob, line);
    if (line.startsWith('resize-window ')) {
      const rows = Number(/ -y (\d+)/.exec(line)?.[1]);
      const window = /-t\s+=\S*?:(\S+)/.exec(line)?.[1]?.replace(/^=/, '') ?? 'default';
      // Track the per-window resize sequence to identify the jiggle steps.
      const seq = resizeSeqs.get(window) ?? [];
      seq.push(rows);
      resizeSeqs.set(window, seq);
      const first = seq[0];
      if (failResizeKnob === 'mid')
        fail = seq.length === 2 && first !== undefined && rows === first - 1;
      else if (failResizeKnob === 'restore')
        fail = seq.length === 3 && first !== undefined && rows === first;
    }
    response([], !fail, line);
  } else if (line.startsWith('capture-pane ')) {
    const pane = paneForTarget(line) ?? '%1';
    response([`seed ${pane} \x1b[31mred\x1b[0m`], true, line);
  } else if (line.includes('#{cursor_x}')) {
    const pane = paneForTarget(line) ?? '%1';
    if (
      flood &&
      !flooded.has(pane) &&
      (!flood.window || [...panes].find(([, p]) => p === pane)?.[0] === flood.window)
    ) {
      // Withhold the cursor reply and emit the flood FIRST: the bridge is
      // subscribed by now, so these bytes land in viewer.pending while init is
      // still being built — exactly the pre-init gap BUG-158 bounds.
      flooded.add(pane);
      const chunk = 'F'.repeat(Math.min(flood.bytes, 64 * 1024));
      for (let left = flood.bytes; left > 0; left -= chunk.length) {
        process.stdout.write(`%output ${pane} ${chunk.slice(0, left)}\n`);
      }
    }
    // The bridge reads #{bracket_paste_flag} in the same round-trip; a third
    // field (1/0) drives whether the init seed replays the bracketed-paste mode.
    response([`2 3 ${bracketPasteKnob === '1' ? '1' : '0'}`], true, line);
    // One output burst per pane, tagged with that pane's id, so a grid test can
    // prove each viewer received ITS stream and not its neighbour's.
    if (!streamed.has(pane)) {
      streamed.add(pane);
      setTimeout(
        () => process.stdout.write(`%output ${pane} live ${pane}\\033[32m!\\033[0m\n`),
        25,
      );
    }
    // Once a pane is established, optionally report its window closing. Real
    // tmux sends %window-close when a window dies; the bridge answers with a
    // list-panes -a probe and finishes only the viewers whose pane is actually
    // gone — so this alone must NOT take a viewer down (pair it with
    // DEAD_PANE to make the probe condemn).
    if (closeWindowKnob && !closeWindowSent) {
      closeWindowSent = true;
      setTimeout(() => process.stdout.write('%window-close @1\n'), 40);
    }
  } else {
    response([], true, line);
  }
});

process.on('SIGTERM', () => {
  note({ type: 'signal', signal: 'SIGTERM' });
  process.exit(0);
});

// Test trigger: every pane "dies" and a %window-close notification is emitted
// as if a watched window was just killed. From here on the window-close probe
// (list-panes -a) answers with NO panes, so the bridge's probe can prove the
// death. The window id is made up — the probe answers by pane id, so the id
// itself is never consulted.
process.on('SIGUSR1', () => {
  panes.clear();
  note({ type: 'window-close' });
  process.stdout.write('%window-close @999\n');
});

note({ type: 'start' });
process.stdout.write('%begin 99 0 0\n%end 99 0 0\n'); // attach-session response: no stdin waiter yet
process.stdout.write('%session-changed $0 fleetdeck-test\n');
