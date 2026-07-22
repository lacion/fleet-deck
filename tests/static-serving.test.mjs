// tests/static-serving.test.mjs
//
// Phase 5 static-serving contract:
//   1. GET /           → the built React board (board-dist/index.html)
//   2. GET /assets/*   → hashed build assets with correct MIME types
//   3. traversal attempts (raw and percent-encoded) → 404, never file leaks
//   4. regression: /state and a hook endpoint still behave as before
//
// The dist under scripts/fleetd/board-dist is COMMITTED, so these tests run
// against the real files the daemon ships with.

import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { startDaemon, REPO_ROOT } from './helpers/daemon.mjs';
import { getJson, postHook } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

const BOARD_DIST = path.join(REPO_ROOT, 'scripts/fleetd/board-dist');

/**
 * fetch() normalizes '..' out of the URL before the request ever leaves the
 * client, so traversal attempts must go out on a raw socket. Returns
 * { status, body } for a GET with the path sent EXACTLY as given.
 */
function rawGet(port, rawPath, { timeout = 5000 } = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: '127.0.0.1', port, path: rawPath, method: 'GET' },
      res => {
        let body = '';
        res.on('data', d => { body += d; });
        res.on('end', () => resolve({ status: res.statusCode, body }));
      },
    );
    req.setTimeout(timeout, () => req.destroy(new Error('raw GET timed out')));
    req.on('error', reject);
    req.end();
  });
}

async function getText(url) {
  const res = await fetch(url, { signal: AbortSignal.timeout(5000) });
  return { status: res.status, type: res.headers.get('content-type') || '', text: await res.text() };
}

test('static serving: board, assets, traversal, API regression', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  let indexHtml = '';

  await t.test('GET / serves board-dist/index.html', async () => {
    const res = await getText(daemon.baseUrl + '/');
    assert.equal(res.status, 200);
    assert.match(res.type, /text\/html/);
    assert.equal(res.text, readFileSync(path.join(BOARD_DIST, 'index.html'), 'utf8'));
    assert.match(res.text, /<div id="root">/);
    indexHtml = res.text;
  });

  await t.test('GET /assets/* serves the hashed build assets with correct MIME types', async () => {
    const js = /\/assets\/[^"']+\.js/.exec(indexHtml)?.[0];
    const css = /\/assets\/[^"']+\.css/.exec(indexHtml)?.[0];
    assert.ok(js, 'index.html references a JS asset');
    assert.ok(css, 'index.html references a CSS asset');

    const jsRes = await getText(daemon.baseUrl + js);
    assert.equal(jsRes.status, 200);
    assert.match(jsRes.type, /text\/javascript/);
    assert.equal(jsRes.text, readFileSync(path.join(BOARD_DIST, js.slice(1)), 'utf8'));

    const cssRes = await getText(daemon.baseUrl + css);
    assert.equal(cssRes.status, 200);
    assert.match(cssRes.type, /text\/css/);
    // the token sheet must have made it into the bundle (design fidelity canary)
    assert.match(cssRes.text, /--act:\s*#F0A63C/i);
    assert.match(cssRes.text, /\[data-theme=["']?light/i);
  });

  await t.test('missing asset is a 404, not a directory listing or crash', async () => {
    const res = await getText(daemon.baseUrl + '/assets/no-such-file.js');
    assert.equal(res.status, 404);
  });

  await t.test('path traversal attempts 404 and leak nothing', async () => {
    // raw '..' — reaches the server verbatim only via a raw socket
    const attempts = [
      '/assets/../fleetd.mjs',
      '/assets/../../../package.json',
      '/assets/..%2f..%2ffleetd.mjs',          // encoded slash: decoded AFTER route match
      '/assets/%2e%2e/%2e%2e/package.json',    // encoded dots
      '/assets/..%5c..%5cfleetd.mjs',          // encoded backslash
      '/%2e%2e/fleetd.mjs',
      '/assets/%c0%ae%c0%ae/fleetd.mjs',       // invalid UTF-8 percent sequence
    ];
    for (const p of attempts) {
      const res = await rawGet(daemon.port, p);
      assert.equal(res.status, 404, `expected 404 for ${p}, got ${res.status}`);
      assert.ok(!res.body.includes('fleetd —'), `body for ${p} must not leak daemon source`);
      assert.ok(!res.body.includes('"name": "fleetdeck"'), `body for ${p} must not leak package.json`);
    }
    // sibling files of board-dist (same directory as http.mjs) stay unreachable
    const direct = await rawGet(daemon.port, '/assets/../fleetd.mjs');
    assert.equal(direct.status, 404);
  });

  await t.test('regression: /state and hook endpoints still behave', async () => {
    const sid = randomUUID();
    const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-static-cwd-'));
    t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

    const hook = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
    assert.equal(hook.status, 200);
    assert.equal(hook.json.ok, true);
    assert.ok(hook.json.callsign, 'SessionStart still assigns a callsign');

    const state = await getJson(daemon.baseUrl + '/state');
    assert.equal(state.status, 200);
    assert.ok(Array.isArray(state.json.sessions));
    assert.ok(state.json.sessions.some(s => s.session_id === sid), 'new session visible in /state');
    assert.ok(Array.isArray(state.json.questions), '/state carries questions');
  });
});
