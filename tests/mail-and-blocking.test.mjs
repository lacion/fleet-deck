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
import { startDaemon } from './helpers/daemon.mjs';
import { postHook, postJson, getJson } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { makeRepoWithWorktree } from './helpers/gitrepo.mjs';
import { openDb } from '../scripts/fleetd/db.mjs';
import { createCore } from '../scripts/fleetd/derive.mjs';

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
    onPaneProbe: null,
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
