import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chmodSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { openDb } from '../scripts/fleetd/db.mjs';
import { startDaemon } from './helpers/daemon.mjs';
import { getJson, postHook, postJson } from './helpers/http.mjs';

// tests/succession.test.mjs — 0.7.1 /clear session succession.
//
// THE FACT UNDER TEST (observed on a live daemon, not a guess): the CLI mints a
// NEW session id on /clear. The old id fires SessionEnd(reason='clear') and, in
// the same second and the same cwd, a brand-new id fires
// SessionStart(source='clear'). Nothing in either payload names the other.
//
// Before 0.7.1 the daemon believed a /clear kept the session id, so it left the
// OLD card "live" — holding the spawn row, the tmux window and therefore the
// terminal/kill chips — while a SECOND card collected all the telemetry with no
// pane. The human's terminal button drove one card and the status updates landed
// on the other. These tests pin the fix: ONE card continues, wearing the same
// callsign, holding the same pane, keeping its mail.
//
// The legacy path (a SessionEnd(clear) with NO successor — an older CLI that
// really does keep the id) must keep behaving exactly as it did; that is pinned
// here too, and by tests/fleet-bugs.test.mjs + tests/adopt.test.mjs unchanged.

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best effort */ }

function scratch(prefix) {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

function withDb(home, fn) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return fn(db); } finally { db.close(); }
}

function cardOf(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

function cardsIn(state, cwd) {
  return state.sessions.filter(s => s.cwd === cwd);
}

async function boot(t, prefix, extraEnv = {}) {
  const home = scratch(`${prefix}-daemon-`);
  const cwd = scratch(`${prefix}-cwd-`);
  const env = { FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE, ...extraEnv };
  const daemon = await startDaemon({ home, env });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  });
  return { daemon, home, cwd, env };
}

// A plain CLI session the board never spawned.
async function startSession(daemon, cwd) {
  const sid = randomUUID();
  const res = await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' });
  return { sid, callsign: res.json.callsign };
}

// The two hooks a /clear actually fires, in the order the CLI fires them.
async function clearInto(daemon, cwd, oldSid) {
  const newSid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: oldSid, cwd, reason: 'clear' });
  const res = await postHook(daemon.baseUrl, 'SessionStart', { session_id: newSid, cwd, source: 'clear' });
  return { newSid, callsign: res.json.callsign };
}

test('a /clear continues the SAME card under a new session id — one card, not two', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-succ-basic');
  const { sid, callsign } = await startSession(daemon, cwd);

  const { newSid, callsign: heirCallsign } = await clearInto(daemon, cwd, sid);
  assert.notEqual(newSid, sid, 'the CLI really does mint a new id');
  assert.equal(heirCallsign, callsign, 'the heir answers to the callsign the human already knows');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const visible = cardsIn(state, cwd);
  assert.equal(visible.length, 1, 'exactly ONE card survives the /clear');
  assert.equal(visible[0].session_id, newSid, 'and it is the live one');
  assert.equal(visible[0].callsign, callsign);
  assert.equal(visible[0].endedAt, null, 'the continued card is live');

  // The predecessor is retired: archived (so it is off the board and its name is
  // free for the heir), superseded, and pointing at its heir.
  const prev = withDb(home, db => db.prepare('SELECT * FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(prev.archived_at, 'predecessor is archived');
  assert.equal(prev.end_reason, 'superseded');
  assert.equal(prev.succeeded_by, newSid);
  assert.ok(String(prev.note).includes('continued as'), 'the retirement says where the card went');
});

test('the pane follows the card across a /clear — the terminal button keeps driving the live session', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-succ-pane');
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'work' });
  assert.equal(spawned.status, 200, JSON.stringify(spawned.json));
  const sid = spawned.json.session_id;
  const callsign = spawned.json.callsign;
  // The pane registers (the fixture's first hook), so the spawn row is live.
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' });

  const before = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.equal(before.spawn.status, 'live');
  const window = before.spawn.tmux_window;

  // The agent in that pane runs /clear.
  const { newSid } = await clearInto(daemon, cwd, sid);

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(cardsIn(state, cwd).length, 1, 'the spawned worker did not split into two cards');
  const heir = cardOf(state, newSid);
  assert.ok(heir.spawn, 'the heir owns the pane');
  assert.equal(heir.spawn.tmux_window, window, 'the SAME window — no pane was left stranded');
  assert.equal(heir.spawn.status, 'live');
  assert.equal(heir.callsign, callsign, 'inheriting the callsign is what keeps fd<port>-<callsign> valid');

  // The row moved rather than being duplicated.
  const rows = withDb(home, db => db.prepare('SELECT session_id, status FROM spawns WHERE tmux_window = ?').all(window));
  assert.equal(rows.length, 1, 'one spawn row, reassigned — not a second lineage');
  assert.equal(rows[0].session_id, newSid);
});

test('pending mail and the ticket follow the card across a /clear', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-succ-mail');
  const { sid, callsign } = await startSession(daemon, cwd);
  // A ticket pin (manual, so it survives independently of any branch).
  await postJson(`${daemon.baseUrl}/command`, { text: `ticket ${callsign} PROJ-42` });
  const ticketed = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid).callsign;

  // Mail arrives while the session is alive, undelivered (no watcher, no pane).
  await postJson(`${daemon.baseUrl}/mail`, { to: ticketed, from: 'operator', text: 'read me after the clear' });

  const { newSid } = await clearInto(daemon, cwd, sid);
  const heir = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, newSid);
  assert.equal(heir.callsign, ticketed, 'the ticket name came along');
  assert.equal(heir.ticket, 'PROJ-42');
  assert.equal(heir.mail_pending.count, 1, 'the undelivered mail followed the card, not the dead id');

  const rows = withDb(home, db => db.prepare('SELECT to_session FROM mail WHERE delivered_at IS NULL').all());
  assert.deepEqual(rows.map(r => r.to_session), [newSid]);
});

test('a retired predecessor is never adopt-eligible — its lineage continued elsewhere', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-succ-noadopt');
  const { sid } = await startSession(daemon, cwd);
  const { newSid } = await clearInto(daemon, cwd, sid);

  // Directly POSTing adopt at the superseded id must refuse: resuming it would
  // fork the lineage into a second live session against a closed transcript.
  const res = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  assert.equal(res.status, 409);
  assert.match(res.json.reason, /board-owned|hook-proven|superseded/i);
  assert.ok(newSid, 'the heir is the one that carries on');
});

test('a straggler hook for the retired id never resurrects it — two cards under one callsign is the bug, not the cure', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-succ-straggler');
  const { sid, callsign } = await startSession(daemon, cwd);
  const { newSid } = await clearInto(daemon, cwd, sid);

  // Hooks are fire-and-forget: one in flight when the /clear landed (a
  // Notification, a FileChanged, a PostToolUse from the dying process) can
  // arrive AFTER the succession. The resurrection rule ("a late hook proves the
  // process is alive") must not apply here — the process is not alive, its
  // conversation moved house, and the heir already wears this callsign.
  await postHook(daemon.baseUrl, 'PostToolUse', { session_id: sid, cwd, tool_name: 'Bash', tool_input: { command: 'echo late' } });
  await postHook(daemon.baseUrl, 'Notification', { session_id: sid, cwd, message: 'late notification' });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const visible = cardsIn(state, cwd);
  assert.equal(visible.length, 1, 'the ghost stayed retired — still ONE card');
  assert.equal(visible[0].session_id, newSid);
  assert.equal(visible[0].callsign, callsign, 'and it is the only holder of the callsign');

  const ghost = withDb(home, db => db.prepare('SELECT archived_at, ended_at, end_reason, succeeded_by FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(ghost.archived_at, 'still archived');
  assert.ok(ghost.ended_at, 'still ended');
  assert.equal(ghost.end_reason, 'superseded', 'still superseded');
  assert.equal(ghost.succeeded_by, newSid);
});

test('a /clear with NO successor still keeps the card live (the legacy same-id path)', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-succ-legacy');
  const { sid, callsign } = await startSession(daemon, cwd);

  // An older CLI keeps the id: SessionEnd(clear) arrives and nothing follows.
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason: 'clear' });

  const card = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, sid);
  assert.ok(card, 'the card is still on the board');
  assert.equal(card.endedAt, null, 'a /clear is still not an end');
  assert.equal(card.callsign, callsign);
  assert.match(card.note, /context cleared/);
  const row = withDb(home, db => db.prepare('SELECT cleared_at, succeeded_by, archived_at FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(row.cleared_at, 'the correlation window was opened');
  assert.equal(row.succeeded_by, null, 'nobody claimed it');
  assert.equal(row.archived_at, null, 'so the card was never retired');
});

test('an unrelated new session in the same cwd is not swallowed as a continuation', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-succ-unrelated');
  const { sid } = await startSession(daemon, cwd);
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason: 'clear' });

  // A genuinely new session (source='startup', not 'clear') starting in the same
  // cwd is a stranger, not an heir — it must get its own card.
  const stranger = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: stranger, cwd, source: 'startup' });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(cardsIn(state, cwd).length, 2, 'two real sessions, two cards');
  assert.ok(cardOf(state, sid), 'the cleared session kept its own card');
  assert.ok(cardOf(state, stranger));
});

test('an ambiguous double-clear in one cwd refuses to merge — a wrong merge is worse than a spare card', async (t) => {
  const { daemon, cwd } = await boot(t, 'fleetdeck-succ-ambiguous');
  const a = await startSession(daemon, cwd);
  const b = await startSession(daemon, cwd);
  // Both bare sessions in the same cwd clear inside the same window: there is no
  // honest way to say which one the heir continues.
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: a.sid, cwd, reason: 'clear' });
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: b.sid, cwd, reason: 'clear' });

  const heir = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: heir, cwd, source: 'clear' });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const card = cardOf(state, heir);
  assert.ok(card, 'the heir got its own ordinary card');
  assert.notEqual(card.callsign, a.callsign);
  assert.notEqual(card.callsign, b.callsign);
  assert.ok(cardOf(state, a.sid) && cardOf(state, b.sid), 'neither candidate was retired on a guess');
});

test('the boot heal repairs a pair already split by a /clear, and is idempotent', async (t) => {
  const { daemon, home, cwd, env } = await boot(t, 'fleetdeck-succ-heal', {
    // Succession off at hook time, so we can manufacture the exact pre-0.7.1
    // wreckage: two cards, the pane stranded on the silent one.
    FLEETDECK_CLEAR_SUCCESSION_MS: '0',
  });
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'work' });
  const sid = spawned.json.session_id;
  const callsign = spawned.json.callsign;
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' });
  const { newSid } = await clearInto(daemon, cwd, sid);

  // The wreckage: two cards, and the pane is on the card that will never update.
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(cardsIn(state, cwd).length, 2, 'this is the bug, reproduced');
  assert.ok(cardOf(state, sid).spawn, 'the stranded card holds the pane');
  assert.equal(cardOf(state, newSid).spawn, undefined, 'the working card has none');
  const window = cardOf(state, sid).spawn.tmux_window;
  await daemon.stop({ keepHome: true });

  // Boot the fixed daemon on the same home: the heal runs at listen.
  const healed = await startDaemon({ home, env: { ...env, FLEETDECK_CLEAR_SUCCESSION_MS: '30000' } });
  t.after(() => healed.stop({ keepHome: true }));
  state = (await getJson(`${healed.baseUrl}/state`)).json;
  assert.equal(cardsIn(state, cwd).length, 1, 'one card after the heal');
  const heir = cardOf(state, newSid);
  assert.ok(heir, 'the surviving card is the live one');
  assert.equal(heir.callsign, callsign, 'it inherited the lineage callsign');
  assert.equal(heir.spawn?.tmux_window, window, 'and the pane came with it');

  const prev = withDb(home, db => db.prepare('SELECT archived_at, end_reason, succeeded_by FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(prev.archived_at);
  assert.equal(prev.end_reason, 'superseded');
  assert.equal(prev.succeeded_by, newSid);

  // The heir's throwaway pre-heal name is NOT kept as the mail anchor — see the
  // dedicated test below for why that matters.
  const anchor = withDb(home, db => db.prepare('SELECT callsign, prev_callsign FROM sessions WHERE session_id = ?').get(newSid));
  assert.equal(anchor.callsign, callsign);
  assert.notEqual(anchor.prev_callsign, 'badger-artifact');

  // Idempotent: a second boot heals nothing and breaks nothing.
  await healed.stop({ keepHome: true });
  const again = await startDaemon({ home, env: { ...env, FLEETDECK_CLEAR_SUCCESSION_MS: '30000' } });
  t.after(() => again.stop({ keepHome: true }));
  const after = (await getJson(`${again.baseUrl}/state`)).json;
  assert.equal(cardsIn(after, cwd).length, 1, 'still one card');
  assert.equal(cardOf(after, newSid).spawn?.tmux_window, window);
});

test('hooks arriving out of order still land one card — the heir can beat its predecessor’s /clear', async (t) => {
  const { daemon, home, cwd } = await boot(t, 'fleetdeck-succ-reorder');
  const { sid, callsign } = await startSession(daemon, cwd);

  // SessionEnd is an ASYNC hook; SessionStart is not. So the heir's birth can
  // reach the daemon FIRST. It arrives as an orphan with its own card…
  const heirSid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: heirSid, cwd, source: 'clear' });
  // …and only then does the /clear that produced it land. The daemon must look
  // FORWARD and claim the heir waiting for it.
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason: 'clear' });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(cardsIn(state, cwd).length, 1, 'out-of-order hooks must not split the card');
  const heir = cardOf(state, heirSid);
  assert.ok(heir, 'the heir survived');
  assert.equal(heir.callsign, callsign, 'wearing the lineage callsign');
  const prev = withDb(home, db => db.prepare('SELECT archived_at, succeeded_by FROM sessions WHERE session_id = ?').get(sid));
  assert.ok(prev.archived_at);
  assert.equal(prev.succeeded_by, heirSid);
});

test('the boot heal never merges two lineages into one heir', async (t) => {
  const { daemon, home, cwd, env } = await boot(t, 'fleetdeck-succ-nomerge', {
    FLEETDECK_CLEAR_SUCCESSION_MS: '0', // succession off at hook time: manufacture the wreckage
  });
  // Two pane-less sessions in ONE cwd, clearing a few seconds apart. Walking
  // predecessors would hand BOTH of them the earlier heir — dumping one
  // conversation's mail, questions and file ledger onto the other's card.
  const a = await startSession(daemon, cwd);
  const heirA = (await clearInto(daemon, cwd, a.sid)).newSid;
  const b = await startSession(daemon, cwd);
  const heirB = (await clearInto(daemon, cwd, b.sid)).newSid;

  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(cardsIn(state, cwd).length, 4, 'four cards: the bug, doubled');
  await daemon.stop({ keepHome: true });

  const healed = await startDaemon({ home, env: { ...env, FLEETDECK_CLEAR_SUCCESSION_MS: '30000' } });
  t.after(() => healed.stop({ keepHome: true }));
  state = (await getJson(`${healed.baseUrl}/state`)).json;

  // Each heir continues its OWN predecessor — two cards, two lineages, no merge.
  assert.equal(cardsIn(state, cwd).length, 2, 'two lineages stay two cards');
  assert.equal(cardOf(state, heirA).callsign, a.callsign, 'heir A continues A');
  assert.equal(cardOf(state, heirB).callsign, b.callsign, 'heir B continues B');
  const claims = withDb(home, db => db.prepare('SELECT session_id, succeeded_by FROM sessions WHERE succeeded_by IS NOT NULL').all());
  assert.equal(claims.length, 2);
  assert.equal(new Set(claims.map(r => r.succeeded_by)).size, 2, 'no heir was claimed twice');
});

test('a healed card keeps the name the fleet knows — the throwaway never becomes the mail anchor', async (t) => {
  const { daemon, home, cwd, env } = await boot(t, 'fleetdeck-succ-anchor', {
    FLEETDECK_CLEAR_SUCCESSION_MS: '0', // manufacture the pre-0.7.1 split
  });
  const { sid, callsign } = await startSession(daemon, cwd);
  const { newSid } = await clearInto(daemon, cwd, sid);
  const stranger = cardOf((await getJson(`${daemon.baseUrl}/state`)).json, newSid).callsign;
  assert.notEqual(stranger, callsign, 'the split gave the heir a name of its own');
  await daemon.stop({ keepHome: true });

  const healed = await startDaemon({ home, env: { ...env, FLEETDECK_CLEAR_SUCCESSION_MS: '30000' } });
  t.after(() => healed.stop({ keepHome: true }));
  assert.equal(cardOf((await getJson(`${healed.baseUrl}/state`)).json, newSid).callsign, callsign);

  // prev_callsign has ONE slot (write-once). The heir's throwaway name existed
  // only because of the bug, while the lineage name is the one in every peer's
  // roster brief, in the ticker, and on the tmux window — so the anchor must
  // hold the lineage name, not the artifact. Prove it by renaming the healed
  // card and then mailing the name the fleet actually knows.
  const renamed = await postJson(`${healed.baseUrl}/api/sessions/${newSid}/name`, { suffix: 'after-heal' });
  assert.equal(renamed.status, 200, JSON.stringify(renamed.json));
  assert.equal(renamed.json.previous, callsign);
  await postJson(`${healed.baseUrl}/mail`, { to: callsign, from: 'operator', text: 'the name everyone knows' });
  const card = cardOf((await getJson(`${healed.baseUrl}/state`)).json, newSid);
  assert.equal(card.mail_pending.count, 1, 'mail to the lineage name still lands after a rename');

  // And reverting hands back the lineage name, never the throwaway.
  const reverted = await postJson(`${healed.baseUrl}/api/sessions/${newSid}/name`, { clear: true });
  assert.equal(reverted.json.callsign, callsign, 'revert restores the name the fleet knows');
  assert.notEqual(reverted.json.callsign, stranger);
});

test('the boot heal is a no-op on a fleet that never forked', async (t) => {
  const { daemon, home, cwd, env } = await boot(t, 'fleetdeck-succ-noop');
  const { sid, callsign } = await startSession(daemon, cwd);
  await daemon.stop({ keepHome: true });

  const rebooted = await startDaemon({ home, env });
  t.after(() => rebooted.stop({ keepHome: true }));
  const state = (await getJson(`${rebooted.baseUrl}/state`)).json;
  const card = cardOf(state, sid);
  assert.ok(card, 'the untouched session is still here');
  assert.equal(card.callsign, callsign, 'with the name it always had');
  assert.equal(card.endedAt, null);
});
