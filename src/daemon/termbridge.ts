// termbridge.ts — live access to daemon-owned tmux panes.
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

import { spawn, type ChildProcess } from 'node:child_process';
import { StringDecoder } from 'node:string_decoder';
import { exactWindowTarget, sanitizePaneText, sessionName } from './spawn.ts';
import { envInt } from './helpers.ts';
import type { SpawnRow } from './statements.ts';

// ---- tmux CONTROL MODE parse events (discriminated union) ----
interface ResponseEvent {
  type: 'response';
  key: string;
  time: string;
  number: string;
  ok: boolean;
  lines: string[];
}
interface OutputEvent {
  type: 'output';
  pane: string;
  data: Buffer;
}
interface ExitEvent {
  type: 'exit';
  reason: string;
}
interface WindowCloseEvent {
  type: 'window-close';
  window: string;
}
interface SessionChangedEvent {
  type: 'session-changed';
  session: string;
  name: string;
}
type ControlEvent =
  | ResponseEvent
  | OutputEvent
  | ExitEvent
  | WindowCloseEvent
  | SessionChangedEvent;

const ACTIVE_STATUSES = new Set(['spawning', 'stalled', 'live']);
const INPUT_CHUNK_BYTES = 1024;
const ATTACH_TIMEOUT_MS = 5_000;

// M-R5: a per-command deadline so a wedged tmux reply cannot hang a viewer open
// forever. Generous — every command here is a fast control op — and overridable
// for tests.
const COMMAND_TIMEOUT_MS = envInt('FLEETDECK_TERM_CMD_TIMEOUT_MS', 10_000, { min: 100 });
// A FAILED window-close probe (a rejected or !ok list-panes) proves nothing —
// so instead of guessing, re-list once after a short settle delay. Without
// this an idle viewer whose pane just died never finishes: nothing else ever
// sends that pane a command that could observe the death.
const CLOSE_RECHECK_MS = envInt('FLEETDECK_TERM_CLOSE_RECHECK_MS', 1_000, { min: 50 });
// M-R4: the most pending keystroke bytes we will hold for ONE viewer before
// evicting it. A human types bytes; only a runaway paste/automation hits this.
const MAX_INPUT_QUEUE_BYTES = envInt('FLEETDECK_TERM_INPUT_MAX_BYTES', 256 * 1024, { min: 1024 });
// The pre-init OUTPUT gap buffer gets the same bound as keystrokes: a chatty
// TUI repainting through the seed/cursor/init round-trips pushes into
// viewer.pending, which used to grow with no cap. A pane that floods past the
// bound before init ships is resynced (finished): holding it unbounded grew
// daemon memory per open viewer and the flushPending burst that finally
// shipped it could trip MAX_TERM_WS_BUFFER and disconnect the viewer anyway.
const MAX_PENDING_OUTPUT_BYTES = envInt('FLEETDECK_TERM_PENDING_MAX_BYTES', MAX_INPUT_QUEUE_BYTES, {
  min: 1024,
});

// Cursor home + erase screen: a fresh viewer must never inherit stale cells.
const CLEAR_SCREEN = '\u001b[H\u001b[2J';
// DEC private mode 2004 (bracketed paste) ON. Replayed into a fresh viewer's seed
// when the pane has the mode set; capture-pane restores cells, not mode state.
const BRACKETED_PASTE_ON = '\u001b[?2004h';
// The input-side bracketed-paste transaction. A browser paste must be ONE
// editable composer operation, not N live Enter keys. Built visibly in source:
// literal ESC bytes are nearly impossible to review correctly.
const BRACKETED_PASTE_START = '\u001b[200~';
const BRACKETED_PASTE_END = '\u001b[201~';
// Delay before the post-seed repaint jiggle — long enough that the seed has
// rendered, short enough that the human never sees the snapshot's seams.
const REPAINT_MS = envInt('FLEETDECK_TERM_REPAINT_MS', 80);

// BUG-055: how often the shared client re-reads #{pane_dead} for the panes it
// is watching. A remain-on-exit pane (the fleet's dead-pane detection default)
// emits NO %window-close when its process exits, still lists, and still
// answers send-keys with %end ok — so neither of the bridge's old death
// signals fires and an open viewer would stay 'live' on a dead pane forever,
// silently discarding keystrokes. pane_dead is the only honest tell.
const PANE_DEAD_POLL_MS = envInt('FLEETDECK_TERM_DEAD_POLL_MS', 5_000, { min: 100 });

function dimensions(cols: unknown, rows: unknown): { cols: number; rows: number } | null {
  const c = Number(cols);
  const r = Number(rows);
  if (!Number.isInteger(c) || !Number.isInteger(r) || c < 1 || r < 1 || c > 1000 || r > 1000)
    return null;
  return { cols: c, rows: r };
}

/** Decode tmux control-mode's octal byte quoting (\NNN). Backslash itself
 * is emitted as \134 by tmux. Unknown/incomplete backslashes are retained.
 *
 * `value` is a LATIN-1 string — one char per byte (see ControlModeParser) — so
 * every char maps straight back to the byte it came from. Anything else would
 * re-encode bytes we do not own: the control stream is a byte pipe, and the
 * UTF-8 sitting inside it is the PANE's business, reassembled downstream. */
export function unescapeControlData(value: string): Buffer {
  const text = value;
  const bytes: number[] = [];
  for (let i = 0; i < text.length; ) {
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
  decoder: StringDecoder;
  pending: string;
  block: { time: string; number: string; lines: string[] } | null;

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

  feed(chunk: Buffer | string): ControlEvent[] {
    this.pending += Buffer.isBuffer(chunk) ? this.decoder.write(chunk) : chunk;
    const events: ControlEvent[] = [];
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

  #line(line: string, events: ControlEvent[]) {
    const boundary = /^%(begin|end|error)\s+(\S+)\s+(\S+)(?:\s+.*)?$/.exec(line);
    if (this.block) {
      if (
        boundary &&
        boundary[1] !== 'begin' &&
        boundary[2] === this.block.time &&
        boundary[3] === this.block.number
      ) {
        events.push({
          type: 'response',
          key: `${this.block.time}:${this.block.number}`,
          time: this.block.time,
          number: this.block.number,
          ok: boundary[1] === 'end',
          lines: this.block.lines,
        });
        this.block = null;
      } else {
        this.block.lines.push(line);
      }
      return;
    }
    if (boundary?.[1] === 'begin') {
      this.block = { time: boundary[2] ?? '', number: boundary[3] ?? '', lines: [] };
      return;
    }
    const output = /^%output\s+(%\S+)\s?(.*)$/.exec(line);
    if (output) {
      events.push({
        type: 'output',
        pane: output[1] ?? '',
        data: unescapeControlData(output[2] ?? ''),
      });
      return;
    }
    const exit = /^%exit(?:\s+(.*))?$/.exec(line);
    if (exit) {
      // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty ("%exit ") reason must fall back too, not just an absent one
      events.push({ type: 'exit', reason: exit[1] || 'tmux control client exited' });
      return;
    }
    const closed = /^%window-close\s+(\S+)/.exec(line);
    if (closed) {
      events.push({ type: 'window-close', window: closed[1] ?? '' });
      return;
    }
    const session = /^%session-changed\s+(\S+)(?:\s+(.*))?$/.exec(line);
    if (session)
      events.push({ type: 'session-changed', session: session[1] ?? '', name: session[2] ?? '' });
  }
}

// Not exported: only thrown/caught inside this file. http.mjs duck-types the
// failure via `err?.reason`, so no importer needs the class itself.
class TermBridgeError extends Error {
  readonly reason: string;
  readonly gone: boolean;
  constructor(reason: string, { gone = false }: { gone?: boolean } = {}) {
    super(reason);
    this.reason = reason;
    // gone: the row still says live but its window/pane is already absent (a
    // race — remain-on-exit is best-effort and the ~10s liveness tick has not
    // reconciled yet). http.mjs reports this as an 'exit' ("the agent ended"),
    // not an 'err' ("viewer refused"), because a vanished pane is the agent
    // ending, not a viewer fault.
    this.gone = gone;
  }
}

// ---- shared-client and viewer shapes ----
interface Waiter {
  resolve: (value: ResponseEvent) => void;
  reject: (err: Error) => void;
}
interface PaneStream {
  decoder: StringDecoder;
  subs: Set<Viewer>;
}
interface Client {
  child: ChildProcess | null;
  parser: ControlModeParser;
  waiters: Waiter[];
  panes: Map<string, PaneStream>;
  manualSizing: Set<string>;
  closed: boolean;
  ready: Promise<void>;
  readyResolve: () => void;
  readyReject: (err: Error) => void;
  command: (line: string) => Promise<ResponseEvent>;
  deadTimer: ReturnType<typeof setInterval>;
  exited: Promise<void>;
  detachDataListeners: () => void;
  detachAllListeners: () => void;
}
interface Viewer {
  pane: string | null;
  window: string;
  established: boolean;
  initialized: boolean;
  finished: boolean;
  pendingExit: string | null;
  pending: string[];
  pendingBytes: number;
  queuedInput: number;
  inputChain: Promise<void>;
  emit(data: string): void;
  flushPending(): void;
  end(reason: string): void;
  finish(reason: string, notify?: boolean): void;
}
export type TermFrame =
  | { t: 'out'; data: string }
  | { t: 'init'; cols: number; rows: number; screen: string }
  | { t: 'paste-refused'; reason: string };
type TermSend = (frame: TermFrame) => void;
interface OpenViewerOptions {
  spawn_id: string;
  cols: number;
  rows: number;
  send: TermSend;
  onClose?: (reason: string) => void;
  isAborted?: () => boolean;
}
interface ViewerHandle {
  input(dataString: string): void;
  paste(dataString: string): void;
  resize(nextCols: number, nextRows: number): void;
  close(): void;
}
interface TermBridgeOptions {
  port: number;
  resolveSpawn: (spawnId: string) => SpawnRow | null | Promise<SpawnRow | null>;
  log?: (message: string) => void;
  /** P1 lifecycle test seam; production keeps the one-second TERM grace. */
  closeGraceMs?: number;
}

/** Factory lifetime equals the daemon lifetime. The shared control client is
 * lazy: it attaches when the first viewer opens and detaches when the last one
 * leaves, so a fleet nobody is watching holds no tmux client at all. */
export function createTermBridge({
  port,
  resolveSpawn,
  log = () => {
    /* silent by default */
  },
  closeGraceMs = 1_000,
}: TermBridgeOptions) {
  const session = sessionName(port);
  const viewers = new Set<Viewer>();
  const clients = new Set<Client>();
  const closeRecheckTimers = new Set<ReturnType<typeof setTimeout>>();
  const attachTimers = new Set<ReturnType<typeof setTimeout>>();
  const delaySettlers = new Set<() => void>();
  const terminateGraceMs =
    Number.isFinite(closeGraceMs) && closeGraceMs >= 1 ? Math.floor(closeGraceMs) : 1_000;
  let client: Client | null = null;
  let phase: 'open' | 'closing' | 'closed' = 'open';
  let closePromise: Promise<void> | null = null;

  // ---------------------------------------------------------------- the client

  const bridgeClosedError = () => new TermBridgeError('terminal bridge is closed');

  /** A close-aware delay: quiesce resolves it immediately instead of leaving an
   * in-progress openViewer parked until a repaint timer fires. */
  function delay(ms: number): Promise<void> {
    return new Promise((resolve) => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const settle = () => {
        if (!delaySettlers.delete(settle)) return;
        if (timer) clearTimeout(timer);
        timer = null;
        resolve();
      };
      delaySettlers.add(settle);
      timer = setTimeout(settle, ms);
      timer.unref();
    });
  }

  function createClient(): Client {
    let readyResolve: () => void = () => {
      /* replaced by the executor below */
    };
    let readyReject: (err: Error) => void = () => {
      /* replaced by the executor below */
    };
    const ready = new Promise<void>((resolve, reject) => {
      readyResolve = () => {
        resolve();
      };
      readyReject = (err) => {
        reject(err);
      };
    });
    ready.catch(() => {
      /* the attach race below reports this */
    });

    const c: Client = {
      child: null,
      parser: new ControlModeParser(),
      waiters: [],
      // pane id -> { decoder, subs:Set<viewer> }. The decoder is PER PANE, not
      // per viewer: it is the pane's byte stream that gets split mid-character,
      // and two tiles watching one pane must not each hold half a glyph.
      panes: new Map(),
      manualSizing: new Set(), // windows we have switched to manual sizing
      closed: false,
      ready,
      readyResolve,
      readyReject,
      command: (line: string): Promise<ResponseEvent> =>
        new Promise<ResponseEvent>((resolve, reject) => {
          if (c.closed || !c.child?.stdin?.writable) {
            reject(new Error('control client is closed'));
            return;
          }
          // M-R5: EVERY command gets a deadline, not just the initial attach. A
          // wedged list-panes/capture used to hang the awaiting viewer forever —
          // counted in `viewers`, pinning the shared client. On timeout we tear the
          // WHOLE client down rather than quietly drop this one waiter: responses are
          // matched to commands purely by FIFO order, so a reply that arrives late
          // would resolve some OTHER command's promise. teardown() rejects every
          // outstanding waiter and each viewer re-opens against a fresh client.
          const timer = setTimeout(() => {
            teardown('terminal control command timed out');
          }, COMMAND_TIMEOUT_MS);
          timer.unref();
          const waiter: Waiter = {
            resolve: (v) => {
              clearTimeout(timer);
              resolve(v);
            },
            reject: (e) => {
              clearTimeout(timer);
              reject(e);
            },
          };
          c.waiters.push(waiter);
          c.child.stdin.write(line + '\n', (err) => {
            if (!err) return;
            const i = c.waiters.indexOf(waiter);
            if (i >= 0) c.waiters.splice(i, 1);
            waiter.reject(err);
          });
        }),
      // BUG-055: periodic #{pane_dead} re-read for every subscribed pane. This is
      // the ONLY signal that fires when a remain-on-exit pane's process exits —
      // %window-close never comes, list-panes still lists, send-keys still
      // answers ok. A dead pane is the agent ENDING (spawns.mjs's liveness tick
      // condemns on the same flag), so viewers get the same 'terminal pane
      // closed' exit as a genuinely vanished pane. A failed/ambiguous read is
      // not proof of death, matching the window-close probe above.
      deadTimer: setInterval(() => {
        if (c.closed || !c.panes.size) return;
        c.command("list-panes -a -F '#{pane_id} #{pane_dead}'")
          .then((res) => {
            if (!res.ok || c.closed) return;
            const state = new Map<string, string>();
            for (const line of res.lines) {
              const m = /^(%\d+)\s+([01])$/.exec(line.trim());
              if (m) state.set(m[1] ?? '', m[2] ?? '');
            }
            for (const [paneId, stream] of [...c.panes]) {
              if (state.get(paneId) !== '1') continue;
              for (const v of [...stream.subs]) v.end('terminal pane closed');
            }
          })
          .catch(() => {
            /* a failed probe is not proof a pane died */
          });
      }, PANE_DEAD_POLL_MS),
      exited: Promise.resolve(),
      detachDataListeners: () => {
        /* installed after spawn */
      },
      detachAllListeners: () => {
        /* installed after spawn */
      },
    };
    c.deadTimer.unref();

    const onEvent = (ev: ControlEvent) => {
      if (c.closed || phase !== 'open') return;
      if (ev.type === 'response') {
        c.waiters.shift()?.resolve(ev);
      } else if (ev.type === 'session-changed') {
        c.readyResolve();
      } else if (ev.type === 'exit') {
        teardown(ev.reason || 'tmux session ended');
      } else if (ev.type === 'window-close') {
        // The client sees every window in the session, so verify which of OUR
        // panes actually died rather than assuming this close was ours. One
        // list-panes answers for every viewer at once.
        if (!c.panes.size) return;
        c.command("list-panes -a -F '#{pane_id}'")
          .then((res) => {
            if (c.closed || phase !== 'open') return;
            if (!res.ok) {
              scheduleCloseRecheck();
              return;
            }
            const alive = new Set(res.lines.map((s) => s.trim()));
            for (const [paneId, stream] of [...c.panes]) {
              if (alive.has(paneId)) continue;
              for (const v of [...stream.subs]) v.end('terminal pane closed');
            }
          })
          .catch(() => {
            scheduleCloseRecheck();
          });
      }
    };

    const override = process.env['FLEETDECK_TERM_CMD']?.trim();
    const socket = process.env['FLEETDECK_TMUX_SOCKET']?.trim();
    const argv = socket
      ? ['-L', socket, '-C', 'attach-session', '-t', '=' + session]
      : ['-C', 'attach-session', '-t', '=' + session];
    // eslint-disable-next-line @typescript-eslint/prefer-nullish-coalescing -- an empty FLEETDECK_TERM_CMD means "unset", so fall back to the real tmux
    const child = spawn(override || 'tmux', override ? [] : argv, {
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
      // Live env, not Bun's startup snapshot (see exec.ts): the attach reads
      // FLEETDECK_TMUX_SOCKET/TMUX_TMPDIR above, so the child tmux must see the
      // same runtime-mutated values; a no-op under Node.
      env: process.env,
    });
    c.child = child;
    clients.add(c);
    let exitedResolve: () => void = () => {
      /* installed by the Promise executor */
    };
    c.exited = new Promise<void>((resolve) => {
      exitedResolve = resolve;
    });
    // M-P6: batch %output per pane across ONE feed() chunk. tmux flushes a TUI
    // redraw as a burst of %output lines; emitting one ws frame per line meant
    // dozens of ws.send calls (× every viewer) for a single keystroke's repaint.
    // We concatenate a pane's bytes from this chunk and emit ONCE — which, as a
    // bonus, hands the pane decoder the whole burst so it splits fewer multibyte
    // glyphs. A non-output event flushes the pending batch first, so a pane's
    // bytes can never overtake its own exit/close.
    const onStdout = (chunk: Buffer) => {
      if (c.closed || phase !== 'open') return;
      const batched = new Map<string, Buffer[]>(); // pane id -> Buffer[]
      const flush = () => {
        if (c.closed || phase !== 'open') return;
        for (const [pane, parts] of batched) {
          const stream = c.panes.get(pane);
          if (!stream) continue;
          const data = stream.decoder.write(Buffer.concat(parts));
          if (data) for (const v of stream.subs) v.emit(data);
        }
        batched.clear();
      };
      for (const ev of c.parser.feed(chunk)) {
        if (ev.type === 'output') {
          if (!c.panes.has(ev.pane)) continue; // a pane nobody is watching — the point of demuxing
          let parts = batched.get(ev.pane);
          if (!parts) batched.set(ev.pane, (parts = []));
          parts.push(ev.data);
        } else {
          flush();
          onEvent(ev);
        }
      }
      flush();
    };
    const onStderr = (chunk: Buffer) => {
      if (c.closed || phase !== 'open') return;
      log(`terminal control stderr: ${String(chunk).trim()}`);
    };
    const onError = (err: Error) => {
      if (!c.closed && phase === 'open') {
        teardown(`terminal control client failed: ${err.message}`);
      }
    };
    const onExit = () => {
      // `exit` means the direct control child has been reaped. `close` can be
      // delayed forever by a grandchild that inherited stdout/stderr, so the
      // lifecycle join below deliberately waits on this event instead.
      exitedResolve();
      if (!c.closed) teardown('terminal control client exited');
    };
    const onClose = () => {
      // A failed spawn can reach `close` without a useful `exit`; either event
      // is terminal for the direct child from this bridge's point of view.
      exitedResolve();
      clients.delete(c);
      c.detachAllListeners();
    };
    let dataListenersDetached = false;
    let allListenersDetached = false;
    c.detachDataListeners = () => {
      if (dataListenersDetached) return;
      dataListenersDetached = true;
      child.stdout.off('data', onStdout);
      child.stderr.off('data', onStderr);
    };
    c.detachAllListeners = () => {
      if (allListenersDetached) return;
      allListenersDetached = true;
      c.detachDataListeners();
      child.off('error', onError);
      child.off('exit', onExit);
      child.off('close', onClose);
    };
    child.stdout.on('data', onStdout);
    child.stderr.on('data', onStderr);
    child.on('error', onError);
    child.on('exit', onExit);
    child.on('close', onClose);
    return c;
  }

  /** Kill the shared client and take every viewer down with it. */
  function teardown(reason: string) {
    const c = client;
    if (!c || c.closed) return;
    c.closed = true;
    client = null;
    clearInterval(c.deadTimer);
    c.readyReject(new Error(reason));
    for (const waiter of c.waiters.splice(0)) waiter.reject(new Error(reason));
    for (const v of [...viewers]) v.finish(reason);
    if (c.child?.exitCode === null && !c.child.killed) {
      try {
        c.child.kill('SIGTERM');
      } catch {
        /* already gone */
      }
    }
  }

  /** A window-close probe failed: re-list ONCE after a short settle delay. The
   * %window-close notification tells us SOME window died; if the re-list can be
   * answered at all, any pane absent from it is provably gone and its viewers
   * are finished. A second failure gives up (still no proof) — the M-R5 per-
   * command deadline keeps the next viewer command from hanging forever either
   * way. Serializes with in-flight close probes and skips clients that are
   * already torn down. */
  function scheduleCloseRecheck() {
    const c = client;
    if (!c || c.closed || phase !== 'open' || !c.panes.size) return;
    const timer = setTimeout(() => {
      closeRecheckTimers.delete(timer);
      if (phase !== 'open' || client !== c || c.closed || !c.panes.size) return;
      c.command("list-panes -a -F '#{pane_id}'")
        .then((res) => {
          if (phase !== 'open' || c.closed) return;
          if (!res.ok) return;
          const alive = new Set(res.lines.map((s) => s.trim()));
          for (const [paneId, stream] of [...c.panes]) {
            if (alive.has(paneId)) continue;
            for (const v of [...stream.subs]) v.end('terminal pane closed');
          }
        })
        .catch(() => {
          /* a failed probe is not proof a pane died */
        });
    }, CLOSE_RECHECK_MS);
    closeRecheckTimers.add(timer);
    timer.unref();
  }

  /** Attach (once) and wait for tmux to confirm. Concurrent openers share it. */
  async function ensureClient(): Promise<Client> {
    if (phase !== 'open') throw bridgeClosedError();
    client ??= createClient();
    const c = client;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        c.ready,
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => {
            if (timer) attachTimers.delete(timer);
            reject(new Error('terminal control attach timed out'));
          }, ATTACH_TIMEOUT_MS);
          attachTimers.add(timer);
        }),
      ]);
    } finally {
      clearTimeout(timer);
      if (timer) attachTimers.delete(timer);
    }
    if (phase !== 'open') throw bridgeClosedError();
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
  //
  // The fallback is for UNSUPPORT, not failure: a resize-window that errors
  // AFTER `window-size manual` was accepted (racing teardown, concurrent
  // resize) is reported to the caller, not silently retried as a client
  // refresh — otherwise the caller believes the window reached the requested
  // geometry when only the client did (BUG-159: the open-time jiggle would
  // then capture a short window and ship an init claiming the full rows).
  async function sizeWindow(
    c: Client,
    window: string,
    cols: number,
    rows: number,
  ): Promise<ResponseEvent> {
    const target = exactWindowTarget(port, window);
    if (!c.manualSizing.has(window)) {
      const opt = await c
        .command(`set-option -w -t ${target} window-size manual`)
        .catch(() => ({ ok: false }));
      if (opt.ok) c.manualSizing.add(window);
    }
    if (c.manualSizing.has(window)) {
      return await c.command(`resize-window -t ${target} -x ${cols} -y ${rows}`);
    }
    let out = await c.command(`refresh-client -C ${cols},${rows}`);
    if (!out.ok) out = await c.command(`refresh-client -C ${cols}x${rows}`); // pre-3.2 syntax
    return out;
  }

  // ---------------------------------------------------------------- viewers

  function subscribe(c: Client, pane: string, viewer: Viewer) {
    let stream = c.panes.get(pane);
    if (!stream) {
      stream = { decoder: new StringDecoder('utf8'), subs: new Set() };
      c.panes.set(pane, stream);
    }
    stream.subs.add(viewer);
  }

  function unsubscribe(c: Client | null, pane: string, viewer: Viewer) {
    const stream = c?.panes.get(pane);
    if (!c || !stream) return;
    stream.subs.delete(viewer);
    if (!stream.subs.size) c.panes.delete(pane);
  }

  async function openViewer({
    spawn_id,
    cols,
    rows,
    send,
    onClose = () => {
      /* no-op by default */
    },
    isAborted = () => false,
  }: OpenViewerOptions): Promise<ViewerHandle> {
    if (phase !== 'open') throw bridgeClosedError();
    if (process.env['FLEETDECK_TERM']?.trim().toLowerCase() === 'off')
      throw new TermBridgeError('live terminal disabled');
    const size = dimensions(cols, rows);
    if (!size) throw new TermBridgeError('invalid terminal dimensions');
    // M-R5: the WS can close mid-open, before a handle exists to close(). Bail
    // between awaits so the half-opened viewer is torn down, not left counted.
    const abortIfClosed = () => {
      if (phase !== 'open') throw bridgeClosedError();
      if (isAborted()) throw new Error('terminal viewer closed during open');
    };

    const row = await resolveSpawn(spawn_id);
    abortIfClosed();
    if (!row) throw new TermBridgeError('no such spawn');
    if (!ACTIVE_STATUSES.has(row.status)) throw new TermBridgeError('spawn is not live');
    const windowName = row.tmux_window;
    if (row.tmux_session !== session || !windowName?.startsWith(`fd${port}-`)) {
      throw new TermBridgeError('spawn is outside this fleet');
    }

    const viewer: Viewer = {
      pane: null,
      window: windowName,
      established: false,
      initialized: false,
      finished: false,
      pendingExit: null,
      pending: [], // R1-4: %output that arrived after we
      // subscribed but before the init frame shipped
      pendingBytes: 0, // byte bound on the above (BUG-158)
      queuedInput: 0, // M-R4: pending input bytes not yet sent
      inputChain: Promise.resolve(), // M-R4: serializes this viewer's send-keys
      emit(data) {
        if (this.finished) return;
        // R1-4: subscribed but not yet initialized — BUFFER, never drop. We now
        // subscribe right after capture-pane (see below), so output landing
        // during the cursor lookup + init build must be held until the seed has
        // shipped, then replayed in order by flushPending(). The buffer is BYTE-
        // BOUNDED like the keystroke queue (M-R4): past the bound the seed this
        // viewer is waiting for can no longer be replayed faithfully, so we
        // finish for a resync rather than hoard unbounded pane output.
        if (!this.initialized) {
          const bytes = Buffer.byteLength(data, 'utf8');
          if (this.pendingBytes + bytes > MAX_PENDING_OUTPUT_BYTES) {
            // finish() only notifies ESTABLISHED viewers, and this one never
            // got its init — but the socket is exactly who must hear this
            // (onClose sends 'exit', which the board reads as "resync"), so
            // stand in as established. The open below then completes against a
            // finished viewer: its init send is dropped by the closed socket.
            this.established = true;
            this.finish('terminal output overflow before init');
            return;
          }
          this.pendingBytes += bytes;
          this.pending.push(data);
          return;
        }
        try {
          send({ t: 'out', data });
        } catch {
          this.finish('terminal socket closed', false);
        }
      },
      // R1-4: replay the gap buffer AFTER the init frame, in arrival order. The
      // captured seed holds only what existed at capture time and this buffer
      // only what arrived after, so there is no double-draw.
      flushPending() {
        const buffered = this.pending;
        this.pending = [];
        this.pendingBytes = 0;
        for (const data of buffered) {
          if (this.finished) return;
          try {
            send({ t: 'out', data });
          } catch {
            this.finish('terminal socket closed', false);
          }
        }
      },
      // The pane-death poll and window-close probe can race the seed sequence:
      // viewers subscribe before capture so no terminal output is lost, but the
      // socket is not established until its init frame is ready. Remember an
      // early terminal exit and deliver it immediately after init instead of
      // silently finishing a viewer that the HTTP layer cannot notify yet.
      end(reason) {
        if (this.finished) return;
        if (!this.established) {
          this.pendingExit ??= reason;
          return;
        }
        this.finish(reason);
      },
      finish(reason, notify = true) {
        if (this.finished) return;
        this.finished = true;
        viewers.delete(this);
        if (this.pane) unsubscribe(client, this.pane, this);
        if (notify && this.established) {
          try {
            onClose(reason);
          } catch {
            /* socket reporting only */
          }
        }
        // Nobody left watching: hand the tmux client back rather than holding a
        // control attach open over an unwatched fleet.
        if (!viewers.size) teardown('no viewers left');
      },
    };
    viewers.add(viewer);

    try {
      const c = await ensureClient();
      abortIfClosed();

      // Lowest pane index speaks for a split window, matching listScopedWindows.
      const panes = await c.command(
        `list-panes -t ${exactWindowTarget(port, windowName)} -F '#{pane_id}'`,
      );
      // The row passed the ACTIVE_STATUSES gate above, so tmux failing to list a
      // pane — or listing none — means the window is already gone: the agent
      // ended between the liveness tick and this open. That is not a viewer
      // fault, so surface it as `gone` (http.mjs → 'exit', not 'err').
      if (!panes.ok)
        throw new TermBridgeError('terminal pane is gone — the agent has ended', { gone: true });
      const pane = panes.lines.map((s) => s.trim()).find((s) => /^%\d+$/.test(s));
      if (!pane)
        throw new TermBridgeError('terminal pane is gone — the agent has ended', { gone: true });
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
      // EVERY step of the jiggle must succeed, not just the first: a failed
      // restore after a successful rows-1 step would leave the window SHORT
      // while the init frame advertises the requested rows, and the client
      // would lay the shorter seed into the wrong geometry. tmux 3.7b keeps
      // the prior geometry on a failed resize-window, so recover by restoring
      // the requested size — and if even that fails, abort before capture.
      if (!(await sizeWindow(c, windowName, size.cols, size.rows)).ok)
        throw new Error('terminal resize failed');
      if (!(await sizeWindow(c, windowName, size.cols, Math.max(1, size.rows - 1))).ok) {
        await sizeWindow(c, windowName, size.cols, size.rows);
        throw new Error('terminal resize failed');
      }
      if (!(await sizeWindow(c, windowName, size.cols, size.rows)).ok) {
        await sizeWindow(c, windowName, size.cols, size.rows);
        throw new Error('terminal resize failed');
      }
      await delay(REPAINT_MS);
      abortIfClosed();

      // R1-4/BUG-056: subscribe BEFORE the seed is even requested — not after the
      // capture resolves, and not after the cursor lookup below. The control
      // client demuxes %output only to panes already in c.panes, so anything the
      // app emits while this viewer is still opening is discarded forever
      // otherwise. The capture made this concrete: tmux flushes
      // `%end …\n%output %N …` in ONE stdout write, the stdout handler resolves
      // the capture waiter and still processes that trailing %output in the same
      // feed() loop, and Promise continuations are microtasks — they always run
      // AFTER the current stack. Subscribing only after the await therefore
      // guaranteed same-chunk post-capture bytes (the post-resize repaint's
      // tail) never reached the viewer.
      //
      // There is no double-draw: the seed holds only what existed at capture
      // time, and emit() buffers every earlier byte into viewer.pending until
      // the init frame ships; flushPending() replays the buffer right after, in
      // arrival order. Bytes from the resize jiggle above may arrive before the
      // seed — they are the app's own freshly-drawn screen, which is exactly
      // what the capture then photographs, so replaying them after the init
      // repaints what is already shown.
      subscribe(c, pane, viewer);

      const captured = await c.command(`capture-pane -p -e -t ${pane}`);
      if (!captured.ok) throw new Error('terminal pane capture failed');

      // Read the cursor AND the pane's bracketed-paste (DEC 2004) state in one
      // round-trip. capture-pane restores cells, not private-mode state, so a
      // fresh viewer's xterm would come up with bracketed paste OFF even when the
      // agent enabled it — and the board then blocks multi-line paste on an agent
      // pane. tmux tracks the mode per pane and exposes it as #{bracket_paste_flag}
      // (1/0); we replay it into the seed below. The flag field is OPTIONAL in the
      // parse so an older tmux without the format degrades to OFF (no regression).
      const cursor = await c.command(
        `display-message -p -t ${pane} '#{cursor_x} #{cursor_y} #{bracket_paste_flag}'`,
      );
      if (!cursor.ok) throw new Error('terminal cursor lookup failed');
      const match = /^(\d+)\s+(\d+)(?:\s+(\d+))?\s*$/.exec(cursor.lines.at(-1)?.trim() ?? '');
      if (!match) throw new Error('terminal cursor lookup returned invalid data');
      const bracketedPaste = match[3] === '1';

      viewer.established = true;
      // CRLF, never bare LF: a raw terminal reads \n as "down one row", NOT
      // "down and back to column 0" — joining a captured screen with \n walks
      // every line one column further right than the last. That staircase is
      // what "the format is all wonky" looked like.
      send({
        t: 'init',
        cols: size.cols,
        rows: size.rows,
        // The parser speaks latin1 (byte-exact); the pane speaks UTF-8. Rebuild
        // the bytes and decode them as one piece so multi-byte glyphs survive.
        screen:
          CLEAR_SCREEN +
          Buffer.from(captured.lines.join('\r\n'), 'latin1').toString('utf8') +
          `\u001b[${Number(match[2]) + 1};${Number(match[1]) + 1}H` +
          // Replay the pane's bracketed-paste (DEC 2004) mode into the seed after
          // the cursor is parked — a mode set is position-independent. The board
          // writes this seed through xterm right after a term.reset(), so
          // BRACKETED_PASTE_ON restores term.modes.bracketedPasteMode and the paste
          // gate stops blocking multi-line paste on agent panes.
          (bracketedPaste ? BRACKETED_PASTE_ON : ''),
      });
      viewer.initialized = true;
      if (viewer.pendingExit) {
        const reason = viewer.pendingExit;
        viewer.pendingExit = null;
        viewer.finish(reason);
      } else {
        // R1-4: replay whatever arrived during the cursor lookup / init build.
        viewer.flushPending();
      }
    } catch (err) {
      const detail = err instanceof Error && err.message ? err.message : 'terminal open failed';
      viewer.finish(detail, false);
      // Preserve a TermBridgeError verbatim (notably its `gone` flag) rather
      // than re-wrapping it into a plain, flagless TermBridgeError.
      if (err instanceof TermBridgeError) throw err;
      throw new TermBridgeError(detail);
    }

    // Queue bytes behind every earlier keystroke/paste. The board can deliver a
    // paste and a key in adjacent websocket frames; one FIFO is what keeps the
    // following Enter from overtaking the paste it submits.
    const queueInput = (bytes: Buffer, beforeSend?: () => Promise<boolean>): void => {
      if (viewer.finished || bytes.length === 0) return;
      const c = client;
      if (!c) return;
      const pane = viewer.pane;
      if (!pane) return;
      // M-R4: bound and SERIALIZE input. Firing one send-keys promise per 1 KB
      // the instant bytes arrived let a multi-megabyte paste stack thousands of
      // hex commands into c.waiters at once, with no backpressure. Instead we
      // refuse to queue more than MAX_INPUT_QUEUE_BYTES of pending input (a
      // viewer that outruns that has lost its trustworthy FIFO slot — evict it)
      // and send the 1 KB chunks strictly one after another, in order.
      if (viewer.queuedInput + bytes.length > MAX_INPUT_QUEUE_BYTES) {
        viewer.finish('terminal input overflow');
        return;
      }
      viewer.queuedInput += bytes.length;
      viewer.inputChain = viewer.inputChain.then(async () => {
        try {
          if (beforeSend && !(await beforeSend())) return;
          for (let offset = 0; offset < bytes.length; offset += INPUT_CHUNK_BYTES) {
            if (viewer.finished || client !== c) return;
            const hex = [...bytes.subarray(offset, offset + INPUT_CHUNK_BYTES)]
              .map((b) => b.toString(16).padStart(2, '0'))
              .join(' ');
            const res = await c.command(`send-keys -t ${pane} -H ${hex}`);
            if (!res.ok) {
              viewer.finish('terminal pane closed');
              return;
            }
          }
        } catch {
          viewer.finish('terminal pane closed');
        } finally {
          viewer.queuedInput -= bytes.length;
        }
      });
    };

    const refusePaste = (reason: string): void => {
      if (viewer.finished) return;
      try {
        send({ t: 'paste-refused', reason });
      } catch {
        viewer.finish('terminal socket closed', false);
      }
    };

    return {
      input(dataString) {
        if (typeof dataString !== 'string' || !dataString) return;
        queueInput(Buffer.from(dataString, 'utf8'));
      },
      paste(dataString) {
        if (typeof dataString !== 'string' || !dataString) return;
        // Reuse the owned-pane injection sanitizer: clipboard text must not be
        // able to smuggle an END marker (or another terminal control) out of the
        // bracket and turn its suffix into live keystrokes. xterm normalizes
        // pasted line endings to CR; preserve those exact native semantics.
        const safe = sanitizePaneText(dataString).replace(/\n/g, '\r');
        if (!safe) return;
        const bytes = Buffer.from(BRACKETED_PASTE_START + safe + BRACKETED_PASTE_END, 'utf8');

        // A Fleet Deck Claude pane is contractually a Claude TUI, which supports
        // bracketed paste even if the browser's reconstructed xterm mode raced
        // its init seed. A shell pane is open-ended: ask tmux for the CURRENT
        // application mode at the paste boundary and refuse rather than execute
        // each line if that shell did not opt in.
        const provePasteSupport =
          row.kind !== 'shell'
            ? undefined
            : async (): Promise<boolean> => {
                const c = client;
                const pane = viewer.pane;
                if (!c || !pane || viewer.finished) return false;
                const mode = await c.command(
                  `display-message -p -t ${pane} '#{bracket_paste_flag}'`,
                );
                if (mode.ok && mode.lines.at(-1)?.trim() === '1') return true;
                refusePaste(
                  'multiline paste needs bracketed-paste support — this shell did not request it',
                );
                return false;
              };
        queueInput(bytes, provePasteSupport);
      },
      resize(nextCols, nextRows) {
        const next = dimensions(nextCols, nextRows);
        const c = client;
        if (viewer.finished || !next || !c) return;
        // Last writer wins when two viewers watch one pane at different sizes —
        // there is one pane and it has one shape. In the grid the tiles are
        // distinct agents, so in practice each window has exactly one author.
        sizeWindow(c, viewer.window, next.cols, next.rows)
          .then((res) => {
            if (!res.ok) viewer.finish('terminal resize failed');
          })
          .catch(() => {
            viewer.finish('terminal resize failed');
          });
      },
      close() {
        viewer.finish('terminal viewer closed', false);
      },
    };
  }

  async function settlesWithin(promise: Promise<void>, milliseconds: number): Promise<boolean> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([
        promise.then(() => true),
        new Promise<false>((resolve) => {
          timer = setTimeout(() => resolve(false), milliseconds);
          timer.unref();
        }),
      ]);
    } finally {
      if (timer) clearTimeout(timer);
    }
  }

  function releaseClientProcessHandles(c: Client, child: ChildProcess): void {
    // Once the direct child has exited (or the post-KILL deadline elapsed), no
    // bridge callback may retain the ChildProcess or its pipe handles. Destroy
    // only the parent-owned ends: descendants that inherited the write ends
    // must not hold daemon shutdown hostage waiting for ChildProcess `close`.
    c.detachAllListeners();
    try {
      child.stdin?.destroy();
    } catch {
      /* already closed */
    }
    try {
      child.stdout?.destroy();
    } catch {
      /* already closed */
    }
    try {
      child.stderr?.destroy();
    } catch {
      /* already closed */
    }
    clients.delete(c);
  }

  async function terminateClient(c: Client): Promise<void> {
    const child = c.child;
    if (!child) return;
    if (child.exitCode === null && child.signalCode === null) {
      try {
        child.kill('SIGTERM');
      } catch {
        /* already gone */
      }
    }

    const exitedDuringGrace = await settlesWithin(c.exited, terminateGraceMs);
    if (!exitedDuringGrace && child.exitCode === null && child.signalCode === null) {
      try {
        child.kill('SIGKILL');
      } catch {
        /* already gone */
      }
    }
    if (!exitedDuringGrace) {
      const exitedAfterKill = await settlesWithin(c.exited, terminateGraceMs);
      // SIGKILL should make the direct child observable promptly. If the host
      // fails to deliver an exit event inside the hard bound, release the host
      // handle as well so an unobservable child cannot pin daemon shutdown.
      if (!exitedAfterKill) child.unref();
    }
    releaseClientProcessHandles(c, child);
  }

  async function closeImpl(): Promise<void> {
    phase = 'closing';
    const reason = 'terminal bridge is closed';
    const ownedClients = [...clients];
    const ownedViewers = [...viewers];
    client = null;

    for (const timer of closeRecheckTimers) clearTimeout(timer);
    closeRecheckTimers.clear();
    for (const timer of attachTimers) clearTimeout(timer);
    attachTimers.clear();
    for (const settle of [...delaySettlers]) settle();

    const inputChains = ownedViewers.map((viewer) => viewer.inputChain);
    for (const c of ownedClients) {
      c.closed = true;
      clearInterval(c.deadTimer);
      c.readyReject(new Error(reason));
      for (const waiter of c.waiters.splice(0)) waiter.reject(new Error(reason));
      // Quiesce output/log callbacks immediately; retain only child lifecycle
      // listeners until the process has been joined below.
      c.detachDataListeners();
    }
    for (const viewer of ownedViewers) {
      viewer.finish(reason);
      viewer.pending = [];
      viewer.pendingBytes = 0;
      viewer.pendingExit = null;
    }
    for (const c of ownedClients) {
      c.panes.clear();
      c.manualSizing.clear();
    }

    await Promise.all([
      ...inputChains.map((chain) => chain.catch(() => {})),
      ...ownedClients.map((c) => terminateClient(c)),
    ]);
    for (const viewer of ownedViewers) {
      viewer.queuedInput = 0;
      viewer.inputChain = Promise.resolve();
    }
    clients.clear();
    phase = 'closed';
  }

  /** Permanently quiesce the bridge and join every control child. The exact
   * Promise is memoized so concurrent/double close shares one release. */
  function close(): Promise<void> {
    if (closePromise) return closePromise;
    // Latch admission synchronously, but defer release work until after the
    // memoized Promise has been published. viewer.finish() invokes arbitrary
    // onClose code, which is allowed to re-enter close() and must receive this
    // exact same Promise rather than start a second release.
    phase = 'closing';
    closePromise = Promise.resolve().then(closeImpl);
    return closePromise;
  }

  return { openViewer, close };
}
