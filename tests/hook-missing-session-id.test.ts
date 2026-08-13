// tests/hook-missing-session-id.test.ts
//
// BUG-111 — a hook payload WITHOUT a usable session_id used to mint (and then
// keep mutating) a REAL card keyed on the literal string 'unknown': every
// malformed SessionStart/UserPromptSubmit/Stop collapsed into one shared
// phantom worker, corrupting its columns, task text and counters. The fix
// validates session_id as a non-empty string at the authenticated hook
// boundary (http.mjs) — such payloads fail open with the hook's 200 no-op and
// are never DISPATCHED — and removes the `|| 'unknown'` sentinel from the
// events.mjs state machine so the telemetry-only applyEvent callers can't
// mint the phantom either.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.ts';
import { postHook, getJson } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import type { StateResponse } from '../contracts/state.ts';

interface HookAck {
  ok?: boolean;
}

function scratchCwd() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-nosid-'));
}

test('authenticated hooks without a session_id fail open and never mint the shared unknown card', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const cwd = scratchCwd();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  // SessionStart with NO session_id — the shim's {}-on-parse-failure shape.
  const start = await postHook(
    daemon.baseUrl,
    'SessionStart',
    { hook_event_name: 'SessionStart', cwd },
    { token: daemon },
  );
  assert.equal(start.status, 200, 'ID-less SessionStart still fails open with 200');
  assert.ok(
    !(start.json as HookAck | null)?.ok,
    'no brief was composed — the handler was not dispatched',
  );

  // The audit's full corruption sequence: several DIFFERENT malformed
  // payloads, including non-string ids, all used to mutate ONE phantom card.
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { hook_event_name: 'SessionStart', session_id: '', cwd },
    { token: daemon },
  );
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { hook_event_name: 'SessionStart', session_id: 42, cwd },
    { token: daemon },
  );
  const prompt = await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    { hook_event_name: 'UserPromptSubmit', cwd, prompt: 'hello' },
    { token: daemon },
  );
  assert.equal(prompt.status, 200, 'ID-less UserPromptSubmit still fails open with 200');
  const stop = await postHook(
    daemon.baseUrl,
    'Stop',
    { hook_event_name: 'Stop', cwd },
    { token: daemon },
  );
  assert.equal(stop.status, 200, 'ID-less Stop still fails open with 200');

  let state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.ok(
    !state.sessions.find((s) => s.session_id === 'unknown'),
    "no phantom 'unknown' card on the board",
  );
  assert.equal(state.sessions.length, 0, 'no session was registered at all');

  // The telemetry-only paths also lose their 'unknown' sid: a FileChanged
  // and an unknown hook event call applyEvent directly (no dispatch gate) —
  // they must no-op too, still answering 200.
  await postHook(
    daemon.baseUrl,
    'FileChanged',
    { hook_event_name: 'FileChanged', cwd, path: 'x.mjs' },
    { token: daemon },
  );
  await postHook(
    daemon.baseUrl,
    'SomethingNew',
    { hook_event_name: 'SomethingNew', cwd },
    { token: daemon },
  );
  state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.ok(
    !state.sessions.find((s) => s.session_id === 'unknown'),
    "telemetry-only hooks mint no 'unknown' card either",
  );
  assert.equal(state.sessions.length, 0, 'still no sessions at all');
});

test('a hook WITH a session_id still registers normally after the ID-less ones were dropped', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const cwd = scratchCwd();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    { hook_event_name: 'UserPromptSubmit', cwd, prompt: 'stray' },
    { token: daemon },
  );

  const sid = randomUUID();
  const res = await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    { token: daemon },
  );
  assert.ok((res.json as HookAck | null)?.ok, 'a well-formed SessionStart still gets its brief');
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.deepEqual(
    state.sessions.map((s) => s.session_id),
    [sid],
    'exactly the real session is on the board',
  );
});
