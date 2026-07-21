// tests/ticket-callsign.test.mjs
//
// 0.6.0 Jira-ticket callsigns (daemon-level, in the style of
// session-lifecycle.test.mjs): a session's hex suffix is replaced by its Jira
// issue key when one is known — `raven-PROJ-123` — auto-detected from the
// server-derived git branch or pinned via the manual `ticket` command.
//
// Contract under test (pinned interfaces):
//   - /state session objects carry `ticket`, `ticket_source`
//     ('branch'|'manual'|null) and `prev_callsign` (null when unset).
//   - Ticketed callsign = `<animal>-<KEY-N>`; when all 12 animals are taken for
//     a ticket, the hex fallback `<animal>-<sid4>` is used but the ticket is
//     still recorded.
//   - POST /command {text:'ticket <target> <KEY|clear>'} → success
//     {ok:true, renamed, callsign, ticket, session_id, previous?}; every
//     error/malformed path {ok:false, reason} and NEVER a note.
//   - SessionStart brief carries `as "<callsign>" (ticket <KEY>)` when ticketed.
//
// These are TDD tests: the daemon implementation lands in parallel, so several
// will fail until scripts/fleetd/{tickets,db,derive,events,commands,helpers,
// mail,snapshot,statements}.mjs are all wired. That is expected.

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeRepoWithWorktree } from './helpers/gitrepo.mjs';

// The 12-word rotation (pinned). Rotation start = countSessions % 12.
const ANIMALS = ['falcon', 'otter', 'raven', 'lynx', 'orca', 'wren', 'viper', 'heron', 'badger', 'comet', 'ember', 'drift'];
const ANIMAL_ALT = ANIMALS.join('|');
const HEX_SUFFIX_RE = /^[a-z]+-[0-9a-f]{4}$/; // ticketless / hex-fallback callsign

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}
function animalOf(callsign) {
  return String(callsign).split('-')[0]; // animal = text before the FIRST hyphen
}
async function getState(daemon) {
  return (await getJson(`${daemon.baseUrl}/state`)).json;
}
async function getCard(daemon, sid) {
  return findSession(await getState(daemon), sid);
}
function sessionStart(daemon, sid, cwd) {
  return postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
}
function command(daemon, text) {
  return postJson(`${daemon.baseUrl}/command`, { text });
}
function plainDir() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-ticket-cwd-'));
}

// ---------------------------------------------------------------------------
// Birth naming
// ---------------------------------------------------------------------------

test('birth on a ticket branch → animal-first ticketed callsign, brief announces the ticket, one join tick, no rename', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fd-ticket-birth', branch: 'feature/PROJ-123-checkout' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const sid = randomUUID();
  const start = await sessionStart(daemon, sid, repo.worktree);
  assert.equal(start.status, 200, 'SessionStart should 200');
  const callsign = start.json.callsign;
  assert.match(callsign, new RegExp(`^(${ANIMAL_ALT})-PROJ-123$`),
    `birth callsign should be <animal>-PROJ-123 (got ${callsign})`);
  assert.match(start.json.brief, /as "[^"]+" \(ticket PROJ-123\)/,
    'the SessionStart brief should announce the ticket after the callsign');

  const card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'PROJ-123', 'the card records the detected ticket');
  assert.equal(card.ticket_source, 'branch', 'the ticket source is the branch');
  assert.equal(card.prev_callsign ?? null, null, 'birth naming is not a rename — no prev_callsign');

  const state = await getState(daemon);
  const joined = state.ticker.filter(tk => tk.msg === `${callsign} joined the fleet`);
  assert.equal(joined.length, 1, 'exactly one "joined the fleet" tick at birth');
});

test('13 sessions on one ticket → 12 distinct animals, then a hex fallback that still records the ticket; all callsigns unique', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fd-ticket-cascade', branch: 'feature/PROJ-42-fleet' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); });

  const callsigns = [];
  for (let i = 0; i < 13; i++) {
    const sid = randomUUID();
    const start = await sessionStart(daemon, sid, repo.worktree);
    assert.equal(start.status, 200, `SessionStart #${i + 1} should 200`);
    const card = await getCard(daemon, sid);
    assert.equal(card.ticket, 'PROJ-42', `session #${i + 1} records the ticket (even the hex-fallback 13th)`);
    callsigns.push(card.callsign);
  }

  assert.equal(new Set(callsigns).size, 13, 'all 13 callsigns must be unique');

  const first12 = callsigns.slice(0, 12);
  for (const cs of first12) {
    assert.match(cs, new RegExp(`^(${ANIMAL_ALT})-PROJ-42$`), `${cs} should be a distinct animal on PROJ-42`);
  }
  assert.equal(new Set(first12.map(animalOf)).size, 12, 'the first 12 sessions take 12 distinct animals');

  const thirteenth = callsigns[12];
  assert.match(thirteenth, HEX_SUFFIX_RE, `the 13th session should hex-fallback once all 12 animals are taken (got ${thirteenth})`);
  assert.ok(!/-PROJ-42$/.test(thirteenth), 'the hex fallback is NOT ticket-suffixed');
});

// ---------------------------------------------------------------------------
// Late detection (rename once)
// ---------------------------------------------------------------------------

test('rename-once: a ticketless session renamed by a ticket-branch event; a later different-ticket branch changes nothing', async (t) => {
  const plain = plainDir();
  const repo77 = makeRepoWithWorktree({ repoName: 'fd-rename-77', branch: 'feature/PROJ-77-fix' });
  const repoEng = makeRepoWithWorktree({ repoName: 'fd-rename-eng', branch: 'feature/ENG-1-later' });
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
    rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    repo77.cleanup();
    repoEng.cleanup();
  });

  const sid = randomUUID();
  // Ticketless birth (plain, non-git dir) → hex suffix, no ticket.
  const start = await sessionStart(daemon, sid, plain);
  const birthCallsign = start.json.callsign;
  assert.match(birthCallsign, HEX_SUFFIX_RE, 'a ticketless birth uses the hex suffix');
  let card = await getCard(daemon, sid);
  assert.equal(card.ticket ?? null, null);
  assert.equal(card.prev_callsign ?? null, null);

  // An event whose server-derived branch carries a key → renamed ONCE.
  await postHook(daemon.baseUrl, 'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd: repo77.worktree }, { prompt: 'work the fix' }), { token: daemon });
  card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'PROJ-77', 'the ticket-branch event detects and pins the ticket');
  assert.equal(card.ticket_source, 'branch');
  const renamed = card.callsign;
  assert.match(renamed, new RegExp(`^(${ANIMAL_ALT})-PROJ-77$`));
  assert.equal(animalOf(renamed), animalOf(birthCallsign), 'the rename keeps the birth animal when free');
  assert.equal(card.prev_callsign, birthCallsign, 'prev_callsign becomes the birth callsign on the first rename');

  const afterRename = await getState(daemon);
  assert.ok(afterRename.ticker.some(tk => tk.msg.includes(birthCallsign) && tk.msg.includes(renamed)),
    'a single ticker line names BOTH the old and the new callsign');

  // A later event from a DIFFERENT ticket branch must NOT rename again.
  await postHook(daemon.baseUrl, 'PostToolUse',
    loadFixture('post-tool-use-bash', { session_id: sid, cwd: repoEng.worktree }), { token: daemon });
  card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'PROJ-77', 'auto-rename fires at most once — the ticket does not change');
  assert.equal(card.callsign, renamed, 'the callsign does not change on a later different-ticket branch');
  assert.equal(card.prev_callsign, birthCallsign, 'prev_callsign stays the birth name');
});

// ---------------------------------------------------------------------------
// Manual override / re-override / clear
// ---------------------------------------------------------------------------

test('manual ticket: override renames, blocks later auto-detect, re-override keeps prev_callsign write-once, clear reverts and pins', async (t) => {
  const plain = plainDir();
  const repo99 = makeRepoWithWorktree({ repoName: 'fd-manual-99', branch: 'feature/PROJ-99-auto' });
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); repo99.cleanup(); });

  const sid = randomUUID();
  const start = await sessionStart(daemon, sid, plain);
  const birthCallsign = start.json.callsign;
  assert.match(birthCallsign, HEX_SUFFIX_RE);

  // 1) Manual override.
  let res = await command(daemon, `ticket ${birthCallsign} PROJ-55`);
  assert.equal(res.status, 200);
  assert.equal(res.json.ok, true, `ticket override should succeed (got ${JSON.stringify(res.json)})`);
  assert.equal(res.json.renamed, true);
  assert.equal(res.json.ticket, 'PROJ-55');
  assert.equal(res.json.session_id, sid);
  assert.match(res.json.callsign, new RegExp(`^(${ANIMAL_ALT})-PROJ-55$`));
  assert.equal(res.json.previous, birthCallsign, 'the response reports the previous callsign');
  const manualCallsign = res.json.callsign;

  let card = await getCard(daemon, sid);
  assert.equal(card.callsign, manualCallsign);
  assert.equal(card.ticket, 'PROJ-55');
  assert.equal(card.ticket_source, 'manual', 'a manual ticket pins ticket_source=manual');
  assert.equal(card.prev_callsign, birthCallsign);

  // 2) A later ticket-branch event must NOT override the manual pin.
  await postHook(daemon.baseUrl, 'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd: repo99.worktree }, { prompt: 'context switch' }), { token: daemon });
  card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'PROJ-55', 'ticket_source=manual permanently blocks auto-detect');
  assert.equal(card.callsign, manualCallsign);

  // 3) Second manual re-renames; prev_callsign stays the birth name (write-once).
  res = await command(daemon, `ticket ${sid} ENG-9`);
  assert.equal(res.json.ok, true, 'manual re-ticket should succeed');
  assert.equal(res.json.renamed, true);
  assert.equal(res.json.ticket, 'ENG-9');
  assert.match(res.json.callsign, new RegExp(`^(${ANIMAL_ALT})-ENG-9$`));
  const engCallsign = res.json.callsign;
  card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'ENG-9');
  assert.equal(card.prev_callsign, birthCallsign, 'prev_callsign is write-once — still the birth name after re-ticket');

  // 4) Clear reverts to the birth name and pins the auto path OFF. The dropped
  // ticketed name moves into prev_callsign (never nulled) so peers that cached
  // it from briefs/ticker can still route mail to it.
  res = await command(daemon, `ticket ${sid} clear`);
  assert.equal(res.json.ok, true, 'ticket clear should succeed');
  card = await getCard(daemon, sid);
  assert.equal(card.callsign, birthCallsign, 'clear reverts the callsign to the birth name');
  assert.equal(card.ticket ?? null, null, 'clear drops the ticket');
  assert.equal(card.ticket_source, 'manual', 'clear pins ticket_source=manual so auto-detect stays off');
  assert.equal(card.prev_callsign, engCallsign, 'clear keeps the dropped ticketed name routable via prev_callsign');

  // 5) Auto-detect stays off after a clear.
  await postHook(daemon.baseUrl, 'PostToolUse',
    loadFixture('post-tool-use-bash', { session_id: sid, cwd: repo99.worktree }), { token: daemon });
  card = await getCard(daemon, sid);
  assert.equal(card.ticket ?? null, null, 'a ticket-branch event after clear must not re-ticket (manual pin holds)');
  assert.equal(card.callsign, birthCallsign);
});

test('manual ticket: an invalid key or an unknown target is refused loudly and never becomes a note', async (t) => {
  const plain = plainDir();
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  const start = await sessionStart(daemon, sid, plain);
  const birthCallsign = start.json.callsign;

  // Invalid key shapes.
  for (const bad of ['notakey', 'PROJ-007', 'proj', 'PROJ-0']) {
    const res = await command(daemon, `ticket ${birthCallsign} ${bad}`);
    assert.equal(res.status, 200);
    assert.equal(res.json.ok, false, `"${bad}" is not a valid key — must be refused`);
    assert.equal(typeof res.json.reason, 'string', 'a refusal carries a string reason');
  }

  // Unknown target.
  let res = await command(daemon, `ticket ghost-9999 PROJ-1`);
  assert.equal(res.json.ok, false, 'an unknown target is refused');
  assert.equal(typeof res.json.reason, 'string');

  // Bare / malformed ticket command (missing arguments).
  res = await command(daemon, `ticket`);
  assert.equal(res.json.ok, false, 'a bare ticket command is refused, not silently accepted');

  // Nothing renamed the real session or set a ticket…
  const card = await getCard(daemon, sid);
  assert.equal(card.callsign, birthCallsign, 'no failed ticket command renamed the session');
  assert.equal(card.ticket ?? null, null);

  // …and none of it fell through to the orchestrator-note path.
  const state = await getState(daemon);
  assert.ok(!state.ticker.some(tk => /orchestrator note/i.test(tk.msg)),
    'a malformed/failed ticket command must NEVER become a note');
});

test('manual ticket: an ambiguous target (a callsign shared by two live sessions) is refused; the session id disambiguates', async (t) => {
  const plain = plainDir();
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // Ticketless births use the hex suffix and — unlike ticketed births — do not
  // consult the taken-check (current behavior, preserved by the plan). Two
  // ticketless sessions collide when their UUIDs share the first 4 hex chars
  // AND land on the same animal. Rotation start = countSessions % 12 (pinned),
  // so session #1 (count 0 → falcon) and session #13 (count 12 → falcon) share
  // a callsign when both UUIDs start with the same 4 hex.
  const sidA = 'dead' + randomUUID().slice(4);
  const startA = await sessionStart(daemon, sidA, plain);
  for (let i = 0; i < 11; i++) {
    await sessionStart(daemon, randomUUID(), plain); // advance count 1 → 12
  }
  const sidM = 'dead' + randomUUID().slice(4);
  const startM = await sessionStart(daemon, sidM, plain);

  assert.equal(startA.json.callsign, startM.json.callsign,
    'precondition: two live ticketless sessions share a callsign (rotation collision)');
  const shared = startA.json.callsign;

  const res = await command(daemon, `ticket ${shared} PROJ-1`);
  assert.equal(res.json.ok, false, 'a target matching two live sessions is ambiguous');
  assert.match(String(res.json.reason ?? ''), /ambig|session id/i,
    'the reason should point the human at the session id');

  // The session id resolves to exactly one row — unambiguous.
  const byId = await command(daemon, `ticket ${sidA} PROJ-1`);
  assert.equal(byId.json.ok, true, 'the session id disambiguates the ambiguous callsign');
  assert.equal(byId.json.session_id, sidA);
});

// ---------------------------------------------------------------------------
// Mail / assign routing to the birth name after a rename
// ---------------------------------------------------------------------------

test('mail and assign to the birth name after a rename still deliver to the renamed session', async (t) => {
  const plain = plainDir();
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  const start = await sessionStart(daemon, sid, plain);
  const birthCallsign = start.json.callsign;

  const renameRes = await command(daemon, `ticket ${sid} PROJ-1`);
  assert.equal(renameRes.json.ok, true);
  const newCallsign = renameRes.json.callsign;
  assert.notEqual(newCallsign, birthCallsign);

  // Mail to the BIRTH (now prev_callsign) name still routes to the session.
  let mailRes = await postJson(`${daemon.baseUrl}/mail`, { to: birthCallsign, from: 'operator', text: 'to the old name' }, { token: daemon });
  assert.equal(mailRes.status, 200);
  assert.equal(mailRes.json.delivered, 1, 'mail to the birth name still finds the renamed session');
  assert.equal(mailRes.json.targets[0].session_id, sid);

  // Mail to the NEW name is unaffected.
  mailRes = await postJson(`${daemon.baseUrl}/mail`, { to: newCallsign, from: 'operator', text: 'to the new name' }, { token: daemon });
  assert.equal(mailRes.json.delivered, 1, 'mail to the current name delivers');
  assert.equal(mailRes.json.targets[0].session_id, sid);

  // Assign (POST /command) to the birth name delivers too.
  const assignRes = await command(daemon, `assign ${birthCallsign} audit the mailbox`);
  assert.equal(assignRes.json.ok, true);
  assert.equal(assignRes.json.delivered, 1, 'assign to the birth name reaches the renamed session');
});

// ---------------------------------------------------------------------------
// Tombstone holds its name
// ---------------------------------------------------------------------------

test('tombstone holds its animal: an ended-but-unarchived ticketed session blocks its animal for that ticket', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fd-tombstone', branch: 'feature/PROJ-42-hold' });
  const plain = plainDir();
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); repo.cleanup(); rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // A: the first session (count 0 → falcon) on PROJ-42.
  const sidA = randomUUID();
  const startA = await sessionStart(daemon, sidA, repo.worktree);
  const callsignA = startA.json.callsign;
  assert.match(callsignA, /^falcon-PROJ-42$/, 'the first session takes falcon on PROJ-42');

  // End A: offline, but NOT archived (retention is 24h) → it still holds its name.
  await postHook(daemon.baseUrl, 'SessionEnd', loadFixture('session-end', { session_id: sidA, cwd: repo.worktree }), { token: daemon });
  const cardA = await getCard(daemon, sidA);
  assert.equal(cardA.col, 'offline', 'A is offline after SessionEnd');
  assert.ok(cardA.endedAt, 'A has an endedAt (tombstone), but is not archived');

  // 11 ticketless fillers so the NEXT ticket session's rotation start returns to
  // falcon (count 12 % 12 = 0) — forcing the taken-check, not mere rotation, to
  // be what pushes B off falcon.
  for (let i = 0; i < 11; i++) {
    await sessionStart(daemon, randomUUID(), plain);
  }

  // B: the next PROJ-42 session. Rotation points at falcon, but the tombstone
  // holds falcon-PROJ-42, so B must take the next free animal instead.
  const sidB = randomUUID();
  const startB = await sessionStart(daemon, sidB, repo.worktree);
  const callsignB = startB.json.callsign;
  const cardB = await getCard(daemon, sidB);

  assert.equal(cardB.ticket, 'PROJ-42');
  assert.notEqual(callsignB, callsignA, 'B must not reuse the tombstone\'s exact callsign');
  assert.notEqual(animalOf(callsignB), 'falcon', 'B skips the animal the dead-but-unarchived session still holds');
  assert.match(callsignB, new RegExp(`^(${ANIMAL_ALT})-PROJ-42$`), 'B is still ticketed on PROJ-42');
});

// ---------------------------------------------------------------------------
// Migration from a pre-0.6.0 (0.5.0-DDL) database
// ---------------------------------------------------------------------------

test('migration: a pre-0.6.0 fleetd.db gains the ticket columns; old rows read ticket:null and the ticket command works', async (t) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-migrate-home-'));

  // Seed a sessions table with the exact 0.5.0 DDL (NO ticket columns) plus a
  // couple of populated rows. openDb()'s DDL is CREATE TABLE IF NOT EXISTS, so
  // the daemon's migrate() must ALTER-ADD the three new columns onto THIS
  // pre-existing table — that is the behavior under test.
  const dbFile = path.join(home, 'fleetd.db');
  const seed = new DatabaseSync(dbFile);
  seed.exec(`
    CREATE TABLE sessions (
      session_id        TEXT PRIMARY KEY,
      callsign          TEXT,
      model             TEXT,
      cwd               TEXT,
      repo_id           TEXT,
      repo_name         TEXT,
      branch            TEXT,
      worktree          TEXT,
      col               TEXT DEFAULT 'queued',
      note              TEXT,
      task              TEXT,
      last_tool         TEXT,
      events            INTEGER DEFAULT 0,
      started_at        INTEGER,
      last_seen         INTEGER,
      ended_at          INTEGER,
      blocked_this_turn INTEGER DEFAULT 0,
      source            TEXT DEFAULT 'hooks',
      notification_type TEXT,
      archived_at       INTEGER
    );`);
  const now = Date.now();
  const ins = seed.prepare(`INSERT INTO sessions
    (session_id, callsign, col, note, events, started_at, last_seen, source)
    VALUES (?, ?, 'idle', 'pre-0.6.0 row', 3, ?, ?, 'hooks')`);
  const sid1 = randomUUID();
  const sid2 = randomUUID();
  ins.run(sid1, 'falcon-old1', now, now);
  ins.run(sid2, 'otter-old2', now, now);
  seed.close();

  const daemon = await startDaemon({ home });
  t.after(async () => { await daemon.stop(); rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  // The old rows survive the migration and read ticket:null.
  const state = await getState(daemon);
  const c1 = findSession(state, sid1);
  const c2 = findSession(state, sid2);
  assert.ok(c1 && c2, 'both seeded 0.5.0 rows survive the migration and appear in /state');
  assert.equal(c1.callsign, 'falcon-old1');
  assert.equal(c1.ticket ?? null, null, 'a migrated 0.5.0 row has a null ticket (truthful backfill)');
  assert.equal(c1.ticket_source ?? null, null);
  assert.equal(c1.prev_callsign ?? null, null);

  // The ticket command WORKS on a migrated row — proof the ticket column was
  // actually added (a no-column migration would make updateSession throw).
  const res = await command(daemon, `ticket ${sid1} PROJ-7`);
  assert.equal(res.status, 200);
  assert.equal(res.json.ok, true, 'the ticket command succeeds on a migrated 0.5.0 row');
  assert.equal(res.json.ticket, 'PROJ-7');

  const after = findSession(await getState(daemon), sid1);
  assert.equal(after.ticket, 'PROJ-7');
  assert.equal(after.ticket_source, 'manual');
  assert.match(after.callsign, /^falcon-PROJ-7$/, 'the migrated row keeps its animal on rename');
  assert.equal(after.prev_callsign, 'falcon-old1', 'the birth name is captured as prev_callsign on the first rename');
});

// ---------------------------------------------------------------------------
// FIELDS-whitelist round-trip regression (black-box)
// ---------------------------------------------------------------------------

test('updateSession round-trips ticket/ticket_source/prev_callsign across later events (FIELDS whitelist regression)', async (t) => {
  const plain = plainDir();
  const daemon = await startDaemon();
  t.after(async () => { await daemon.stop(); rmSync(plain, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  const start = await sessionStart(daemon, sid, plain);
  const birthCallsign = start.json.callsign;

  // Set a ticket via the command — this writes all three new columns at once.
  const res = await command(daemon, `ticket ${sid} PROJ-3`);
  assert.equal(res.json.ok, true);
  const ticketedCallsign = res.json.callsign;

  let card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'PROJ-3');
  assert.equal(card.ticket_source, 'manual');
  assert.equal(card.prev_callsign, birthCallsign);

  // Drive several more hook events; each runs updateSession one-to-three times
  // on OTHER columns (last_seen/events/col/note/…). A column silently dropped
  // from the FIELDS whitelist would vanish here.
  await postHook(daemon.baseUrl, 'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd: plain }, { prompt: 'keep going' }), { token: daemon });
  await postHook(daemon.baseUrl, 'PostToolUse',
    loadFixture('post-tool-use-edit', { session_id: sid, cwd: plain }), { token: daemon });
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd: plain }), { token: daemon });

  card = await getCard(daemon, sid);
  assert.equal(card.ticket, 'PROJ-3', 'ticket persists across subsequent events');
  assert.equal(card.ticket_source, 'manual', 'ticket_source persists');
  assert.equal(card.prev_callsign, birthCallsign, 'prev_callsign persists');
  assert.equal(card.callsign, ticketedCallsign, 'the renamed callsign persists across subsequent events');
});
