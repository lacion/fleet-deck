// tests/helpers/transcript.ts — synthetic Claude Code transcript JSONL
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

import { appendFileSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

/** A single transcript JSONL entry — freeform on purpose (tests serialize it
 *  verbatim to exercise fleetd's tail parser). */
export type TranscriptEntry = Record<string, unknown>;

/** Fresh scratch dir to hold synthetic transcript files for one test. */
export function makeTranscriptDir(): string {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-transcript-'));
}

export interface WriteTranscriptOptions {
  sessionId: string;
  assistantText: string;
  userText?: string;
}

/**
 * Write a synthetic transcript JSONL under `dir` whose last line is an
 * assistant message with `assistantText`. Returns the absolute file path,
 * suitable for a Stop payload's `transcript_path`.
 */
export function writeTranscript(
  dir: string,
  {
    sessionId,
    assistantText,
    userText = 'Please help me pick an approach.',
  }: WriteTranscriptOptions,
): string {
  const now = (): string => new Date().toISOString();
  const lines: TranscriptEntry[] = [
    { type: 'user', sessionId, message: { role: 'user', content: userText }, timestamp: now() },
    {
      type: 'assistant',
      sessionId,
      message: { role: 'assistant', content: [{ type: 'text', text: assistantText }] },
      timestamp: now(),
    },
  ];
  const file = path.join(dir, `${sessionId}.jsonl`);
  writeFileSync(file, lines.map((l) => JSON.stringify(l)).join('\n') + '\n');
  return file;
}

// ---------------------------------------------------------------- model tracking
// Real transcripts record the model that produced each turn on the assistant
// entry itself (`message.model`) — the ONLY on-disk trace of a mid-session
// /model switch. The builders below let a test compose that history line by
// line: which model, whether it was a subagent (isSidechain), and whether the
// entry has any text at all (mid-tool-loop it does not, but it still carries
// the model — which is exactly what makes it the earliest switch signal).

export interface AssistantLineOptions {
  model: string;
  text?: string;
  isSidechain?: boolean;
}

/** An assistant turn produced by `model`. `isSidechain` marks a subagent's turn. */
export function assistantLine({
  model,
  text = 'ok',
  isSidechain = false,
}: AssistantLineOptions): TranscriptEntry {
  return {
    type: 'assistant',
    isSidechain,
    message: { role: 'assistant', model, content: [{ type: 'text', text }] },
    timestamp: new Date().toISOString(),
  };
}

export interface ToolUseLineOptions {
  model: string;
  name?: string;
}

/** An assistant turn mid-tool-loop: carries a model but NO text block. */
export function toolUseLine({ model, name = 'Bash' }: ToolUseLineOptions): TranscriptEntry {
  return {
    type: 'assistant',
    isSidechain: false,
    message: {
      role: 'assistant',
      model,
      content: [{ type: 'tool_use', id: 'toolu_x', name, input: {} }],
    },
    timestamp: new Date().toISOString(),
  };
}

export interface UserLineOptions {
  text?: string;
  bulk?: number;
}

/** A user turn. `bulk` pads the line, to push earlier lines far from EOF. */
export function userLine({ text = 'go on', bulk = 0 }: UserLineOptions = {}): TranscriptEntry {
  return {
    type: 'user',
    message: { role: 'user', content: text + (bulk ? ' ' + 'x'.repeat(bulk) : '') },
    timestamp: new Date().toISOString(),
  };
}

/** Write `entries` as a transcript JSONL under `dir`. Returns the path. */
export function writeTranscriptLines(
  dir: string,
  sessionId: string,
  entries: TranscriptEntry[],
): string {
  const file = path.join(dir, `${sessionId}.jsonl`);
  writeFileSync(file, entries.map((e) => JSON.stringify(e)).join('\n') + '\n');
  return file;
}

/** Append more turns to an existing transcript, as a live session would. */
export function appendTranscriptLines(file: string, entries: TranscriptEntry[]): string {
  appendFileSync(file, entries.map((e) => JSON.stringify(e)).join('\n') + '\n');
  return file;
}
