// tests/transcript-reader.test.mjs
//
// Pure tests for scripts/fleetd/transcript.mjs — no daemon, no HTTP.
//
// The transcript is the only on-disk record of which model produced a turn.
// Claude Code sends `model` in the SessionStart hook payload and NOWHERE else,
// so without this reader a card's model badge is frozen at whatever the session
// launched with, forever. These tests pin the four things the reader has to get
// right, each of which is a way the badge could lie:
//
//   1. it follows a mid-session /model switch          → "follows a switch"
//   2. a SUBAGENT's model never wins                   → "sidechain"
//   3. a tool_use-only entry still yields its model    → "tool_use-only"
//      (this is what makes the switch visible at the first tool call rather
//       than at Stop — and it is where lastAssistantModel deliberately
//       diverges from lastAssistantText, which requires a text block)
//   4. minOffset hides a RESUMED session's old turns   → "minOffset"
//
// Plus: the 256 KB → 2 MB retry, and that nothing here ever throws (a bad
// transcript must never take down a hook response).

import test from 'node:test';
import assert from 'node:assert/strict';
import { statSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { lastAssistantModel, lastAssistantText, tailLines } from '../scripts/fleetd/transcript.mjs';
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

test('follows a switch: fable turns then opus turns → the model is opus', () => {
  const dir = makeTranscriptDir();
  const file = writeTranscriptLines(dir, 's1', [
    userLine(), assistantLine({ model: FABLE }),
    userLine(), assistantLine({ model: FABLE }),
    userLine(), assistantLine({ model: OPUS }), // /model opus happened here
  ]);
  assert.equal(lastAssistantModel(file), OPUS);
});

test('sidechain: a subagent running another model never overwrites the main thread', () => {
  const dir = makeTranscriptDir();
  const file = writeTranscriptLines(dir, 's2', [
    userLine(), assistantLine({ model: OPUS }),
    // A Task subagent's turns land AFTER the main thread's, and can run a
    // different model. The board tracks the session, not its subagents.
    assistantLine({ model: HAIKU, isSidechain: true }),
    assistantLine({ model: HAIKU, isSidechain: true }),
  ]);
  assert.equal(lastAssistantModel(file), OPUS);
});

test('tool_use-only: an entry with no text block still yields its model', () => {
  const dir = makeTranscriptDir();
  const file = writeTranscriptLines(dir, 's3', [
    userLine(), assistantLine({ model: FABLE, text: 'starting' }),
    userLine(), toolUseLine({ model: OPUS }), // mid-turn, no text yet
  ]);
  // The model reader sees it — that is why a switch shows up at the first tool
  // call and not only at Stop.
  assert.equal(lastAssistantModel(file), OPUS);
  // The TEXT reader deliberately does not: a tool_use is not "the assistant's
  // final words". Pinning the divergence so a future refactor can't merge them.
  assert.equal(lastAssistantText(file), 'starting');
});

test('minOffset: a resumed session ignores the previous run, then follows the new one', () => {
  const dir = makeTranscriptDir();
  // The previous run, already on disk when `claude --resume` reopens the file.
  const file = writeTranscriptLines(dir, 's4', [
    userLine(), assistantLine({ model: FABLE }),
  ]);
  const floor = statSync(file).size; // what SessionStart stamps

  // Nothing new has been written yet: the old fable turn is not evidence about
  // THIS run. Without the floor, this returns FABLE and stomps the fresh model
  // the SessionStart payload just supplied.
  assert.equal(lastAssistantModel(file, { minOffset: floor }), null);

  // A user prompt alone is still not evidence — it appends no assistant line.
  appendTranscriptLines(file, [userLine()]);
  assert.equal(lastAssistantModel(file, { minOffset: floor }), null);

  // The first assistant turn of the new run is.
  appendTranscriptLines(file, [assistantLine({ model: OPUS })]);
  assert.equal(lastAssistantModel(file, { minOffset: floor }), OPUS);

  // And the floor doesn't freeze it: switch back mid-run and it follows.
  appendTranscriptLines(file, [userLine(), assistantLine({ model: FABLE })]);
  assert.equal(lastAssistantModel(file, { minOffset: floor }), FABLE);
});

test('retry: an assistant line pushed >256 KB from EOF by a huge tool result is still found', () => {
  const dir = makeTranscriptDir();
  // 400 KB of user payload after the last assistant turn — past the 256 KB
  // first window, so only the 2 MB retry can reach it.
  const file = writeTranscriptLines(dir, 's5', [
    userLine(), assistantLine({ model: OPUS }),
    userLine({ bulk: 400_000 }),
  ]);
  assert.ok(statSync(file).size > 262_144);
  assert.equal(lastAssistantModel(file), OPUS);
});

test('a transcript with no assistant model at all yields null, not a guess', () => {
  const dir = makeTranscriptDir();
  const file = writeTranscriptLines(dir, 's6', [userLine(), userLine()]);
  assert.equal(lastAssistantModel(file), null);
});

test('best-effort: absent, empty and corrupt transcripts return null and never throw', () => {
  const dir = makeTranscriptDir();

  assert.equal(lastAssistantModel(path.join(dir, 'nope.jsonl')), null);
  assert.equal(lastAssistantText(path.join(dir, 'nope.jsonl')), null);

  const empty = path.join(dir, 'empty.jsonl');
  writeFileSync(empty, '');
  assert.equal(lastAssistantModel(empty), null);

  // Garbage lines are skipped, and a good line after them still wins.
  const mixed = path.join(dir, 'mixed.jsonl');
  writeFileSync(mixed, [
    'not json at all',
    '{"type":"assistant","message":{"model":',   // truncated JSON
    JSON.stringify(assistantLine({ model: OPUS })),
    '{{{',
  ].join('\n') + '\n');
  assert.equal(lastAssistantModel(mixed), OPUS);
});

test('tailLines yields newest-first with absolute byte offsets', () => {
  const dir = makeTranscriptDir();
  const file = path.join(dir, 'offsets.jsonl');
  writeFileSync(file, 'aa\nbb\ncc\n'); // offsets 0, 3, 6
  const got = [...tailLines(file)];
  assert.deepEqual(got, [
    { line: 'cc', offset: 6 },
    { line: 'bb', offset: 3 },
    { line: 'aa', offset: 0 },
  ]);
});
