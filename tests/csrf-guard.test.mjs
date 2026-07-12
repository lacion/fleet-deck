// tests/csrf-guard.test.mjs
//
// C1 (CRITICAL) + M-B3. Loopback auto-authorizes, and a browser is a loopback
// peer, so the ONLY thing standing between "any website the user visits" and
// this daemon is the same-origin gate: an Origin/Sec-Fetch-Site check on every
// state-changing POST and both WS upgrades, a Host allowlist that defeats DNS
// rebinding, and a Content-Type requirement that forces a CORS preflight. This
// suite pins all of that on a plain loopback daemon (no token needed — the gate
// under test is orthogonal to auth). It also pins the byte-exact POST body
// (M-B3): a multibyte glyph split across TCP chunks must survive, and an
// oversized body must 413 on a control path.
//
// fetch() forbids setting Origin/Host from script, so a "malicious browser" is
// simulated with a raw socket (http.request) that can send arbitrary headers.

import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocket } from 'ws';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

// A single raw HTTP request with fully controlled headers and (optionally) a
// body written in explicit parts, so a multibyte char can be split across the
// wire. Resolves { status, body } — never rejects on a non-2xx.
function raw(port, { method = 'GET', path: reqPath = '/', headers = {}, parts = [] } = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: reqPath, method, headers }, res => {
      let body = '';
      res.on('data', d => { body += d; });
      res.on('end', () => resolve({ status: res.statusCode, body }));
    });
    req.on('socket', s => s.setNoDelay(true)); // don't let Nagle re-merge our split writes
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    (async () => {
      for (let i = 0; i < parts.length; i++) {
        req.write(parts[i]);
        if (i < parts.length - 1) await new Promise(r => setTimeout(r, 30)); // force a second 'data' event
      }
      req.end();
    })();
  });
}

const JSON_CT = { 'content-type': 'application/json' };

// Resolve 'open' with the first frame, or 'refused' if the socket is torn down
// during the upgrade (the server's destroy() path).
function wsAttempt(url, options) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url, options);
    const timer = setTimeout(() => { ws.terminate(); reject(new Error('WS attempt hung')); }, 5000);
    ws.once('message', raw => {
      clearTimeout(timer);
      let frame = null; try { frame = JSON.parse(raw.toString('utf8')); } catch { /* server junk */ }
      ws.close();
      resolve({ outcome: 'open', frame });
    });
    ws.once('open', () => { /* wait for the first message (or a refusal) */ });
    ws.once('error', () => { clearTimeout(timer); resolve({ outcome: 'refused' }); });
    ws.once('close', () => { clearTimeout(timer); resolve({ outcome: 'refused' }); });
  });
}

test('C1: same-origin gate on POSTs, WS upgrades, Host, and Content-Type', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const { port, baseUrl } = daemon;
  const origin = `http://127.0.0.1:${port}`;

  await t.test('cross-origin POST is refused (403), no side effect', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { ...JSON_CT, origin: 'https://evil.example' },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 403, 'a page on another site must not drive /mail');
  });

  await t.test('Sec-Fetch-Site: cross-site is refused (403) even with our own Origin absent', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { ...JSON_CT, 'sec-fetch-site': 'cross-site' },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 403);
  });

  await t.test('same-origin POST is allowed (200)', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { ...JSON_CT, origin },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 200, 'the board POSTing to its own origin must work');
  });

  await t.test('no-Origin loopback POST is allowed (the CLI/hook path)', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { ...JSON_CT }, // a CLI tool sends no Origin
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 200);
  });

  await t.test('control POST without application/json is 415', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { 'content-type': 'text/plain' }, // a CORS "simple" content-type — no preflight
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 415, 'a text/plain POST must be refused so /api/spawn needs a preflight');
  });

  await t.test('hooks stay fail-open: bad content-type still 200 {}', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/hook/UnknownHook',
      headers: { 'content-type': 'text/plain' },
      parts: ['{}'],
    });
    assert.equal(res.status, 200, 'a hook must never be broken by a content-type check');
  });

  await t.test('a cross-origin hook is dropped but still answers 200 {} (never wedges a session)', async () => {
    const res = await raw(port, {
      method: 'POST', path: '/hook/UserPromptSubmit',
      headers: { ...JSON_CT, origin: 'https://evil.example' },
      parts: [JSON.stringify({ session_id: 'x', prompt: 'y' })],
    });
    assert.equal(res.status, 200);
    assert.deepEqual(JSON.parse(res.body), {});
  });

  await t.test('DNS rebinding: a foreign Host is refused on a data route (403)', async () => {
    const res = await raw(port, {
      method: 'GET', path: '/state',
      headers: { host: `evil.example:${port}` }, // rebinding domain → 127.0.0.1
    });
    assert.equal(res.status, 403, 'evil.example:port must not read /state even though it resolves to loopback');
  });

  await t.test('the public shell stays open despite a foreign Host and Origin', async () => {
    const res = await raw(port, {
      method: 'GET', path: '/',
      headers: { host: `evil.example:${port}`, origin: 'https://evil.example' },
    });
    assert.equal(res.status, 200, 'a browser must be able to load the data-free shell to then present a key');
  });

  await t.test('cross-origin WS /ws is refused, same-origin and no-Origin are allowed', async () => {
    const wsBase = baseUrl.replace(/^http/, 'ws');
    const evil = await wsAttempt(`${wsBase}/ws`, { headers: { origin: 'https://evil.example' } });
    assert.equal(evil.outcome, 'refused', 'a cross-site page must not open the snapshot socket');

    const same = await wsAttempt(`${wsBase}/ws`, { headers: { origin } });
    assert.equal(same.outcome, 'open', 'the board on its own origin must still connect');
    assert.equal(same.frame?.type, 'snapshot');

    const cli = await wsAttempt(`${wsBase}/ws`); // node client sends no Origin
    assert.equal(cli.outcome, 'open');
    assert.equal(cli.frame?.type, 'snapshot');
  });

  await t.test('cross-origin WS /ws/term is refused', async () => {
    const wsBase = baseUrl.replace(/^http/, 'ws');
    const evil = await wsAttempt(`${wsBase}/ws/term?spawn=x&cols=80&rows=24`, { headers: { origin: 'https://evil.example' } });
    assert.equal(evil.outcome, 'refused', 'a cross-site page must not reach a live pane');
  });
});

test('M-B3: POST body is byte-exact and byte-capped', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const { port } = daemon;

  await t.test('a multibyte glyph split across TCP chunks survives intact', async () => {
    const sid = randomUUID();
    const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-mb3-'));
    t.after(() => rmSync(cwd, { recursive: true, force: true }));
    const start = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }));
    assert.equal(start.status, 200);

    // 'A☃B' — the snowman is 3 bytes (E2 98 83). Split the JSON body one byte
    // into the snowman so the daemon receives its bytes across two 'data'
    // events. `body += d` used to stringify each Buffer alone, turning the
    // straddling glyph into U+FFFD.
    const text = 'A☃B';
    const bodyBuf = Buffer.from(JSON.stringify({ to: sid, from: 'board', text }), 'utf8');
    const snowmanAt = bodyBuf.indexOf(Buffer.from('☃', 'utf8'));
    assert.ok(snowmanAt > 0, 'sanity: found the snowman in the wire body');
    const cut = snowmanAt + 1; // mid-glyph
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { 'content-type': 'application/json', 'content-length': String(bodyBuf.length) },
      parts: [bodyBuf.subarray(0, cut), bodyBuf.subarray(cut)],
    });
    assert.equal(res.status, 200, res.body);

    const box = await fetch(`${daemon.baseUrl}/mail?session=${sid}`).then(r => r.json());
    const delivered = box.mail?.find(m => m.text.includes('B'));
    assert.ok(delivered, `mail was not delivered: ${JSON.stringify(box)}`);
    assert.equal(delivered.text, text, 'the split multibyte glyph must round-trip exactly');
    assert.ok(!delivered.text.includes('�'), 'no replacement character may survive the split');
  });

  await t.test('an oversized control body is 413', async () => {
    // > MAX_BODY (1e6 bytes). The cap is now measured in BYTES, not UTF-16 units.
    const huge = 'x'.repeat(1_100_000);
    const res = await raw(port, {
      method: 'POST', path: '/mail',
      headers: { 'content-type': 'application/json' },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: huge })],
    });
    assert.equal(res.status, 413, 'a body past the byte cap must be refused on a control path');
  });
});
