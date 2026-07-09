// tests/helpers/transcript.mjs — synthetic Claude Code transcript JSONL
// fixtures for F3d (free-text question detection at Stop).
//
// The Stop hook payload does NOT carry `last_assistant_message` per the
// official hooks docs -- fleetd is expected to read the last assistant
// message from `transcript_path`
// (JSONL tail, deterministic, no model call). Real transcripts are
// newline-delimited JSON, one turn per line, matching Claude Code's on-disk
// session transcript format (~/.claude/projects/<proj>/<sid>.jsonl):
//   {"type":"user", "message": {"role":"user","content": "..."}, ...}
//   {"type":"assistant", "message": {"role":"assistant","content":[{"type":"text","text":"..."}]}, ...}
//
// This helper writes a minimal two-line transcript (one user turn, one
// assistant turn) ending in the given assistant text, so tests can control
// exactly what the "trailing question" heuristic sees.

import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

/** Fresh scratch dir to hold synthetic transcript files for one test. */
export function makeTranscriptDir() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-transcript-'));
}

/**
 * Write a synthetic transcript JSONL under `dir` whose last line is an
 * assistant message with `assistantText`. Returns the absolute file path,
 * suitable for a Stop payload's `transcript_path`.
 */
export function writeTranscript(dir, { sessionId, assistantText, userText = 'Please help me pick an approach.' }) {
  const now = () => new Date().toISOString();
  const lines = [
    { type: 'user', sessionId, message: { role: 'user', content: userText }, timestamp: now() },
    {
      type: 'assistant',
      sessionId,
      message: { role: 'assistant', content: [{ type: 'text', text: assistantText }] },
      timestamp: now(),
    },
  ];
  const file = path.join(dir, `${sessionId}.jsonl`);
  writeFileSync(file, lines.map(l => JSON.stringify(l)).join('\n') + '\n');
  return file;
}
