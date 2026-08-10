// tests/mail-and-blocking.test.mjs
//
// Stop endpoint / mailbox sharp edges:
//   - at most one mailbox block per session per turn, enforced server-side
//     (never trust stop_hook_active)
//   - UserPromptSubmit drains mail as additionalContext
//   - POST /mail targeting: session_id/callsign, "all", "repo:<name>"

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.ts';
import { postHook, postJson, getJson } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { makeRepoWithWorktree } from './helpers/gitrepo.ts';
import { openDb } from '../scripts/fleetd/db.ts';
import { createCore } from '../scripts/fleetd/derive.ts';

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

test('one block per turn: Stop blocks once on pending mail, then passes, then blocks again after a new turn + new mail', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon.token });
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon.token });

  const mailRes = await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'tester', text: 'please wrap up soon' }, { token: daemon.token });
  assert.equal(mailRes.status, 200, 'POST /mail should 200');

  // First Stop: mail is pending -> block, exactly once.
  const stop1 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }), { token: daemon.token });
  assert.equal(stop1.json?.decision, 'block', 'Stop with pending mail should block');
  assert.match(stop1.json?.reason ?? '', /\[FLEETDECK MAIL\]/, 'block reason should carry the FLEETDECK MAIL marker');

  // Immediate second Stop in the same turn: must NOT block again (server-side
  // one-block-per-turn guard; must not rely on stop_hook_active).
  const stop2 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }), { token: daemon.token });
  assert.deepEqual(stop2.json, {}, 'immediate second Stop in the same turn must return {} (no repeat block)');

  // New turn boundary via UserPromptSubmit clears the blocked_this_turn flag.
  await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { token: daemon, session_id: sid, cwd }, {
    prompt: 'continue',
  }), { token: daemon.token });

  // Fresh mail arrives mid-turn.
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'tester', text: 'second message' }, { token: daemon.token });

  // Stop should be able to block again now that a new turn has started.
  const stop3 = await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', { session_id: sid, cwd }), { token: daemon.token });
  assert.equal(stop3.json?.decision, 'block', 'Stop should block again after a new turn + new mail');
  assert.match(stop3.json?.reason ?? '', /\[FLEETDECK MAIL\]/);
});

test('UserPromptSubmit drains pending mail as additionalContext', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon.token });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'ops', text: 'ping from the board' }, { token: daemon.token });

  const res = await postHook(daemon.baseUrl, 'UserPromptSubmit', loadFixture('user-prompt-submit', { session_id: sid, cwd }), { token: daemon.token });
  const hso = res.json?.hookSpecificOutput;
  assert.ok(hso, 'UserPromptSubmit with pending mail should return hookSpecificOutput');
  assert.equal(hso.hookEventName, 'UserPromptSubmit');
  assert.match(hso.additionalContext, /^\[FLEETDECK\]/);
  assert.ok(hso.additionalContext.includes('ping from the board'), 'delivered context should carry the mail text');

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(state.mail_pending?.[sid] ?? 0, 0, 'mailbox should be drained after delivery');
});

test('GET /mail?session=<sid> drains the mailbox directly', async (t) => {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-cwd-'));
  t.after(async () => { await daemon.stop(); rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon.token });
  await postJson(`${daemon.baseUrl}/mail`, { to: sid, from: 'skill', text: 'direct drain check' }, { token: daemon.token });

  const drained = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(sid)}`);
  assert.equal(drained.status, 200);
  assert.ok(Array.isArray(drained.json?.mail), 'GET /mail should return a mail array');
  assert.equal(drained.json.mail.length, 1, 'the pending message should be present');
  assert.ok(drained.json.mail.some(m => m.text === 'direct drain check'), 'drained mail should carry the original text');

  // A second GET should come back empty -- it's a drain, not a peek.
  const second = await getJson(`${daemon.baseUrl}/mail?session=${encodeURIComponent(sid)}`);
  assert.equal((second.json?.mail || []).length, 0, 'GET /mail should drain the box, not just read it');
});

test('POST /mail targeting: session/callsign, "all", "repo:<name>"', async (t) => {
  const daemon = await startDaemon();
  const repoA = makeRepoWithWorktree({ repoName: 'fleet-repo-a' });
  const repoB = makeRepoWithWorktree({ repoName: 'fleet-repo-b' });
  t.after(async () => { await daemon.stop(); repoA.cleanup(); repoB.cleanup(); });

  const sidX = randomUUID(); // repoA
  const sidY = randomUUID(); // repoA
  const sidZ = randomUUID(); // repoB

  const regX = await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidX, cwd: repoA.root }), { token: daemon.token });
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidY, cwd: repoA.root }), { token: daemon.token });
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sidZ, cwd: repoB.root }), { token: daemon.token });

  const callsignX = regX.json?.callsign;
  assert.ok(callsignX, 'registration should hand back a callsign to target by');

  const pendingOf = (state, sid) => state.mail_pending?.[sid] ?? 0;

  // Target by callsign: only X should gain mail.
  await postJson(`${daemon.baseUrl}/mail`, { to: callsignX, from: 'operator', text: 'to X only' }, { token: daemon.token });
  let state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(pendingOf(state, sidX), 1, 'callsign-targeted mail should land on X');
  assert.equal(pendingOf(state, sidY), 0, 'Y should be untouched by callsign targeting');
  assert.equal(pendingOf(state, sidZ), 0, 'Z should be untouched by callsign targeting');

  // Target "all": every registered session gains one more.
  await postJson(`${daemon.baseUrl}/mail`, { to: 'all', from: 'operator', text: 'to everyone' }, { token: daemon.token });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(pendingOf(state, sidX), 2, '"all" should add one more to X');
  assert.equal(pendingOf(state, sidY), 1, '"all" should reach Y');
  assert.equal(pendingOf(state, sidZ), 1, '"all" should reach Z (different repo, still "all")');

  // Target "repo:<name>": only sessions in repoA gain mail; repoB untouched.
  const repoTarget = `repo:${repoA.repoName}`;
  await postJson(`${daemon.baseUrl}/mail`, { to: repoTarget, from: 'operator', text: 'to repo A' }, { token: daemon.token });
  state = (await getJson(`${daemon.baseUrl}/state`)).json;
  assert.equal(pendingOf(state, sidX), 3, `"${repoTarget}" should reach X`);
  assert.equal(pendingOf(state, sidY), 2, `"${repoTarget}" should reach Y`);
  assert.equal(pendingOf(state, sidZ), 1, `"${repoTarget}" must not reach Z (repo B)`);
});

// ---------------------------------------------------------------------------
// In-memory core harness. The tests above drive a real daemon over HTTP, but
// owned-pane mail delivery (BUG 8) and the exact stored mail columns (BUG 12)
// can only be exercised against createCore with a fake tmux adapter — the HTTP
// daemon would need a live tmux server to ever route mail to a pane. This is a
// deliberately-minimal copy of the fakeTmux/memoryCore harness in
// daemon-maintenance.test.mjs / fleet-bugs.test.mjs (it is defined inline in
// each suite, not shared), plus one hook: `state.onPaneProbe` fires inside
// paneCurrentCommand so a test can mutate session state DURING the awaited
// probe — exactly the window BUG 8 closes.
function setEnv(t, values) {
  const before = new Map(Object.keys(values).map(k => [k, process.env[k]]));
  for (const [k, v] of Object.entries(values)) process.env[k] = String(v);
  t.after(() => {
    for (const [k, v] of before) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  });
}

function fakeTmux(port = 4711) {
  const state = {
    windows: [], argv: null, calls: [], pasteOk: true, enterOk: true, killed: [],
    onPaneProbe: null, paneProbes: 0,
  };
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    sessionName: p => `fleetdeck-${p}`,
    windowName: (p, callsign) => `fd${p}-${callsign}`,
    ensureSession: async p => `fleetdeck-${p}`,
    newWindow: async spec => {
      state.argv = spec.argv;
      const win = {
        session: `fleetdeck-${spec.port}`,
        window: `fd${spec.port}-${spec.callsign}`,
        window_id: '@1', pane_dead: false, pane_cmd: 'claude',
      };
      state.windows.push(win);
      return { session: win.session, window: win.window, window_id: win.window_id };
    },
    listScopedWindows: async () => state.windows,
    paneCurrentCommand: async target => {
      // The injection point: a hook landing mid-probe (BUG 8) runs here, between
      // tryOwnedPaneDelivery's eligibility gate and its post-probe recheck.
      if (state.onPaneProbe) state.onPaneProbe();
      state.paneProbes++; // BUG-128: probes are observable (timer-storm detection)
      const win = state.windows.find(w => w.window_id === target || w.window === target);
      return win ? { dead: win.pane_dead, cmd: win.pane_cmd } : null;
    },
    pasteText: async (target, text) => {
      state.calls.push(['pasteText', target, text]);
      return state.pasteOk;
    },
    sendEnter: async target => {
      state.calls.push(['sendEnter', target]);
      return state.enterOk;
    },
    sendBringupEnter: async target => {
      state.calls.push(['sendBringupEnter', target]);
      return true;
    },
    killWindowVerified: async name => {
      state.killed.push(name);
      return { ok: true, window_id: state.windows.find(w => w.window === name)?.window_id ?? '@1' };
    },
    launchOverride: () => {},
  };
  return { state, adapter, port };
}

function memoryCore(t, { env = {}, tmux = fakeTmux(), home = '/daemon-home' } = {}) {
  // Push the auto-delivery + nudge timers far out so mail() never fires its own
  // tryOwnedPaneDelivery during a test — each case drives delivery explicitly.
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000, ...env });
  const db = openDb(':memory:');
  const core = createCore(db, { port: tmux.port, home, tmuxAdapter: tmux.adapter });
  t.after(() => db.close());
  return { db, core, ...tmux, home };
}

test('BUG 12: an oversized mail `from` is clamped at insert time (paste + ticker stay bounded)', async (t) => {
  const { db, core } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-from-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  const storedFrom = () =>
    db.prepare('SELECT from_id FROM mail WHERE to_session = ? ORDER BY id DESC LIMIT 1').get(sid).from_id;

  // A multi-KB sender must not become a multi-KB from_id — it is embedded
  // VERBATIM into the owned-pane paste and every ticker row.
  const huge = 'y'.repeat(5000);
  await core.postMail({ to: sid, from: huge, text: 'bounded sender' });
  const clamped = storedFrom();
  assert.ok(clamped.length < huge.length, 'the oversized `from` must be clamped, not stored whole');
  assert.ok(clamped.length >= 1 && clamped.length <= 256, 'clamped `from` is bounded to a short sane cap');

  // Surrogate-safe like the text clamp (BUG 6): an astral char straddling the
  // cap must not leave a lone high surrogate at the tail.
  await core.postMail({ to: sid, from: 'z'.repeat(199) + '\u{1F600}', text: 'astral sender' });
  const astral = storedFrom();
  assert.ok(astral.isWellFormed(), 'the clamped `from` is well-formed — no orphaned high surrogate');
  const lastCode = astral.charCodeAt(astral.length - 1);
  assert.ok(lastCode < 0xd800 || lastCode > 0xdbff, 'the final code unit is not an unpaired high surrogate');

  // A normal short sender is stored verbatim — the clamp only touches oversize.
  await core.postMail({ to: sid, from: 'ops', text: 'short sender' });
  assert.equal(storedFrom(), 'ops', 'a normal `from` passes through untouched');
});

test('BUG 8: owned-pane mail bails without pasting when the turn-state flips to needs-you mid-probe', async (t) => {
  const { db, core, state } = memoryCore(t);
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-toctou-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // Queue mail for an eligible idle/queued owned pane: postMail routes to 'pane'
  // (its route probe runs while onPaneProbe is still unset, so col stays idle).
  const posted = await core.postMail({ to: sid, from: 'ops', text: 'do not inject me into a prompt' });
  assert.equal(posted.targets[0].route, 'pane', 'setup: mail is routed to the owned pane');

  // Now simulate a PermissionRequest/Notification hook landing DURING delivery's
  // awaited tmux probe: flip the card out of idle/queued into needs-you. Without
  // the recheck this text would be pasted + Entered into the permission TUI.
  state.onPaneProbe = () => {
    db.prepare("UPDATE sessions SET col = 'needsyou' WHERE session_id = ?").run(sid);
  };

  const delivered = await core.tryOwnedPaneDelivery(sid);
  assert.equal(delivered, false, 'delivery must bail once the pane is no longer idle/queued');
  assert.deepEqual(state.calls, [], 'nothing is pasted or Entered into the (now-needs-you) pane');

  // Claimed nothing: the mail is still pending for an honest later turn-boundary.
  const row = db.prepare('SELECT delivered_at FROM mail WHERE to_session = ? ORDER BY id DESC LIMIT 1').get(sid);
  assert.equal(row.delivered_at, null, 'a bailed delivery claims no mail — it stays pending');
  assert.equal(core.snapshot().mail_meta[sid].queued, 1,
    'the mailbox still shows the undelivered message');
});

test('BUG-128: a burst of mail coalesces onto ONE grace timer — one tmux probe per session, not one per row', async (t) => {
  const { db, core, state } = memoryCore(t, { env: { FLEETDECK_PANE_MAIL_GRACE_MS: 25 } });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-coalesce-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // Queue a burst against the idle owned pane via postMail: every row goes
  // through mail()'s timer path. (Direct core.mail is NOT used so this test
  // also runs against the pre-fix core, where each row arms its own timer and
  // every stray probe re-pastes the still-pending tail.)
  for (let i = 0; i < 5; i++) {
    const posted = await core.postMail({ to: sid, from: 'ops', text: `burst ${i}` });
    assert.equal(posted.ok, true, `burst ${i} queued`);
  }

  // Past the grace window, the whole burst has been delivered by ONE probe:
  // exactly one pane probe, one paste. postMail's own pre-insert route probes
  // (ownedPaneDeliverable) also call paneCurrentCommand, so measure the
  // DELIVERY probes: total probes minus the 5 route probes from the posts.
  await new Promise(r => setTimeout(r, 400));
  assert.equal(state.windows.length, 1, 'setup: one owned window');
  assert.equal(state.paneProbes - 5, 1, 'the whole burst is delivered by ONE tmux probe, not one per row');
  const pastes = state.calls.filter(c => c[0] === 'pasteText');
  assert.equal(pastes.length, 1, 'the burst becomes ONE paste, not one probe per row');
  const text = pastes[0][2];
  for (let i = 0; i < 5; i++) assert.ok(text.includes(`burst ${i}`), `paste carries burst ${i}`);
  assert.equal(core.snapshot().mail_meta[sid].queued, 0, 'mailbox drained by the single probe');

  // The defect's signature: with one timer per ROW, the burst leaves 4 stray
  // timers that each fire a full tmux probe AFTER the queue is already
  // drained — the timer/tmux storm of BUG-128. Coalescing means: the probe
  // count above stays 1 and nothing more fires.
  const callsAfterDrain = state.calls.length;
  await new Promise(r => setTimeout(r, 300));
  assert.equal(state.calls.length, callsAfterDrain, 'no stray per-row timers fire after the drain');
});

test('BUG-128: the pending mailbox has a count and byte budget — over-budget inserts are refused before the write', async (t) => {
  // Tiny overridden budgets (createCore passes them through to mail.mjs) so
  // the test doesn't queue hundreds of rows.
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000 });
  const db2 = openDb(':memory:');
  const core2 = createCore(db2, {
    port: 4712, home: '/daemon-home', tmuxAdapter: fakeTmux().adapter,
    MAIL_PENDING_MAX: 3, MAIL_PENDING_MAX_BYTES: 100,
  });
  t.after(() => db2.close());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-budget-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawn = await core2.spawn({ cwd });
  const sid = spawn.body.session_id;
  core2.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // Count budget: three fit, the fourth is refused LOUDLY by postMail (429)
  // and never stored. On the pre-fix core there is no budget at all, so the
  // fourth insert succeeds and this test fails at the 429 assertion.
  for (let i = 0; i < 3; i++) {
    const r = await core2.postMail({ to: sid, from: 'ops', text: `m${i}` });
    assert.equal(r.ok, true, `mail ${i} fits the count budget`);
  }
  const res = await core2.postMail({ to: sid, from: 'ops', text: 'one too many' });
  assert.equal(res.status, 429, 'a fully-refused fanout 429s');
  assert.equal(res.body.ok, false);
  assert.match(res.body.reason, /mailbox is full/);
  const count = db2.prepare('SELECT COUNT(*) AS n FROM mail WHERE to_session = ? AND delivered_at IS NULL').get(sid).n;
  assert.equal(count, 3, 'the refused row was never inserted');

  // Byte budget: drain, then a message whose clamped length would push the
  // pending bytes over the cap is refused even under the count limit.
  db2.prepare('UPDATE mail SET delivered_at = 1 WHERE to_session = ?').run(sid);
  const fits = await core2.postMail({ to: sid, from: 'ops', text: 'x'.repeat(90) });
  assert.equal(fits.ok, true, 'a drained mailbox accepts again');
  const tooBig = await core2.postMail({ to: sid, from: 'ops', text: 'y'.repeat(50) });
  assert.equal(tooBig.status, 429, 'the byte budget refuses once the pending sum would exceed the cap');
  const stillCount = db2.prepare('SELECT COUNT(*) AS n FROM mail WHERE to_session = ? AND delivered_at IS NULL').get(sid).n;
  assert.equal(stillCount, 1, 'the byte-refused row was never inserted');
});

test('BUG-128: owned-pane delivery pastes a bounded batch and leaves the rest pending', async (t) => {
  const db = openDb(':memory:');
  const tmux = fakeTmux(4713);
  const core = createCore(db, {
    port: 4713, home: '/daemon-home', tmuxAdapter: tmux.adapter,
    MAIL_PENDING_MAX: 10, MAIL_PENDING_MAX_BYTES: 1_000_000,
    MAIL_PANE_BATCH: 3, MAIL_PANE_BATCH_BYTES: 1_000_000,
  });
  setEnv(t, { FLEETDECK_NUDGE_MS: 1_000_000, FLEETDECK_PANE_MAIL_GRACE_MS: 1_000_000 });
  t.after(() => db.close());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fd-mail-batch-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const spawn = await core.spawn({ cwd });
  const sid = spawn.body.session_id;
  core.hookSessionStart({ session_id: sid, cwd, source: 'startup' });

  // Queue 5 rows directly (the insert columns predate this fix, so the same
  // statements run against the pre-fix core — where the claim below takes ALL
  // FIVE into one paste and the "tail stays pending" assertions fail).
  const insert = db.prepare('INSERT INTO mail (to_session, from_id, text, at, delivered_at) VALUES (?, ?, ?, ?, NULL)');
  for (let i = 0; i < 5; i++) insert.run(sid, 'ops', `batch-msg-${i}`, Date.now() + i);

  const delivered = await core.tryOwnedPaneDelivery(sid);
  assert.equal(delivered, true, 'delivery succeeds for the bounded batch');
  const pastes = tmux.state.calls.filter(c => c[0] === 'pasteText');
  assert.equal(pastes.length, 1, 'one paste for the batch');
  const text = pastes[0][2];
  for (let i = 0; i < 3; i++) assert.ok(text.includes(`batch-msg-${i}`), `batch carries msg ${i}`);
  assert.ok(!text.includes('batch-msg-3') && !text.includes('batch-msg-4'),
    'rows past the batch bound are NOT in this paste');

  // The remainder stayed pending (never claimed), oldest-first order intact.
  const pending = db.prepare('SELECT text FROM mail WHERE to_session = ? AND delivered_at IS NULL ORDER BY at, id').all(sid);
  assert.deepEqual(pending.map(r => r.text), ['batch-msg-3', 'batch-msg-4'],
    'the unclaimed tail is still pending for the next round');

  // A second round delivers the rest — bounded batches drain, never stall.
  const delivered2 = await core.tryOwnedPaneDelivery(sid);
  assert.equal(delivered2, true, 'the second round delivers the remainder');
  const pastes2 = tmux.state.calls.filter(c => c[0] === 'pasteText');
  assert.ok(pastes2[1][2].includes('batch-msg-3') && pastes2[1][2].includes('batch-msg-4'),
    'the tail arrives in the next paste');
  assert.equal(core.snapshot().mail_meta[sid].queued, 0, 'mailbox fully drained after two rounds');
});
