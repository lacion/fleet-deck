import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createTermBridge } from '../scripts/fleetd/termbridge.ts';

// BUG-056: tmux flushes `%end …\n%output %N <bytes>\n` for a capture-pane in
// ONE stdout write. The bridge's stdout handler resolves the capture waiter and
// still processes that trailing %output in the same feed() loop — and a Promise
// continuation is a microtask, so it always runs AFTER the current stack. A
// viewer that subscribed only once its `await capture-pane` returned was
// therefore guaranteed to miss every same-chunk post-capture byte: the pane was
// not yet in c.panes, so the demux dropped it. helpers/term-capture-race-fixture.ts
// appends `%output %1 after-capture` to the capture-pane response block,
// reproducing that single-write flush exactly.
const HERE = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(HERE, 'helpers/term-capture-race-fixture.ts');
try { chmodSync(FIXTURE, 0o755); } catch { /* best effort */ }

test('BUG-056: %output flushed in the capture-pane chunk is replayed after the init, not dropped', async t => {
  const PREV_TERM_CMD = process.env.FLEETDECK_TERM_CMD;
  process.env.FLEETDECK_TERM_CMD = FIXTURE;
  t.after(() => {
    if (PREV_TERM_CMD === undefined) delete process.env.FLEETDECK_TERM_CMD;
    else process.env.FLEETDECK_TERM_CMD = PREV_TERM_CMD;
  });

  const bridge = createTermBridge({
    port: 21999,
    resolveSpawn: async () => ({ status: 'live', tmux_session: 'fleetdeck-21999', tmux_window: 'fd21999-viper' }),
  });
  const frames = [];
  const handle = await bridge.openViewer({
    spawn_id: 'sp_test',
    cols: 80,
    rows: 24,
    send: (frame) => frames.push(frame),
  });
  t.after(() => handle.close());

  const init = frames.find(f => f.t === 'init');
  assert.ok(init, 'the init frame must ship');
  assert.ok(init.screen.includes('seed %1'), 'the captured seed is in the init frame');
  assert.deepEqual(
    frames.filter(f => f.t === 'out').map(f => f.data),
    ['after-capture'],
    'same-chunk post-capture %output must be buffered at subscribe time and replayed after the init frame',
  );
});
