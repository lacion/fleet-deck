// http-ws-snapshot-leaves.test.ts — the focused isolation fixture for the P6.4
// WS-snapshot ingress slice. The /ws snapshot surface is CONVERTED-BY-OWNERSHIP
// (its lifecycle already runs under the P6.3 HttpServer owner) plus
// PURE-LEAVES-ONLY: the three decisions the surface makes are lifted into
// src/daemon/http-policy.ts so each is testable without a live socket. This file
// pins those leaves in pure isolation — the exact boundaries, the frozen frame
// shape, and byte-identity with the pre-extraction inline expressions.
//
// The REAL-DAEMON behaviour these leaves feed is pinned separately and stays
// untouched-green: tests/ws-hardening.test.ts drives coalescing (M-P1), the
// buffered-byte eviction with the cap forced to -1 (R1-2), the 30 s keepalive
// ping/terminate (H-R3), the tokenless snapshot frame (H-S1), and the
// legacy_upgrade banner riding /ws frames (BUG-031). The wire contract is
// byte-for-byte per docs/v1/evidence/effect/p6-http-matrix.md §3.

import assert from 'node:assert/strict';

import {
  assembleSnapshotFrame,
  wsBufferEviction,
  wsKeepaliveAction,
} from '../../src/daemon/http-policy.ts';

import test from '../helpers/harness-test.ts';

// ============================ wsBufferEviction ============================
// R1-2 backpressure: strictly-greater-than the cap evicts, at-or-under sends. The
// signal is the per-socket buffered-BYTE count vs a cap (P6.5 preserve-as-
// implemented), never a send() return value.

test('wsBufferEviction evicts strictly above the cap and sends at or under it', () => {
  // Above the cap → evict.
  assert.equal(wsBufferEviction(1025, 1024), 'evict');
  assert.equal(wsBufferEviction(1_000_000, 1024), 'evict');
  // Exactly at the cap is NOT over → send (the boundary is strictly-greater-than).
  assert.equal(wsBufferEviction(1024, 1024), 'send');
  // Under the cap → send.
  assert.equal(wsBufferEviction(0, 1024), 'send');
  assert.equal(wsBufferEviction(512, 1024), 'send');
});

test('wsBufferEviction with the cap forced to -1 evicts every peer (the R1-2 lever)', () => {
  // ws-hardening sets FLEETDECK_WS_BUFFER_MAX=-1 so EVERY broadcast evicts, because
  // bufferedAmount is always >= 0. This is the exact comparison that must hold: an
  // idle socket at 0 bytes is 0 > -1 → evict.
  assert.equal(wsBufferEviction(0, -1), 'evict');
  assert.equal(wsBufferEviction(1, -1), 'evict');
  // And the harmless default: an idle socket at 0 bytes under a real cap sends.
  assert.equal(wsBufferEviction(0, 0), 'send');
});

// ============================ wsKeepaliveAction ============================
// H-R3 liveness: a peer that has not ponged since the previous tick (isAlive ===
// false) is terminated; a live one is pinged. Both /ws snapshot sockets and
// /ws/term viewers share this one rule through the shared keepalive loop.

test('wsKeepaliveAction pings the live and terminates the stale', () => {
  assert.equal(wsKeepaliveAction(true), 'ping');
  assert.equal(wsKeepaliveAction(false), 'terminate');
});

// ============================ assembleSnapshotFrame ============================
// H-S1 / BUG-031: the frame is `type:'snapshot'` first, the core snapshot spread
// (own key order preserved), legacy_upgrade LAST. The leaf is a faithful wrapper —
// it injects NOTHING beyond the discriminator and the trailing banner, so a
// tokenless core.snapshot() in yields a tokenless frame out (H-S1 stays a call-site
// choice), and legacy_upgrade rides even when null (BUG-031).

test('assembleSnapshotFrame emits type first, snapshot spread, legacy_upgrade last', () => {
  const snapshot = { schema_version: 3, sessions: [{ id: 'a' }], repos: [] };
  const frame = assembleSnapshotFrame(snapshot, { kind: 'restart' });
  assert.deepEqual(frame, {
    type: 'snapshot',
    schema_version: 3,
    sessions: [{ id: 'a' }],
    repos: [],
    legacy_upgrade: { kind: 'restart' },
  });
  // Frozen key order: discriminator, then the snapshot's own keys verbatim, then
  // the banner. A reordered body is a wire break the freeze suite catches.
  assert.deepEqual(Object.keys(frame), [
    'type',
    'schema_version',
    'sessions',
    'repos',
    'legacy_upgrade',
  ]);
});

test('assembleSnapshotFrame carries legacy_upgrade even when null (BUG-031)', () => {
  const frame = assembleSnapshotFrame({ schema_version: 3 }, null);
  // The key must be PRESENT with value null — a live board treats /ws as
  // authoritative, so a missing key wipes the restart banner on connect.
  assert.ok('legacy_upgrade' in frame);
  assert.equal(frame.legacy_upgrade, null);
});

test('assembleSnapshotFrame injects nothing beyond type + legacy_upgrade (H-S1)', () => {
  // A core.snapshot() carries no token/lan block; the leaf must not add one. The
  // frame keys are EXACTLY the discriminator, the input keys, and the banner.
  const snapshot = { schema_version: 3, sessions: [] };
  const frame = assembleSnapshotFrame(snapshot, null);
  assert.deepEqual(Object.keys(frame), ['type', 'schema_version', 'sessions', 'legacy_upgrade']);
});

test('assembleSnapshotFrame is byte-identical to the pre-extraction inline expression', () => {
  // The exact expression http.ts used before the leaf was lifted. JSON.stringify is
  // the wire path (broadcast() and the connect handler both stringify the frame), so
  // equal serialization IS the frozen contract.
  const snapshot = { schema_version: 7, up_ms: 1234, sessions: [{ id: 'x', pane: 2 }], repos: [] };
  const legacyUpgrade = { kind: 'restart', from: '0.15.0' };
  const inline = { type: 'snapshot', ...snapshot, legacy_upgrade: legacyUpgrade };
  const viaLeaf = assembleSnapshotFrame(snapshot, legacyUpgrade);
  assert.equal(JSON.stringify(viaLeaf), JSON.stringify(inline));
  assert.deepEqual(viaLeaf, inline);
});
