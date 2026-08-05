// tests/security-headers.test.mjs
//
// Response-header hardening on the board surface:
//   - X-Content-Type-Options: nosniff on EVERY response the daemon sends (the
//     static assets AND the JSON API), added once in the two central helpers
//     (serveBoardAsset / json) so no route can forget it.
//   - Content-Security-Policy on the HTML shell ONLY — the one response a browser
//     parses as a document. Subresources (hashed JS/CSS) inherit the document's
//     policy, so they carry nosniff but no CSP of their own.
//
// The exact CSP string is pinned: it is calibrated to board-dist/index.html
// (IBM Plex from fonts.googleapis.com + fonts.gstatic.com, a data: SVG favicon,
// blob: paste images, React inline style attributes) and a silent drift would
// either break the board or widen the policy.

import test from 'node:test';
import assert from 'node:assert/strict';
import { startDaemon } from './helpers/daemon.mjs';

// 0.16.0: connect-src drops the bare ws:/wss: wildcards — every board
// WebSocket is same-origin, which 'self' already covers, so the wildcards only
// ever widened exfiltration for injected JS.
const EXPECTED_CSP = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; connect-src 'self'; img-src 'self' data: blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

// 0.19.2: index.html is the ONLY thing that names the current asset
// fingerprints; if a browser caches it, an upgrade goes invisible (old shell
// loads old hashed assets against the new daemon). no-store on the shell means
// an upgrade cannot be invisible; immutable on the fingerprinted assets means
// it stays cheap. Pinned exactly like the CSP — silent drift here reopens that
// incident class.
const EXPECTED_SHELL_CACHE_CONTROL = 'no-store';
const EXPECTED_ASSET_CACHE_CONTROL = 'public, max-age=31536000, immutable';

test('security headers: CSP on the shell, nosniff everywhere', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  let indexHtml = '';

  await t.test('GET / (the HTML shell) carries both the CSP and nosniff', async () => {
    const res = await fetch(`${daemon.baseUrl}/`);
    assert.equal(res.status, 200);
    indexHtml = await res.text();
    assert.equal(res.headers.get('x-content-type-options'), 'nosniff');
    assert.equal(res.headers.get('content-security-policy'), EXPECTED_CSP, 'the shell CSP must match the pinned policy exactly');
    assert.equal(res.headers.get('cache-control'), EXPECTED_SHELL_CACHE_CONTROL,
      'the shell must be no-store — a cached index.html makes an upgrade invisible (0.19.2)');
  });

  await t.test('GET / sends no Referer anywhere (the boot URL carries the token)', async () => {
    const res = await fetch(`${daemon.baseUrl}/`);
    assert.equal(res.headers.get('referrer-policy'), 'no-referrer',
      '0.16.0: the Google Fonts stylesheet fires before token.js can scrub ?t= — no subresource may ever see the URL');
  });

  await t.test('GET /assets/<hashed> carries nosniff but no CSP (it is a subresource)', async () => {
    const asset = /\/assets\/[^"']+\.js/.exec(indexHtml)?.[0];
    assert.ok(asset, 'index.html must reference a hashed JS asset');
    const res = await fetch(`${daemon.baseUrl}${asset}`);
    assert.equal(res.status, 200);
    assert.equal(res.headers.get('x-content-type-options'), 'nosniff', 'every asset must carry nosniff');
    assert.equal(res.headers.get('content-security-policy'), null, 'CSP belongs on the document, not on each subresource');
    assert.equal(res.headers.get('cache-control'), EXPECTED_ASSET_CACHE_CONTROL,
      'fingerprinted assets must be immutable — the filename changes when the content does');
  });

  await t.test('a JSON route (GET /health) carries nosniff', async () => {
    const res = await fetch(`${daemon.baseUrl}/health`);
    assert.equal(res.status, 200);
    assert.equal(res.headers.get('x-content-type-options'), 'nosniff', 'the JSON API must carry nosniff too');
    assert.match(res.headers.get('content-type') || '', /application\/json/);
  });
});
