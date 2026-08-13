// tests/http-stall.test.ts
//
// A1 RESHAPE regression gate. Bun.serve runs with idleTimeout:0 (deliberate —
// a held long-poll response up to ~600s must survive, and close()→stop(true)
// depends on it). That makes a STALLED request body immortal: an in-zone peer
// can POST /hook/Stop with Content-Length: 200, send 50 bytes, and hold the
// socket forever — 'end' never fires, drainThenRespond's grace timer merely
// adopts a promise that never settles, and nothing reaps it.
//
// The fix (HttpResShim.boundStalledDrain) arms a per-request idle FIN ONLY when
// the body-drain grace (BODY_DRAIN_GRACE_MS) expires with the body still
// un-drained. It keys on drain STALL, never on response latency, so a body that
// actually completes is never touched — held holdHook/watchHook long-polls stay
// exempt by construction (their bodies drain in ms, so their grace never fires).
//
// Both cases run against a REAL daemon started via the shared helper, driven
// over a raw node:net socket (the only way to declare a Content-Length then
// withhold the body — fetch/http.request always complete or abort the body as a
// unit). FLEETDECK_STALL_FIN_S=4 shrinks the 120s production bound so the reap
// is observable inside a test window; uSockets' timer granularity is coarse, so
// the observed close lands a few seconds past the nominal 4s.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import net from 'node:net';
import { randomUUID } from 'node:crypto';

import { startDaemon } from './helpers/daemon.ts';
import { scaleMs } from './helpers/wait.ts';

// The fixed, valid header block shared by both cases: same route, Host (the
// DNS-rebinding wall 403s a portless 127.0.0.1), Content-Type, and bearer, so
// the ONLY difference between "reaped" and "answered" is whether the declared
// body is ever completed.
function headerBlock(port: number, token: string, contentLength: number): string {
  return (
    'POST /hook/Stop HTTP/1.1\r\n' +
    `Host: 127.0.0.1:${port}\r\n` +
    'Content-Type: application/json\r\n' +
    `Authorization: Bearer ${token}\r\n` +
    `Content-Length: ${contentLength}\r\n` +
    '\r\n'
  );
}

test('A1: a stalled request body is reaped; a completed body still responds', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_STALL_FIN_S: '4' } });
  t.after(() => daemon.stop());
  const { port } = daemon;
  const token = daemon.token ?? '';

  await t.test('a stalled body (declares 200, sends ~50) is FINed by the daemon', async () => {
    // Declare 200 bytes, send ~50 and hold. 'end' never fires, so the handler
    // never runs and res.done never resolves; the grace timer (1s) fires with
    // the body un-drained and boundStalledDrain arms the ~4s FIN. The verdict is
    // the socket CLOSING — a leak would hang here until the fail deadline.
    await new Promise<void>((resolve, reject) => {
      const sock = net.connect(port, '127.0.0.1');
      let closed = false;
      const done = (): void => {
        if (closed) return;
        closed = true;
        clearTimeout(fail);
        resolve();
      };
      // Generous: the FIN is armed ~1s in with a 4s idle bound, and uSockets'
      // coarse timer wheel can defer the actual close a few more seconds. A leak
      // never closes at all, so this deadline is what fails the test.
      const fail = setTimeout(() => {
        if (closed) return;
        closed = true;
        sock.destroy();
        reject(new Error('the daemon never reaped the stalled-body socket (idleTimeout:0 leak)'));
      }, scaleMs(15000));
      fail.unref();
      sock.setNoDelay(true);
      sock.on('connect', () => {
        sock.write(headerBlock(port, token, 200));
        sock.write(`{"session_id":"${'a'.repeat(35)}`); // 50 bytes, far short of 200
      });
      // A stalled hook produces no response, so any 'data' before the FIN would
      // be a surprise; the verdict is decided on 'end'/'close' either way.
      sock.on('data', () => {});
      sock.on('error', () => {}); // ECONNRESET on the server FIN is expected, not a failure
      sock.on('end', done);
      sock.on('close', done);
    });
  });

  await t.test(
    'a body that completes still gets its 200 (FIN keys on stall, not on every body)',
    async () => {
      // Same partial write, then after a short pause (well under the 1s grace)
      // send the rest to reach Content-Length. The body drains, the grace timer is
      // cleared before it can fire, boundStalledDrain is never armed, and the hook
      // answers 200 normally.
      const body = Buffer.from(
        JSON.stringify({ session_id: randomUUID(), stop_hook_active: false }),
        'utf8',
      );
      const cut = Math.floor(body.length / 2);
      await new Promise<void>((resolve, reject) => {
        const sock = net.connect(port, '127.0.0.1');
        let raw = '';
        let settled = false;
        const fail = setTimeout(() => {
          if (settled) return;
          settled = true;
          sock.destroy();
          reject(new Error(`a completed hook body never answered; got: ${JSON.stringify(raw)}`));
        }, scaleMs(15000));
        fail.unref();
        sock.setNoDelay(true);
        sock.on('connect', () => {
          sock.write(headerBlock(port, token, body.length));
          sock.write(body.subarray(0, cut)); // partial — same stall as case 1...
          const rest = setTimeout(() => {
            sock.write(body.subarray(cut)); // ...but completed before the grace fires
          }, scaleMs(250));
          rest.unref();
        });
        sock.on('data', (d: Buffer) => {
          raw += d.toString('utf8');
          if (settled) return;
          const sep = raw.indexOf('\r\n\r\n');
          if (sep === -1) return;
          const clMatch = /^content-length:\s*(\d+)/im.exec(raw.slice(0, sep));
          if (!clMatch?.[1]) return;
          if (Buffer.byteLength(raw.slice(sep + 4), 'utf8') < Number(clMatch[1])) return; // body still arriving
          settled = true;
          clearTimeout(fail);
          try {
            const statusLine = raw.slice(0, raw.indexOf('\r\n'));
            assert.match(
              statusLine,
              /^HTTP\/1\.1 200\b/,
              `a completed hook body must answer 200, got: ${statusLine}`,
            );
            resolve();
          } catch (err) {
            reject(err instanceof Error ? err : new Error(String(err)));
          } finally {
            sock.destroy();
          }
        });
        sock.on('error', (e) => {
          if (settled) return;
          settled = true;
          clearTimeout(fail);
          reject(e);
        });
      });
    },
  );
});
