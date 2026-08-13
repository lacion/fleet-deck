// tests/csrf-guard.test.ts
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

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocket } from 'ws';
import { startDaemon } from './helpers/daemon.ts';
import { postHook } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';

import type { StateResponse } from '../contracts/state.ts';

interface MailItem {
  text: string;
}
interface MailBox {
  mail?: MailItem[];
}

// A single raw HTTP request with fully controlled headers and (optionally) a
// body written in explicit parts, so a multibyte char can be split across the
// wire. Resolves { status, body } — never rejects on a non-2xx.
interface RawOptions {
  method?: string;
  path?: string;
  headers?: Record<string, string>;
  parts?: (string | Buffer)[];
}

function raw(
  port: number,
  { method = 'GET', path: reqPath = '/', headers = {}, parts = [] }: RawOptions = {},
): Promise<{ status: number | undefined; body: string }> {
  return new Promise<{ status: number | undefined; body: string }>((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: reqPath, method, headers }, (res) => {
      let body = '';
      res.on('data', (d: Buffer) => {
        body += d.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, body });
      });
    });
    req.on('socket', (s) => s.setNoDelay(true)); // don't let Nagle re-merge our split writes
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    void (async () => {
      for (let i = 0; i < parts.length; i++) {
        const part = parts[i];
        if (part === undefined) continue;
        req.write(part);
        if (i < parts.length - 1) await new Promise<void>((r) => setTimeout(r, 30)); // force a second 'data' event
      }
      req.end();
    })();
  });
}

const JSON_CT = { 'content-type': 'application/json' };

// Resolve 'open' with the first frame, or 'refused' if the socket is torn down
// during the upgrade (the server's destroy() path).
//
// Two ordering details keep the outcome identical under BOTH node and bun's `ws`
// (the daemon runs under whichever `process.execPath` launched the suite):
//   1. Record 'open' BEFORE ws.close(). Bun's ws.close() re-entrantly emits
//      'close' in the SAME tick, so closing first lets the 'close' handler settle
//      'refused' ahead of this 'open'. Node emits 'close' asynchronously, so the
//      order is immaterial there — the settled outcome is the same under both.
//   2. A persistent 'error' sink (.on, not .once). Bun's ws can emit a SECOND
//      'error' after a refusal is already recorded (the server destroy path), and
//      an EventEmitter 'error' with no listener is fatal — it would crash an
//      unrelated later test. Node never emits that second error, so it is inert.
// A single `settled` latch keeps the first outcome authoritative for both.
function wsAttempt(
  url: string,
  options?: { headers?: Record<string, string> },
): Promise<{ outcome: 'open' | 'refused'; frame?: { type?: string } | null }> {
  return new Promise<{ outcome: 'open' | 'refused'; frame?: { type?: string } | null }>(
    (resolve, reject) => {
      const ws = new WebSocket(url, options);
      let settled = false;
      const timer = setTimeout(() => {
        ws.terminate();
        if (settled) return;
        settled = true;
        reject(new Error('WS attempt hung'));
      }, 5000);
      const finish = (result: {
        outcome: 'open' | 'refused';
        frame?: { type?: string } | null;
      }) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(result);
      };
      ws.on('message', (raw) => {
        let frame: { type?: string } | null = null;
        try {
          frame = JSON.parse((raw as Buffer).toString('utf8')) as { type?: string };
        } catch {
          /* server junk */
        }
        finish({ outcome: 'open', frame });
        ws.close();
      });
      ws.on('open', () => {
        /* wait for the first message (or a refusal) */
      });
      ws.on('error', () => {
        finish({ outcome: 'refused' });
      });
      ws.on('close', () => {
        finish({ outcome: 'refused' });
      });
    },
  );
}

// A raw WebSocket upgrade handshake with fully controlled headers. The ws client
// refuses to forge Host or Sec-Fetch-Site (exactly the headers a real attacker
// cannot set from a page either — which is the point), so drive the 101 by hand.
// Resolves 'upgraded' when the server completes the switch, or 'rejected' when it
// destroys the socket (the upgrade handler's socket.destroy() path) or answers
// any non-101.
function rawUpgrade(
  port: number,
  { path: reqPath = '/ws', headers = {} }: { path?: string; headers?: Record<string, string> } = {},
): Promise<'upgraded' | 'rejected'> {
  return new Promise<'upgraded' | 'rejected'>((resolve) => {
    const req = http.request({
      host: '127.0.0.1',
      port,
      path: reqPath,
      method: 'GET',
      headers: {
        connection: 'Upgrade',
        upgrade: 'websocket',
        'sec-websocket-version': '13',
        // A VALID 16-byte key. The ws server rejects a malformed one outright, so
        // a bogus key would make every case "reject" — the control upgrade below
        // would fail and the guard test would be tautological (rejecting the
        // forged headers for the wrong reason). UUID-minus-dashes is 32 hex = 16
        // bytes → a spec-shaped base64 key.
        'sec-websocket-key': Buffer.from(randomUUID().replace(/-/g, ''), 'hex').toString('base64'),
        ...headers,
      },
    });
    let settled = false;
    const done = (outcome: 'upgraded' | 'rejected') => {
      if (settled) return;
      settled = true;
      try {
        req.destroy();
      } catch {
        /* noop */
      }
      resolve(outcome);
    };
    req.on('upgrade', (_res, socket) => {
      try {
        socket.destroy();
      } catch {
        /* noop */
      }
      done('upgraded');
    });
    req.on('response', (res) => {
      // Node fires 'upgrade' for a 101 and 'response' only for a non-101, so this
      // handler is the "any non-101 answer" path there. Bun's http client instead
      // surfaces a completed 101 switch AS a 'response' (statusCode 101) and never
      // fires 'upgrade' — so treat a 101 here as 'upgraded'. Under node a 101 never
      // reaches this branch, so the outcome is identical on both runtimes.
      done(res.statusCode === 101 ? 'upgraded' : 'rejected');
    });
    req.on('error', () => {
      done('rejected');
    }); // server tore the socket down
    req.on('socket', (s) =>
      s.on('close', () => {
        done('rejected');
      }),
    );
    req.setTimeout(5000, () => {
      done('rejected');
    });
    req.end();
  });
}

test('C1: same-origin gate on POSTs, WS upgrades, Host, and Content-Type', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const { port, baseUrl } = daemon;
  const origin = `http://127.0.0.1:${port}`;

  await t.test('cross-origin POST is refused (403), no side effect', async () => {
    // 0.16.0: the bearer rides along so the AUTH wall passes and the 403 can
    // only come from the same-origin gate under test.
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: {
        ...JSON_CT,
        origin: 'https://evil.example',
        authorization: `Bearer ${daemon.token ?? ''}`,
      },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 403, 'a page on another site must not drive /mail');
  });

  await t.test('POST /api/settings rides the same cross-origin wall', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/api/settings',
      headers: { ...JSON_CT, origin: 'https://evil.example' },
      parts: [JSON.stringify({ repos_dir: '/tmp/evil-repos' })],
    });
    assert.equal(res.status, 403, 'a page on another site must not change the managed repos root');
  });

  await t.test(
    'Sec-Fetch-Site: cross-site is refused (403) even with our own Origin absent',
    async () => {
      const res = await raw(port, {
        method: 'POST',
        path: '/mail',
        headers: {
          ...JSON_CT,
          'sec-fetch-site': 'cross-site',
          authorization: `Bearer ${daemon.token ?? ''}`,
        },
        parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
      });
      assert.equal(res.status, 403);
    },
  );

  await t.test('same-origin POST is allowed (200)', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: { ...JSON_CT, origin, authorization: `Bearer ${daemon.token ?? ''}` },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 200, 'the board POSTing to its own origin must work');
  });

  await t.test('no-Origin loopback POST is allowed (the CLI/hook path)', async () => {
    // 0.16.0: POST /mail is bearer-gated even on loopback — the "CLI path" is
    // now the fleet skill / hook shims, which attach $FLEETDECK_HOME/token.
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: { ...JSON_CT, authorization: `Bearer ${daemon.token ?? ''}` }, // a shim sends no Origin, but does send the token
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(res.status, 200);
  });

  await t.test('control POST without application/json is 415', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: { 'content-type': 'text/plain', authorization: `Bearer ${daemon.token ?? ''}` }, // a CORS "simple" content-type — no preflight
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
    });
    assert.equal(
      res.status,
      415,
      'a text/plain POST must be refused so /api/spawn needs a preflight',
    );
  });

  await t.test('hooks stay fail-open: bad content-type still 200 {}', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/hook/UnknownHook',
      headers: { 'content-type': 'text/plain', authorization: `Bearer ${daemon.token ?? ''}` },
      parts: ['{}'],
    });
    assert.equal(res.status, 200, 'a hook must never be broken by a content-type check');
  });

  await t.test(
    'a cross-origin hook is dropped but still answers 200 {} (never wedges a session)',
    async () => {
      // BUG-161: the 200 {} alone proves NOTHING — a regression that waves the
      // forged Origin through still answers the empty hook response (there was
      // no mail to deliver). Only a live VICTIM proves the request was dropped
      // before it could touch state: a real session with queued mail and a real
      // live hold, where a processed UserPromptSubmit would DRAIN the mail and
      // RELEASE the hold. Assert the byte-observable state survives unchanged.
      const victim = randomUUID();
      const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-csrf-'));
      t.after(() => {
        rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      });

      const start = await postHook(
        baseUrl,
        'SessionStart',
        loadFixture('session-start', { session_id: victim, cwd }),
        { token: daemon },
      );
      assert.equal(start.status, 200, 'setup: the victim session must register');

      // A real pending hold. The response parks (hold-open relay); it settles
      // later via dismiss() once the CSRF assertions are done.
      const hold = postHook(
        baseUrl,
        'PermissionRequest',
        loadFixture('permission-request', { session_id: victim, cwd }),
        { token: daemon, timeout: 30_000 },
      );

      // Mail queued for the victim (a processed UserPromptSubmit would drain it).
      const posted = await raw(port, {
        method: 'POST',
        path: '/mail',
        headers: { ...JSON_CT, authorization: `Bearer ${daemon.token ?? ''}` },
        parts: [JSON.stringify({ to: victim, from: 'board', text: 'csrf-victim-mail' })],
      });
      assert.equal(posted.status, 200, 'setup: the victim must have mail to lose');

      // The attacker: a page on another site forging an authenticated hook FOR
      // THE VICTIM. crossSiteReason must turn it away before any hook handler,
      // drain, or expiry runs — while still answering the fail-open dialect.
      const res = await raw(port, {
        method: 'POST',
        path: '/hook/UserPromptSubmit',
        headers: {
          ...JSON_CT,
          origin: 'https://evil.example',
          authorization: `Bearer ${daemon.token ?? ''}`,
        },
        parts: [JSON.stringify({ session_id: victim, prompt: 'attacker prompt' })],
      });
      assert.equal(res.status, 200);
      assert.deepEqual(JSON.parse(res.body), {});

      // The mail must NOT be drained: the GET /mail drain still finds it.
      const box = (await fetch(`${baseUrl}/mail?session=${victim}`).then((r) =>
        r.json(),
      )) as MailBox;
      assert.equal(
        box.mail?.length,
        1,
        `the hostile-origin hook must not drain the victim's mail: ${JSON.stringify(box)}`,
      );
      const firstMail = box.mail[0];
      assert.ok(firstMail, 'the victim mail item survives');
      assert.equal(firstMail.text, 'csrf-victim-mail');

      // The hold must NOT be released: the question is still pending and held.
      const state = (await fetch(`${baseUrl}/state`).then((r) => r.json())) as StateResponse;
      const qrow = state.questions.find((q) => q.session_id === victim);
      assert.ok(qrow, 'the victim hold must still exist');
      assert.equal(qrow.status, 'pending', 'a dropped hook must not settle the victim hold');
      assert.equal(qrow.held, true, 'a dropped hook must not release the live hold');
      assert.equal(
        state.mail_meta[victim]?.queued,
        0,
        'the drain above, not the hostile hook, consumed the mail',
      );

      // The forged session_id must NOT be upgraded to a card of its own.
      assert.equal(
        state.sessions.filter((s) => s.session_id === victim).length,
        1,
        'no second card may appear under the forged session id',
      );

      // Teardown: the victim card is parked in needs-you (the live hold) —
      // releasing it keeps this daemon's state clean for the subtests below.
      const answer = await raw(port, {
        method: 'POST',
        path: `/api/questions/${qrow.id}/dismiss`,
        headers: { ...JSON_CT, authorization: `Bearer ${daemon.token ?? ''}` },
        parts: ['{}'],
      });
      assert.equal(answer.status, 200, `dismiss must release the victim hold: ${answer.body}`);
      const settled = await hold;
      assert.deepEqual(settled.json, {}, 'the dismissed hold fails open');
    },
  );

  await t.test('DNS rebinding: a foreign Host is refused on a data route (403)', async () => {
    const res = await raw(port, {
      method: 'GET',
      path: '/state',
      headers: { host: `evil.example:${port}` }, // rebinding domain → 127.0.0.1
    });
    assert.equal(
      res.status,
      403,
      'evil.example:port must not read /state even though it resolves to loopback',
    );
  });

  await t.test('the public shell stays open despite a foreign Host and Origin', async () => {
    const res = await raw(port, {
      method: 'GET',
      path: '/',
      headers: { host: `evil.example:${port}`, origin: 'https://evil.example' },
    });
    assert.equal(
      res.status,
      200,
      'a browser must be able to load the data-free shell to then present a key',
    );
  });

  await t.test(
    'cross-origin WS /ws is refused, same-origin and no-Origin are allowed',
    async () => {
      const wsBase = baseUrl.replace(/^http/, 'ws');
      const evil = await wsAttempt(`${wsBase}/ws`, { headers: { origin: 'https://evil.example' } });
      assert.equal(evil.outcome, 'refused', 'a cross-site page must not open the snapshot socket');

      const same = await wsAttempt(`${wsBase}/ws`, { headers: { origin } });
      assert.equal(same.outcome, 'open', 'the board on its own origin must still connect');
      assert.equal(same.frame?.type, 'snapshot');

      const cli = await wsAttempt(`${wsBase}/ws`); // node client sends no Origin
      assert.equal(cli.outcome, 'open');
      assert.equal(cli.frame?.type, 'snapshot');
    },
  );

  await t.test('cross-origin WS /ws/term is refused', async () => {
    const wsBase = baseUrl.replace(/^http/, 'ws');
    const evil = await wsAttempt(`${wsBase}/ws/term?spawn=x&cols=80&rows=24`, {
      headers: { origin: 'https://evil.example' },
    });
    assert.equal(evil.outcome, 'refused', 'a cross-site page must not reach a live pane');
  });

  // R1-1: method is not the boundary — state change is. GET /mail DRAINS a
  // mailbox and GET /api/watch CLAIMS mail; both mutate, so both get the same
  // cross-site verdict as a POST. A simple cross-site fetch() carries an Origin
  // but needs no preflight, so without this a page could drain a session's mail.
  await t.test(
    'mutating GET /mail is refused cross-origin (403), allowed with no Origin',
    async () => {
      const evil = await raw(port, {
        method: 'GET',
        path: '/mail?session=nobody',
        headers: { origin: 'https://evil.example' },
      });
      assert.equal(
        evil.status,
        403,
        'a page on another site must not DRAIN a mailbox via GET /mail',
      );

      const cli = await raw(port, { method: 'GET', path: '/mail?session=nobody' }); // a CLI/fleet-watch caller sends no Origin
      assert.equal(cli.status, 200, 'a no-Origin loopback caller must still drain mail');
    },
  );

  await t.test(
    'mutating GET /api/watch is refused cross-site (403), allowed with no Origin',
    async () => {
      const evil = await raw(port, {
        method: 'GET',
        path: '/api/watch?session=nobody',
        headers: { 'sec-fetch-site': 'cross-site' },
      });
      assert.equal(evil.status, 403, 'a cross-site page must not CLAIM mail via GET /api/watch');

      // Unknown session ⇒ session_alive:false ⇒ the long-poll answers idle at once
      // (no hold), so the no-Origin path returns promptly rather than hanging.
      const cli = await raw(port, { method: 'GET', path: '/api/watch?session=nobody' });
      assert.equal(cli.status, 200, 'a no-Origin watcher must still be served');
    },
  );

  // BUG-030: WHATWG URL normalizes an explicit default port away, so an Origin
  // of plain http://127.0.0.1 / http://localhost (a page served by ANY other
  // local service on :80) used to read as "port absent ⇒ allow". Against a
  // daemon on a non-default port that opened the whole same-origin wall. The
  // absent port must resolve to the SCHEME default (80/443), which is not our
  // port — these must all 403/refuse. (The same-origin controls above already
  // pin that the Origin carrying the real daemon port still succeeds.)
  await t.test(
    'a default-port loopback Origin is NOT same-origin with a non-default daemon port',
    async () => {
      const bare = ['http://127.0.0.1', 'http://localhost'];
      for (const o of bare) {
        const get = await raw(port, {
          method: 'GET',
          path: '/mail?session=nobody',
          headers: { origin: o },
        });
        assert.equal(get.status, 403, `mutating GET with Origin ${o} must be refused`);

        const post = await raw(port, {
          method: 'POST',
          path: '/mail',
          headers: { ...JSON_CT, origin: o, authorization: `Bearer ${daemon.token ?? ''}` },
          parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hi' })],
        });
        assert.equal(post.status, 403, `state-changing POST with Origin ${o} must be refused`);

        const ws = await wsAttempt(`${baseUrl.replace(/^http/, 'ws')}/ws`, {
          headers: { origin: o },
        });
        assert.equal(ws.outcome, 'refused', `WS upgrade with Origin ${o} must be refused`);
      }
      // https resolves its absent port to 443 — equally not our port.
      const tls = await raw(port, {
        method: 'GET',
        path: '/mail?session=nobody',
        headers: { origin: 'https://127.0.0.1' },
      });
      assert.equal(tls.status, 403, 'an https default-port Origin must be refused too');
    },
  );

  // Item-4 WS gaps, on RAW sockets so the forged headers actually reach the
  // server (the ws client silently drops Host / Sec-Fetch-Site). A control case
  // proves the guard is discriminating, not refusing every raw upgrade.
  await t.test('WS /ws upgrade with a forged foreign Host is refused (raw socket)', async () => {
    assert.equal(
      await rawUpgrade(port, { path: '/ws', headers: { host: `evil.example:${port}` } }),
      'rejected',
      'a DNS-rebinding Host must not complete the /ws upgrade',
    );
  });

  await t.test(
    'WS /ws upgrade with cross-site Sec-Fetch-Site is refused (raw socket)',
    async () => {
      assert.equal(
        await rawUpgrade(port, { path: '/ws', headers: { 'sec-fetch-site': 'cross-site' } }),
        'rejected',
        'a cross-site page must not complete the /ws upgrade',
      );
    },
  );

  await t.test(
    'WS /ws upgrade with our own Host and no cross-site marker completes (control)',
    async () => {
      assert.equal(
        await rawUpgrade(port, { path: '/ws', headers: { host: `127.0.0.1:${port}` } }),
        'upgraded',
        'a same-origin upgrade must still succeed — the guard is discriminating, not blanket-refusing',
      );
    },
  );
});

test('M-B3: POST body is byte-exact and byte-capped', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const { port } = daemon;

  await t.test('a multibyte glyph split across TCP chunks survives intact', async () => {
    const sid = randomUUID();
    const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-mb3-'));
    t.after(() => {
      rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    });
    const start = await postHook(
      daemon.baseUrl,
      'SessionStart',
      loadFixture('session-start', { session_id: sid, cwd }),
      { token: daemon },
    );
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
      method: 'POST',
      path: '/mail',
      headers: {
        'content-type': 'application/json',
        'content-length': String(bodyBuf.length),
        authorization: `Bearer ${daemon.token ?? ''}`,
      },
      parts: [bodyBuf.subarray(0, cut), bodyBuf.subarray(cut)],
    });
    assert.equal(res.status, 200, res.body);

    const box = (await fetch(`${daemon.baseUrl}/mail?session=${sid}`).then((r) =>
      r.json(),
    )) as MailBox;
    const delivered = box.mail?.find((m) => m.text.includes('B'));
    assert.ok(delivered, `mail was not delivered: ${JSON.stringify(box)}`);
    assert.equal(delivered.text, text, 'the split multibyte glyph must round-trip exactly');
    assert.ok(!delivered.text.includes('�'), 'no replacement character may survive the split');
  });

  await t.test('an oversized control body is 413', async () => {
    // > MAX_BODY (1e6 bytes). The cap is now measured in BYTES, not UTF-16 units.
    const huge = 'x'.repeat(1_100_000);
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${daemon.token ?? ''}`,
      },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: huge })],
    });
    assert.equal(res.status, 413, 'a body past the byte cap must be refused on a control path');
  });

  await t.test(
    'the cap counts BYTES not UTF-16 units: a sub-cap-length multibyte body still 413s',
    async () => {
      // 400k snowmen: .length is 400_000 UTF-16 units — comfortably UNDER
      // MAX_BODY=1e6, so a `body.length` cap would wave this straight through — but
      // 1_200_000 UTF-8 bytes, OVER the cap. Only a byte-measured cap refuses it,
      // which is the whole point of M-B3. An ASCII 'x'.repeat test cannot show this
      // (its length and byte count are equal), so it is not proof the cap is bytes.
      const text = '☃'.repeat(400_000);
      assert.ok(text.length < 1_000_000, 'sanity: the UTF-16 length is under the cap');
      assert.ok(
        Buffer.byteLength(text, 'utf8') > 1_000_000,
        'sanity: the byte length is over the cap',
      );
      const res = await raw(port, {
        method: 'POST',
        path: '/mail',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${daemon.token ?? ''}`,
        },
        parts: [JSON.stringify({ to: 'all', from: 'board', text })],
      });
      assert.equal(
        res.status,
        413,
        'a body whose BYTES exceed the cap must 413 even when its UTF-16 length does not',
      );
    },
  );

  await t.test(
    'an oversized declared Content-Length cannot wedge the keep-alive connection',
    async () => {
      // BUG-125: the declared-length 413 path used to answer and return without
      // reading the request stream or destroying it. Under keep-alive Node then
      // held the socket open waiting for a body that (here) never fully arrives —
      // a second request on the same connection timed out instead of being
      // served. The fix answers once and tears the request down; the regression
      // contract is that the connection is CLOSED after the 413 (so no client
      // can hang on it) and the daemon keeps serving fresh connections.
      //
      // Driven over a raw net.Socket, not http.request, because the contract is
      // about the SOCKET closing: bun's http *client* never fires the socket
      // 'close' event when the server tears the connection down (Node does), so
      // an http.request probe reports a false wedge under bun even though the
      // server closed correctly. A raw socket observes the server's FIN
      // identically on both runtimes, and the server side is byte-for-byte the
      // same — so this proves the one regression contract under Node and bun.
      await new Promise<void>((resolve, reject) => {
        let raw = '';
        let closed = false;
        const sock = net.connect(port, '127.0.0.1');
        const timer = setTimeout(() => {
          sock.destroy();
          reject(
            new Error(
              `server wedged the keep-alive connection (socketClosed=${String(closed)}, bytes=${String(raw.length)})`,
            ),
          );
        }, 6000); // BUG-125 under Bun.serve: the 413 body is delivered in ms, but the
        // socket FIN is floored at ~4s by uSockets' 4-second timer granularity
        // (res.shouldKeepAlive=false → server.timeout(req,1); see
        // memory bun-serve-runtime-limits). 2000 raced that FIN; 6000 clears it. The
        // contract (answered, THEN closed, daemon keeps serving) is unchanged — only
        // the accepted close latency widened from instant to a bounded ~4s.
        timer.unref();
        sock.on('connect', () => {
          // Send only a fragment of the declared body and keep the socket open —
          // the pre-fix server left the connection half-parsed at exactly this
          // point. With the fix the server answers the 413 in full AND closes the
          // socket instead of lingering on a body that never arrives.
          sock.write(
            'POST /mail HTTP/1.1\r\n' +
              // Host MUST carry the port: the daemon's Host allowlist (the
              // DNS-rebinding defence) 403s a bare `127.0.0.1`, and a 403 is a
              // normal keep-alive response that never tears the socket down — so
              // a portless Host would wedge here without ever reaching the 413.
              `Host: 127.0.0.1:${port}\r\n` +
              'Content-Type: application/json\r\n' +
              `Authorization: Bearer ${daemon.token ?? ''}\r\n` +
              'Content-Length: 2000000\r\n' + // over MAX_BODY, declared up front
              '\r\n' +
              '{"partial":',
          );
        });
        sock.on('data', (d: Buffer) => {
          raw += d.toString('utf8');
        });
        // The verdict is decided on 'close', which always follows a FIN or reset;
        // a bare 'error' sink just keeps an ECONNRESET from throwing unhandled.
        sock.on('error', () => {});
        sock.on('close', () => {
          closed = true;
          clearTimeout(timer);
          try {
            const sep = raw.indexOf('\r\n\r\n');
            assert.notEqual(sep, -1, 'the 413 response headers must be fully readable');
            const statusLine = raw.slice(0, raw.indexOf('\r\n'));
            assert.match(
              statusLine,
              /^HTTP\/1\.1 413\b/,
              `expected a 413 status line, got: ${statusLine}`,
            );
            const clMatch = /^content-length:\s*(\d+)/im.exec(raw.slice(0, sep));
            assert.ok(clMatch?.[1], 'the 413 response must declare a Content-Length');
            const bodyBytes = Buffer.byteLength(raw.slice(sep + 4), 'utf8');
            assert.equal(
              bodyBytes,
              Number(clMatch[1]),
              'the full 413 body must arrive before the socket closes',
            );
            resolve();
          } catch (err) {
            reject(err instanceof Error ? err : new Error(String(err)));
          }
        });
      });

      // And the daemon is unharmed: a fresh request is served immediately.
      const health = (await fetch(`${daemon.baseUrl}/health`).then((r) => r.json())) as {
        ok?: boolean;
      };
      assert.equal(
        health.ok,
        true,
        'the daemon must keep serving after refusing an oversized body',
      );
    },
  );
});

// ---------------------------------------------------------------------------
// FLEETDECK_TRUSTED_ORIGINS / FLEETDECK_PROXY_AUTH — the standalone/reverse-proxy
// surface. Behind a proxy (Coder, nginx, Traefik) the browser-facing Host and
// Origin are the PROXY's — Coder's reverse proxy never rewrites req.Host — so
// every wall above refuses the very traffic we are trying to serve. These knobs
// are the operator's way to say "this other origin is also me", and their whole
// job is to widen the walls by EXACTLY the named origin and not one inch more.
//
// The suite above is the no-regression contract: it runs with no trusted origin
// configured, and every case in it must keep passing unchanged.

const PROXY_ORIGIN = 'https://fd--main--ws--luis.coder.example.com';
const PROXY_HOST = 'fd--main--ws--luis.coder.example.com';

test('trusted origins widen the walls by exactly the named origin', async (t: TestContext) => {
  const daemon = await startDaemon({
    env: {
      FLEETDECK_TRUSTED_ORIGINS: PROXY_ORIGIN,
      // 'trust' keeps this suite about the WALLS; the token gate is exercised
      // separately below, so a failure here can only mean an origin bug.
      FLEETDECK_PROXY_AUTH: 'trust',
    },
  });
  t.after(() => daemon.stop());
  const { port } = daemon;

  await t.test('a POST from the trusted origin is allowed', async () => {
    // 0.16.0: PROXY_AUTH=trust clears the AUTH wall; POST /mail is additionally
    // bearer-gated as a route, so the proxied board still presents the token.
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: {
        ...JSON_CT,
        origin: PROXY_ORIGIN,
        host: PROXY_HOST,
        authorization: `Bearer ${daemon.token ?? ''}`,
      },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'through the proxy' })],
    });
    assert.equal(
      res.status,
      200,
      'the whole point: the board behind the proxy must be able to drive the fleet',
    );
  });

  await t.test('an untrusted origin is STILL refused', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: {
        ...JSON_CT,
        origin: 'https://evil.example',
        host: PROXY_HOST,
        authorization: `Bearer ${daemon.token ?? ''}`,
      },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'nope' })],
    });
    assert.equal(res.status, 403, 'trusting one origin must not trust every origin');
  });

  await t.test('the scheme is part of the origin: http:// is not https://', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: {
        ...JSON_CT,
        origin: `http://${PROXY_HOST}`,
        host: PROXY_HOST,
        authorization: `Bearer ${daemon.token ?? ''}`,
      },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'downgraded' })],
    });
    assert.equal(res.status, 403, 'naming an https origin must not also trust its http twin');
  });

  await t.test('the proxy Host passes the DNS-rebinding wall on a data route', async () => {
    const res = await raw(port, { path: '/state', headers: { host: PROXY_HOST } });
    assert.equal(res.status, 200);
  });

  await t.test('an unnamed Host is still refused on a data route', async () => {
    const res = await raw(port, { path: '/state', headers: { host: 'evil.example' } });
    assert.equal(res.status, 403, 'the rebinding wall must only open for the operator-named host');
  });

  await t.test(
    'both WS upgrades accept the trusted origin and refuse an untrusted one',
    async () => {
      assert.equal(
        await rawUpgrade(port, {
          path: '/ws',
          headers: { host: PROXY_HOST, origin: PROXY_ORIGIN },
        }),
        'upgraded',
        'the board behind a proxy cannot work without its snapshot socket',
      );
      assert.equal(
        await rawUpgrade(port, {
          path: '/ws',
          headers: { host: PROXY_HOST, origin: 'https://evil.example' },
        }),
        'rejected',
      );
      // /ws/term is the one that can DRIVE a pane, so it matters most.
      assert.equal(
        await rawUpgrade(port, {
          path: '/ws/term?spawn=nope',
          headers: { host: PROXY_HOST, origin: 'https://evil.example' },
        }),
        'rejected',
      );
    },
  );

  await t.test('a local CLI hook (no Origin) still sails through', async () => {
    const res = await raw(port, {
      method: 'POST',
      path: '/mail',
      headers: { ...JSON_CT, authorization: `Bearer ${daemon.token ?? ''}` },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'from the hook' })],
    });
    assert.equal(
      res.status,
      200,
      'the hook path must never be collateral damage of a proxy setting',
    );
  });
});

test('a wildcard trusted origin matches exactly one label', async (t: TestContext) => {
  const daemon = await startDaemon({
    env: {
      FLEETDECK_TRUSTED_ORIGINS: 'https://*.coder.example.com',
      FLEETDECK_PROXY_AUTH: 'trust',
    },
  });
  t.after(() => daemon.stop());
  const { port } = daemon;

  const post = (origin: string, host: string) =>
    raw(port, {
      method: 'POST',
      path: '/mail',
      headers: { ...JSON_CT, origin, host, authorization: `Bearer ${daemon.token ?? ''}` },
      parts: [JSON.stringify({ to: 'all', from: 'board', text: 'x' })],
    });

  await t.test('one label matches', async () => {
    const res = await post(
      'https://fd--main--ws--luis.coder.example.com',
      'fd--main--ws--luis.coder.example.com',
    );
    assert.equal(res.status, 200);
  });

  await t.test('the bare apex does NOT match', async () => {
    const res = await post('https://coder.example.com', 'coder.example.com');
    assert.equal(res.status, 403, '*.example.com must not silently include example.com itself');
  });

  await t.test('two labels do NOT match', async () => {
    const res = await post('https://a.b.coder.example.com', 'a.b.coder.example.com');
    assert.equal(res.status, 403, 'a single-label wildcard must not become a subtree wildcard');
  });

  await t.test('a lookalike suffix does NOT match', async () => {
    const res = await post('https://evilcoder.example.com', 'evilcoder.example.com');
    assert.equal(res.status, 403, 'suffix matching must be label-aware, not a naive endsWith');
  });
});

test('FLEETDECK_PROXY_AUTH=token still demands the token from a proxied browser', async (t: TestContext) => {
  const TOKEN = 'x'.repeat(32);
  const daemon = await startDaemon({
    env: {
      FLEETDECK_TRUSTED_ORIGINS: PROXY_ORIGIN,
      FLEETDECK_TOKEN: TOKEN,
      // 'token' is the DEFAULT — pinned explicitly so a future default flip
      // cannot quietly turn this suite into a no-op.
      FLEETDECK_PROXY_AUTH: 'token',
    },
  });
  t.after(() => daemon.stop());
  const { port } = daemon;

  await t.test('a proxied read with no token is refused despite the trusted origin', async () => {
    const res = await raw(port, {
      path: '/state',
      headers: { host: PROXY_HOST, origin: PROXY_ORIGIN },
    });
    assert.equal(res.status, 401, 'clearing the CSRF wall is not the same as being authenticated');
  });

  await t.test('a proxied read WITH the token is allowed', async () => {
    const res = await raw(port, {
      path: '/state',
      headers: { host: PROXY_HOST, origin: PROXY_ORIGIN, authorization: `Bearer ${TOKEN}` },
    });
    assert.equal(res.status, 200);
  });

  await t.test(
    'the local hook path (no Origin) is NOT dragged into the PROXY token gate',
    async () => {
      // 0.16.0: POST /mail itself is bearer-gated on loopback too (the shims
      // carry $FLEETDECK_HOME/token) — what this pins is that PROXY_AUTH=token
      // does not drag the local path into the PROXY's token (FLEETDECK_TOKEN).
      const res = await raw(port, {
        method: 'POST',
        path: '/mail',
        headers: { ...JSON_CT, authorization: `Bearer ${daemon.token ?? ''}` },
        parts: [JSON.stringify({ to: 'all', from: 'board', text: 'hook' })],
      });
      assert.equal(
        res.status,
        200,
        'turning on proxy auth must not break every local hook on the box',
      );
    },
  );

  await t.test('a same-origin loopback board still needs no token', async () => {
    const res = await raw(port, {
      path: '/state',
      headers: { origin: `http://127.0.0.1:${port}` },
    });
    assert.equal(res.status, 200, 'the loopback exemption survives: proxy auth is about the PROXY');
  });
});
