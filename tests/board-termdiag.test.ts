// tests/board-termdiag.test.mjs
//
// BUG-186 — the pre-frame terminal close diagnosis. A /ws/term socket that
// closes before its first frame was refused at the upgrade, and the daemon
// destroys refused upgrades without a word, so the board infers the cause.
// The old inference — hasToken() alone — is FALSE under the tokenless trust
// modes: FLEETDECK_PROXY_AUTH=trust and FLEETDECK_TRUST_LOOPBACK=on both
// authorize tokenless upgrades (scripts/fleetd/http.mjs authorized()), so a
// keyless board behind a broken proxy was told "you need a key" and sent down
// an ineffective token-recovery path. The diagnosis now keys off the daemon's
// own /health capability (auth.term_token); these tests pin the contract.
//
// board/src/termDiag.js has no imports and board/package.json is "type":
// "module", so it loads under node --test with no bundler — same as
// tests/board-util.test.mjs. The backend half of the contract (the capability
// mirrors the real gate under every trust mode) lives in
// tests/loopback-gates.test.mjs.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';

import { MAX_RECONNECT, reconnectPlan, refusedUpgradeText } from '../board/src/termDiag.ts';

test('a held key still blames the key (it may be stale)', () => {
  for (const gates of [true, false, null, undefined]) {
    const text = refusedUpgradeText(true, gates);
    assert.match(text, /key may be stale/, `held key, gates=${String(gates)}`);
  }
});

test('no key + a gating daemon keeps the 0.16.0 missing-key wording', () => {
  const text = refusedUpgradeText(false, true);
  assert.match(
    text,
    /no key/,
    'the gated loopback case is the one the original wording exists for',
  );
  assert.match(text, /fleetdeck token/);
});

test('no key + a WAIVING daemon names transport causes, not a missing key', () => {
  const text = refusedUpgradeText(false, false);
  // The BUG-186 contract: proxy-trust and trust-loopback deployments may hold
  // no key at all, so the diagnosis must point at the proxy's WebSocket
  // forwarding and daemon connectivity instead.
  assert.match(text, /proxy/i, 'the proxy upgrade-forwarding cause must be named');
  assert.match(text, /daemon/i, 'daemon connectivity must be named');
  assert.doesNotMatch(text, /no key/, 'must not claim a key is required in a mode that waives it');
  assert.doesNotMatch(text, /fleetdeck token/, 'the token-recovery path is the wrong advice here');
});

test('an unknown capability falls back to the historical key-based inference', () => {
  // /health unanswered (old daemon, or the fetch itself failed): the board
  // cannot know the mode, and the gated default is the safe assumption.
  for (const gates of [null, undefined]) {
    const text = refusedUpgradeText(false, gates);
    assert.match(text, /no key/, `gates=${String(gates)} must degrade to the pre-fix inference`);
  }
});

// ---------------------------------------------------------------------------
// BB2-2 — a dropped transport must not latch the pane dead.
//
// Before this, ANY close after the first frame ran end(): disableStdin on, note
// set, socket closed, and the socket effect only re-runs when the SPAWN
// changes. So a daemon restart (every version takeover does one), a slept
// laptop or a re-routed tailnet left every open terminal rendering its last
// screen while silently swallowing keystrokes — indistinguishable from a hung
// agent, and curable only by closing and reopening the tile.
// ---------------------------------------------------------------------------

test('a transient close (frames were seen) retries with backoff', () => {
  assert.deepEqual(reconnectPlan(true, 0), { action: 'retry', delayMs: 500 });
  assert.deepEqual(reconnectPlan(true, 1), { action: 'retry', delayMs: 1000 });
  assert.deepEqual(reconnectPlan(true, 2), { action: 'retry', delayMs: 2000 });
});

test('backoff is capped so a wall of tiles cannot storm a daemon still booting', () => {
  for (let n = 0; n < MAX_RECONNECT; n++) {
    const plan = reconnectPlan(true, n);
    assert.ok(plan.action === 'retry', `attempt ${n} must retry`);
    assert.ok(plan.delayMs <= 5000, `attempt ${n} must stay capped, got ${plan.delayMs}`);
  }
});

test('an initial PRE-FRAME close is diagnosed, but recovery can span daemon startup', () => {
  assert.deepEqual(reconnectPlan(false, 0), { action: 'diagnose' });
  assert.deepEqual(reconnectPlan(false, 1), { action: 'retry', delayMs: 1000 });
  assert.deepEqual(reconnectPlan(false, 3), { action: 'retry', delayMs: 4000 });
  assert.deepEqual(reconnectPlan(false, MAX_RECONNECT), { action: 'diagnose' });
});

test('retries are bounded — the pane eventually settles instead of reconnecting forever', () => {
  assert.deepEqual(reconnectPlan(true, MAX_RECONNECT), { action: 'give-up' });
  assert.deepEqual(reconnectPlan(true, MAX_RECONNECT + 1), { action: 'give-up' });
});
