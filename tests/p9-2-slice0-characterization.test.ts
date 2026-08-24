// p9-2-slice0-characterization.test.ts — P9.2 Slice 0 GAP pins (tests only; NO
// source change). These freeze the three failure-dialect byte contracts on the
// LEGACY repo/worktree routes so the P9.2 Effect conversions (GET /api/worktrees,
// POST /api/worktrees/remove, POST /api/repos/preflight) have wire anchors that a
// conversion cannot silently move. Every contract is copied verbatim from
// docs/v1/evidence/effect/p9-2-design.md §1C and the source it cites:
//
//   GAP-2  GET  /api/worktrees        fail-SOFT: any worktrees() rejection folds to
//          200 {ok:true,worktrees:[]} + log 'fleetd worktree inspector error:'
//          (http.ts:2194-2208). NEVER a 500 — the one route where a defect must
//          not surface as an error status.
//   GAP-1  POST /api/repos/preflight   an unexpected preflightRepo() rejection folds
//          to the DISTINCT 500 {ok:false,reason:'Git access check failed internally'}
//          + log 'fleetd repo preflight error:' (http.ts:2526-2549). Distinct from
//          remove's 'internal' — the controlAsync fold hardcodes 'internal', so this
//          route cannot reuse it (§1C byte-diff, DANGER §4.6).
//   GAP-3  POST /api/worktrees/remove  TWO distinct 500 sources that must not collapse:
//            (a) an unexpected removeWorktree() rejection folds to the generic
//                500 {ok:false,reason:'internal'} + log 'fleetd worktree removal error:'
//                (http.ts:2437-2450, the route .catch).
//            (b) a RESOLVED purge-path 500 {ok:false,reason:`could not purge worktree
//                rows: ${detail}`} (worktrees.ts:722-725) passes through the route's
//                .then verbatim — no fold, no error log.
//
// PRODUCTION ROUTE PATH: every pin drives the real createHttp dispatcher over a
// real loopback socket (mirrors the in-process board harness in
// tests/effect/http-workflow-control.test.ts §C). The failure is INJECTED AT THE
// CORE SEAM — each route reads core.worktrees / core.removeWorktree /
// core.preflightRepo fresh per request, so overriding the method after createHttp
// reaches the production handler's .then/.catch. This is the same core-stub
// technique the P9.1 spawn-route 500-dialect pins already use
// (http-workflow-spawn-route.test.ts: "the C-section 500 pins the settler dialect
// with core.spawn STUBBED"). effectRoutes is left null throughout: these are the
// LEGACY handlers, which is exactly what Slice 0 must characterize.

import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { createHttp } from '../src/daemon/http.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { getJson, postJson } from './helpers/http.ts';
import test, { type TestContext } from './helpers/harness-test.ts';

interface Board {
  core: ReturnType<typeof createCore>;
  port: number;
}

// An idle :memory: core behind createHttp, bound on a real loopback port (probe a
// throwaway port first, then hand createHttp the real port so the Host wall pins
// to it — the pilot's pattern). token:null + plain loopback authorizes every
// route here (none is in tokenGatedRoute, requireToken defaults off). effectRoutes
// stays null: the legacy .then/.catch handlers answer, which is the whole point.
function startBoard(t: TestContext): Promise<Board> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home', runControlDetached });
  const probe = http.createServer();
  return new Promise<Board>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        const handle = createHttp(core, { port, token: null as unknown as string, lan: null });
        handle.server.once('error', reject);
        handle.server.listen(port, '127.0.0.1', () => {
          t.after(() => {
            handle.server.close();
            db.close();
          });
          resolve({ core, port });
        });
      });
    });
  });
}

// Capture console.error for the duration of one request. The legacy route's
// .catch runs and calls console.error BEFORE res.end, so the log is recorded by
// the time getJson/postJson resolves; console.error is restored in finally either
// way (the spawn-route 500-dialect pins use the identical shape).
async function withCapturedErrors<T>(
  fn: () => Promise<T>,
): Promise<{ result: T; logged: unknown[][] }> {
  const logged: unknown[][] = [];
  const originalError = console.error;
  console.error = (...args: unknown[]) => {
    logged.push(args);
  };
  try {
    const result = await fn();
    return { result, logged };
  } finally {
    console.error = originalError;
  }
}

// ---------------------------------------------------------------- GAP-2 (fail-soft GET)

test('GAP-2: GET /api/worktrees folds a worktrees() rejection to the fail-soft 200 wire (never 500) + inspector log', async (t) => {
  const { core, port } = await startBoard(t);
  const boom = new Error('worktree inspector boom');
  // Inject at the core seam: the GET handler reads core.worktrees() fresh per
  // request, so this override reaches its production .catch fold.
  core.worktrees = () => Promise.reject(boom);

  const { result, logged } = await withCapturedErrors(() =>
    getJson(`http://127.0.0.1:${port}/api/worktrees`),
  );

  // §1C fail-SOFT: ANY rejection → 200 {ok:true,worktrees:[]}, NEVER a 500.
  assert.equal(result.status, 200, 'a broken inspector must not 500 the fleet-wide view');
  assert.equal(
    result.text,
    JSON.stringify({ ok: true, worktrees: [] }),
    'exact fail-soft wire bytes',
  );
  assert.deepEqual(result.json, { ok: true, worktrees: [] });

  const log = logged.find((a) => a[0] === 'fleetd worktree inspector error:');
  assert.ok(log, "console.error('fleetd worktree inspector error:', err) fired");
  assert.equal(log?.[1], boom, 'the caught rejection is logged identity-preserved');
});

// ---------------------------------------------------------------- GAP-1 (preflight distinct 500)

test("GAP-1: POST /api/repos/preflight folds an unexpected preflightRepo() rejection to the DISTINCT 500 'Git access check failed internally' + preflight log", async (t) => {
  const { core, port } = await startBoard(t);
  const boom = new Error('preflight probe boom');
  core.preflightRepo = () => Promise.reject(boom);

  // A body that PASSES repoPreflightBodyError (repo is a string) so the request
  // reaches core.preflightRepo rather than the pre-core 400.
  const { result, logged } = await withCapturedErrors(() =>
    postJson(`http://127.0.0.1:${port}/api/repos/preflight`, { repo: 'owner/name' }),
  );

  assert.equal(result.status, 500);
  assert.equal(
    result.text,
    JSON.stringify({ ok: false, reason: 'Git access check failed internally' }),
    'exact preflight generic-500 wire bytes',
  );
  assert.deepEqual(result.json, { ok: false, reason: 'Git access check failed internally' });
  // DISTINCT from remove's fold (§1C byte-diff / DANGER §4.6): must NOT be 'internal'.
  assert.notEqual(
    result.text,
    JSON.stringify({ ok: false, reason: 'internal' }),
    "preflight's 500 dialect must never collapse to the controlAsync 'internal' fold",
  );

  const log = logged.find((a) => a[0] === 'fleetd repo preflight error:');
  assert.ok(log, "console.error('fleetd repo preflight error:', err) fired");
  assert.equal(log?.[1], boom, 'the caught rejection is logged identity-preserved');
});

// ---------------------------------------------------------------- GAP-3a (remove generic 500)

test("GAP-3a: POST /api/worktrees/remove folds a removeWorktree() rejection to the generic 500 'internal' + removal log", async (t) => {
  const { core, port } = await startBoard(t);
  const boom = new Error('remove core boom');
  core.removeWorktree = () => Promise.reject(boom);

  const { result, logged } = await withCapturedErrors(() =>
    postJson(`http://127.0.0.1:${port}/api/worktrees/remove`, { path: '/tmp/whatever' }),
  );

  assert.equal(result.status, 500);
  assert.equal(
    result.text,
    JSON.stringify({ ok: false, reason: 'internal' }),
    'exact remove generic-500 wire bytes',
  );
  assert.deepEqual(result.json, { ok: false, reason: 'internal' });
  // Distinct from the purge-path 500 (GAP-3b): the rejection fold NEVER carries
  // the purge detail.
  assert.doesNotMatch(result.text, /could not purge worktree rows/);

  const log = logged.find((a) => a[0] === 'fleetd worktree removal error:');
  assert.ok(log, "console.error('fleetd worktree removal error:', err) fired");
  assert.equal(log?.[1], boom, 'the caught rejection is logged identity-preserved');
});

// ---------------------------------------------------------------- GAP-3b (remove purge 500 passthrough)

test('GAP-3b: POST /api/worktrees/remove passes a RESOLVED purge-path 500 through verbatim — distinct body, no fold, no removal log', async (t) => {
  const { core, port } = await startBoard(t);
  // The purge-path 500 is a RESOLVED {status,body} the core returns from its
  // BEGIN IMMEDIATE catch (worktrees.ts:722-725) — NOT a rejection. Its body
  // template is `could not purge worktree rows: ${detail}`, reproduced here so
  // the anchor tracks the source expression byte-for-byte.
  const detail = 'disk I/O error';
  const purge500 = {
    status: 500,
    body: { ok: false, reason: `could not purge worktree rows: ${detail}` },
  };
  core.removeWorktree = () => Promise.resolve(purge500);

  const { result, logged } = await withCapturedErrors(() =>
    postJson(`http://127.0.0.1:${port}/api/worktrees/remove`, { path: '/tmp/whatever' }),
  );

  assert.equal(result.status, 500);
  assert.equal(
    result.text,
    JSON.stringify(purge500.body),
    'the resolved purge-500 body passes through the route .then verbatim',
  );
  assert.deepEqual(result.json, purge500.body);
  assert.match(result.text, /could not purge worktree rows: disk I\/O error/);
  // The passthrough must NOT collapse to the generic rejection-fold wire...
  assert.notEqual(
    result.text,
    JSON.stringify({ ok: false, reason: 'internal' }),
    'a resolved purge-500 must never be flattened into the generic 500 fold',
  );
  // ...and a RESOLVED outcome never trips the route .catch, so nothing is logged.
  assert.equal(
    logged.find((a) => a[0] === 'fleetd worktree removal error:'),
    undefined,
    'a resolved purge-500 is not a .catch fold — no removal-error log',
  );
});
