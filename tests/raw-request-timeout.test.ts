// tests/raw-request-timeout.test.mjs
//
// BUG-162 — the raw node:http audit helpers (gateway/hook-auth/lan-auth, now
// routed through helpers/http.ts rawRequest) carried no request or socket
// deadline: a route that accepts the connection but never finishes the
// response would hang the whole test process until the outer CI timeout,
// turning a small route hang into a long opaque stall. rawRequest must
// destroy the request and reject with route diagnostics on a scaled deadline.

import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';

import { rawRequest } from './helpers/http.ts';

function hangingServer() {
  return new Promise(resolve => {
    // Accept the connection, never write a response byte.
    const server = createServer(() => { /* hang forever */ });
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

test('rawRequest rejects on a hung response instead of blocking until the CI timeout', async t => {
  const server = await hangingServer();
  t.after(() => new Promise(r => server.close(r)));
  const { port } = server.address();

  const started = Date.now();
  await assert.rejects(
    rawRequest({ port, path: '/state', method: 'GET', timeout: 150 }),
    /raw GET \/state timed out/,
  );
  // 10s of headroom: the deadline is scaled by FLEETDECK_TEST_WAIT_SCALE, so
  // slow-lane runs legitimately take longer — but never anywhere near the
  // minutes-long outer CI timeout the old helpers fell through to.
  assert.ok(Date.now() - started < 10_000, 'rejects near the authored deadline, not the CI timeout');
});
