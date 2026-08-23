// tests/p6-hook-failopen-contract.test.ts
//
// FAIL-OPEN CONTRACT TEST for the daemon's hooks route group — the LAST and most
// sensitive P6.4 Effect-migration conversion. The fail-open `200 {}` contract is
// the #1 drift risk of that conversion: `/hook/*` must NEVER 401, NEVER surface
// an error into Claude, and NEVER leak a non-`{}` body — every refusal, however
// caused, is HTTP 200 with the byte-exact body `{}` and the daemon stays up. This
// file exists so the conversion worker can prove, mechanically, that the wire
// behaviour is byte-preserved BEFORE any hook route is rewritten.
//
// It is a CHARACTERIZATION test: it pins what HEAD (a1ea6020) actually does, not
// what any spec says. Where an observed byte diverged from a §5 matrix row the
// rule is to pin the OBSERVED byte and report the discrepancy — never edit the
// matrix, never weaken the assertion. No source is touched.
//
// Each fail-open assertion checks the full wire response: status === 200, body
// text === '{}', parsed body deepEqual {}, and the header trio json() pins on
// EVERY JSON response — content-type: application/json, content-length: '2' (the
// two bytes of `{}`), x-content-type-options: nosniff — then confirms the daemon
// SURVIVED via a follow-up GET /health (ok:true). The contrast sentinels (Test 3)
// pin the fail-open BOUNDARY: sibling non-hook conditions that must NOT fail open.
//
// ─────────────────────────────────────────────────────────────────────────────
// COVERAGE MAP — docs/v1/evidence/effect/p6-http-matrix.md §5 (fail-open inventory)
// Every §5 row is COVERED by a named test below, or SKIP'd with a one-line
// justification emitted as a visible `test.skip(...)` so the map is verifiable by
// running this file, not just by reading it.
//
//   §5 CSRF wall fail on a hook POST        (http.ts @2045-2049) → Test 1 "csrf-wall"
//   §5 Host-wall fail on a hook             (http.ts @1904-1908) → Test 1 "host-wall"
//   §5 Oversized hook body (refuseOversize) (http.ts @2079-2091) → Test 1 "oversize"
//   §5 Bad JSON on a hook                   (http.ts @2110-2114) → Test 1 "bad-json"
//   §5 Tokenless / wrong-token hook         (http.ts @2123-2126,
//                                             silentHookRefusal @1460) → Test 1 "tokenless" + "wrong-token"
//   §5 PermissionRequest+AskUserQuestion    (http.ts @2142-2149) → Test 1 "permission-pairing"
//   §5 Unknown hook event name              (http.ts @2159-2164) → Test 1 "unknown-name"
//   §5 validateHookEvent failure            (http.ts @2175-2178) → Test 1 "validate-fail-*" (×4)
//   §5 Known hook handler returns {}        (http.ts @2179, handlers @1650-1684)
//                                                                → Test 1 "known-handler-*" (×3)
//   §5 holdHook null-row / quiescing / intake-error (http.ts @1691-1724)
//        · null-row edge (relay off → hookHoldQuestion null) → Test 2 "holdhook-nullrow-*" (×3)
//        · quiescing sub-edge                                → SKIP (mid-shutdown race)
//        · intake-error sub-edge                             → SKIP (source-level fault injection)
//   §5 POST inner catch on /hook/*          (http.ts @2698-2707) → SKIP (source-level fault injection)
//   §5 Outer handler catch on /hook/*       (http.ts @2713-2720) → SKIP (source-level fault injection)
//   §5 fetchHandler quiescing, hook path    (http.ts @3110-3121) → SKIP (mid-quiesce admission race)
//
// CONTRAST SENTINELS — §1b (the fail-open BOUNDARY: these must NOT fail open):
//   401 unauthorized  → Test 3 "sentinel-401"  (POST /api/spawn/arm-unsupervised, no bearer)
//   403 host wall     → Test 3 "sentinel-403-host" (GET /state, forged Host)
//   403 CSRF wall     → Test 3 "sentinel-403-csrf" (POST /command, sec-fetch-site cross-site)
//   415 content-type  → Test 3 "sentinel-415"  (POST /command, text/plain)
//   400 bad json      → Test 3 "sentinel-400"  (POST /command, non-JSON body)
//   413 oversize      → Test 3 "sentinel-413"  (POST /command, >MAX_BODY body)
//   500 non-hook      → SKIP (source-level fault injection; no wire trigger at HEAD)
// ─────────────────────────────────────────────────────────────────────────────

import assert from 'node:assert/strict';
import http from 'node:http';
import test, { type TestContext } from './helpers/harness-test.ts';
import { startDaemon } from './helpers/daemon.ts';
import { scaleMs } from './helpers/wait.ts';

interface RawResult {
  status: number;
  text: string;
  headers: http.IncomingHttpHeaders;
}

interface RawOpts {
  port: number;
  path: string;
  method?: string;
  headers?: http.OutgoingHttpHeaders;
  body?: string;
  timeoutMs?: number;
}

// Low-level HTTP client. The suite's shared helpers (rawRequest/postJson/postHook)
// deliberately drop response headers; this contract has to assert the header trio
// AND forge Host / content-type / sec-fetch-site / Authorization uniformly, so it
// carries its own node:http sender. It always dials 127.0.0.1 (a forged Host lives
// only in the Host HEADER, never the connect target) and tolerates a mid-upload
// write error once the response has begun — the oversized-body cases get an early
// 200/413 while the 1.1 MB body is still streaming, and node reports the reset as a
// request 'error' we must swallow rather than surface.
function raw(opts: RawOpts): Promise<RawResult> {
  const { port, path, method = 'GET', headers = {}, body, timeoutMs = 10_000 } = opts;
  return new Promise<RawResult>((resolve, reject) => {
    let settled = false;
    let responseBegan = false;
    let timer: ReturnType<typeof setTimeout>;
    const finish = (fn: () => void): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      fn();
    };
    const req = http.request({ host: '127.0.0.1', port, path, method, headers }, (res) => {
      responseBegan = true;
      let text = '';
      res.setEncoding('utf8');
      res.on('data', (c: string) => {
        text += c;
      });
      res.on('end', () =>
        finish(() => resolve({ status: res.statusCode ?? 0, text, headers: res.headers })),
      );
      res.on('error', (e) => finish(() => reject(e)));
    });
    timer = setTimeout(
      () =>
        finish(() => {
          req.destroy();
          reject(new Error(`raw ${method} ${path} timed out`));
        }),
      scaleMs(timeoutMs),
    );
    // A write error (EPIPE/ECONNRESET) AFTER the daemon answered early is expected
    // for the oversize cases — ignore it and let the response 'end' resolve us.
    req.on('error', (e) => {
      if (responseBegan) return;
      finish(() => reject(e));
    });
    req.end(body ?? undefined);
  });
}

// The one canonical fail-open assertion, used by every §5 refusal. Pins the byte-
// exact wire response AND the json() header trio (§0 of the matrix: json() sets
// content-type/content-length/nosniff on every JSON response; for `{}` the length
// is the string '2').
function assertFailOpen(r: RawResult, label: string): void {
  assert.equal(
    r.status,
    200,
    `${label}: fail-open status must be 200 (got ${r.status}: ${r.text})`,
  );
  assert.equal(
    r.text,
    '{}',
    `${label}: fail-open body must be byte-exact {} (got ${JSON.stringify(r.text)})`,
  );
  assert.deepEqual(JSON.parse(r.text), {}, `${label}: parsed fail-open body must be {}`);
  assert.equal(r.headers['content-type'], 'application/json', `${label}: content-type trio`);
  assert.equal(r.headers['content-length'], '2', `${label}: content-length of {} is 2`);
  assert.equal(r.headers['x-content-type-options'], 'nosniff', `${label}: nosniff trio`);
}

// A fail-open that silently refuses must not take the daemon down. GET /health is
// loopback-open (no bearer) and returns { ok: true, ... }.
async function assertSurvives(baseUrl: string, label: string): Promise<void> {
  const res = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(scaleMs(5000)) });
  assert.equal(res.status, 200, `${label}: daemon must survive — /health status`);
  const body = (await res.json()) as { ok?: unknown };
  assert.equal(body.ok, true, `${label}: daemon must survive — /health ok:true`);
}

const JSON_CT = 'application/json';

// ─────────────────────────────────────────────────────────────────────────────
// Test 1 — the main loopback surface. Loopback auto-trust is turned OFF so the
// authorization guard behaves like a remote peer would: hooks are authenticated
// unconditionally and a tokenless/wrong-token hook falls to the silent refusal.
// Hold scope is left at the daemon-helper default ('all'), which is fine here —
// none of these rows reach the hold-relay path (holdHook's null-row edge gets its
// own scope='off' daemon in Test 2).
// ─────────────────────────────────────────────────────────────────────────────
test('hook fail-open contract: main loopback surface — every §5 refusal answers 200 {} and the daemon survives', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'off' } });
  t.after(() => daemon.stop());
  const port = daemon.port;
  const base = daemon.baseUrl;
  const token = daemon.token;
  assert.ok(typeof token === 'string' && token.length > 0, 'daemon must mint a token');

  const authHeaders = (extra: http.OutgoingHttpHeaders = {}): http.OutgoingHttpHeaders => ({
    'content-type': JSON_CT,
    authorization: `Bearer ${token}`,
    ...extra,
  });

  // §5 CSRF wall fail on a hook POST — a cross-site hook POST would 403 on any
  // other mutating route; on a hook it fails OPEN to 200 {} (http.ts @2045-2049).
  await t.test('§5 csrf-wall: cross-site hook POST fails open to 200 {}', async () => {
    const r = await raw({
      port,
      path: '/hook/Notification',
      method: 'POST',
      headers: authHeaders({ 'sec-fetch-site': 'cross-site' }),
      body: JSON.stringify({ session_id: 's' }),
    });
    assertFailOpen(r, 'csrf-wall');
    await assertSurvives(base, 'csrf-wall');
  });

  // §5 Host-wall fail on a hook — a forged Host header 403s any non-hook route;
  // on a hook the host wall fails OPEN (http.ts @1904-1908, before the POST block).
  await t.test('§5 host-wall: forged Host on a hook POST fails open to 200 {}', async () => {
    const r = await raw({
      port,
      path: '/hook/Notification',
      method: 'POST',
      headers: authHeaders({ host: 'evil.example.com' }),
      body: JSON.stringify({ session_id: 's' }),
    });
    assertFailOpen(r, 'host-wall');
    await assertSurvives(base, 'host-wall');
  });

  // §5 Oversized hook body — a body over MAX_BODY (1e6) but under the global cap
  // (14e6). On non-hook routes refuseOversize is a 413; on a hook it fails OPEN
  // via the declared-content-length check (http.ts @2079-2091). The 1.1 MB body is
  // still uploading when the early 200 arrives — raw() tolerates the reset.
  await t.test('§5 oversize: >MAX_BODY hook body fails open to 200 {}', async () => {
    const big = JSON.stringify({ session_id: 's', pad: 'x'.repeat(1_100_000) });
    assert.ok(
      big.length > 1_000_000 && big.length < 14_000_000,
      'body must be >MAX_BODY and <global cap',
    );
    const r = await raw({
      port,
      path: '/hook/Notification',
      method: 'POST',
      headers: authHeaders(),
      body: big,
      timeoutMs: 20_000,
    });
    assertFailOpen(r, 'oversize');
    await assertSurvives(base, 'oversize');
  });

  // §5 Bad JSON on a hook — an unparseable body 400s any non-hook JSON route; on
  // a hook it fails OPEN to 200 {} before dispatch (http.ts @2110-2114).
  await t.test('§5 bad-json: unparseable hook body fails open to 200 {}', async () => {
    const r = await raw({
      port,
      path: '/hook/Notification',
      method: 'POST',
      headers: authHeaders(),
      body: 'this is not json',
    });
    assertFailOpen(r, 'bad-json');
    await assertSurvives(base, 'bad-json');
  });

  // §5 Tokenless hook — no Authorization at all. A remote-shaped peer (loopback
  // trust off) is not authorized; the hook route refuses SILENTLY via
  // silentHookRefusal → 200 {} (http.ts @2123-2126, @1460). Body must be VALID
  // JSON so control reaches the refusal rather than the bad-json branch.
  await t.test(
    '§5 tokenless: hook POST with no bearer fails open to 200 {} (silent refusal)',
    async () => {
      const r = await raw({
        port,
        path: '/hook/Notification',
        method: 'POST',
        headers: { 'content-type': JSON_CT },
        body: JSON.stringify({ session_id: 's' }),
      });
      assertFailOpen(r, 'tokenless');
      await assertSurvives(base, 'tokenless');
    },
  );

  // §5 Wrong-token hook — a garbage bearer. Same silent-refusal path (@2123-2126).
  await t.test(
    '§5 wrong-token: hook POST with a bad bearer fails open to 200 {} (silent refusal)',
    async () => {
      const r = await raw({
        port,
        path: '/hook/Notification',
        method: 'POST',
        headers: { 'content-type': JSON_CT, authorization: 'Bearer not-the-real-token' },
        body: JSON.stringify({ session_id: 's' }),
      });
      assertFailOpen(r, 'wrong-token');
      await assertSurvives(base, 'wrong-token');
    },
  );

  // §5 PermissionRequest + tool_name === 'AskUserQuestion' — the pairing path
  // applies the event and answers 200 {} immediately without parking a question
  // (http.ts @2142-2149).
  await t.test(
    '§5 permission-pairing: PermissionRequest+AskUserQuestion answers 200 {} immediately',
    async () => {
      const r = await raw({
        port,
        path: '/hook/PermissionRequest',
        method: 'POST',
        headers: authHeaders(),
        body: JSON.stringify({ session_id: 's', tool_name: 'AskUserQuestion' }),
      });
      assertFailOpen(r, 'permission-pairing');
      await assertSurvives(base, 'permission-pairing');
    },
  );

  // §5 Unknown hook event name — a well-formed POST to /hook/<UnknownName> has no
  // handler; it applies the event and answers 200 {} (http.ts @2159-2164). The
  // name must match /^\/hook\/([A-Za-z]+)$/ to be treated as a hook at all.
  await t.test('§5 unknown-name: unknown hook event name answers 200 {}', async () => {
    const r = await raw({
      port,
      path: '/hook/Bogus',
      method: 'POST',
      headers: authHeaders(),
      body: JSON.stringify({ session_id: 's' }),
    });
    assertFailOpen(r, 'unknown-name');
    await assertSurvives(base, 'unknown-name');
  });

  // §5 validateHookEvent failure — a KNOWN handler (PreToolUse) whose payload
  // fails validation is refused with 200 {} and no dispatch (http.ts @2175-2178).
  // Four shapes: missing session_id, blank session_id, wrong-type session_id, and
  // a non-object body — all must fail open identically.
  await t.test('§5 validate-fail-missing: known handler, missing session_id → 200 {}', async () => {
    const r = await raw({
      port,
      path: '/hook/PreToolUse',
      method: 'POST',
      headers: authHeaders(),
      body: JSON.stringify({ tool_name: 'Bash' }),
    });
    assertFailOpen(r, 'validate-fail-missing');
    await assertSurvives(base, 'validate-fail-missing');
  });

  await t.test('§5 validate-fail-blank: known handler, blank session_id → 200 {}', async () => {
    const r = await raw({
      port,
      path: '/hook/PreToolUse',
      method: 'POST',
      headers: authHeaders(),
      body: JSON.stringify({ session_id: '   ' }),
    });
    assertFailOpen(r, 'validate-fail-blank');
    await assertSurvives(base, 'validate-fail-blank');
  });

  await t.test(
    '§5 validate-fail-wrongtype: known handler, non-string session_id → 200 {}',
    async () => {
      const r = await raw({
        port,
        path: '/hook/PreToolUse',
        method: 'POST',
        headers: authHeaders(),
        body: JSON.stringify({ session_id: 123 }),
      });
      assertFailOpen(r, 'validate-fail-wrongtype');
      await assertSurvives(base, 'validate-fail-wrongtype');
    },
  );

  await t.test(
    '§5 validate-fail-nonobject: known handler, non-object JSON body → 200 {}',
    async () => {
      const r = await raw({
        port,
        path: '/hook/PreToolUse',
        method: 'POST',
        headers: authHeaders(),
        body: '42',
      });
      assertFailOpen(r, 'validate-fail-nonobject');
      await assertSurvives(base, 'validate-fail-nonobject');
    },
  );

  // §5 Known hook handler returns {} — the happy path for the neutral handlers.
  // Notification / FileChanged / CwdChanged all return {} (http.ts @1650-1684),
  // so a valid POST is byte-identical to a fail-open at the wire. Pins that the
  // happy-path body is ALSO exactly {} + trio, so the conversion can't diverge it.
  for (const name of ['Notification', 'FileChanged', 'CwdChanged'] as const) {
    await t.test(`§5 known-handler-${name}: valid POST returns 200 {}`, async () => {
      const r = await raw({
        port,
        path: `/hook/${name}`,
        method: 'POST',
        headers: authHeaders(),
        body: JSON.stringify({ session_id: 's' }),
      });
      assertFailOpen(r, `known-handler-${name}`);
      await assertSurvives(base, `known-handler-${name}`);
    });
  }
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 2 — the holdHook null-row edge. Under FLEETDECK_HOLD_SCOPE=off,
// shouldRelayQuestion is always false, so hookHoldQuestion returns null and
// holdHook answers 200 {} immediately (http.ts @1691-1724, events.ts @211-213 /
// @874) — the ONLY way to exercise holdHook's fail-open ROW without the held-
// response machinery. Loopback trust is left ON here (default); these POSTs carry
// a valid bearer, so authorization is satisfied regardless, and the row under test
// is the hold-relay decision, not auth.
// ─────────────────────────────────────────────────────────────────────────────
test('hook fail-open contract: holdHook null-row edge under FLEETDECK_HOLD_SCOPE=off answers 200 {}', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_HOLD_SCOPE: 'off' } });
  t.after(() => daemon.stop());
  const port = daemon.port;
  const base = daemon.baseUrl;
  const token = daemon.token;
  assert.ok(typeof token === 'string' && token.length > 0, 'daemon must mint a token');

  const hold = (name: string, payload: Record<string, unknown>) =>
    raw({
      port,
      path: `/hook/${name}`,
      method: 'POST',
      headers: { 'content-type': JSON_CT, authorization: `Bearer ${token}` },
      body: JSON.stringify(payload),
    });

  // Elicitation is a pure hold event: with relay off, hookHoldQuestion → null →
  // 200 {} without parking.
  await t.test('§5 holdhook-nullrow-Elicitation: relay off → null row → 200 {}', async () => {
    const r = await hold('Elicitation', { session_id: 's' });
    assertFailOpen(r, 'holdhook-nullrow-Elicitation');
    await assertSurvives(base, 'holdhook-nullrow-Elicitation');
  });

  // A benign (non-AskUserQuestion, non-ExitPlanMode) PermissionRequest routes to
  // the hold path rather than the pairing shortcut; relay off → null → 200 {}.
  await t.test(
    '§5 holdhook-nullrow-PermissionRequest: benign tool, relay off → 200 {}',
    async () => {
      const r = await hold('PermissionRequest', { session_id: 's', tool_name: 'Bash' });
      assertFailOpen(r, 'holdhook-nullrow-PermissionRequest');
      await assertSurvives(base, 'holdhook-nullrow-PermissionRequest');
    },
  );

  await t.test('§5 holdhook-nullrow-AskUserQuestion: relay off → null row → 200 {}', async () => {
    const r = await hold('AskUserQuestion', { session_id: 's' });
    assertFailOpen(r, 'holdhook-nullrow-AskUserQuestion');
    await assertSurvives(base, 'holdhook-nullrow-AskUserQuestion');
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// Test 3 — CONTRAST SENTINELS (§1b). The fail-open boundary is only meaningful if
// the SAME conditions on NON-hook routes still refuse LOUDLY. These pin that every
// wall that a hook route fails open on remains a hard error off the hook path, so
// the conversion can't over-broaden fail-open. All on the loopback-off daemon.
// /command and /state are loopback-open (no bearer needed on loopback); the 401
// sentinel targets the token-gated /api/spawn/arm-unsupervised, whose 401 check
// (http.ts @1893) runs ahead of the POST walls.
// ─────────────────────────────────────────────────────────────────────────────
test('hook fail-open boundary: §1b contrast sentinels must NOT fail open (401/403/415/400/413)', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'off' } });
  t.after(() => daemon.stop());
  const port = daemon.port;

  const assertNotFailOpen = (r: RawResult, label: string, status: number, body: unknown): void => {
    assert.equal(
      r.status,
      status,
      `${label}: expected hard ${status} (got ${r.status}: ${r.text})`,
    );
    assert.notEqual(r.text, '{}', `${label}: must NOT fail open — body must not be {}`);
    assert.deepEqual(JSON.parse(r.text), body, `${label}: exact refusal body`);
  };

  // 401 — a token-gated mutating route with no bearer (loopback trust off). Not a
  // hook, so authorization refuses LOUDLY.
  await t.test(
    '§1b sentinel-401: tokenless non-hook POST → 401 unauthorized (not fail-open)',
    async () => {
      const r = await raw({
        port,
        path: '/api/spawn/arm-unsupervised',
        method: 'POST',
        headers: { 'content-type': JSON_CT },
        body: JSON.stringify({}),
      });
      assertNotFailOpen(r, 'sentinel-401', 401, { ok: false, reason: 'unauthorized' });
    },
  );

  // 403 host wall — forged Host on a non-hook GET refuses LOUDLY.
  await t.test(
    '§1b sentinel-403-host: forged Host on a non-hook GET → 403 forbidden (not fail-open)',
    async () => {
      const r = await raw({
        port,
        path: '/state',
        method: 'GET',
        headers: { host: 'evil.example.com' },
      });
      assertNotFailOpen(r, 'sentinel-403-host', 403, { ok: false, reason: 'forbidden' });
    },
  );

  // 403 CSRF wall — a cross-site non-hook mutating POST refuses LOUDLY.
  await t.test(
    '§1b sentinel-403-csrf: cross-site non-hook POST → 403 forbidden (not fail-open)',
    async () => {
      const r = await raw({
        port,
        path: '/command',
        method: 'POST',
        headers: { 'content-type': JSON_CT, 'sec-fetch-site': 'cross-site' },
        body: JSON.stringify({ cmd: 'noop' }),
      });
      assertNotFailOpen(r, 'sentinel-403-csrf', 403, { ok: false, reason: 'forbidden' });
    },
  );

  // 415 content-type wall — a non-JSON content-type on a non-hook POST refuses
  // LOUDLY (the wall hooks are exempt from).
  await t.test(
    '§1b sentinel-415: wrong content-type on a non-hook POST → 415 (not fail-open)',
    async () => {
      const r = await raw({
        port,
        path: '/command',
        method: 'POST',
        headers: { 'content-type': 'text/plain' },
        body: 'cmd=noop',
      });
      assertNotFailOpen(r, 'sentinel-415', 415, { ok: false, reason: 'expected application/json' });
    },
  );

  // 400 bad json — an unparseable body on a non-hook JSON POST refuses LOUDLY.
  await t.test(
    '§1b sentinel-400: bad JSON on a non-hook POST → 400 bad json (not fail-open)',
    async () => {
      const r = await raw({
        port,
        path: '/command',
        method: 'POST',
        headers: { 'content-type': JSON_CT },
        body: 'this is not json',
      });
      assertNotFailOpen(r, 'sentinel-400', 400, { err: 'bad json' });
    },
  );

  // 413 oversize — a body over MAX_BODY on a non-hook POST refuses LOUDLY. The
  // early 413 lands mid-upload; raw() tolerates the reset.
  await t.test('§1b sentinel-413: >MAX_BODY on a non-hook POST → 413 (not fail-open)', async () => {
    const big = JSON.stringify({ pad: 'x'.repeat(1_100_000) });
    assert.ok(
      big.length > 1_000_000 && big.length < 14_000_000,
      'body must be >MAX_BODY and <global cap',
    );
    const r = await raw({
      port,
      path: '/command',
      method: 'POST',
      headers: { 'content-type': JSON_CT },
      body: big,
      timeoutMs: 20_000,
    });
    assertNotFailOpen(r, 'sentinel-413', 413, { ok: false, reason: 'payload too large' });
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// SKIPPED §5 rows — emitted as visible skipped tests so the coverage map is
// verifiable from the runner output. Each requires source-level fault injection
// (a thrown handler / router / intake error) or a mid-shutdown admission race that
// cannot be induced deterministically from the wire without editing source, which
// this characterization pass forbids. The holdHook fail-open ROW is still covered
// (its null-row edge, Test 2); only its throw/quiesce sub-edges are skipped.
// ─────────────────────────────────────────────────────────────────────────────
test.skip(
  '§5 holdHook quiescing sub-edge — SKIP: requires a mid-shutdown quiesce race, not deterministically wire-inducible (holdHook fail-open row covered via null-row in Test 2)',
);
test.skip(
  '§5 holdHook intake-error sub-edge — SKIP: requires core.hookHoldQuestion to throw (source-level fault injection)',
);
test.skip(
  '§5 POST inner catch on /hook/* — SKIP: requires a thrown hook-handler error (source-level fault injection)',
);
test.skip(
  '§5 outer handler catch on /hook/* — SKIP: requires a thrown router error (source-level fault injection)',
);
test.skip(
  '§5 fetchHandler quiescing hook path — SKIP: requires a mid-quiesce admission race, not deterministically wire-inducible',
);
test.skip(
  '§1b contrast sentinel 500 (non-hook internal error) — SKIP: requires a thrown handler error (source-level fault injection); no wire trigger at HEAD',
);
