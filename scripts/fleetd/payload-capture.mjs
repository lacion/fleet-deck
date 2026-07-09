// payload-capture.mjs — pin real CLI hook payload shapes for validation.
//
// Logs the FIRST 3 raw payloads per hook event name EVER seen (counts are
// rebuilt from the file itself on startup, so the cap survives daemon
// restarts) to FLEETDECK_HOME/hook-payloads.jsonl. Each line:
//   {"at": <ms>, "event": "<HookName>", "keys": [...], "payload": {...}}
// `keys` is the payload's top-level key list — the quick answer to questions
// like "does 2.1.206 actually send last_assistant_message on Stop?" (the
// question behind the F3d async-rewake watcher work) without wading through
// the payload itself.
//
// Best-effort by design: the file is size-capped (~1 MB, checked before every
// append), and every failure — unwritable home, full disk, giant payload —
// is swallowed. Capture must never affect a hook response.

import fs from 'node:fs';
import path from 'node:path';

const MAX_FILE_BYTES = 1_000_000;
const PER_EVENT = 3;

export function createPayloadCapture(homeDir, { maxBytes = MAX_FILE_BYTES, perEvent = PER_EVENT } = {}) {
  const file = path.join(homeDir, 'hook-payloads.jsonl');
  const counts = new Map();
  try {
    for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
      if (!line.trim()) continue;
      try {
        const rec = JSON.parse(line);
        if (rec?.event) counts.set(rec.event, (counts.get(rec.event) || 0) + 1);
      } catch { /* truncated tail line */ }
    }
  } catch { /* no capture file yet */ }

  return function capture(event, payload) {
    try {
      if (!event || (counts.get(event) || 0) >= perEvent) return;
      let size = 0;
      try { size = fs.statSync(file).size; } catch { size = 0; }
      const line = JSON.stringify({
        at: Date.now(),
        event,
        keys: payload && typeof payload === 'object' ? Object.keys(payload) : [],
        payload,
      }) + '\n';
      if (size + Buffer.byteLength(line) > maxBytes) return; // size cap
      fs.appendFileSync(file, line);
      counts.set(event, (counts.get(event) || 0) + 1);
    } catch { /* best-effort only */ }
  };
}
