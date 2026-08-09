// tests/mail-delivery-lease.test.mjs
//
// BUG-034 — mail was permanently claimed before the consumer acknowledged:
// /api/watch set delivered_at before writing the response (a socket closing
// mid-response lost the mail), claimAllMail committed delivery before the
// tmux paste (a daemon exit in between lost the box), and the board's
// GET /mail drain finalized before the browser held the body.
//
// The fix: claims are EXPIRING IN-FLIGHT LEASES (mail.claimed_at, delivered_at
// still NULL). Delivery is finalized only on explicit ack (POST /mail/ack
// from the watcher, ack_mail_ids from the board drain, a confirmed tmux
// Enter), and the retention sweep hands back any lease whose deadline passed
// — so a dead consumer or a daemon restart RE-DELIVERS instead of losing.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { openDb } from '../scripts/fleetd/db.ts';
import { createCore } from '../scripts/fleetd/derive.ts';

// The scratch daemon's fleetd.db — openDb's CREATE TABLE IF NOT EXISTS DDL is
// idempotent, so a second handle on the live file is safe for read assertions.
function dbAt(daemon) {
  return openDb(path.join(daemon.home, 'fleetd.db'));
}

test('BUG-034: a watch claim leases — unacked mail is re-claimed after the lease lapses, not lost', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_MAIL_CLAIM_LEASE_MS: '300' } });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-lease-'));
  const db = dbAt(daemon);
  t.after(async () => { db.close(); await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon.token });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'lease me' }, { token: daemon.token });

  const row = () => db.prepare('SELECT id, delivered_at, claimed_at FROM mail WHERE to_session = ?').get(sid);

  // Claim WITHOUT acking — the socket-close-before-read case.
  const first = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=100`, { token: daemon.token });
  assert.equal(first.json?.status, 'mail', 'pending mail resolves the poll');
  assert.equal(first.json?.text, 'lease me');
  const claimed = row();
  assert.equal(claimed.delivered_at, null, 'a bare claim must NOT finalize delivery — that was the loss window');
  assert.ok(claimed.claimed_at > Date.now(), 'the claim is a lease with a future deadline');

  // While the lease is live the row is invisible to every other path.
  const blocked = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=200`, { token: daemon.token });
  assert.equal(blocked.json?.status, 'idle', 'a live lease hides the row from a second watcher');

  // The lease lapses (consumer never acked) → the sweep hands the row back…
  await new Promise(r => setTimeout(r, 450));
  const second = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=100`, { token: daemon.token });
  assert.equal(second.json?.status, 'mail', 'an unacked claim is re-delivered after the lease lapses');
  assert.equal(second.json?.mail_id, claimed.id, 'same mail row, not a copy');

  // …and an explicit ack finalizes delivery permanently.
  const ack = await postJson(`${daemon.baseUrl}/mail/ack`, { mail_id: second.json.mail_id }, { token: daemon.token });
  assert.equal(ack.status, 200);
  assert.equal(ack.json?.acked, 1, 'the ack finalizes the leased row');
  assert.ok(row().delivered_at > 0, 'delivered_at is set only after the ack');
  assert.equal(row().claimed_at, null, 'the lease is cleared by the ack');

  const third = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=200`, { token: daemon.token });
  assert.equal(third.json?.status, 'idle', 'a delivered row is never claimed again');
});

test('BUG-034: a failed owned-pane delivery releases the lease — the hook drain still re-delivers', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_MAIL_CLAIM_LEASE_MS: '300' } });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-lease-pane-'));
  const db = dbAt(daemon);
  t.after(async () => { db.close(); await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon.token });
  // No owned pane exists for a hooks-only session, so the grace-timer pane
  // delivery (1.5 s) probes, finds nothing claimable into a pane, and bails.
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'pane or bust' }, { token: daemon.token });

  // Wait out the pane grace so tryOwnedPaneDelivery has fired and backed off,
  // then claim and let the lease LAPSE (no ack).
  await new Promise(r => setTimeout(r, 1_800));
  const first = await getJson(`${daemon.baseUrl}/api/watch?session=${sid}&hold_ms=100`, { token: daemon.token });
  assert.equal(first.json?.status, 'mail', 'pane bailed (no owned pane) — the watcher claims instead');
  await new Promise(r => setTimeout(r, 450));

  // The stalled lease was swept; a HOOK drain (UserPromptSubmit) now delivers
  // the same row — before the fix the watch claim had already finalized it and
  // this drain would have returned nothing.
  const res = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon.token });
  const ctx = res.json?.hookSpecificOutput?.additionalContext ?? '';
  assert.ok(ctx.includes('pane or bust'), 'lapsed-lease mail reaches the session at the turn boundary');
  assert.ok(db.prepare('SELECT delivered_at FROM mail WHERE to_session = ?').get(sid).delivered_at > 0,
    'the hook drain finalizes the row it hands to the session');
});

test('BUG-034: the board GET /mail drain re-delivers until the ids are acked back', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_MAIL_CLAIM_LEASE_MS: '300' } });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-lease-get-'));
  const db = dbAt(daemon);
  t.after(async () => { db.close(); await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon.token });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'board poll' }, { token: daemon.token });

  const row = () => db.prepare('SELECT id, delivered_at, claimed_at FROM mail WHERE to_session = ?').get(sid);

  // First poll drains WITHOUT acking anything (the response-lost case).
  const first = await getJson(`${daemon.baseUrl}/mail?session=${sid}`, { token: daemon.token });
  assert.equal(first.json?.mail?.length, 1);
  assert.deepEqual(first.json?.ack_mail_ids, [first.json.mail[0].id], 'the drain names the ids it leased');
  assert.equal(row().delivered_at, null, 'an unacked board drain must NOT finalize delivery');
  assert.ok(row().claimed_at > Date.now(), 'the drained row is under lease');

  // Immediate re-poll: the live lease hides the row (no double delivery).
  const hidden = await getJson(`${daemon.baseUrl}/mail?session=${sid}`, { token: daemon.token });
  assert.equal(hidden.json?.mail?.length, 0, 'a live lease is invisible to the next poll');

  // After the lease lapses (no ack ever came), the SAME row comes back.
  await new Promise(r => setTimeout(r, 450));
  const second = await getJson(`${daemon.baseUrl}/mail?session=${sid}`, { token: daemon.token });
  assert.equal(second.json?.mail?.length, 1, 'unacked mail is re-delivered after the lease lapses');
  assert.equal(second.json.mail[0].id, first.json.mail[0].id);

  // The NEXT poll acks what the previous one handed it — final delivery.
  const ackParam = second.json.ack_mail_ids.join(',');
  const third = await getJson(`${daemon.baseUrl}/mail?session=${sid}&ack=${encodeURIComponent(ackParam)}`, { token: daemon.token });
  assert.equal(third.json?.mail?.length, 0);
  assert.ok(row().delivered_at > 0, 'the acked drain finalizes the row');
});

test('BUG-034: an ack for an unknown or already-settled row is a safe no-op', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); });

  const bogus = await postJson(`${daemon.baseUrl}/mail/ack`, { mail_id: 424242 }, { token: daemon.token });
  assert.equal(bogus.status, 200);
  assert.equal(bogus.json?.acked, 0, 'acking a row that does not exist settles silently');

  const malformed = await postJson(`${daemon.baseUrl}/mail/ack`, { mail_id: 'not-a-number' }, { token: daemon.token });
  assert.equal(malformed.status, 200);
  assert.equal(malformed.json?.acked, 0, 'a non-integer mail_id is refused by the guard, never thrown');
});

test('BUG-034: in-process lease lifecycle — claim, double-claim guard, sweep release, ack', async (t) => {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 21601, home: '/fd-lease-home' });
  t.after(() => db.close());

  const sid = 'lease-sid';
  core.hookSessionStart({ session_id: sid, cwd: '/tmp/fd-lease', source: 'startup' });
  const posted = await core.postMail({ to: sid, from: 'tester', text: 'in-process lease' });
  assert.equal(posted.ok, true, 'setup: postMail accepted');

  const row = () => db.prepare('SELECT id, delivered_at, claimed_at FROM mail WHERE to_session = ?').get(sid);

  const claimed = core.claimMail(sid);
  assert.ok(claimed, 'mail is claimable');
  assert.equal(row().delivered_at, null, 'claim alone never delivers');
  assert.ok(row().claimed_at > Date.now(), 'the lease deadline is in the future');

  assert.equal(core.claimMail(sid), null, 'a live lease blocks a second claim');

  // The deadline passes → the retention sweep hands the row back.
  db.prepare('UPDATE mail SET claimed_at = ? WHERE id = ?').run(Date.now() - 1, claimed.mail_id);
  const sweep = await core.retentionSweep(Date.now());
  assert.equal(sweep.changed, true, 'releasing a stalled lease is a mutation');
  assert.equal(row().claimed_at, null, 'the sweep clears the lapsed lease');

  const reclaimed = core.claimMail(sid);
  assert.equal(reclaimed?.mail_id, claimed.mail_id, 'the same row is claimable again');

  assert.deepEqual(core.ackMail([claimed.mail_id]), { acked: 1 }, 'the ack finalizes');
  assert.ok(row().delivered_at > 0);
  assert.deepEqual(core.ackMail([claimed.mail_id]), { acked: 0 }, 'a double ack settles silently');
});
