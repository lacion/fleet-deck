// p6-http-freeze.test.ts — CHARACTERIZATION tests that pin the CURRENT wire-facing
// behavior of the daemon HTTP/WS surface for the coverage gaps enumerated in §7 of
// the frozen behavior matrix docs/v1/evidence/effect/p6-http-matrix.md.
//
// These tests are a FREEZE, not a spec: they assert the behavior HEAD emits today so
// the Effect migration can prove it preserved it. They make NO source changes and,
// where a probe would settle a §8 UNVERIFIED row, they pin the OBSERVED transport
// value. Each block names the matrix gap it covers.
//
// Gaps covered here: 1 (derive-owned control-route error status enums at transport),
// 3 (Bun global maxRequestBodySize fast-413), 4 (gateway_* bearer gate + trustLoopback
// waiver), 6 (POST /command relays core.command at 200), 7 (WS-upgrade refuse statuses).
// Gaps 2, 8, 9, 10 are dispositioned in the FINAL REPORT, not here — they require
// source-level fault injection, real-time waits, or describe non-daemon coverage, none
// of which a characterization test can honestly pin. Gap 5 is already covered by
// tests/lan-auth.test.ts (matrix §7.5 is stale — recorded as a discrepancy).

import assert from 'node:assert/strict';
import net from 'node:net';
import { randomUUID } from 'node:crypto';
import test, { type TestContext } from './helpers/harness-test.ts';
import { startDaemon } from './helpers/daemon.ts';
import { postJson } from './helpers/http.ts';
import { scaleMs } from './helpers/wait.ts';

// A valid 16-byte Sec-WebSocket-Key so a refused upgrade is refused for the reason
// under test (auth / host / path), not for a malformed key.
const validKey = (): string =>
  Buffer.from(randomUUID().replace(/-/g, ''), 'hex').toString('base64');

// Send a raw WS upgrade request and resolve the numeric HTTP status of the server's
// answer. Every gap-7 case is a REFUSAL (non-101): handleUpgrade returns
// `new Response(null, { status })`, which Bun emits as an ordinary HTTP status line
// before arming the keep-alive FIN — so the status is readable off the first chunk and
// we destroy the socket without waiting for the (120s) FIN.
function rawUpgradeStatus(
  port: number,
  opts: { path: string; key?: string; host?: string; token?: string | null },
): Promise<number> {
  return new Promise<number>((resolve, reject) => {
    const sock = net.connect(port, '127.0.0.1');
    sock.setNoDelay(true);
    let buf = '';
    let settled = false;
    const finish = (fn: () => void): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      sock.destroy();
      fn();
    };
    const timer = setTimeout(
      () => finish(() => reject(new Error(`upgrade to ${opts.path} timed out`))),
      scaleMs(5000),
    );
    timer.unref();
    sock.on('connect', () => {
      const lines = [
        `GET ${opts.path} HTTP/1.1`,
        `Host: ${opts.host ?? `127.0.0.1:${port}`}`,
        'Connection: Upgrade',
        'Upgrade: websocket',
        'Sec-WebSocket-Version: 13',
        `Sec-WebSocket-Key: ${opts.key ?? validKey()}`,
      ];
      if (opts.token) lines.push(`Authorization: Bearer ${opts.token}`);
      sock.write(`${lines.join('\r\n')}\r\n\r\n`);
    });
    sock.on('data', (d: Buffer) => {
      buf += d.toString('utf8');
      const nl = buf.indexOf('\r\n');
      if (nl === -1) return;
      const m = /^HTTP\/1\.1 (\d{3})\b/.exec(buf.slice(0, nl));
      if (m) finish(() => resolve(Number(m[1])));
    });
    sock.on('error', (e: Error) => finish(() => reject(e)));
    sock.on('close', () =>
      finish(() =>
        reject(
          new Error(
            `socket closed before a status line for ${opts.path}; got: ${JSON.stringify(buf.slice(0, 80))}`,
          ),
        ),
      ),
    );
  });
}

// GAPS 1, 3, 4-gate, 6, 7 — all probes against a single default loopback daemon.
// Default = FLEETDECK_TRUST_LOOPBACK:'off' (passed explicitly; daemon.ts does not
// scrub it, so a dev shell that exported 'on' would otherwise leak the waiver in).
test('P6 HTTP/WS freeze: default loopback surface (gaps 1, 3, 4-gate, 6, 7)', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'off' } });
  t.after(() => daemon.stop());
  const base = daemon.baseUrl;
  const token = daemon.token;

  // ── Gap 4 (gate): the gateway_* bearer gate on POST /api/settings ────────────
  // Off the waiver, a gateway_* mutation without the bearer is refused with the
  // exact 401 body http.ts:1785-1810 emits; WITH the bearer it reaches
  // core.setSettings and returns 200 {ok:true,...}.
  await t.test('gap 4: gateway_* POST without the bearer → 401 (exact body)', async () => {
    const res = await postJson(`${base}/api/settings`, {
      gateway_base_url: 'https://gw.example.com',
    });
    assert.equal(res.status, 401);
    assert.deepEqual(res.json, {
      ok: false,
      reason: 'gateway settings require the bearer token',
    });
  });

  await t.test('gap 4: gateway_* POST with the bearer → 200 ok:true', async () => {
    const res = await postJson(
      `${base}/api/settings`,
      { gateway_base_url: 'https://gw.example.com' },
      { token },
    );
    assert.equal(res.status, 200);
    assert.equal((res.json as { ok?: unknown }).ok, true);
  });

  // ── Gap 1 — derive-owned control-route error status enums at transport ───────
  // An unknown id on each control route. These settle the §8 "derive-owned status
  // enums unconfirmed at transport" row: the value asserted is the OBSERVED HTTP
  // status HEAD relays from core.* (or, for name/questions.dismiss, the status
  // http.ts hardcodes from out.ok). If any relays 500, that is itself the finding.
  const SPAWN = 'p6-freeze-nonexistent'; // matches [A-Za-z0-9-]+ (kill/revive/rc)
  const SESSION = 'p6-freeze-nonexistent'; // matches [^/]+ (adopt/dismiss/name)
  const NUM = 99_999_999; // matches \d+ (questions/plans)

  const probes: { name: string; path: string; body: unknown; expect: number }[] = [
    { name: 'kill', path: `/api/spawn/${SPAWN}/kill`, body: {}, expect: 404 },
    { name: 'revive', path: `/api/spawn/${SPAWN}/revive`, body: {}, expect: 404 },
    { name: 'rc', path: `/api/spawn/${SPAWN}/rc`, body: {}, expect: 404 },
    { name: 'adopt', path: `/api/sessions/${SESSION}/adopt`, body: {}, expect: 404 },
    { name: 'dismiss', path: `/api/sessions/${SESSION}/dismiss`, body: {}, expect: 404 },
    {
      name: 'dismiss-retry',
      path: `/api/sessions/${SESSION}/dismiss/retry`,
      body: {},
      expect: 404,
    },
    // http.ts:1998-2029 hardcodes `out.ok ? 200 : 409` — an unknown name target is
    // never ok, so 409 regardless of the derive reason. `suffix` is the accepted
    // body field, so a well-formed request reaches the unknown-id path (not the 400
    // body validator).
    { name: 'name', path: `/api/sessions/${SESSION}/name`, body: { suffix: 'x' }, expect: 409 },
    {
      name: 'questions.answer',
      path: `/api/questions/${NUM}/answer`,
      body: {},
      expect: 404,
    },
    // http.ts:2096-2102 hardcodes `out.ok ? 200 : 404`.
    {
      name: 'questions.dismiss',
      path: `/api/questions/${NUM}/dismiss`,
      body: {},
      expect: 404,
    },
    {
      name: 'plans.mark',
      path: `/api/plans/${NUM}/mark`,
      body: { status: 'executed' },
      expect: 404,
    },
    {
      name: 'plans.assign',
      path: `/api/plans/${NUM}/assign`,
      body: { to: SPAWN },
      expect: 404,
    },
  ];

  for (const probe of probes) {
    await t.test(`gap 1: ${probe.name} unknown id → ${probe.expect}`, async () => {
      const res = await postJson(`${base}${probe.path}`, probe.body, { token });
      assert.equal(
        res.status,
        probe.expect,
        `${probe.name} relayed ${res.status} (body ${JSON.stringify(res.json)})`,
      );
    });
  }

  // ── Gap 6 — POST /command relays core.command(text) at 200 ───────────────────
  // http.ts:1812-1814 always answers 200 with core.command()'s ControlResult. A
  // plain note with no live sessions delivers to none (commands.ts:249).
  await t.test('gap 6: POST /command → 200 relaying core.command', async () => {
    const res = await postJson(`${base}/command`, { text: 'p6 characterization note' });
    assert.equal(res.status, 200);
    const body = res.json as { ok?: unknown; delivered?: unknown; parsed?: unknown };
    assert.equal(body.ok, true);
    assert.equal(body.delivered, 0);
    assert.equal(typeof body.parsed, 'object');
  });

  await t.test('gap 6: POST /command with no text is still 200 (transport)', async () => {
    const res = await postJson(`${base}/command`, {});
    assert.equal(res.status, 200);
  });

  // ── Gap 7 — WS-upgrade refuse statuses at transport ──────────────────────────
  // handleUpgrade (http.ts:2473-2517): unknown path → 404; a failed /ws or /ws/term
  // upgrade → 400; a tokenless tokenGated /ws/term → 401. A valid key isolates each
  // refusal to the reason under test.
  await t.test('gap 7: unknown /ws/* path → 404', async () => {
    assert.equal(await rawUpgradeStatus(daemon.port, { path: '/ws/bogus' }), 404);
  });

  await t.test('gap 7: /ws with a malformed key → 400 (failed upgrade)', async () => {
    assert.equal(await rawUpgradeStatus(daemon.port, { path: '/ws', key: 'x' }), 400);
  });

  await t.test('gap 7: /ws/term with the bearer but a malformed key → 400', async () => {
    assert.equal(await rawUpgradeStatus(daemon.port, { path: '/ws/term', key: 'x', token }), 400);
  });

  await t.test('gap 7: tokenless /ws/term → 401 (tokenGated auth refusal)', async () => {
    assert.equal(await rawUpgradeStatus(daemon.port, { path: '/ws/term' }), 401);
  });

  // ── Gap 3 — Bun global maxRequestBodySize fast-413 ───────────────────────────
  // maxRequestBodySize is MAX_PASTE_BODY (14e6). A body over that ceiling is
  // rejected by Bun BEFORE fetch runs: a bodyless 413 carrying `Connection: close`,
  // distinct from the in-handler 413 (which answers a JSON body and keeps the
  // connection alive). Run last: it asserts the daemon survives, then later blocks
  // (none, here) would still be safe. Settles the §8 fast-413 transport row.
  await t.test(
    'gap 3: a >14e6 body → bodyless 413 + Connection: close; daemon survives',
    async () => {
      const OVER = 14_000_000 + 100_000; // unambiguously over the 14e6 global ceiling
      const raw = await new Promise<string>((resolve, reject) => {
        const sock = net.connect(daemon.port, '127.0.0.1');
        sock.setNoDelay(true);
        let buf = '';
        let settled = false;
        const settle = (fn: () => void): void => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          sock.destroy();
          fn();
        };
        const timer = setTimeout(
          () => settle(() => reject(new Error(`no 413 within budget (got ${buf.length} bytes)`))),
          scaleMs(8000),
        );
        timer.unref();
        sock.on('connect', () => {
          sock.write(
            `POST /mail HTTP/1.1\r\n` +
              `Host: 127.0.0.1:${daemon.port}\r\n` +
              `Content-Type: application/json\r\n` +
              `Authorization: Bearer ${token}\r\n` +
              `Content-Length: ${OVER}\r\n` +
              `\r\n`,
          );
          // Stream the oversized body. Bun fast-413s pre-fetch, so this write may EPIPE
          // mid-flight — absorbed by the error sink; the assertion is on the RESPONSE.
          sock.write(Buffer.alloc(OVER, 0x78));
        });
        sock.on('data', (d: Buffer) => {
          buf += d.toString('utf8');
          // Settle on the header terminator rather than socket close: the fast-413 may
          // keep the connection alive, so waiting for `close` would hang. A short grace
          // lets any (unexpected) body arrive before we assert bodylessness.
          if (buf.includes('\r\n\r\n')) setTimeout(() => settle(() => resolve(buf)), scaleMs(200));
        });
        sock.on('error', () => {});
        sock.on('close', () => settle(() => resolve(buf)));
      });
      // OBSERVED on HEAD: "HTTP/1.1 413 Request Entity Too Large\r\nConnection: close\r\n\r\n"
      // — a bodyless 413 carrying Connection: close, exactly what matrix §8 froze for the
      // global fast-413 (distinct from the in-handler 413, which answers a JSON body and
      // keeps the connection alive).
      const sep = raw.indexOf('\r\n\r\n');
      assert.notEqual(sep, -1, 'the 413 response headers must be readable');
      const statusLine = raw.slice(0, raw.indexOf('\r\n'));
      assert.match(statusLine, /^HTTP\/1\.1 413\b/, `expected 413, got: ${statusLine}`);
      const headers = raw.slice(0, sep);
      assert.match(
        headers,
        /\r\nconnection:\s*close/i,
        `the global fast-413 must carry Connection: close; headers: ${JSON.stringify(headers)}`,
      );
      const responseBody = raw.slice(sep + 4);
      assert.equal(
        responseBody.length,
        0,
        `the global fast-413 must be bodyless; got body: ${JSON.stringify(responseBody)}`,
      );
      const health = (await fetch(`${base}/health`).then((r) => r.json())) as { ok?: unknown };
      assert.equal(health.ok, true, 'the daemon must survive the oversized-body rejection');
    },
  );
});

// GAP 4 (waiver) — FLEETDECK_TRUST_LOOPBACK waives the gateway_* bearer gate for a
// loopback peer. A separate daemon because the mode is process-wide (http.ts:1785-1810,
// bearerWaived = trustLoopback && !viaTrustedProxy && loopback).
test('P6 gap 4 waiver: FLEETDECK_TRUST_LOOPBACK waives the gateway_* bearer gate', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'on' } });
  t.after(() => daemon.stop());

  const res = await postJson(`${daemon.baseUrl}/api/settings`, {
    gateway_base_url: 'https://gw.example.com',
  });
  assert.equal(res.status, 200, `waiver should pass the gate; got ${JSON.stringify(res.json)}`);
  assert.equal((res.json as { ok?: unknown }).ok, true);
});
