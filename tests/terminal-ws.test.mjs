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

function records(file) {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
}

async function waitUntil(fn, label, timeoutMs = 6000) {
  const deadline = Date.now() + timeoutMs;
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

async function createSpawn(daemon, cwd) {
  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'terminal test' });
  assert.equal(res.status, 200, res.text);
  return res.json.spawn_id;
}

test('live terminal WS seeds, streams, relays hex input/resize, and kills its control fixture', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });
  const spawnId = await createSpawn(daemon, dir);
  const { ws, frames } = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=${spawnId}&cols=90&rows=30`);

  const init = await waitUntil(() => frames.find(frame => frame.t === 'init'), 'init frame');
  assert.deepEqual({ t: init.t, cols: init.cols, rows: init.rows }, { t: 'init', cols: 90, rows: 30 });
  // The seed clears first — a viewer must never inherit stale cells — and
  // ends by parking the cursor where the pane's cursor actually sits.
  assert.equal(init.screen, '\u001b[H\u001b[2Jseed \u001b[31mred\u001b[0m\u001b[4;3H');
  const out = await waitUntil(() => frames.find(frame => frame.t === 'out'), 'output frame');
  assert.equal(out.data, 'live\u001b[32m!\u001b[0m');

  ws.send(JSON.stringify({ t: 'in', data: 'A\u0003é' }));
  ws.send(JSON.stringify({ t: 'resize', cols: 101, rows: 41 }));
  await waitUntil(() => records(record).some(r => r.line === 'send-keys -t %1 -H 41 03 c3 a9'), 'hex send-keys');
  await waitUntil(() => records(record).some(r => r.line === 'refresh-client -C 101,41'), 'resize command');
  ws.close();
  await waitUntil(() => records(record).some(r => r.type === 'signal' && r.signal === 'SIGTERM'), 'fixture SIGTERM');
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

test('live terminal WS returns err for unknown spawn when enabled and enforces viewer cap', async t => {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-term-cap-'));
  const record = path.join(dir, 'term.jsonl');
  const daemon = await startDaemon({ env: env(record, { FLEETDECK_TERM_MAX_VIEWERS: '1' }) });
  t.after(async () => { await daemon.stop(); rmSync(dir, { recursive: true, force: true }); });

  const unknown = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=not-a-spawn&cols=80&rows=24`);
  const missing = await waitUntil(() => unknown.frames.find(frame => frame.t === 'err'), 'unknown-spawn err');
  assert.match(missing.reason, /no such spawn/);

  const spawnId = await createSpawn(daemon, dir);
  const first = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=${spawnId}&cols=80&rows=24`);
  await waitUntil(() => first.frames.find(frame => frame.t === 'init'), 'first viewer init');
  const second = connect(`${daemon.baseUrl.replace('http', 'ws')}/ws/term?spawn=${spawnId}&cols=80&rows=24`);
  const capped = await waitUntil(() => second.frames.find(frame => frame.t === 'err'), 'viewer-cap err');
  assert.match(capped.reason, /cap/);
  first.ws.close();
});
