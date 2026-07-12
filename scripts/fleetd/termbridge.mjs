// termbridge.mjs — live access to daemon-owned tmux panes.
//
// CONTRACT: the daemon keeps ONE tmux CONTROL MODE client for the whole fleet,
// shared by every viewer, and demultiplexes `%output` to subscribers by pane id.
// There is deliberately no PTY and no native dependency. Production launches
// argv [tmux, [-L socket], -C, attach-session, -t, =fleetdeck-<port>]. `-C` is
// used exactly once: unlike `-CC`, it retains the documented tmux 3.x command
// echo/response behavior parsed below.
//
// The database supplies only an already-scoped spawn row. The browser never
// supplies a tmux target. Pane discovery, the ANSI seed and cursor lookup all
// run through the attached control client; input is hex bytes so no human text
// is ever parsed as tmux syntax. FLEETDECK_TERM_CMD replaces the complete
// production argv with argv [cmd] for protocol-fixture tests.
//
// WHY ONE CLIENT (v1.9, the terminal grid). Every viewer used to own its own
// `tmux -C attach-session`, and a control client is attached to the SESSION, so
// each one received `%output` for every pane in the fleet and discarded all but
// its own. Eight tiles meant eight tmux processes each parsing eight agents'
// output to keep an eighth of it. Worse, sizing went through `refresh-client -C`
// — which sets the CLIENT's size, and a window's geometry is derived from the
// clients watching it. With one viewer that is invisible. With eight tiles of
// differing sizes it is eight clients fighting over every pane's dimensions.
//
// So: one client, output routed by pane id, and geometry set per WINDOW with
// `resize-window` under `window-size manual` — which decouples a pane's size
// from whoever happens to be watching it. That is what lets N tiles each hold
// their own shape. It also retires the old viewer cap, which existed to bound
// the per-viewer process count that no longer exists.

import { spawn } from 'node:child_process';
import { StringDecoder } from 'node:string_decoder';

const ACTIVE_STATUSES = new Set(['spawning', 'stalled', 'live']);
const INPUT_CHUNK_BYTES = 1024;
const ATTACH_TIMEOUT_MS = 5_000;

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
    // byte); the pane's real UTF-8 is reassembled by the pane stream's
    // decoder, which is the only place that knows where the character resumes.
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

/** Factory lifetime equals the daemon lifetime. The shared control client is
 * lazy: it attaches when the first viewer opens and detaches when the last one
 * leaves, so a fleet nobody is watching holds no tmux client at all. */
export function createTermBridge({ port, resolveSpawn, log = () => {} } = {}) {
  const session = `fleetdeck-${port}`;
  const viewers = new Set();
  let client = null;

  // ---------------------------------------------------------------- the client

  function createClient() {
    const c = {
      child: null,
      parser: new ControlModeParser(),
      waiters: [],
      // pane id -> { decoder, subs:Set<viewer> }. The decoder is PER PANE, not
      // per viewer: it is the pane's byte stream that gets split mid-character,
      // and two tiles watching one pane must not each hold half a glyph.
      panes: new Map(),
      manualSizing: new Set(), // windows we have switched to manual sizing
      closed: false,
      ready: null,
      readyResolve: null,
      readyReject: null,
    };
    c.ready = new Promise((resolve, reject) => { c.readyResolve = resolve; c.readyReject = reject; });
    c.ready.catch(() => { /* the attach race below reports this */ });

    c.command = (line) => new Promise((resolve, reject) => {
      if (c.closed || !c.child?.stdin?.writable) return reject(new Error('control client is closed'));
      const waiter = { resolve, reject };
      c.waiters.push(waiter);
      c.child.stdin.write(line + '\n', (err) => {
        if (!err) return;
        const i = c.waiters.indexOf(waiter);
        if (i >= 0) c.waiters.splice(i, 1);
        reject(err);
      });
    });

    const onEvent = (ev) => {
      if (ev.type === 'response') {
        c.waiters.shift()?.resolve(ev);
      } else if (ev.type === 'session-changed') {
        c.readyResolve();
      } else if (ev.type === 'output') {
        const stream = c.panes.get(ev.pane);
        if (!stream) return; // a pane nobody is watching — the whole point of demuxing
        const data = stream.decoder.write(ev.data);
        if (!data) return; // an incomplete character; it lands with the next event
        for (const v of stream.subs) v.emit(data);
      } else if (ev.type === 'exit') {
        teardown(ev.reason || 'tmux session ended');
      } else if (ev.type === 'window-close') {
        // The client sees every window in the session, so verify which of OUR
        // panes actually died rather than assuming this close was ours. One
        // list-panes answers for every viewer at once.
        if (!c.panes.size) return;
        c.command("list-panes -a -F '#{pane_id}'").then((res) => {
          if (!res.ok) return;
          const alive = new Set(res.lines.map((s) => s.trim()));
          for (const [paneId, stream] of [...c.panes]) {
            if (alive.has(paneId)) continue;
            for (const v of [...stream.subs]) v.finish('terminal pane closed');
          }
        }).catch(() => { /* a failed probe is not proof a pane died */ });
      }
    };

    const override = process.env.FLEETDECK_TERM_CMD?.trim();
    const socket = process.env.FLEETDECK_TMUX_SOCKET?.trim();
    const argv = socket
      ? ['-L', socket, '-C', 'attach-session', '-t', '=' + session]
      : ['-C', 'attach-session', '-t', '=' + session];
    c.child = spawn(override || 'tmux', override ? [] : argv, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    c.child.stdout.on('data', (chunk) => { for (const ev of c.parser.feed(chunk)) onEvent(ev); });
    c.child.stderr.on('data', (chunk) => log(`terminal control stderr: ${String(chunk).trim()}`));
    c.child.on('error', (err) => teardown(`terminal control client failed: ${err.message}`));
    c.child.on('exit', () => { if (!c.closed) teardown('terminal control client exited'); });
    return c;
  }

  /** Kill the shared client and take every viewer down with it. */
  function teardown(reason) {
    const c = client;
    if (!c || c.closed) return;
    c.closed = true;
    client = null;
    c.readyReject(new Error(reason));
    for (const waiter of c.waiters.splice(0)) waiter.reject(new Error(reason));
    for (const v of [...viewers]) v.finish(reason);
    if (c.child && c.child.exitCode === null && !c.child.killed) {
      try { c.child.kill('SIGTERM'); } catch { /* already gone */ }
    }
  }

  /** Attach (once) and wait for tmux to confirm. Concurrent openers share it. */
  async function ensureClient() {
    if (!client) client = createClient();
    const c = client;
    let timer;
    try {
      await Promise.race([
        c.ready,
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error('terminal control attach timed out')), ATTACH_TIMEOUT_MS);
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
    if (c.closed) throw new Error('terminal control client exited');
    return c;
  }

  // ---------------------------------------------------------------- geometry

  // A window's size normally follows the clients watching it. With one shared
  // client that would mean every tile shares one geometry — so put each window
  // in `window-size manual` and drive its size ourselves. This is the whole
  // reason a grid of differently-shaped tiles can work at all.
  //
  // `window-size` is a WINDOW option: it must be set with `-w` on OUR window and
  // never with `-g`, which would reach across the tmux server and re-size the
  // human's own sessions. (Verified on tmux 3.7b: three windows, three sizes.)
  //
  // Fallback: on a tmux too old for `resize-window`, go back to sizing the
  // client. That restores the pre-v1.9 behaviour — contention and all — which is
  // strictly better than refusing to show a terminal at all.
  async function sizeWindow(c, window, cols, rows) {
    const target = `=${session}:${window}`;
    if (!c.manualSizing.has(window)) {
      const opt = await c.command(`set-option -w -t ${target} window-size manual`).catch(() => ({ ok: false }));
      if (opt.ok) c.manualSizing.add(window);
    }
    if (c.manualSizing.has(window)) {
      const res = await c.command(`resize-window -t ${target} -x ${cols} -y ${rows}`);
      if (res.ok) return res;
      c.manualSizing.delete(window); // resize-window is unavailable; stop pretending
    }
    let out = await c.command(`refresh-client -C ${cols},${rows}`);
    if (!out.ok) out = await c.command(`refresh-client -C ${cols}x${rows}`); // pre-3.2 syntax
    return out;
  }

  // ---------------------------------------------------------------- viewers

  function subscribe(c, pane, viewer) {
    let stream = c.panes.get(pane);
    if (!stream) {
      stream = { decoder: new StringDecoder('utf8'), subs: new Set() };
      c.panes.set(pane, stream);
    }
    stream.subs.add(viewer);
  }

  function unsubscribe(c, pane, viewer) {
    const stream = c?.panes.get(pane);
    if (!stream) return;
    stream.subs.delete(viewer);
    if (!stream.subs.size) c.panes.delete(pane);
  }

  async function openViewer({ spawn_id, cols, rows, send, onClose = () => {} }) {
    if (process.env.FLEETDECK_TERM?.trim().toLowerCase() === 'off') throw new TermBridgeError('live terminal disabled');
    const size = dimensions(cols, rows);
    if (!size) throw new TermBridgeError('invalid terminal dimensions');

    const row = await resolveSpawn?.(spawn_id);
    if (!row) throw new TermBridgeError('no such spawn');
    if (!ACTIVE_STATUSES.has(row.status)) throw new TermBridgeError('spawn is not live');
    if (row.tmux_session !== session || !String(row.tmux_window || '').startsWith(`fd${port}-`)) {
      throw new TermBridgeError('spawn is outside this fleet');
    }

    const viewer = {
      pane: null,
      window: row.tmux_window,
      established: false,
      initialized: false,
      finished: false,
      emit(data) {
        if (this.finished || !this.initialized) return;
        try { send({ t: 'out', data }); } catch { this.finish('terminal socket closed', false); }
      },
      finish(reason, notify = true) {
        if (this.finished) return;
        this.finished = true;
        viewers.delete(this);
        if (this.pane) unsubscribe(client, this.pane, this);
        if (notify && this.established) {
          try { onClose(reason); } catch { /* socket reporting only */ }
        }
        // Nobody left watching: hand the tmux client back rather than holding a
        // control attach open over an unwatched fleet.
        if (!viewers.size) teardown('no viewers left');
      },
    };
    viewers.add(viewer);

    try {
      const c = await ensureClient();

      // Lowest pane index speaks for a split window, matching listScopedWindows.
      const panes = await c.command(`list-panes -t =${session}:${row.tmux_window} -F '#{pane_id}'`);
      if (!panes.ok) throw new Error('terminal pane not found');
      const pane = panes.lines.map((s) => s.trim()).find((s) => /^%\d+$/.test(s));
      if (!pane) throw new Error('terminal pane not found');
      viewer.pane = pane;

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
      // So: size the window, jiggle one row to guarantee a SIGWINCH even when
      // the size was already correct, let the app draw itself, and only THEN
      // capture. The snapshot is now the app's own freshly-drawn screen.
      // This is a terminal event, not keystroke injection — nothing reaches the
      // pane's input, so it stays outside the keystroke doctrine.
      if (!(await sizeWindow(c, row.tmux_window, size.cols, size.rows)).ok) throw new Error('terminal resize failed');
      await sizeWindow(c, row.tmux_window, size.cols, Math.max(1, size.rows - 1));
      await sizeWindow(c, row.tmux_window, size.cols, size.rows);
      await new Promise((r) => { const t = setTimeout(r, REPAINT_MS); t.unref?.(); });

      const captured = await c.command(`capture-pane -p -e -t ${pane}`);
      if (!captured.ok) throw new Error('terminal pane capture failed');
      const cursor = await c.command(`display-message -p -t ${pane} '#{cursor_x} #{cursor_y}'`);
      if (!cursor.ok) throw new Error('terminal cursor lookup failed');
      const match = /^(\d+)\s+(\d+)$/.exec(cursor.lines.at(-1)?.trim() || '');
      if (!match) throw new Error('terminal cursor lookup returned invalid data');

      // Subscribe only now: everything the app emitted while repainting is
      // already baked into the snapshot below, and replaying it would double-draw.
      subscribe(c, pane, viewer);
      viewer.established = true;
      // CRLF, never bare LF: a raw terminal reads \n as "down one row", NOT
      // "down and back to column 0" — joining a captured screen with \n walks
      // every line one column further right than the last. That staircase is
      // what "the format is all wonky" looked like.
      send({ t: 'init', cols: size.cols, rows: size.rows,
        // The parser speaks latin1 (byte-exact); the pane speaks UTF-8. Rebuild
        // the bytes and decode them as one piece so multi-byte glyphs survive.
        screen: CLEAR_SCREEN + Buffer.from(captured.lines.join('\r\n'), 'latin1').toString('utf8') + `\u001b[${Number(match[2]) + 1};${Number(match[1]) + 1}H` });
      viewer.initialized = true;
    } catch (err) {
      viewer.finish(err?.message || 'terminal open failed', false);
      throw new TermBridgeError(err?.message || 'terminal open failed');
    }

    return {
      input(dataString) {
        if (viewer.finished || typeof dataString !== 'string' || !dataString) return;
        const c = client;
        if (!c) return;
        const bytes = Buffer.from(dataString, 'utf8');
        for (let offset = 0; offset < bytes.length; offset += INPUT_CHUNK_BYTES) {
          const hex = [...bytes.subarray(offset, offset + INPUT_CHUNK_BYTES)]
            .map((b) => b.toString(16).padStart(2, '0')).join(' ');
          c.command(`send-keys -t ${viewer.pane} -H ${hex}`).then((res) => {
            if (!res.ok) viewer.finish('terminal pane closed');
          }).catch(() => viewer.finish('terminal pane closed'));
        }
      },
      resize(nextCols, nextRows) {
        const next = dimensions(nextCols, nextRows);
        const c = client;
        if (viewer.finished || !next || !c) return;
        // Last writer wins when two viewers watch one pane at different sizes —
        // there is one pane and it has one shape. In the grid the tiles are
        // distinct agents, so in practice each window has exactly one author.
        sizeWindow(c, viewer.window, next.cols, next.rows)
          .then((res) => { if (!res.ok) viewer.finish('terminal resize failed'); })
          .catch(() => viewer.finish('terminal resize failed'));
      },
      close() {
        viewer.finish('terminal viewer closed', false);
      },
    };
  }

  return { openViewer, get activeViewers() { return viewers.size; } };
}
