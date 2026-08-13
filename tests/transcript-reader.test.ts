// tests/transcript-reader.test.ts
//
// Pure tests for src/daemon/transcript.mjs — no daemon, no HTTP.
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

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { appendFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { lastAssistantModel, lastAssistantText, tailLines } from '../src/daemon/transcript.ts';
import {
  appendTranscriptLines,
  assistantLine,
  makeTranscriptDir,
  toolUseLine,
  userLine,
  writeTranscriptLines,
} from './helpers/transcript.ts';

const FABLE = 'claude-fable-5';
const OPUS = 'claude-opus-4-8';
const HAIKU = 'claude-haiku-4-5';

test('follows a switch: fable turns then opus turns → the model is opus', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const file = writeTranscriptLines(dir, 's1', [
    userLine(),
    assistantLine({ model: FABLE }),
    userLine(),
    assistantLine({ model: FABLE }),
    userLine(),
    assistantLine({ model: OPUS }), // /model opus happened here
  ]);
  assert.equal(lastAssistantModel(file), OPUS);
});

test('sidechain: a subagent running another model never overwrites the main thread', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const file = writeTranscriptLines(dir, 's2', [
    userLine(),
    assistantLine({ model: OPUS }),
    // A Task subagent's turns land AFTER the main thread's, and can run a
    // different model. The board tracks the session, not its subagents.
    assistantLine({ model: HAIKU, isSidechain: true }),
    assistantLine({ model: HAIKU, isSidechain: true }),
  ]);
  assert.equal(lastAssistantModel(file), OPUS);
});

test('tool_use-only: an entry with no text block still yields its model', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const file = writeTranscriptLines(dir, 's3', [
    userLine(),
    assistantLine({ model: FABLE, text: 'starting' }),
    userLine(),
    toolUseLine({ model: OPUS }), // mid-turn, no text yet
  ]);
  // The model reader sees it — that is why a switch shows up at the first tool
  // call and not only at Stop.
  assert.equal(lastAssistantModel(file), OPUS);
  // The TEXT reader deliberately does not: a tool_use is not "the assistant's
  // final words". Pinning the divergence so a future refactor can't merge them.
  assert.equal(lastAssistantText(file), 'starting');
});

test('minOffset: a resumed session ignores the previous run, then follows the new one', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  // The previous run, already on disk when `claude --resume` reopens the file.
  const file = writeTranscriptLines(dir, 's4', [userLine(), assistantLine({ model: FABLE })]);
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

test('retry: an assistant line pushed >256 KB from EOF by a huge tool result is still found', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  // 400 KB of user payload after the last assistant turn — past the 256 KB
  // first window, so only the 2 MB retry can reach it.
  const file = writeTranscriptLines(dir, 's5', [
    userLine(),
    assistantLine({ model: OPUS }),
    userLine({ bulk: 400_000 }),
  ]);
  assert.ok(statSync(file).size > 262_144);
  assert.equal(lastAssistantModel(file), OPUS);
});

test('a transcript with no assistant model at all yields null, not a guess', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const file = writeTranscriptLines(dir, 's6', [userLine(), userLine()]);
  assert.equal(lastAssistantModel(file), null);
});

test('partial append: a truncated newest line yields no model, not an older one', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const file = writeTranscriptLines(dir, 's7', [userLine(), assistantLine({ model: FABLE })]);
  // Mid-append of the NEXT turn (say, a /model switch to opus): the newest
  // line is truncated JSON. lastAssistantText already answers "no evidence
  // yet" here; the model reader must hold the same contract, or derive.mjs
  // caches the stale FABLE until the next size change.
  appendFileSync(file, '{"type":"assistant","message":{"model":"claude-opus-4-8"'); // no newline, no closing braces
  assert.equal(lastAssistantModel(file), null);
  assert.equal(lastAssistantText(file), null); // same fixture, same contract

  // The append completes — a normal write, newline-terminated — and the real
  // model shows up.
  appendFileSync(file, ',"content":[]}}\n');
  assert.equal(lastAssistantModel(file), OPUS);
});

test('best-effort: absent, empty and corrupt transcripts return null and never throw', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  assert.equal(lastAssistantModel(path.join(dir, 'nope.jsonl')), null);
  assert.equal(lastAssistantText(path.join(dir, 'nope.jsonl')), null);

  const empty = path.join(dir, 'empty.jsonl');
  writeFileSync(empty, '');
  assert.equal(lastAssistantModel(empty), null);

  // Garbage lines DEEP in history are skipped, and a good line older than
  // them still wins. The truncated row sits below a complete, parseable
  // newest row — an unparseable NEWEST line is a partial append, which the
  // "partial append" test above pins to null instead.
  const mixed = path.join(dir, 'mixed.jsonl');
  writeFileSync(
    mixed,
    [
      'not json at all',
      JSON.stringify(assistantLine({ model: OPUS })),
      '{"type":"assistant","message":{"model":', // truncated JSON, mid-history
      JSON.stringify(userLine()), // complete newest row
    ].join('\n') + '\n',
  );
  assert.equal(lastAssistantModel(mixed), OPUS);
});

test('tailLines yields newest-first with absolute byte offsets', (t) => {
  const dir = makeTranscriptDir();
  t.after(() => {
    rmSync(dir, { recursive: true, force: true });
  });
  const file = path.join(dir, 'offsets.jsonl');
  writeFileSync(file, 'aa\nbb\ncc\n'); // offsets 0, 3, 6
  const got = [...tailLines(file)];
  assert.deepEqual(got, [
    { line: 'cc', offset: 6 },
    { line: 'bb', offset: 3 },
    { line: 'aa', offset: 0 },
  ]);
});

// A user-line JSON + '\n' of EXACTLY n bytes, padded via `bulk` (n must be at
// least the unpadded line's length).
function paddedUserLine(n: number) {
  const base = JSON.stringify(userLine()) + '\n';
  const extra = n - base.length;
  const line = extra <= 0 ? base : JSON.stringify(userLine({ bulk: extra - 1 })) + '\n';
  assert.equal(line.length, n);
  return line;
}

// A transcript whose ONLY assistant row starts exactly at EOF - `tier`: one
// prefix line (so the read window starts at start > 0), then the assistant
// row, then user lines filling the window to exactly `tier` bytes.
function boundaryTranscript(dir: string, sessionId: string, tier: number) {
  const first = JSON.stringify(assistantLine({ model: OPUS })) + '\n';
  const base = JSON.stringify(userLine()) + '\n';
  let window = first;
  while (tier - window.length - base.length >= base.length) window += base;
  window += paddedUserLine(tier - window.length);
  assert.equal(window.length, tier);
  const prefix = JSON.stringify(userLine()) + '\n';
  const file = path.join(dir, `${sessionId}.jsonl`);
  writeFileSync(file, prefix + window);
  return { file, assistantOffset: prefix.length };
}

test('exact boundary: a row starting exactly at EOF - maxBytes is complete, not partial (BUG-194)', () => {
  const dir = makeTranscriptDir();
  const file = path.join(dir, 'edge.jsonl');
  writeFileSync(file, 'aaaa\nbbbb\n'); // 'bbbb' starts at byte 5, preceded by '\n'
  // The window starts exactly on the row boundary: the row is whole and must
  // be yielded. (Before the fix it was discarded as supposedly partial.)
  assert.deepEqual([...tailLines(file, { maxBytes: 5 })], [{ line: 'bbbb', offset: 5 }]);
  // A genuinely mid-line start still drops the partial row.
  assert.deepEqual([...tailLines(file, { maxBytes: 7 })], [{ line: 'bbbb', offset: 5 }]);
});

test('exact boundary at every read tier: the newest assistant model survives (BUG-194)', () => {
  const dir = makeTranscriptDir();
  // The three windows lastAssistantModel reads: 16 KB, 256 KB, 2 MB. At the
  // 2 MB tier there is no wider retry, so dropping a boundary-aligned row
  // there lost the model outright (the audit's 2,000,002-byte fixture).
  for (const [i, tier] of [16_384, 262_144, 2_000_000].entries()) {
    const { file, assistantOffset } = boundaryTranscript(dir, `boundary-${i}`, tier);
    assert.equal(statSync(file).size - tier, assistantOffset); // row sits exactly on the window edge
    assert.equal(lastAssistantModel(file), OPUS);
  }
});
