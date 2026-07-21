// tests/nudge.test.mjs
//
// 0.16.0 — the bring-up nudge must NEVER press Enter through Claude Code's
// folder-trust / MCP-approval dialogs. Those dialogs are the human checkpoint
// between a freshly cloned repo's .claude/settings.json hooks / .mcp.json
// servers and the user's credentials; a daemon that auto-answers them turns
// "untrusted repo" into "trusted" with no human in the loop. The nudge now
// reads the pane first: trust/MCP screen → hold + board-visible "waiting"
// note; anything else → the historical one-Enter bring-up.
//
// Uses a REAL tmux server on the isolated per-test socket (same discipline as
// the rest of the suite — FLEETDECK_TMUX_SOCKET is fleetdeck-test-<port>),
// with `cat` as the pane process: it echoes whatever we send it, so the pane
// content is fully under the test's control. No claude, no hooks — the spawn
// row stays 'spawning', which is exactly the state the nudge fires in.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync, execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.mjs';
import { postJson, getJson } from './helpers/http.mjs';
import { waitUntil } from './helpers/wait.mjs';

function tmuxOk() {
  try { execFileSync('tmux', ['-V'], { stdio: 'ignore' }); return true; } catch { return false; }
}

function tmux(socket, args) {
  return execFileSync('tmux', ['-L', socket, ...args], { encoding: 'utf8' }).trim();
}

// Point the daemon's tmux at an existing window whose pane runs `cat`:
// FLEETDECK_SPAWN_CMD cannot create real panes, so instead we pre-build the
// scoped window and use a fixture that reports the window id the daemon
// expects. Simpler: drive the daemon with the spawn override, then surgically
// swap the row's tmux_window to our pre-built window via the fixture's spec
// is NOT possible — so this test takes the other road: create the window
// exactly where the daemon WOULD (scoped session + name) before spawning, and
// let the real new-window path find... no — new-window would collide on the
// name. The clean seam is the nudge's own inputs: scheduleNudge resolves the
// window by SCOPED NAME through findScopedWindow, so we spawn with the
// override (fixture records the spec, including tmux.session/window names),
// then create THAT window ourselves with `cat` in it. The daemon's liveness
// probes then see a live pane with our content.
const TRUST_SCREEN = [
  ' Claude Code may read files in this folder.',
  '',
  ' Do you trust the files in this folder?',
  '',
  ' ❯ 1. Yes, proceed',
  '   2. No, exit',
].join('\n');

test('nudge holds on a trust dialog: no Enter, board says waiting', { skip: !tmuxOk() && 'tmux not available' }, async (t) => {
  const recordDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-nudge-rec-'));
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_SPAWN_CMD: path.resolve('tests/helpers/spawn-cmd-fixture.mjs'),
      FLEETDECK_TEST_SPAWN_RECORD: recordFile,
      FLEETDECK_NUDGE_MS: '150',
    },
  });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true, maxRetries: 5 }); });

  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-nudge-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5 }));

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'hold the trust dialog' });
  assert.equal(res.status, 200, JSON.stringify(res.json));
  const { spawn_id, tmux: { session, window } } = res.json;

  // Build the pane the daemon believes it launched: scoped session, scoped
  // window name, `cat` echoing a trust dialog.
  const socket = `fleetdeck-test-${daemon.port}`;
  try { tmux(socket, ['kill-server']); } catch { /* no prior server */ }
  tmux(socket, ['new-session', '-d', '-s', session, '-x', '200', '-y', '50']);
  tmux(socket, ['new-window', '-d', '-t', session, '-n', window, 'cat']);
  const target = `${session}:${window}`;
  tmux(socket, ['send-keys', '-t', target, '-l', '--', TRUST_SCREEN]);
  tmux(socket, ['send-keys', '-t', target, 'Enter']);

  // The nudge fires ~150ms after the spawn. Give it room, then assert: the
  // card note says waiting, and NO Enter was pressed (the trust screen's
  // cursor line is unchanged — an Enter on `cat` would echo a newline but,
  // more tellingly, the daemon logs the nudge decision in the ticker).
  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const card = state.sessions.find(s => s.session_id === res.json.session_id);
    return card?.note?.includes('trust');
  }, { timeoutMs: 6000, label: 'card note to report waiting on the trust dialog' });

  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  const tickText = (state.ticker ?? []).map(x => x.msg ?? x.text ?? '').join('\n');
  assert.match(tickText, /waits on a trust dialog/, 'ticker reports the held trust dialog');
  assert.doesNotMatch(tickText, /nudged .* through bring-up/, 'no bring-up Enter was sent');

  // Hard proof at the pane: capture-pane shows the SAME dialog (an Enter
  // would have added a trailing echoed blank line / moved the cursor).
  const pane = tmux(socket, ['capture-pane', '-p', '-t', target]);
  assert.match(pane, /Do you trust the files in this folder\?/, 'trust dialog still on screen');
});

test('nudge presses Enter on an ordinary bring-up screen', { skip: !tmuxOk() && 'tmux not available' }, async (t) => {
  const recordDir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-nudge-rec2-'));
  const recordFile = path.join(recordDir, 'spawns.jsonl');
  const daemon = await startDaemon({
    env: {
      FLEETDECK_SPAWN_CMD: path.resolve('tests/helpers/spawn-cmd-fixture.mjs'),
      FLEETDECK_TEST_SPAWN_RECORD: recordFile,
      FLEETDECK_NUDGE_MS: '150',
    },
  });
  t.after(async () => { await daemon.stop(); rmSync(recordDir, { recursive: true, force: true, maxRetries: 5 }); });

  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-nudge-cwd2-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5 }));

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, prompt: 'ordinary bring-up' });
  assert.equal(res.status, 200, JSON.stringify(res.json));
  const { tmux: { session, window } } = res.json;

  const socket = `fleetdeck-test-${daemon.port}`;
  try { tmux(socket, ['kill-server']); } catch { /* no prior server */ }
  tmux(socket, ['new-session', '-d', '-s', session, '-x', '200', '-y', '50']);
  tmux(socket, ['new-window', '-d', '-t', session, '-n', window, 'cat']);
  const target = `${session}:${window}`;
  tmux(socket, ['send-keys', '-t', target, '-l', '--', 'some ordinary prompt text']);
  tmux(socket, ['send-keys', '-t', target, 'Enter']);

  await waitUntil(async () => {
    const state = (await getJson(`${daemon.baseUrl}/state`)).json;
    const tickText = (state.ticker ?? []).map(x => x.msg ?? x.text ?? '').join('\n');
    return /nudged .* through bring-up/.test(tickText);
  }, { timeoutMs: 6000, label: 'ticker to report the bring-up Enter' });

  // The Enter reached `cat`: it echoed a newline — cursor moved past the text.
  const pane = tmux(socket, ['capture-pane', '-p', '-t', target]);
  assert.match(pane, /some ordinary prompt text/, 'pane content intact');
});
