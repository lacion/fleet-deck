// transcript.mjs — bounded, backwards reads of a Claude Code session
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

// Walk a JSONL file's tail backwards, newest line first, reading at most
// `maxBytes` from the end. Yields { line, offset } where `offset` is the
// line's ABSOLUTE byte offset in the file — that is what lets a caller ignore
// lines written before some watermark (see lastAssistantModel's minOffset).
//
// `truncated` on the returned iterator says whether the window hit the cap,
// i.e. whether re-reading with a bigger window could find more.
export function tailLines(transcriptPath, { maxBytes = 262_144 } = {}) {
  const stat = fs.statSync(transcriptPath);
  const start = Math.max(0, stat.size - maxBytes);
  const buf = Buffer.alloc(stat.size - start);
  const fd = fs.openSync(transcriptPath, 'r');
  try { fs.readSync(fd, buf, 0, buf.length, start); } finally { fs.closeSync(fd); }
  let chunk = buf.toString('utf8');
  if (start > 0) chunk = chunk.slice(chunk.indexOf('\n') + 1); // drop the partial first line

  const lines = chunk.split('\n');
  const it = (function* () {
    // Offsets are computed from the END of the file backwards, so dropping the
    // partial first line above cannot shift them.
    let end = stat.size; // absolute end (exclusive) of lines[i]
    for (let i = lines.length - 1; i >= 0; i--) {
      const bytes = Buffer.byteLength(lines[i], 'utf8');
      const offset = end - bytes;
      end = offset - 1; // consume the '\n' preceding this line
      const line = lines[i].trim();
      if (line) yield { line, offset };
    }
  })();
  it.truncated = start > 0;
  return it;
}

// Read the LAST assistant message's text from a transcript. An assistant
// entry's text blocks are concatenated; entries with no text at all
// (tool_use-only) and sidechain entries are skipped, so what gets scanned is
// "the assistant's final words". Any read/parse failure returns null —
// detection is best-effort and must never disturb the Stop response.
export function lastAssistantText(transcriptPath, { maxBytes = 2_000_000 } = {}) {
  try {
    for (const { line } of tailLines(transcriptPath, { maxBytes })) {
      let entry;
      try { entry = JSON.parse(line); } catch { continue; }
      if (entry?.type !== 'assistant' || entry.isSidechain === true) continue;
      const content = entry.message?.content;
      const text = Array.isArray(content)
        ? content.filter(b => b?.type === 'text' && typeof b.text === 'string').map(b => b.text).join('\n').trim()
        : (typeof content === 'string' ? content.trim() : '');
      if (text) return text;
    }
  } catch { /* unreadable/absent transcript: no detection */ }
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
//   • It reads a small window first (256 KB is many turns) and only pays for a
//     2 MB read if that window was truncated and turned up nothing — which
//     happens when a single huge tool_result has pushed the last assistant
//     entry far from the end of the file.
function scanForModel(transcriptPath, maxBytes, minOffset) {
  const it = tailLines(transcriptPath, { maxBytes });
  for (const { line, offset } of it) {
    // Cheap reject before JSON.parse: a tool_result line can be hundreds of KB
    // and parsing it just to discard it is the whole cost of this function.
    if (!line.includes('"assistant"') || !line.includes('"model"')) continue;
    let entry;
    try { entry = JSON.parse(line); } catch { continue; }
    if (entry?.type !== 'assistant' || entry.isSidechain === true) continue; // a subagent is not the main thread
    const model = entry.message?.model;
    if (typeof model !== 'string' || !model.trim()) continue;
    return { model: offset >= minOffset ? model : null, found: true };
  }
  return { model: null, found: false, truncated: it.truncated };
}

export function lastAssistantModel(transcriptPath, { minOffset = 0 } = {}) {
  try {
    const near = scanForModel(transcriptPath, 262_144, minOffset);
    if (near.found) return near.model;
    if (!near.truncated) return null; // we saw the whole file; there is nothing more to find
    return scanForModel(transcriptPath, 2_000_000, minOffset).model;
  } catch { /* unreadable/absent transcript: leave the model as it was */ }
  return null;
}
