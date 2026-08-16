// transcript.ts — bounded, backwards reads of a Claude Code session
// transcript (the JSONL at a hook payload's `transcript_path`).
//
// The transcript is the only place two things are recorded:
//
//   • the assistant's final words  → lastAssistantText (free-text question
//     detection at Stop, F3d)
//   • the model that produced each turn → lastAssistantModel
//
// The model matters because Claude Code reports `model` in the SessionStart
// hook payload and NOWHERE else. A mid-session `/model` switch is invisible to
// hooks, so without reading the transcript a card's model badge is frozen at
// whatever the session launched with. Every assistant line carries
// `message.model`, so the transcript tracks the switch faithfully.
//
// Both readers are best-effort: any stat/read/parse failure returns null
// rather than throwing, because neither may disturb the hook response.

import fs from 'node:fs';

// One yielded record: a non-empty transcript line and its ABSOLUTE byte offset.
interface TailLine {
  line: string;
  offset: number;
}
// tailLines returns a generator carrying an extra `truncated` flag (whether the
// read window hit the byte cap), so the type is the generator intersected with
// that property.
type TailIterator = Generator<TailLine, void, unknown> & { truncated: boolean };

// The shape of the untrusted JSONL rows this module reads. Fields are `unknown`
// because they come straight off `JSON.parse` — every access below narrows
// before use (the original JS did the same defensive optional-chaining).
interface ContentBlock {
  type?: unknown;
  text?: unknown;
}
interface TranscriptEntry {
  type?: unknown;
  isSidechain?: unknown;
  message?: { content?: unknown; model?: unknown };
}
// scanForModel's three-shaped result: a model (or null), whether an assistant
// turn was found, and — only on the not-found paths — whether more remains.
interface ModelScan {
  model: string | null;
  found: boolean;
  truncated?: boolean;
}

// Walk a JSONL file's tail backwards, newest line first, reading at most
// `maxBytes` from the end. Yields { line, offset } where `offset` is the
// line's ABSOLUTE byte offset in the file — that is what lets a caller ignore
// lines written before some watermark (see lastAssistantModel's minOffset).
//
// `truncated` on the returned iterator says whether the window hit the cap,
// i.e. whether re-reading with a bigger window could find more.
export function tailLines(
  transcriptPath: string,
  { maxBytes = 262_144 }: { maxBytes?: number } = {},
): TailIterator {
  const stat = fs.statSync(transcriptPath);
  const start = Math.max(0, stat.size - maxBytes);
  const buf = Buffer.alloc(stat.size - start);
  const fd = fs.openSync(transcriptPath, 'r');
  let nread: number;
  try {
    nread = fs.readSync(fd, buf, 0, buf.length, start);
  } finally {
    fs.closeSync(fd);
  }
  // WHY: stat and read are separate syscalls. Transcripts are append-only in
  // practice, but if a file shrinks between them, Buffer.alloc's unread tail
  // is NUL padding — never let those zeroes corrupt the newest JSONL record.
  let chunk = buf.subarray(0, nread).toString('utf8');
  // WHY: drop the first row ONLY when the read began mid-line. When the window
  // starts exactly on a row boundary (byte start-1 is '\n') the first row is
  // complete — discarding it anyway can hide the newest assistant model
  // (BUG-194).
  let firstRowIsPartial = start > 0;
  if (firstRowIsPartial) {
    const prev = Buffer.alloc(1);
    const pfd = fs.openSync(transcriptPath, 'r');
    try {
      fs.readSync(pfd, prev, 0, 1, start - 1);
    } finally {
      fs.closeSync(pfd);
    }
    firstRowIsPartial = prev[0] !== 0x0a; // '\n'
  }
  if (firstRowIsPartial) chunk = chunk.slice(chunk.indexOf('\n') + 1); // drop the partial first line

  const lines = chunk.split('\n');
  const gen = (function* (): Generator<TailLine, void, unknown> {
    // Offsets are computed from the END of the file backwards, so dropping the
    // partial first line above cannot shift them.
    let end = stat.size; // absolute end (exclusive) of lines[i]
    for (let i = lines.length - 1; i >= 0; i--) {
      const raw = lines[i];
      if (raw === undefined) continue; // in-bounds: never undefined; satisfies noUncheckedIndexedAccess
      const bytes = Buffer.byteLength(raw, 'utf8');
      const offset = end - bytes;
      end = offset - 1; // consume the '\n' preceding this line
      const line = raw.trim();
      if (line) yield { line, offset };
    }
  })();
  // Object.assign returns the generator intersected with { truncated }, so the
  // flag rides along without an unsafe cast.
  const it: TailIterator = Object.assign(gen, { truncated: start > 0 });
  return it;
}

// Read the LAST assistant message's text from a transcript. An assistant
// entry's text blocks are concatenated; entries with no text at all
// (tool_use-only) and sidechain entries are skipped, so what gets scanned is
// "the assistant's final words". Any read/parse failure returns null —
// detection is best-effort and must never disturb the Stop response.
export function lastAssistantText(
  transcriptPath: string,
  { maxBytes = 2_000_000 }: { maxBytes?: number } = {},
): string | null {
  try {
    let newest = true;
    for (const { line } of tailLines(transcriptPath, { maxBytes })) {
      // Most transcript rows are user/tool_result entries, occasionally very
      // large ones. WHY: the newest row must still be parsed unconditionally
      // so a partial append cannot resurrect old text; only older rows get the
      // cheap pre-parse reject. Structural checks remain authoritative.
      const maybeAssistant = line.includes('"assistant"');
      if (!newest && !maybeAssistant) continue;
      let entry: TranscriptEntry | null;
      try {
        entry = JSON.parse(line) as TranscriptEntry | null;
      } catch {
        // The newest non-empty line may still be in the middle of an append.
        // Falling through to an older assistant turn can resurrect a question
        // the user already answered, so "not stable yet" is represented as no
        // result. Corruption deeper in history stays best-effort-skippable.
        if (newest) return null;
        newest = false;
        continue;
      }
      newest = false;
      if (!maybeAssistant) continue;
      if (entry?.type !== 'assistant' || entry.isSidechain === true) continue;
      const content: unknown = entry.message?.content;
      let text = '';
      if (typeof content === 'string') {
        text = content.trim();
      } else if (Array.isArray(content)) {
        // Concatenate every text block; skip tool_use blocks and non-strings.
        const parts: string[] = [];
        for (const b of content as (ContentBlock | null)[]) {
          if (b?.type === 'text' && typeof b.text === 'string') parts.push(b.text);
        }
        text = parts.join('\n').trim();
      }
      if (text) return text;
    }
  } catch {
    /* unreadable/absent transcript: no detection */
  }
  return null;
}

// Read the model that produced the most recent MAIN-THREAD assistant turn.
//
// Three things this does that lastAssistantText deliberately does not:
//
//   • It accepts a tool_use-only entry. Mid-tool-loop the newest assistant
//     entries carry no text block at all, but they still carry the model — and
//     that is the earliest moment a /model switch becomes visible.
//   • It honours `minOffset`: a line below the watermark belongs to a PREVIOUS
//     run of a resumed session (`claude --resume` appends to the same file), so
//     it is not evidence about the model running now. Finding one means "no
//     evidence yet", not "the old model".
//   • It reads a tiny common-case window first, then 256 KB, and only pays for
//     a 2 MB read when a huge tool_result pushed the last assistant entry far
//     from EOF. Most hook events therefore read 16 KB rather than 256 KB.
function scanForModel(transcriptPath: string, maxBytes: number, minOffset: number): ModelScan {
  const it = tailLines(transcriptPath, { maxBytes });
  let newest = true;
  for (const { line, offset } of it) {
    // Cheap reject before JSON.parse: a tool_result line can be hundreds of KB
    // and parsing it just to discard it is the whole cost of this function.
    // WHY: the newest row must still be parsed unconditionally so a partial
    // append cannot resurrect an older turn's model (the same contract
    // lastAssistantText enforces for text); only older rows get the cheap
    // pre-parse reject.
    const maybeAssistant = line.includes('"assistant"') && line.includes('"model"');
    if (!newest && !maybeAssistant) continue;
    let entry: TranscriptEntry | null;
    try {
      entry = JSON.parse(line) as TranscriptEntry | null;
    } catch {
      // The newest non-empty line may still be in the middle of an append.
      // Falling through to an older assistant turn reports a STALE model —
      // and derive.mjs would cache it until the next size change — so "not
      // stable yet" is represented as no result. Corruption deeper in history
      // stays best-effort-skippable.
      if (newest) return { model: null, found: false, truncated: false };
      newest = false;
      continue;
    }
    newest = false;
    if (!maybeAssistant) continue;
    if (entry?.type !== 'assistant' || entry.isSidechain === true) continue; // a subagent is not the main thread
    const model: unknown = entry.message?.model;
    if (typeof model !== 'string' || !model.trim()) continue;
    return { model: offset >= minOffset ? model : null, found: true };
  }
  return { model: null, found: false, truncated: it.truncated };
}

export function lastAssistantModel(
  transcriptPath: string,
  { minOffset = 0 }: { minOffset?: number } = {},
): string | null {
  try {
    const near = scanForModel(transcriptPath, 16_384, minOffset);
    if (near.found) return near.model;
    if (!near.truncated) return null; // we saw the whole file; there is nothing more to find
    const wider = scanForModel(transcriptPath, 262_144, minOffset);
    if (wider.found) return wider.model;
    if (!wider.truncated) return null;
    return scanForModel(transcriptPath, 2_000_000, minOffset).model;
  } catch {
    /* unreadable/absent transcript: leave the model as it was */
  }
  return null;
}
