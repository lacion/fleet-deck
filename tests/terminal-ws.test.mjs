import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocket } from 'ws';
import { startDaemon } from './helpers/daemon.mjs';
import { postJson, getJson } from './helpers/http.mjs';
import { waitUntil as waitUntilBase } from './helpers/wait.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
const TERM_FIXTURE = path.join(HERE, 'helpers/term-cmd-fixture.mjs');
try { chmodSync(SPAWN_FIXTURE, 0o755); chmodSync(TERM_FIXTURE, 0o755); } catch { /* best effort */ }

// Built, never written literally: an ESC in a source string is an invisible
// control character, and this file is full of ANSI expectations.
const ESC = String.fromCharCode(27);
const ETX = String.fromCharCode(3); // ^C — proves control bytes survive the relay

function records(file) {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
}

// Positional-signature adapter over the shared scaled poller: this file's call
// sites pass (fn, label) with an authored 6000ms budget and a 25ms poll.
const waitUntil = (fn, label, timeoutMs = 6000) =>
  waitUntilBase(fn, { label, timeoutMs, intervalMs: 25 });

function connect(url) {
  const ws = new WebSocket(url);
  const frames = [];
  const closes = [];
  ws.on('message', raw => { try { frames.push(JSON.parse(raw.toString())); } catch { /* malformed server frame */ } });
  ws.on('close', (code, reason) => { closes.push({ code, reason: reason?.toString?.() ?? '' }); });
  return { ws, frames, closes };
}

/** The spawn's board row: its column and spawn status, or null if not shown. */
async function spawnRow(daemon, spawnId) {
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const s = state.sessions.find(x => x.spawn?.spawn_id === spawnId);
  return s ? { col: s.col, status: s.spawn.status } : null;
}

function env(record, extra = {}) {
  return {
    FLEETDECK_SPAWN_CMD: SPAWN_FIXTURE,
    FLEETDECK_TERM_CMD: TERM_FIXTURE,
    FLEETDECK_TEST_TERM_RECORD: record,
    FLEETDECK_NUDGE_MS: '60000',
    ...extra,
  };
}

/** Full spawn response — the tmux window name is needed to assert per-window sizing. */
async function createSpawn(daemon, cwd) {
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'terminal test' });
  assert.equal(res.status, 200, res.text);
  return res.json;
}

function termUrl(daemon, spawnId, cols, rows) {
  // 0.16.0: /ws/term requires the bearer at upgrade (?t= carries it for WS).
  return `${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=${spawnId}&cols=${cols}&rows=${rows}&t=${daemon.token}`;
}

test('live terminal WS seeds, streams, relays hex input/resize, and kills its control fixture', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);
  const { ws, frames } = connect(termUrl(daemon, spawned.spawn_id, 90, 30));

  const init = await waitUntil(() => frames.find(frame => frame.t === 'init'), 'init frame');
  assert.deepEqual({ t: init.t, cols: init.cols, rows: init.rows }, { t: 'init', cols: 90, rows: 30 });
  // The seed clears first — a viewer must never inherit stale cells — and
  // ends by parking the cursor where the pane's cursor actually sits.
  assert.equal(init.screen, `${ESC}[H${ESC}[2Jseed %1 ${ESC}[31mred${ESC}[0m${ESC}[4;3H`);
  const out = await waitUntil(() => frames.find(frame => frame.t === 'out'), 'output frame');
  assert.equal(out.data, `live %1${ESC}[32m!${ESC}[0m`);

  ws.send(JSON.stringify({ t: 'in', data: `A${ETX}é` }));
  ws.send(JSON.stringify({ t: 'resize', cols: 101, rows: 41 }));
  await waitUntil(() => records(record).some(r => r.line === 'send-keys -t %1 -H 41 03 c3 a9'), 'hex send-keys');

  // Shift+Enter → a newline in the agent's composer, not a submit. The board
  // sends ESC CR (1b 0d) — the sequence Claude Code's own /terminal-setup asks
  // terminals to bind — and the bridge must relay those two bytes untouched.
  // Verified against a real Claude TUI: it splits the line and does not submit.
  ws.send(JSON.stringify({ t: 'in', data: `${ESC}\r` }));
  await waitUntil(() => records(record).some(r => r.line === 'send-keys -t %1 -H 1b 0d'), 'ESC CR relayed as bytes');

  // v1.9: geometry is set on the WINDOW, not on the client. `refresh-client -C`
  // sized whoever was attached — which is precisely what made N tiles fight over
  // one pane's shape. `resize-window` under `window-size manual` does not.
  await waitUntil(
    () => records(record).some(r => r.line === `resize-window -t =${spawned.tmux.session}:=${spawned.tmux.window} -x 101 -y 41`),
    'per-window resize command',
  );
  assert.ok(
    records(record).some(r => r.line === `set-option -w -t =${spawned.tmux.session}:=${spawned.tmux.window} window-size manual`),
    'the window must be put in manual sizing, or tmux re-derives its size from whatever clients are attached',
  );
  // `-g` would reach across the whole tmux server and resize the human's OWN
  // sessions. The option is set on our window or not at all.
  assert.equal(records(record).filter(r => / -g .*window-size|window-size.* -g /.test(r.line || '')).length, 0,
    'window-size must never be set globally — that is the user\'s tmux, not ours');

  ws.close();
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'fixture SIGTERM');
});

test('grid: many viewers share ONE control client, each sized and streamed independently', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-grid-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // Six tiles — past the old FLEETDECK_TERM_MAX_VIEWERS default of 4, which used
  // to refuse the 5th outright.
  const spawns = [];
  for (let i = 0; i < 6; i++) spawns.push(await createSpawn(daemon, dir));

  // Deliberately DIFFERENT geometry per tile. The whole point of per-window
  // sizing is that these no longer overwrite one another.
  const tiles = spawns.map((s, i) => connect(termUrl(daemon, s.spawn_id, 80 + i, 24 + i)));
  t.after(() => { for (const tile of tiles) tile.ws.close(); });

  for (const [i, tile] of tiles.entries()) {
    const init = await waitUntil(() => tile.frames.find(f => f.t === 'init'), `tile ${i} init`);
    assert.equal(init.cols, 80 + i, `tile ${i} must keep its own width`);
    assert.equal(init.rows, 24 + i, `tile ${i} must keep its own height`);
  }

  // THE headline: one tmux process for the whole grid. It used to be one per
  // viewer, each parsing every agent's output in order to keep a sixth of it.
  const pids = new Set(records(record).filter(r => r.type === 'start').map(r => r.pid));
  assert.equal(pids.size, 1, `the grid must share a single control client, saw ${pids.size}`);

  // Each tile sized its OWN window.
  for (const [i, s] of spawns.entries()) {
    assert.ok(
      records(record).some(r => r.line === `resize-window -t =${s.tmux.session}:=${s.tmux.window} -x ${80 + i} -y ${24 + i}`),
      `tile ${i} should have sized its own window ${s.tmux.window}`,
    );
  }

  // ...and the demuxing is honest: each tile sees ITS pane and nobody else's.
  // A leak here would paint one agent's screen inside another agent's tile.
  //
  // Which pane a window got is whatever the fixture assigned (first-seen order,
  // and six viewers connect at once) — so read the mapping rather than assume
  // tile i is pane %i+1. That assumption is what the first draft of this test
  // got wrong; the demuxing was right all along.
  const paneOf = new Map(records(record).filter(r => r.type === 'pane').map(r => [r.window, r.pane]));
  assert.equal(paneOf.size, 6, 'each of the 6 windows should have been resolved to its own pane');
  assert.equal(new Set(paneOf.values()).size, 6, 'the 6 panes must be distinct');

  for (const [i, tile] of tiles.entries()) {
    const pane = paneOf.get(spawns[i].tmux.window);
    const out = await waitUntil(() => tile.frames.find(f => f.t === 'out'), `tile ${i} output`);
    assert.ok(out.data.includes(`live ${pane}`),
      `tile ${i} must receive its own pane (${pane}), got ${JSON.stringify(out.data)}`);
    // BUG-180: the old predicate counted a frame as safe whenever it contained
    // the own marker — so a demux regression that sent one tile a MIXED frame
    // (its own pane's marker AND another pane's) passed isolation. Assert the
    // strict form instead: every output frame must exclude every OTHER pane's
    // marker, own-marker frames included.
    const others = [...paneOf.values()].filter(p => p !== pane);
    const mixed = tile.frames.filter(f => f.t === 'out' && others.some(p => f.data.includes(`live ${p}`)));
    assert.deepEqual(mixed.map(f => f.data), [],
      `tile ${i} (pane ${pane}) received another pane's output, possibly in a mixed frame`);

  // The predicate itself: a frame mixing the own marker with a foreign one must
  // be condemned. Simulating the exact regression (demux sends tile 0
  // "live %1 ... live %2") proves the new assertion fails where the old one
  // passed silently.
  {
    const pane = paneOf.get(spawns[0].tmux.window);
    const others = [...paneOf.values()].filter(p => p !== pane);
    const mixedFrame = { t: 'out', data: `live ${pane} ... live ${others[0]}` };
    const condemned = [mixedFrame].filter(f => f.t === 'out' && others.some(p => f.data.includes(`live ${p}`)));
    assert.equal(condemned.length, 1, 'a mixed own+foreign frame must be condemned by the new predicate');
    const oldPredicateMisses = [mixedFrame].filter(f => f.t === 'out' && !f.data.includes(`live ${pane}`));
    assert.equal(oldPredicateMisses.length, 0, 'the OLD predicate would have accepted this mixed frame — that was BUG-180');
  }
  }

  // Closing one tile must not take the shared client — and everyone else — down.
  tiles[0].ws.close();
  await new Promise(r => setTimeout(r, 300));
  assert.equal(records(record).filter(r => r.type === 'signal').length, 0,
    'the shared client must survive one viewer leaving while others are still watching');
});

test('the shared control client is released once the last viewer leaves', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-release-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const a = await createSpawn(daemon, dir);
  const b = await createSpawn(daemon, dir);
  const first = connect(termUrl(daemon, a.spawn_id, 80, 24));
  const second = connect(termUrl(daemon, b.spawn_id, 80, 24));
  await waitUntil(() => first.frames.find(f => f.t === 'init'), 'first init');
  await waitUntil(() => second.frames.find(f => f.t === 'init'), 'second init');

  first.ws.close();
  await new Promise(r => setTimeout(r, 250));
  assert.equal(records(record).filter(r => r.type === 'signal').length, 0, 'one viewer left, one remains: keep the client');

  // A fleet nobody is watching should hold no tmux control attach at all.
  second.ws.close();
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'client released on last viewer');
});

<<<<<<< /tmp/mf-ours
// BUG-055: a remain-on-exit pane whose process has exited emits NO
// %window-close, still answers list-panes, and still answers send-keys with ok
// — so neither of the bridge's old death signals fired and an open viewer
// stayed 'live' on a dead pane forever, silently discarding keystrokes. The
// bridge must poll #{pane_dead} for its subscribed panes and end the viewer.
// Here every pane reports pane_dead=1 from the first poll: the viewer opens
// (the row still says live — the ~10s liveness tick hasn't reconciled), and
// the very next dead-poll tick must end it with {t:'exit'}.
test('live terminal WS ends the viewer when its pane is dead under remain-on-exit (BUG-055)', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-dead-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({
    env: env(record, {
      FLEETDECK_TEST_TERM_DEAD_PANE: '*',       // every fixture pane is dead
      FLEETDECK_TERM_DEAD_POLL_MS: '100',       // don't sit through the 5s default
=======
// BUG-158: %output that lands between subscribe and the init frame is buffered
// in viewer.pending. That buffer is BYTE-BOUNDED like the keystroke queue — a
// pane flooding past FLEETDECK_TERM_PENDING_MAX_BYTES before its init ships is
// finished for a resync (exit frame), never hoarded unbounded in daemon memory.
test('pre-init output past the pending byte bound resyncs the viewer instead of buffering unbounded', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-flood-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({
    env: env(record, {
      FLEETDECK_TERM_PENDING_MAX_BYTES: '4096',
      FLEETDECK_TEST_TERM_PREINIT_FLOOD: '65536',
>>>>>>> /tmp/mf-theirs
    }),
  });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);
<<<<<<< /tmp/mf-ours
  const { ws, frames, closes } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));

  await waitUntil(() => frames.find(f => f.t === 'init'), 'init frame');
  const exit = await waitUntil(() => frames.find(f => f.t === 'exit'), 'exit frame from the pane_dead poll');
  assert.match(exit.reason, /pane closed/);
  assert.equal(frames.some(f => f.t === 'err'), false,
    'a dead pane is the agent ending, never "viewer refused"');
  await waitUntil(() => closes.length > 0, 'socket close');
  assert.ok([1000, 1005].includes(closes[0].code), `clean close, got ${closes[0].code}`);
  assert.equal(ws.readyState, WebSocket.CLOSED);

  // send-keys success is not liveness proof — but the poll must already have
  // finished the viewer, so nothing may ever reach the (dead) pane.
  assert.equal(records(record).filter(r => /send-keys/.test(r.line || '')).length, 0);
  // The viewer's end is also the last viewer leaving: the shared control
  // client is handed back rather than pinned on a dead fleet.
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'),
    'control client released once the dead-pane viewer ended');
});

// The same poll must NOT kill a viewer on a LIVE pane: a dead read is only
// acted on when pane_dead is exactly 1, so a healthy pane's viewer survives.
test('live terminal WS keeps the viewer open while pane_dead is 0 (BUG-055)', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-alive-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TERM_DEAD_POLL_MS: '100' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);
  const { ws, frames } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));

  await waitUntil(() => frames.find(f => f.t === 'init'), 'init frame');
  // Several poll intervals pass with pane_dead=0: the viewer must stay live.
  await waitUntil(
    () => records(record).filter(r => /list-panes -a/.test(r.line || '')).length >= 3,
    'several dead-poll rounds',
  );
  assert.equal(frames.some(f => f.t === 'exit'), false, 'a live pane must never end its viewer');
  assert.equal(frames.some(f => f.t === 'err'), false);
  ws.close();
=======

  const { frames, closes } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  const exit = await waitUntil(() => frames.find(f => f.t === 'exit'), 'exit frame');
  assert.match(exit.reason, /output overflow before init/);
  assert.equal(frames.some(f => f.t === 'init'), false,
    'a flooded pre-init buffer must not ship a seed it can no longer replay');
  await waitUntil(() => closes.length > 0, 'socket close');
>>>>>>> /tmp/mf-theirs
});

test('live terminal WS refuses an unknown spawn and honors FLEETDECK_TERM=off', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-refuse-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TERM: 'off' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const unknown = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=not-a-spawn&cols=80&rows=24&t=${daemon.token}`);
  const disabled = await waitUntil(() => unknown.frames.find(frame => frame.t === 'err'), 'disabled err');
  assert.match(disabled.reason, /disabled/);
  assert.equal(records(record).length, 0, 'disabled bridge must not launch fixture');
});

test('live terminal WS returns err for an unknown spawn when enabled', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-unknown-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const unknown = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=not-a-spawn&cols=80&rows=24&t=${daemon.token}`);
  const missing = await waitUntil(() => unknown.frames.find(frame => frame.t === 'err'), 'unknown-spawn err');
  assert.match(missing.reason, /no such spawn/);
  // An unresolvable spawn must not leave a control client attached behind it.
  assert.equal(records(record).filter(r => r.type === 'start').length, 0,
    'a refused viewer must not have launched a control client');
});

<<<<<<< /tmp/mf-ours
<<<<<<< /tmp/mf-ours
// BUG-157: a watched window dies, tmux emits %window-close, and the bridge's
// list-panes -a probe FAILS (%error — a control-client blip). The old code
// swallowed that failure, and an idle viewer with no input in flight had no
// other path that could observe the pane's death: it stayed in `viewers`,
// pinning the shared control client until the WS keepalive or a later command
// timeout. The bridge must re-list once after a settle delay; the fixture
// (FLEETDECK_TEST_TERM_PROBE_FAILS=1) fails only the FIRST probe and answers
// the recheck with an empty pane list — proof the pane is gone.
test('a failed window-close probe is re-listed once so an idle viewer on a dead pane still finishes', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-recheck-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({
    env: env(record, {
      FLEETDECK_TEST_TERM_PROBE_FAILS: '1',
      FLEETDECK_TERM_CLOSE_RECHECK_MS: '200',
    }),
  });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const spawned = await createSpawn(daemon, dir);
  const { ws, frames, closes } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  t.after(() => { if (ws.readyState !== WebSocket.CLOSED) ws.close(); });
  await waitUntil(() => frames.find(f => f.t === 'init'), 'init frame');

  // The viewer is now IDLE — no input, no resize. Only the window-close
  // recheck can release it. %window-close's window id is made up; the probe
  // answers by pane id, so the id itself is never consulted.
  const procs = records(record).filter(r => r.type === 'start');
  assert.equal(procs.length, 1, 'one shared control client');
  const fixture = procs[0].pid;
  process.kill(fixture, 'SIGUSR1');
  await waitUntil(() => records(record).some(r => r.type === 'window-close'), 'fixture emitted %window-close');

  const exit = await waitUntil(() => frames.find(f => f.t === 'exit'), 'exit frame');
  assert.match(exit.reason, /pane closed/);
  await waitUntil(() => closes.length > 0, 'socket close');

  // The first probe failed, so this exit could only have come from a SECOND
  // list-panes -a — the recheck — answering with the pane gone.
  const probes = records(record).filter(r => /^list-panes -a /.test(r.line || ''));
  assert.ok(probes.length >= 2, `the failed probe must be re-listed, saw ${probes.length}`);

  // And releasing the last viewer hands the control client back: a fleet
  // nobody is watching holds no tmux attach.
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'client released on last viewer');
});
=======
// BUG-159: the open-time SIGWINCH jiggle is three resize steps — size(rows),
// size(rows-1), size(rows) — and only the first used to be checked. A failed
// restore after a successful rows-1 step left the window SHORT while the init
// frame advertised the requested rows: the client laid the shorter seed into
// the wrong geometry and the first paint came out scrambled. Now a failed mid
// or restore step aborts the open BEFORE capture — the client gets {t:'err'},
// never a mismatched init — and the bridge makes a best-effort restore to the
// requested rows (tmux 3.7b keeps the prior geometry on a failed resize).
// FLEETDECK_TEST_TERM_FAIL_RESIZE makes the fixture fail exactly one jiggle step.
for (const step of ['mid', 'restore']) {
  test(`live terminal WS aborts the open when the jiggle's ${step} resize step fails`, async t => {
    const dir = mkdtempSync(path.join(tmpdir(), `fleetdeck-term-jiggle-${step}-`));
    const record = path.join(dir, 'term.jsonl');
    const daemon = await startDaemon({ env: env(record, { FLEETDECK_TEST_TERM_FAIL_RESIZE: step }) });
    t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
    const spawned = await createSpawn(daemon, dir);

    const { frames } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
    const err = await waitUntil(() => frames.find(f => f.t === 'err'), 'resize-failure err');
    assert.match(err.reason, /resize failed/);
    assert.equal(frames.some(f => f.t === 'init'), false,
      'a failed jiggle step must never ship an init whose rows the window does not have');
    assert.equal(records(record).filter(r => (r.line || '').startsWith('capture-pane ')).length, 0,
      'the seed must not be captured from a window that failed to reach the requested size');

    // Best-effort recovery: whatever failed, the LAST resize on this window is
    // a restore attempt to the requested geometry, so the pane is not left
    // parked at rows-1 for the next viewer (or the human's own client).
    const window = spawned.tmux.window;
    await waitUntil(() => {
      const resizes = records(record)
        .filter(r => (r.line || '').startsWith(`resize-window -t =${spawned.tmux.session}:=${window} `));
      return resizes.length > 0 && resizes.at(-1).line.endsWith('-x 80 -y 24');
    }, 'final resize restores the requested rows');
  });
}
>>>>>>> /tmp/mf-theirs
=======
// BUG-165: the fixture's bare-success default branch used to hide every
// lifecycle edge — these fault knobs (see term-cmd-fixture.mjs) open them.

test('live terminal WS: failed resize refuses the open (FAIL_RESIZE fault)', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-resize-fail-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TEST_TERM_FAIL_RESIZE: '1' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);

  const { frames, closes } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  const err = await waitUntil(() => frames.find(f => f.t === 'err'), 'resize-failure err frame');
  assert.match(err.reason, /resize failed/);
  assert.equal(frames.some(f => f.t === 'init'), false, 'a failed resize must never ship an init frame');
  await waitUntil(() => closes.length > 0, 'socket close after refused open');

  // A refused open must release the shared client too — the fleet is unwatched.
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'client released after failed open');
});

test('live terminal WS: send-keys %error finishes the viewer (DEAD_PANE fault)', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-sendkeys-dead-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TEST_TERM_DEAD_PANE: '1' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);
  const { ws, frames } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  await waitUntil(() => frames.find(f => f.t === 'init'), 'init frame');

  // The pane died under an established viewer: tmux refuses the keystroke with
  // %error and the bridge must end the viewer, not swallow it silently.
  ws.send(JSON.stringify({ t: 'in', data: 'x' }));
  const exit = await waitUntil(() => frames.find(f => f.t === 'exit'), 'exit frame after dead-pane input');
  assert.match(exit.reason, /pane closed/);
});

test('live terminal WS: wedged control reply trips the command deadline and tears the client down (HANG fault)', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-hang-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({
    env: env(record, {
      // Every reply arrives 800ms late; the bridge's per-command deadline is
      // 300ms (M-R5). A wedged reply must NOT hang the viewer open forever:
      // the deadline tears the WHOLE client down (a late reply would resolve
      // the wrong FIFO waiter), rejecting every waiter with the timeout —
      // which surfaces as an open refusal here — and SIGTERMing the fixture,
      // so nothing stays pinned on the shared client.
      FLEETDECK_TEST_TERM_HANG_MS: '800',
      FLEETDECK_TERM_CMD_TIMEOUT_MS: '300',
      FLEETDECK_TERM_REPAINT_MS: '1',
    }),
  });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);

  const { frames, closes } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  const err = await waitUntil(() => frames.find(f => f.t === 'err'), 'wedged command refusal');
  assert.match(err.reason, /timed out/);
  assert.equal(frames.some(f => f.t === 'init'), false, 'a wedged seed must never ship an init frame');
  await waitUntil(() => closes.length > 0, 'socket close after wedged open');

  // The wedged client is killed, not left attached behind the refused viewer.
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'wedged client SIGTERMd by teardown');
  // And the bridge recovers: the next open gets a fresh client, whose own
  // first command wedges and dies the same way — proving teardown creates no
  // stuck global state.
  const again = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  t.after(() => again.ws.close());
  await waitUntil(() => records(record).filter(r => r.type === 'start').length >= 2, 'fresh client for the next viewer');
  const retry = await waitUntil(() => again.frames.find(f => f.t === 'err'), 'retried open also wedged');
  assert.match(retry.reason, /timed out/);
});

test('live terminal WS: input past the queue bound evicts the viewer (overflow)', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-overflow-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({
    env: env(record, {
      // The queue bound is 1024 (its floor); the fixture never answers
      // send-keys, so the input chain never drains and the second paste lands
      // on a still-full queue — it must evict the viewer rather than pile
      // send-keys commands into the control client.
      FLEETDECK_TERM_INPUT_MAX_BYTES: '1024',
      FLEETDECK_TEST_TERM_HANG_MS: 'send-keys:60000',
    }),
  });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);
  const { ws, frames } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  await waitUntil(() => frames.find(f => f.t === 'init'), 'init frame');

  ws.send(JSON.stringify({ t: 'in', data: 'x'.repeat(1024) })); // fills the bound, never acked
  ws.send(JSON.stringify({ t: 'in', data: 'y' }));              // one byte past it → evict
  const exit = await waitUntil(() => frames.find(f => f.t === 'exit'), 'overflow eviction exit');
  assert.match(exit.reason, /input overflow/);
});

test('live terminal WS: %window-close with the pane dead finishes exactly that viewer', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-winclose-'));
  const record = path.join(dir, 'term.jsonl');
  // CLOSE_WINDOW alone must be survivable (a close the probe clears is not
  // ours); paired with DEAD_PANE the probe finds no pane alive and condemns.
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TEST_TERM_CLOSE_WINDOW: '1' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const spawned = await createSpawn(daemon, dir);
  const tile = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
  t.after(() => tile.ws.close());
  await waitUntil(() => tile.frames.find(f => f.t === 'init'), 'init frame');

  // The probe reports every pane alive (no DEAD_PANE knob): the close was some
  // other window's, and this viewer must ride through it untouched.
  await new Promise(r => setTimeout(r, 400));
  assert.equal(tile.frames.some(f => f.t === 'exit'), false, 'a cleared %window-close must not finish the viewer');
  assert.equal(tile.closes.length, 0, 'a cleared %window-close must not close the socket');
  tile.ws.close();
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'client released after viewer leaves');

  // Now the pane really is dead: the same %window-close probe finds nothing
  // alive and the viewer must be told its pane closed.
  const dead = await startDaemon({ env: env(path.join(dir, 'term-dead.jsonl'), { FLEETDECK_TEST_TERM_CLOSE_WINDOW: '1', FLEETDECK_TEST_TERM_DEAD_PANE: '1' }) });
  t.after(() => dead.stop());
  const deadSpawn = await createSpawn(dead, dir);
  const dying = connect(termUrl(dead, deadSpawn.spawn_id, 80, 24));
  await waitUntil(() => dying.frames.find(f => f.t === 'init'), 'dead-pane init frame');
  const exit = await waitUntil(() => dying.frames.find(f => f.t === 'exit'), 'window-close exit frame');
  assert.match(exit.reason, /pane closed/);
});
>>>>>>> /tmp/mf-theirs

// Item 6: the row said live but its pane was already gone (the agent ended
// between the ~10s liveness tick and this open). A vanished pane is the agent
// ENDING, not a viewer fault — so the client must receive {t:'exit'}
// ("agent ended — …"), never the alarming {t:'err'} ("viewer refused: …").
// FLEETDECK_TEST_TERM_NO_PANE makes the fixture's per-window pane lookup report
// the window as gone: 'error' fails list-panes (%error, termbridge.mjs:437),
// 'empty' returns no pane id (termbridge.mjs:439). Both throws now carry gone.
for (const mode of ['error', 'empty']) {
  test(`live terminal WS reports a vanished pane as EXIT, not a refusal (${mode} lookup)`, async t => {
    const dir = mkdtempSync(path.join(tmpdir(), `fleetdeck-term-gone-${mode}-`));
    const record = path.join(dir, 'term.jsonl');
    const daemon = await startDaemon({ env: env(record, { FLEETDECK_TEST_TERM_NO_PANE: mode }) });
    t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
    const spawned = await createSpawn(daemon, dir);

    // The row is live-eligible before the open — so the viewer gets PAST the
    // ACTIVE_STATUSES gate and genuinely exercises the vanished-pane path.
    const before = await spawnRow(daemon, spawned.spawn_id);
    assert.ok(before, 'sanity: the spawn is on the board');
    assert.ok(['spawning', 'stalled', 'live'].includes(before.status),
      `sanity: the row must be live-eligible, saw ${before.status}`);

    const { ws, frames, closes } = connect(termUrl(daemon, spawned.spawn_id, 80, 24));
    const exit = await waitUntil(() => frames.find(f => f.t === 'exit'), 'exit frame');
    assert.match(exit.reason, /pane is gone|agent has ended/);
    assert.equal(frames.some(f => f.t === 'err'), false,
      'a vanished pane must never surface as "viewer refused"');

    // The socket closes cleanly (a normal close, not a 1006 abnormal drop).
    await waitUntil(() => closes.length > 0, 'socket close');
    assert.ok([1000, 1005].includes(closes[0].code), `clean close, got ${closes[0].code}`);
    assert.equal(ws.readyState, WebSocket.CLOSED);

    // The viewer failure fires a fire-and-forget liveness reconcile but must NOT
    // itself condemn the row: window-absence is UNKNOWN by house doctrine, so
    // only the tick condemns — and here it has no matching tmux window to act on
    // (the fixture spawns no real window). Give the kicked tick time to land,
    // then prove the status the row had before is the status it still has.
    await new Promise(r => setTimeout(r, 400));
    const after = await spawnRow(daemon, spawned.spawn_id);
    assert.deepEqual(after, before, 'the viewer failure must not flip the spawn row');
  });
}
