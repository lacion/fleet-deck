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
// Cases 1-2 run against a REAL daemon over a raw node:net socket (the only way
// to declare a Content-Length then withhold the body — fetch/http.request always
// complete or abort the body as a unit). Case 3 is the mirror image: a normal
// held long-poll (GET /api/watch) whose body drains instantly, asserting the
// stall-FIN is NEVER armed on it. These three share one daemon at
// FLEETDECK_STALL_FIN_S=4, which shrinks the 120s production bound so the reap
// (case 1) is observable inside a test window AND a wrongful FIN on the held
// watch (case 3) would fire well before its 10s hold; uSockets' timer
// granularity is coarse, so an observed close lands a few seconds past nominal.
//
// The retraction guard (arm-then-complete) is a SEPARATE top-level test on its
// OWN daemon at FLEETDECK_STALL_FIN_S=12. The stall-FIN is a uSockets idle
// timeout quantized to a coarse ~4s GLOBAL timer wheel, so server.timeout(req,N)
// fires anywhere in a ~N±4s window by the wheel's phase — and a preceding held
// long-poll advances that phase. At N=4 (one tick) a 2s-late body races a FIN
// that can fire as early as ~2s and flakes; at N=12 the earliest FIN is a stable
// ~10s after the last inbound byte (measured), so a 2s-late body beats it by ~8s
// regardless of phase while a regression's un-retracted FIN still fires ~12s in.
// See that test for the full rationale.
//
// The last three tests cover option C — the SIBLING gap the stall-FIN does not
// reach. boundStalledDrain bounds a request WHILE its body is in flight; it does
// nothing for the BETWEEN-requests keep-alive-idle phase (a completed request,
// keep-alive 200, then a silent socket), which idleTimeout:0 leaves immortal.
// HttpResShim.end arms a KEEPALIVE_FIN_S idle FIN as each response completes, and
// fetchHandler clears it at the top of the next request so a reused socket runs
// unbounded again. The first C test proves the reap (a completed /health then
// idle is FINed); the second proves the entry-clear exemption (a held /api/watch
// issued as a SECOND request on the same socket is NOT severed by req1's FIN).
// The third guards the refusal bypass: a WS upgrade that reaches srv.upgrade() and
// FAILS (garbage Sec-WebSocket-Key) returns 400 WITHOUT going through end(), and a
// failed upgrade attempt disarms Bun's ~12s linger reaper — so handleUpgrade's
// refuse() must arm the FIN itself or that 400 leaks an fd forever. All three run
// at FLEETDECK_KEEPALIVE_FIN_S=4 to make the ~4s bound observable in a test
// window; the same uSockets ~4s wheel coarseness applies.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import net from 'node:net';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { startDaemon } from './helpers/daemon.ts';
import { getJson, postHook } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
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

// A valid keep-alive GET (bodyless, so it completes the instant it arrives and
// end() runs — arming C's keep-alive-idle FIN). Same Host wall + bearer as
// headerBlock; `target` selects the route (/health, /api/watch?...). Public
// routes ignore the bearer, authed ones require it, so attaching it always is
// safe and lets one socket pipeline both.
function getBlock(port: number, token: string, target: string): string {
  return (
    `GET ${target} HTTP/1.1\r\n` +
    `Host: 127.0.0.1:${port}\r\n` +
    `Authorization: Bearer ${token}\r\n` +
    'Connection: keep-alive\r\n' +
    '\r\n'
  );
}

// Return the exact Content-Length body from a raw HTTP response buffer once it
// has fully arrived, else null (headers incomplete / body still streaming).
// Slices to CL so a pipelined next response's leading bytes never bleed in.
function completeBody(raw: string): string | null {
  const sep = raw.indexOf('\r\n\r\n');
  if (sep === -1) return null;
  const clMatch = /^content-length:\s*(\d+)/im.exec(raw.slice(0, sep));
  if (!clMatch?.[1]) return null;
  const body = raw.slice(sep + 4);
  const len = Number(clMatch[1]);
  return Buffer.byteLength(body, 'utf8') < len ? null : body.slice(0, len);
}

function statusLineOf(raw: string): string {
  return raw.slice(0, raw.indexOf('\r\n'));
}

// A WS-upgrade handshake that REACHES srv.upgrade() and fails it: /ws on loopback
// is auth-exempt (no bearer needed), the Upgrade/Connection headers route it to
// handleUpgrade, and a garbage Sec-WebSocket-Key makes srv.upgrade() return false
// → handleUpgrade's refuse(400). That 400 bypasses HttpResShim.end(), so its
// socket is reaped only if refuse() arms the keep-alive FIN itself.
function failedUpgradeBlock(port: number): string {
  return (
    'GET /ws HTTP/1.1\r\n' +
    `Host: 127.0.0.1:${port}\r\n` +
    'Upgrade: websocket\r\n' +
    'Connection: upgrade\r\n' +
    'Sec-WebSocket-Key: x\r\n' +
    'Sec-WebSocket-Version: 13\r\n' +
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
          // ...but completed before the grace fires. Fixed 250ms, NOT scaleMs:
          // BODY_DRAIN_GRACE_MS is a fixed 1000ms wall-clock on the daemon, so
          // scaling this up (250×4 on a loaded CI) could push completion PAST the
          // grace and silently degrade this case into arm-then-complete coverage
          // (it would still 200 — the grace resolves the held response — just no
          // longer proving the pre-grace path). 250ms stays comfortably under 1000ms.
          const rest = setTimeout(() => {
            sock.write(body.subarray(cut));
          }, 250);
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

  await t.test(
    'a held /api/watch long-poll survives PAST the stall-FIN bound (exempt by construction)',
    async () => {
      // The stall-FIN keys on drain STALL, never on response latency, so a
      // legitimately held long-poll — whose bodyless GET drains instantly — must
      // never be armed. This daemon runs FLEETDECK_STALL_FIN_S=4, so a regression
      // that armed the bound on a held response would sever this socket at ~5s;
      // holding 10s makes that failure loud. A LIVE session is required first:
      // watchHook short-circuits an unknown session to an immediate 200 idle
      // (session_alive=false) and never holds, which would vacuously pass.
      const cwd = mkdtempSync(path.join(tmpdir(), 'fd-stall-watch-'));
      try {
        const sid = randomUUID();
        await postHook(
          daemon.baseUrl,
          'SessionStart',
          loadFixture('session-start', { session_id: sid, cwd }),
          { token: daemon.token },
        );
        // hold_ms=10000 >> the 4s FIN bound. getJson's client abort is only a
        // hang backstop; a wrongful FIN rejects the fetch well before it fires. A
        // correct hold resolves at ~10s with 200 { status: 'idle' }.
        const res = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=10000`, {
          token: daemon.token,
          timeout: scaleMs(20000),
        });
        assert.equal(res.status, 200, 'a held watch must answer 200, not be FINed mid-hold');
        assert.equal(
          (res.json as { status?: string }).status,
          'idle',
          'a held watch with no activity must lapse to idle, not be severed',
        );
      } finally {
        rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      }
    },
  );
});

test('A1 retraction: a body that drains PAST the grace retracts the stall-FIN (arm-then-complete)', async (t: TestContext) => {
  // The direct regression guard for clearStalledFin, on its OWN daemon at
  // FLEETDECK_STALL_FIN_S=12 (NOT the 4 the cases above share) — see the file
  // header for why: the stall-FIN is quantized to a coarse ~4s global timer
  // wheel, so a 2s-late body races a wheel-phase-dependent FIN. N=4 flakes
  // (earliest FIN ~2s); N=12 puts the earliest FIN a stable ~10s out, giving the
  // 2s-late body an ~8s margin while a regression's un-retracted FIN still fires
  // ~12s in (2s-late body + ~10s bound) — caught by the survive-watch.
  const daemon = await startDaemon({ env: { FLEETDECK_STALL_FIN_S: '12' } });
  t.after(() => daemon.stop());
  const { port } = daemon;
  const token = daemon.token ?? '';

  // A real upload arrives slowly: partial write, then the rest ~2s later — PAST
  // the 1s grace, so the grace FIRES and boundStalledDrain arms the stall FIN, and
  // THEN the body completes. clearStalledFin must retract that FIN; otherwise the
  // now-answered keep-alive socket is reaped ~12s later even though nothing
  // actually stalled. Verdict: the 200 arrives AND the socket survives past the
  // bound. (Delete the clearStalledFin() call in drainThenRespond and this fails:
  // the answered socket is severed by the un-retracted FIN.)
  const body = Buffer.from(
    JSON.stringify({ session_id: randomUUID(), stop_hook_active: false }),
    'utf8',
  );
  const cut = Math.floor(body.length / 2);
  await new Promise<void>((resolve, reject) => {
    const sock = net.connect(port, '127.0.0.1');
    let raw = '';
    let answered = false;
    let done = false;
    let survive: ReturnType<typeof setTimeout> | undefined;
    const settle = (err?: Error): void => {
      if (done) return;
      done = true;
      clearTimeout(fail);
      if (survive) clearTimeout(survive);
      sock.destroy();
      if (err) reject(err);
      else resolve();
    };
    // Overall backstop — only fires if nothing happens at all; the real verdicts
    // come from the 'data' (200) and 'close' (early reap) handlers.
    const fail = setTimeout(
      () => settle(new Error(`arm-then-complete: no 200 seen; got ${JSON.stringify(raw)}`)),
      scaleMs(30000),
    );
    fail.unref();
    sock.setNoDelay(true);
    sock.on('connect', () => {
      sock.write(headerBlock(port, token, body.length));
      sock.write(body.subarray(0, cut)); // partial — stalls the drain...
      // ...held ~2s, PAST the fixed 1000ms grace, so the FIN is armed, THEN
      // completed. Fixed 2000ms (NOT scaleMs): it must clear the daemon's fixed
      // 1000ms grace yet stay well under the ~10s FIN — the completion is what
      // proves retraction, not precise timing.
      const rest = setTimeout(() => sock.write(body.subarray(cut)), 2000);
      rest.unref();
    });
    sock.on('data', (d: Buffer) => {
      raw += d.toString('utf8');
      if (answered) return;
      const sep = raw.indexOf('\r\n\r\n');
      if (sep === -1) return;
      const clMatch = /^content-length:\s*(\d+)/im.exec(raw.slice(0, sep));
      if (!clMatch?.[1]) return;
      if (Buffer.byteLength(raw.slice(sep + 4), 'utf8') < Number(clMatch[1])) return;
      const statusLine = raw.slice(0, raw.indexOf('\r\n'));
      try {
        assert.match(
          statusLine,
          /^HTTP\/1\.1 200\b/,
          `arm-then-complete must answer 200, got: ${statusLine}`,
        );
      } catch (err) {
        settle(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      answered = true;
      // The 200 arrived. On CORRECT code the FIN was retracted (idleTimeout:0),
      // so the socket lives; a regression that dropped clearStalledFin leaves the
      // stall FIN armed and it reaps this answered socket ~12s in. Watch past that
      // — survival is the pass.
      survive = setTimeout(() => settle(), scaleMs(20000));
      survive.unref();
    });
    sock.on('error', () => {}); // a mid-body reset surfaces as 'close' below
    sock.on('close', () => {
      if (done) return;
      settle(
        new Error(
          answered
            ? 'the answered socket was reaped by an un-retracted stall FIN (clearStalledFin regression)'
            : 'socket closed before the 200 (the grace/FIN severed a real slow upload)',
        ),
      );
    });
  });
});

test('C: a completed keep-alive request that then idles is reaped (keep-alive-idle FIN)', async (t: TestContext) => {
  // idleTimeout:0 makes the BETWEEN-requests keep-alive-idle phase immortal: a
  // client completes one request, gets its keep-alive 200, then holds the socket
  // silent forever. boundStalledDrain never covers this (no in-flight body), so
  // HttpResShim.end arms a per-request KEEPALIVE_FIN_S idle FIN as the response
  // completes. This daemon runs FLEETDECK_KEEPALIVE_FIN_S=4 so the reap is
  // observable in-window; the verdict is the socket CLOSING after it answered —
  // a leak (no end-arm) never closes and hangs to the fail deadline.
  const daemon = await startDaemon({ env: { FLEETDECK_KEEPALIVE_FIN_S: '4' } });
  t.after(() => daemon.stop());
  const { port } = daemon;
  const token = daemon.token ?? '';

  await new Promise<void>((resolve, reject) => {
    const sock = net.connect(port, '127.0.0.1');
    let raw = '';
    let answered = false;
    let closed = false;
    const settle = (err?: Error): void => {
      if (closed) return;
      closed = true;
      clearTimeout(fail);
      sock.destroy();
      if (err) reject(err);
      else resolve();
    };
    // A leak never closes; this deadline is what fails the test. The FIN is armed
    // as /health answers (~ms) with a 4s idle bound, and uSockets' coarse wheel
    // can defer the actual close a few more seconds — 15s clears both.
    const fail = setTimeout(
      () =>
        settle(new Error('the daemon never reaped the idle keep-alive socket (C end-arm leak)')),
      scaleMs(15000),
    );
    fail.unref();
    sock.setNoDelay(true);
    sock.on('connect', () => sock.write(getBlock(port, token, '/health')));
    sock.on('data', (d: Buffer) => {
      raw += d.toString('utf8');
      if (answered) return; // answered → now just wait for the idle FIN to close it
      const body = completeBody(raw);
      if (body === null) return;
      const statusLine = statusLineOf(raw);
      try {
        assert.match(
          statusLine,
          /^HTTP\/1\.1 200\b/,
          `/health must answer 200, got: ${statusLine}`,
        );
      } catch (err) {
        settle(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      answered = true;
      // The 200 landed; the socket is now idle keep-alive. Send NOTHING more —
      // C's end-arm must FIN it within the window. 'close' below is the pass.
    });
    sock.on('error', () => {}); // the server FIN surfaces as 'close'
    sock.on('close', () => {
      if (closed) return;
      if (!answered) {
        settle(new Error('socket closed before /health answered 200'));
        return;
      }
      settle(); // reaped after answering — the pass
    });
  });
});

test('C: a held long-poll on a REUSED keep-alive socket survives past the keep-alive FIN', async (t: TestContext) => {
  // C arms the keep-alive-idle FIN at end() and CLEARS it at fetchHandler entry,
  // so a second request on a reused socket runs unbounded again. The regression
  // this guards: a held /api/watch issued right after a normal request on the
  // same socket must NOT inherit req1's short keep-alive FIN and be severed
  // mid-hold. Daemon at FLEETDECK_KEEPALIVE_FIN_S=4; the watch holds 10s, so a
  // wrongful sever (~4s after req1's arm) fails loudly while a correct entry-clear
  // lets it lapse to idle at ~10s. A LIVE session is required first — watchHook
  // short-circuits an unknown session to an immediate idle 200 and never holds,
  // which would pass vacuously.
  const daemon = await startDaemon({ env: { FLEETDECK_KEEPALIVE_FIN_S: '4' } });
  t.after(() => daemon.stop());
  const { port } = daemon;
  const token = daemon.token ?? '';

  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-keepalive-watch-'));
  const sid = randomUUID();
  try {
    await postHook(
      daemon.baseUrl,
      'SessionStart',
      loadFixture('session-start', { session_id: sid, cwd }),
      { token: daemon.token },
    );
    await new Promise<void>((resolve, reject) => {
      const sock = net.connect(port, '127.0.0.1');
      let raw = '';
      let phase: 'health' | 'watch' = 'health';
      let done = false;
      const settle = (err?: Error): void => {
        if (done) return;
        done = true;
        clearTimeout(fail);
        sock.destroy();
        if (err) reject(err);
        else resolve();
      };
      const fail = setTimeout(
        () =>
          settle(
            new Error(
              `reused-socket held watch: no idle 200 (phase=${phase}); got ${JSON.stringify(raw)}`,
            ),
          ),
        scaleMs(20000),
      );
      fail.unref();
      sock.setNoDelay(true);
      sock.on('connect', () => sock.write(getBlock(port, token, '/health')));
      sock.on('data', (d: Buffer) => {
        raw += d.toString('utf8');
        if (done) return;
        const body = completeBody(raw);
        if (body === null) return;
        if (phase === 'health') {
          // req1's 200 landed → end() armed the 4s keep-alive FIN. IMMEDIATELY
          // pipeline the held watch on the SAME socket; the entry-clear must
          // protect it. Reset the buffer so only the watch response is parsed.
          const statusLine = statusLineOf(raw);
          if (!/^HTTP\/1\.1 200\b/.test(statusLine)) {
            settle(new Error(`/health must answer 200, got: ${statusLine}`));
            return;
          }
          phase = 'watch';
          raw = '';
          sock.write(getBlock(port, token, `/api/watch?session=${sid}&hold_ms=10000`));
          return;
        }
        // phase === 'watch': the held response arrived — it SURVIVED the FIN.
        try {
          const parsed = JSON.parse(body) as { status?: string };
          assert.equal(
            parsed.status,
            'idle',
            `a held watch on a reused socket must lapse to idle, got: ${body}`,
          );
          settle();
        } catch (err) {
          settle(err instanceof Error ? err : new Error(String(err)));
        }
      });
      sock.on('error', () => {}); // a mid-hold sever surfaces as 'close' below
      sock.on('close', () => {
        if (done) return;
        settle(
          new Error(
            phase === 'watch'
              ? 'the reused socket was severed mid-hold — req1 keep-alive FIN leaked past the entry-clear'
              : 'socket closed during /health, before the held watch was issued',
          ),
        );
      });
    });
  } finally {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
});

test('C: a refused WS upgrade (bypasses end()) is reaped by handleUpgrade refuse()', async (t: TestContext) => {
  // The keep-alive FIN is armed in HttpResShim.end(), but the WS-upgrade refusals
  // in handleUpgrade never construct a shim — they return `new Response(null,...)`
  // directly. Worse, an ATTEMPTED-and-failed srv.upgrade() disarms Bun's fixed
  // ~12s linger reaper (probed on bun 1.3.14), so pre-fix that 400's socket sat
  // IMMORTAL under idleTimeout:0 — the exact leak class end()'s FIN closes, reached
  // through a different door. handleUpgrade's refuse() now arms KEEPALIVE_FIN_S on
  // the way out. This daemon runs FLEETDECK_KEEPALIVE_FIN_S=4; the verdict is the
  // socket CLOSING after its 400 — a leak never closes and hangs to the deadline.
  const daemon = await startDaemon({ env: { FLEETDECK_KEEPALIVE_FIN_S: '4' } });
  t.after(() => daemon.stop());
  const { port } = daemon;

  await new Promise<void>((resolve, reject) => {
    const sock = net.connect(port, '127.0.0.1');
    let raw = '';
    let refused = false;
    let done = false;
    const settle = (err?: Error): void => {
      if (done) return;
      done = true;
      clearTimeout(fail);
      sock.destroy();
      if (err) reject(err);
      else resolve();
    };
    // A leak never closes; this deadline fails the test. The FIN is armed as the
    // 400 is written (~ms) with a 4s idle bound, and uSockets' coarse wheel can
    // defer the actual close a few more seconds — 15s clears both.
    const fail = setTimeout(
      () =>
        settle(new Error('the daemon never reaped the refused-upgrade socket (C refuse() leak)')),
      scaleMs(15000),
    );
    fail.unref();
    sock.setNoDelay(true);
    sock.on('connect', () => sock.write(failedUpgradeBlock(port)));
    sock.on('data', (d: Buffer) => {
      raw += d.toString('utf8');
      if (refused || !raw.includes('\r\n')) return;
      // 101 = upgrade succeeded (wrong key would not); 401 = auth refusal (a
      // different bypass); we want the srv.upgrade()-FAILED 400 — the path this
      // guards, which bypasses end() AND disarms Bun's linger reaper.
      const statusLine = statusLineOf(raw);
      if (!/^HTTP\/1\.1 400\b/.test(statusLine)) {
        settle(new Error(`expected a 400 refused upgrade, got: ${statusLine}`));
        return;
      }
      refused = true;
      // The 400 landed; the socket is now idle. Send NOTHING more — refuse()'s
      // keep-alive FIN must reap it. 'close' below is the pass.
    });
    sock.on('error', () => {}); // the server FIN surfaces as 'close'
    sock.on('close', () => {
      if (done) return;
      if (!refused) {
        settle(new Error('socket closed before the 400 refusal was seen'));
        return;
      }
      settle(); // reaped after refusing — the pass
    });
  });
});
