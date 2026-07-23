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
