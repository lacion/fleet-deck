// tests/model-tracking.test.mjs
//
// The model on a card must be the model that is RUNNING, not the one the
// session launched with.
//
// Claude Code puts `model` in the SessionStart hook payload and in no other
// payload. So a card whose model comes only from hooks is frozen at its launch
// value: switch to Opus with /model and the board goes on saying "Fable 5"
// forever. The fix reads the session transcript — the only place the model of
// each turn is written down — on every event after SessionStart.
//
// Coverage map:
//   1. SessionStart payload sets the launch model (bare string + legacy object)
//        -> "SessionStart payload ...", "legacy object-shaped payload ..."
//   2. THE BUG: a /model switch mid-session is picked up from the transcript
//        -> "a mid-session /model switch ..."
//   3. It lands at the first tool call, not only at Stop
//        -> "the switch shows up at the first PostToolUse ..."
//   4. A subagent's model never hijacks the card
//        -> "a subagent's model never hijacks ..."
//   5. A missing/corrupt transcript leaves the model ALONE (never nulls it)
//        -> "an unreadable transcript leaves the model untouched"
//   6. THE RESUME FLOOR: a resumed session must not bounce back to the model of
//      its PREVIOUS run (its old turns are still in the same transcript file)
//        -> "a resumed session does not fall back ...", "... and still follows a real switch"
//   7. No model anywhere -> null, never the string "undefined"
//        -> "a session with no model anywhere reports null"

import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { startDaemon } from './helpers/daemon.mjs';
import { getJson, postHook } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import {
  appendTranscriptLines,
  assistantLine,
  makeTranscriptDir,
  toolUseLine,
  userLine,
  writeTranscriptLines,
} from './helpers/transcript.mjs';

const FABLE = 'claude-fable-5';
const OPUS = 'claude-opus-4-8';
const HAIKU = 'claude-haiku-4-5';

function findSession(state, sid) {
  return state.sessions.find(s => s.session_id === sid);
}

async function modelOf(daemon, sid) {
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  return findSession(state, sid)?.model;
}

async function tickerOf(daemon) {
  const state = (await getJson(`${daemon.baseUrl}/state`)).json;
  return (state.ticker ?? []).map(t => t.msg);
}

/** A daemon + a scratch cwd + a transcript dir, all cleaned up after the test. */
async function harness(t) {
  const daemon = await startDaemon();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-model-cwd-'));
  const tdir = makeTranscriptDir();
  t.after(async () => {
    await daemon.stop();
    rmSync(cwd, { recursive: true, force: true });
    rmSync(tdir, { recursive: true, force: true });
  });
  return { daemon, cwd, tdir, sid: randomUUID() };
}

test('SessionStart payload sets the launch model', async (t) => {
  const { daemon, cwd, sid } = await harness(t);
  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }, { model: FABLE }));
  assert.equal(await modelOf(daemon, sid), FABLE);
});

test('legacy object-shaped payload model is still accepted', async (t) => {
  const { daemon, cwd, sid } = await harness(t);
  // Today the CLI sends a bare id string; the statusline shape is {id, display_name}.
  // Accepting it costs nothing and means a future CLI can hand us a pretty name.
  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }, { model: { id: OPUS, display_name: 'Opus 4.8' } }));
  assert.equal(await modelOf(daemon, sid), 'Opus 4.8');
});

test('a mid-session /model switch is picked up from the transcript at Stop', async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  // Launches on fable — this is all the hook payloads will ever tell us.
  const transcript = writeTranscriptLines(tdir, sid, [userLine(), assistantLine({ model: FABLE })]);
  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', tokens, { model: FABLE, transcript_path: transcript }));
  assert.equal(await modelOf(daemon, sid), FABLE);

  // The human types /model opus. No hook fires. The next turn runs on opus and
  // the transcript records it.
  appendTranscriptLines(transcript, [userLine(), assistantLine({ model: OPUS })]);
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens, { transcript_path: transcript }));

  assert.equal(await modelOf(daemon, sid), OPUS, 'the card must follow the running model');
  assert.ok((await tickerOf(daemon)).some(m => m.includes('switched model') && m.includes(OPUS)),
    'a switch is a real event and belongs in the feed');
});

test('the switch shows up at the first PostToolUse, without waiting for Stop', async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  const transcript = writeTranscriptLines(tdir, sid, [userLine(), assistantLine({ model: FABLE })]);
  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', tokens, { model: FABLE, transcript_path: transcript }));

  // Mid-turn: the assistant entry carrying the tool_use is written BEFORE the
  // tool runs, and it carries the model — so the badge can be right seconds
  // into the new turn rather than at the end of it.
  appendTranscriptLines(transcript, [userLine(), toolUseLine({ model: OPUS })]);
  await postHook(daemon.baseUrl, 'PostToolUse',
    loadFixture('post-tool-use-bash', tokens, { transcript_path: transcript }));

  assert.equal(await modelOf(daemon, sid), OPUS);
});

test("a subagent's model never hijacks the card", async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  const transcript = writeTranscriptLines(tdir, sid, [userLine(), assistantLine({ model: OPUS })]);
  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', tokens, { model: OPUS, transcript_path: transcript }));

  // A Task subagent runs haiku. Its turns land after the main thread's, but the
  // card tracks the session, not its subagents.
  appendTranscriptLines(transcript, [
    assistantLine({ model: HAIKU, isSidechain: true }),
    assistantLine({ model: HAIKU, isSidechain: true }),
  ]);
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens, { transcript_path: transcript }));

  assert.equal(await modelOf(daemon, sid), OPUS);
});

test('an unreadable transcript leaves the model untouched', async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', tokens, { model: FABLE }));

  // A path that does not exist must not null the model out.
  await postHook(daemon.baseUrl, 'Stop',
    loadFixture('stop', tokens, { transcript_path: path.join(tdir, 'gone.jsonl') }));
  assert.equal(await modelOf(daemon, sid), FABLE);

  // Neither must a corrupt one.
  const junk = path.join(tdir, 'junk.jsonl');
  writeFileSync(junk, 'not json\n{{{\n');
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens, { transcript_path: junk }));
  assert.equal(await modelOf(daemon, sid), FABLE);
});

test('a resumed session does not fall back to the model of its previous run', async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  // The previous run's turns are still in the transcript: `claude --resume`
  // reopens the SAME file. This is the trap — the newest assistant line on disk
  // is fable, but the session has just been resumed on opus.
  const transcript = writeTranscriptLines(tdir, sid, [
    userLine(), assistantLine({ model: FABLE }),
    userLine(), assistantLine({ model: FABLE }),
  ]);

  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', tokens, { source: 'resume', model: OPUS, transcript_path: transcript }));
  assert.equal(await modelOf(daemon, sid), OPUS);

  // A prompt appends only a USER line — there is still no assistant turn from
  // this run. Without the byte floor stamped at SessionStart, the reader would
  // find the old fable line and stomp the fresh opus with it.
  appendTranscriptLines(transcript, [userLine()]);
  await postHook(daemon.baseUrl, 'UserPromptSubmit',
    loadFixture('user-prompt-submit', tokens, { transcript_path: transcript }));
  assert.equal(await modelOf(daemon, sid), OPUS, 'must not bounce back to the previous run\'s model');

  // The first real turn of the new run confirms opus.
  appendTranscriptLines(transcript, [assistantLine({ model: OPUS })]);
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens, { transcript_path: transcript }));
  assert.equal(await modelOf(daemon, sid), OPUS);
});

test('the resume floor does not freeze the model — a real switch after it still lands', async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  const transcript = writeTranscriptLines(tdir, sid, [userLine(), assistantLine({ model: FABLE })]);
  await postHook(daemon.baseUrl, 'SessionStart',
    loadFixture('session-start', tokens, { source: 'resume', model: OPUS, transcript_path: transcript }));

  // Resumed on opus, then switched to haiku mid-run: above the floor, so it counts.
  appendTranscriptLines(transcript, [userLine(), assistantLine({ model: HAIKU })]);
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens, { transcript_path: transcript }));
  assert.equal(await modelOf(daemon, sid), HAIKU);
});

test('a session with no model anywhere reports null, not the string "undefined"', async (t) => {
  const { daemon, cwd, tdir, sid } = await harness(t);
  const tokens = { session_id: sid, cwd };

  // No `model` in the payload, and a transcript whose assistant turns carry none.
  const transcript = writeTranscriptLines(tdir, sid, [
    userLine(),
    { type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'hi' }] } },
  ]);
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', tokens));
  await postHook(daemon.baseUrl, 'Stop', loadFixture('stop', tokens, { transcript_path: transcript }));

  assert.equal(await modelOf(daemon, sid), null);
});
