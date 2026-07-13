import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocket } from 'ws';
import { startDaemon } from './helpers/daemon.mjs';
import { postJson } from './helpers/http.mjs';

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

const WAIT_SCALE = Number(process.env.FLEETDECK_TEST_WAIT_SCALE) || 1;

async function waitUntil(fn, label, timeoutMs = 6000) {
  const deadline = Date.now() + timeoutMs * WAIT_SCALE;
  for (;;) {
    const value = fn();
    if (value) return value;
    if (Date.now() >= deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise(resolve => setTimeout(resolve, 25));
  }
}

function connect(url) {
  const ws = new WebSocket(url);
  const frames = [];
  ws.on('message', raw => { try { frames.push(JSON.parse(raw.toString())); } catch { /* malformed server frame */ } });
  return { ws, frames };
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
  return `${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=${spawnId}&cols=${cols}&rows=${rows}`;
}

test('live terminal WS seeds, streams, relays hex input/resize, and kills its control fixture', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });
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
    () => records(record).some(r => r.line === `resize-window -t =${spawned.tmux.session}:${spawned.tmux.window} -x 101 -y 41`),
    'per-window resize command',
  );
  assert.ok(
    records(record).some(r => r.line === `set-option -w -t =${spawned.tmux.session}:${spawned.tmux.window} window-size manual`),
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
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });

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
      records(record).some(r => r.line === `resize-window -t =${s.tmux.session}:${s.tmux.window} -x ${80 + i} -y ${24 + i}`),
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
    const foreign = tile.frames.filter(f => f.t === 'out' && !f.data.includes(`live ${pane}`));
    assert.deepEqual(foreign, [], `tile ${i} must never receive another pane's output`);
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
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });

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

test('live terminal WS refuses an unknown spawn and honors FLEETDECK_TERM=off', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-refuse-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TERM: 'off' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });
  const unknown = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=not-a-spawn&cols=80&rows=24`);
  const disabled = await waitUntil(() => unknown.frames.find(frame => frame.t === 'err'), 'disabled err');
  assert.match(disabled.reason, /disabled/);
  assert.equal(records(record).length, 0, 'disabled bridge must not launch fixture');
});

test('live terminal WS returns err for an unknown spawn when enabled', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-unknown-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });

  const unknown = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=not-a-spawn&cols=80&rows=24`);
  const missing = await waitUntil(() => unknown.frames.find(frame => frame.t === 'err'), 'unknown-spawn err');
  assert.match(missing.reason, /no such spawn/);
  // An unresolvable spawn must not leave a control client attached behind it.
  assert.equal(records(record).filter(r => r.type === 'start').length, 0,
    'a refused viewer must not have launched a control client');
});
