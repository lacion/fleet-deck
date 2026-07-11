// termbridge.mjs — live access to daemon-owned tmux panes.
//
// CONTRACT: every viewer owns one plain-pipe tmux CONTROL MODE client. There
// is deliberately no PTY and no native dependency. Production launches argv
// [tmux, [-L socket], -C, attach-session, -t, =fleetdeck-<port>]. `-C` is used
// exactly once: unlike `-CC`, it retains the documented tmux 3.x command
// echo/response behavior parsed below.
//
// The database supplies only an already-scoped spawn row. The browser never
// supplies a tmux target. Pane discovery, the ANSI seed and cursor lookup all
// run through the attached control client; input is hex bytes so no human
// text is ever parsed as tmux syntax. FLEETDECK_TERM_CMD replaces the complete
// production argv with argv [cmd] for protocol-fixture tests.

import { spawn } from 'node:child_process';
import { StringDecoder } from 'node:string_decoder';

const ACTIVE_STATUSES = new Set(['spawning', 'stalled', 'live']);
const INPUT_CHUNK_BYTES = 1024;

function envInt(name, fallback, { min = 0 } = {}) {
  const n = Number(process.env[name]);
  return Number.isFinite(n) && n >= min ? Math.floor(n) : fallback;
}

// Cursor home + erase screen: a fresh viewer must never inherit stale cells.
const CLEAR_SCREEN = '\u001b[H\u001b[2J';
// Delay before the post-seed repaint jiggle — long enough that the seed has
// rendered, short enough that the human never sees the snapshot's seams.
const REPAINT_MS = envInt('FLEETDECK_TERM_REPAINT_MS', 80);

function dimensions(cols, rows) {
  const c = Number(cols);
  const r = Number(rows);
  if (!Number.isInteger(c) || !Number.isInteger(r) || c < 1 || r < 1 || c > 1000 || r > 1000) return null;
  return { cols: c, rows: r };
}

/** Decode tmux control-mode's octal byte quoting (\NNN). Backslash itself
 * is emitted as \134 by tmux. Unknown/incomplete backslashes are retained.
 *
 * `value` is a LATIN-1 string — one char per byte (see ControlModeParser) — so
 * every char maps straight back to the byte it came from. Anything else would
 * re-encode bytes we do not own: the control stream is a byte pipe, and the
 * UTF-8 sitting inside it is the PANE's business, reassembled downstream. */
export function unescapeControlData(value) {
  const text = String(value);
  const bytes = [];
  for (let i = 0; i < text.length;) {
    if (text[i] === '\\' && /^[0-7]{3}$/.test(text.slice(i + 1, i + 4))) {
      bytes.push(Number.parseInt(text.slice(i + 1, i + 4), 8));
      i += 4;
      continue;
    }
    bytes.push(text.charCodeAt(i) & 0xff);
    i += 1;
  }
  return Buffer.from(bytes);
}

/** Incremental, pure tmux CONTROL MODE parser. `feed` has no I/O and may be
 * tested with arbitrarily split Buffer/string chunks. Response blocks close
 * only on an %end/%error with the matching timestamp and command number;
 * unknown notifications are ignored for forward compatibility. */
export class ControlModeParser {
  constructor() {
    // LATIN-1, deliberately: the control stream is a BYTE protocol, and tmux
    // splits a pane's output at arbitrary byte boundaries across %output
    // lines. Decoding it as UTF-8 here would meet the first 1-2 bytes of a
    // box-drawing glyph, then the protocol's own '\n', call the sequence
    // invalid and burn the character down to U+FFFD — two junk cells that
    // shove the rest of the row sideways. latin1 is byte-exact (1 char == 1
    // byte); the pane's real UTF-8 is reassembled by the viewer's decoder,
    // which is the only place that knows where the character stream resumes.
    this.decoder = new StringDecoder('latin1');
    this.pending = '';
    this.block = null;
  }

  feed(chunk) {
    this.pending += Buffer.isBuffer(chunk) ? this.decoder.write(chunk) : String(chunk);
    const events = [];
    for (;;) {
      const nl = this.pending.indexOf('\n');
      if (nl < 0) break;
      let line = this.pending.slice(0, nl);
      this.pending = this.pending.slice(nl + 1);
      if (line.endsWith('\r')) line = line.slice(0, -1);
      this.#line(line, events);
    }
    return events;
  }

  #line(line, events) {
    const boundary = /^%(begin|end|error)\s+(\S+)\s+(\S+)(?:\s+.*)?$/.exec(line);
    if (this.block) {
      if (boundary && boundary[1] !== 'begin' && boundary[2] === this.block.time && boundary[3] === this.block.number) {
        events.push({ type: 'response', key: `${this.block.time}:${this.block.number}`, time: this.block.time,
          number: this.block.number, ok: boundary[1] === 'end', lines: this.block.lines });
        this.block = null;
      } else {
        this.block.lines.push(line);
      }
      return;
    }
    if (boundary?.[1] === 'begin') {
      this.block = { time: boundary[2], number: boundary[3], lines: [] };
      return;
    }
    const output = /^%output\s+(%\S+)\s?(.*)$/.exec(line);
    if (output) {
      events.push({ type: 'output', pane: output[1], data: unescapeControlData(output[2]) });
      return;
    }
    const exit = /^%exit(?:\s+(.*))?$/.exec(line);
    if (exit) {
      events.push({ type: 'exit', reason: exit[1] || 'tmux control client exited' });
      return;
    }
    const closed = /^%window-close\s+(\S+)/.exec(line);
    if (closed) {
      events.push({ type: 'window-close', window: closed[1] });
      return;
    }
    const session = /^%session-changed\s+(\S+)(?:\s+(.*))?$/.exec(line);
    if (session) events.push({ type: 'session-changed', session: session[1], name: session[2] || '' });
  }
}

export function parseControlChunk(chunk) {
  return new ControlModeParser().feed(chunk);
}

export class TermBridgeError extends Error {
  constructor(reason) {
    super(reason);
    this.reason = reason;
  }
}

/** Factory lifetime equals the daemon lifetime, so the cap is shared by all
 * live-terminal sockets rather than sampled separately for every request. */
export function createTermBridge({ port, resolveSpawn,
  maxViewers = envInt('FLEETDECK_TERM_MAX_VIEWERS', 4, { min: 1 }), log = () => {} } = {}) {
  let active = 0;

  async function openViewer({ spawn_id, cols, rows, send, onClose = () => {} }) {
    if (process.env.FLEETDECK_TERM?.trim().toLowerCase() === 'off') throw new TermBridgeError('live terminal disabled');
    const size = dimensions(cols, rows);
    if (!size) throw new TermBridgeError('invalid terminal dimensions');
    if (active >= maxViewers) throw new TermBridgeError('live terminal viewer cap reached');
    // Reserve before the resolver's await: two simultaneous upgrades must
    // not both observe the same final slot and exceed the cap.
    active++;
    let row;
    try {
      row = await resolveSpawn?.(spawn_id);
      if (!row) throw new TermBridgeError('no such spawn');
      if (!ACTIVE_STATUSES.has(row.status)) throw new TermBridgeError('spawn is not live');
      if (row.tmux_session !== `fleetdeck-${port}` || !String(row.tmux_window || '').startsWith(`fd${port}-`)) {
        throw new TermBridgeError('spawn is outside this fleet');
      }
    } catch (err) {
      active--;
      throw err;
    }
    let child;
    let pane = null;
    let intentional = false;
    let finished = false;
    let established = false;
    let initialized = false;
    const outDecoder = new StringDecoder('utf8'); // spans %output boundaries
    const pendingOutput = [];
    const responseWaiters = [];
    const parser = new ControlModeParser();
    let readyResolve;
    let readyReject;
    const ready = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject; });
    ready.catch(() => { /* also handled by the attach wait; covers synchronous spawn failure */ });

    const finish = (reason, notify = true) => {
      if (finished) return;
      finished = true;
      active--;
      readyReject(new Error(reason));
      for (const waiter of responseWaiters.splice(0)) waiter.reject(new Error(reason));
      if (child && child.exitCode === null && !child.killed) {
        try { child.kill('SIGTERM'); } catch { /* already gone */ }
      }
      if (notify && established) {
        try { onClose(reason); } catch { /* socket reporting only */ }
      }
    };

    const command = line => new Promise((resolve, reject) => {
      if (finished || !child?.stdin?.writable) return reject(new Error('control client is closed'));
      const waiter = { resolve, reject };
      responseWaiters.push(waiter);
      child.stdin.write(line + '\n', err => {
        if (!err) return;
        const index = responseWaiters.indexOf(waiter);
        if (index >= 0) responseWaiters.splice(index, 1);
        reject(err);
      });
    });

    const handleEvent = ev => {
      if (ev.type === 'response') {
        responseWaiters.shift()?.resolve(ev);
      } else if (ev.type === 'session-changed') {
        readyResolve();
      } else if (ev.type === 'output' && pane && ev.pane === pane) {
        // Decode the pane stream with a PERSISTENT decoder. tmux emits %output
        // as bytes arrive, so a multi-byte character (every box-drawing glyph
        // Claude's TUI is made of) can land half in one notification and half
        // in the next. Decoding each notification on its own turned that char
        // into two U+FFFDs — two extra cells that shoved the rest of the row
        // over and wrapped the line. The decoder holds the partial sequence
        // until its tail arrives.
        const data = outDecoder.write(ev.data);
        if (!data) return; // an incomplete character; it lands with the next event
        if (!initialized) pendingOutput.push(data);
        else try { send({ t: 'out', data }); } catch { finish('terminal socket closed', false); }
      } else if (ev.type === 'exit') {
        finish(ev.reason || 'tmux session ended');
      } else if (ev.type === 'window-close' && pane) {
        // The client sees all windows in its session; verify our pane so an
        // unrelated window closing does not terminate this viewer.
        command(`display-message -p -t ${pane} '#{pane_id}'`).then(res => {
          if (!res.ok) finish('terminal pane closed');
        }).catch(() => finish('terminal pane closed'));
      }
    };

    try {
      const override = process.env.FLEETDECK_TERM_CMD?.trim();
      const socket = process.env.FLEETDECK_TMUX_SOCKET?.trim();
      const argv = socket ? ['-L', socket, '-C', 'attach-session', '-t', '=' + row.tmux_session]
        : ['-C', 'attach-session', '-t', '=' + row.tmux_session];
      child = spawn(override || 'tmux', override ? [] : argv, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
      child.stdout.on('data', chunk => {
        for (const ev of parser.feed(chunk)) handleEvent(ev);
      });
      child.stderr.on('data', chunk => log(`terminal control stderr: ${String(chunk).trim()}`));
      child.on('error', err => finish(`terminal control client failed: ${err.message}`));
      child.on('exit', () => { if (!intentional) finish('terminal control client exited'); });

      let attachTimer;
      try {
        await Promise.race([
          ready,
          new Promise((_, reject) => { attachTimer = setTimeout(() => reject(new Error('terminal control attach timed out')), 5_000); }),
        ]);
      } finally {
        clearTimeout(attachTimer);
      }

      // Lowest pane index speaks for a split window, matching listScopedWindows.
      const panes = await command(`list-panes -t =${row.tmux_session}:${row.tmux_window} -F '#{pane_id}'`);
      if (!panes.ok) throw new Error('terminal pane not found');
      pane = panes.lines.map(s => s.trim()).find(s => /^%\d+$/.test(s));
      if (!pane) throw new Error('terminal pane not found');

      // Make the app repaint FIRST, then photograph the result.
      //
      // A capture-pane seed carries cells, not the TUI's render state, and a
      // pane belonging to a DETACHED tmux session is sized 80x24 by default —
      // not the size this viewer asked for. Capturing straight after the resize
      // photographs the pre-repaint screen (old layout, old width); the app's
      // redraw then lands on top of it, leaving the seams and stale borders
      // that made the modal look scrambled until the human typed and forced a
      // full redraw of their own.
      //
      // So: size the client, jiggle one row to guarantee a SIGWINCH even when
      // the size was already correct, let the app draw itself, and only THEN
      // capture. The snapshot is now the app's own freshly-drawn screen.
      // This is a terminal event, not keystroke injection — nothing reaches the
      // pane's input, so it stays outside the keystroke doctrine.
      const setSize = async (c, r) => {
        let out = await command(`refresh-client -C ${c},${r}`);
        if (!out.ok) out = await command(`refresh-client -C ${c}x${r}`); // pre-3.2 syntax
        return out;
      };
      if (!(await setSize(size.cols, size.rows)).ok) throw new Error('terminal resize failed');
      await setSize(size.cols, Math.max(1, size.rows - 1));
      await setSize(size.cols, size.rows);
      await new Promise(r => { const t = setTimeout(r, REPAINT_MS); t.unref?.(); });

      const captured = await command(`capture-pane -p -e -t ${pane}`);
      if (!captured.ok) throw new Error('terminal pane capture failed');
      const cursor = await command(`display-message -p -t ${pane} '#{cursor_x} #{cursor_y}'`);
      if (!cursor.ok) throw new Error('terminal cursor lookup failed');
      const match = /^(\d+)\s+(\d+)$/.exec(cursor.lines.at(-1)?.trim() || '');
      if (!match) throw new Error('terminal cursor lookup returned invalid data');
      established = true;
      // CRLF, never bare LF: a raw terminal reads \n as "down one row", NOT
      // "down and back to column 0" — joining a captured screen with \n walks
      // every line one column further right than the last. That staircase is
      // what "the format is all wonky" looked like.
      send({ t: 'init', cols: size.cols, rows: size.rows,
        // The parser speaks latin1 (byte-exact); the pane speaks UTF-8. Rebuild
        // the bytes and decode them as one piece so multi-byte glyphs survive.
        screen: CLEAR_SCREEN + Buffer.from(captured.lines.join('\r\n'), 'latin1').toString('utf8') + `\u001b[${Number(match[2]) + 1};${Number(match[1]) + 1}H` });
      initialized = true;
      // Whatever the app emitted while repainting is ALREADY baked into the
      // snapshot above — replaying it would double-draw. Stream starts clean.
      pendingOutput.length = 0;
    } catch (err) {
      intentional = true;
      finish(err?.message || 'terminal open failed', false);
      throw new TermBridgeError(err?.message || 'terminal open failed');
    }

    return {
      input(dataString) {
        if (finished || typeof dataString !== 'string' || !dataString) return;
        const bytes = Buffer.from(dataString, 'utf8');
        for (let offset = 0; offset < bytes.length; offset += INPUT_CHUNK_BYTES) {
          const hex = [...bytes.subarray(offset, offset + INPUT_CHUNK_BYTES)]
            .map(b => b.toString(16).padStart(2, '0')).join(' ');
          command(`send-keys -t ${pane} -H ${hex}`).then(res => {
            if (!res.ok) finish('terminal pane closed');
          }).catch(() => finish('terminal pane closed'));
        }
      },
      resize(nextCols, nextRows) {
        const next = dimensions(nextCols, nextRows);
        if (finished || !next) return;
        command(`refresh-client -C ${next.cols},${next.rows}`).then(async res => {
          if (!res.ok) res = await command(`refresh-client -C ${next.cols}x${next.rows}`);
          if (!res.ok) finish('terminal resize failed');
        }).catch(() => finish('terminal resize failed'));
      },
      close() {
        intentional = true;
        finish('terminal viewer closed', false);
      },
    };
  }

  return { openViewer, get activeViewers() { return active; } };
}
