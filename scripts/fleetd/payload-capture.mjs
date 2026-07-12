// payload-capture.mjs — pin real CLI hook payload shapes for validation.
//
// When explicitly enabled, logs the FIRST 3 raw payloads per hook event name
// EVER seen (counts are
// rebuilt from the file itself on startup, so the cap survives daemon
// restarts) to FLEETDECK_HOME/hook-payloads.jsonl. Each line:
//   {"at": <ms>, "event": "<HookName>", "keys": [...], "payload": {...}}
// `keys` is the payload's top-level key list — the quick answer to questions
// like "does 2.1.206 actually send last_assistant_message on Stop?" (the
// question behind the F3d async-rewake watcher work) without wading through
// the payload itself.
//
// Best-effort by design: capture is OFF unless FLEETDECK_CAPTURE_PAYLOADS=on,
// each payload is projected into a bounded diagnostic value before JSON ever
// sees it, the file is owner-only and size-capped (~1 MB, checked before every
// append), and every failure — unwritable home, full disk, giant payload — is
// swallowed. Capture must never affect a hook response.

import fs from 'node:fs';
import path from 'node:path';

const MAX_FILE_BYTES = 1_000_000;
const MAX_PAYLOAD_BYTES = 64_000;
const PER_EVENT = 3;
const NOOP = () => {};

// WHY this is a projection instead of `JSON.stringify(payload).slice(...)`:
// hook payloads can contain multi-megabyte file contents/tool inputs. The
// latter approach first materializes the very secret-bearing giant string we
// are trying to bound. This copier spends a byte budget while walking and
// therefore hands JSON.stringify only a small, acyclic value. The accounting
// is deliberately conservative; exact line size is still checked below.
function boundedPayload(value, maxBytes) {
  let remaining = Math.max(0, maxBytes);
  const seen = new WeakSet();
  const marker = '[truncated]';

  function textWithinBudget(value) {
    if (remaining <= 0) return marker;
    // Slice by characters first so Buffer.byteLength never has to inspect a
    // giant string. UTF-8 may use several bytes/character, hence the small
    // correction loop over an already-bounded slice.
    let out = String(value).slice(0, remaining);
    while (out && Buffer.byteLength(out) > remaining) out = out.slice(0, Math.floor(out.length * 0.75));
    remaining -= Buffer.byteLength(out);
    return out.length < String(value).length ? `${out}${marker}` : out;
  }

  function visit(current, depth = 0) {
    if (remaining <= 0) return marker;
    remaining -= 8; // WHY: reserve structural JSON punctuation per node.
    if (current === null || typeof current === 'boolean' || typeof current === 'number') return current;
    if (typeof current === 'string') return textWithinBudget(current);
    if (typeof current === 'bigint') return textWithinBudget(current);
    if (typeof current !== 'object') return textWithinBudget(String(current));
    if (depth >= 12) return '[max-depth]';
    if (seen.has(current)) return '[circular]';
    seen.add(current);

    if (Array.isArray(current)) {
      const out = [];
      // WHY: a hostile/surprising sparse array can advertise an enormous
      // length. The byte charge makes the walk finite even in that case.
      for (let i = 0; i < current.length && remaining > 0; i++) out.push(visit(current[i], depth + 1));
      if (out.length < current.length) out.push(marker);
      return out;
    }

    const out = {};
    for (const key in current) {
      if (!Object.hasOwn(current, key) || remaining <= 0) continue;
      remaining -= Math.min(remaining, Buffer.byteLength(key) + 4);
      out[key] = visit(current[key], depth + 1);
    }
    return out;
  }

  return visit(value);
}

export function createPayloadCapture(homeDir, {
  maxBytes = MAX_FILE_BYTES,
  maxPayloadBytes = MAX_PAYLOAD_BYTES,
  perEvent = PER_EVENT,
  enabled = process.env.FLEETDECK_CAPTURE_PAYLOADS?.trim().toLowerCase() === 'on',
} = {}) {
  // WHY return a function rather than null: fleetd/http can call capture on
  // every hook without a feature-flag branch or a wiring change.
  if (!enabled) return NOOP;

  const file = path.join(homeDir, 'hook-payloads.jsonl');
  const counts = new Map();
  // mode on append only affects creation. Tighten a file left behind by an
  // older Fleet Deck before reading or appending any more sensitive records.
  try { fs.chmodSync(file, 0o600); } catch { /* absent/unwritable: best effort */ }
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
      const safePayload = boundedPayload(payload, maxPayloadBytes);
      const line = JSON.stringify({
        at: Date.now(),
        event,
        keys: safePayload && typeof safePayload === 'object' && !Array.isArray(safePayload)
          ? Object.keys(safePayload)
          : [],
        payload: safePayload,
      }) + '\n';
      if (size + Buffer.byteLength(line) > maxBytes) return; // size cap
      // WHY both mode and chmod: mode protects first creation against ambient
      // umask; chmod also repairs an existing legacy 0644 capture file.
      fs.appendFileSync(file, line, { encoding: 'utf8', mode: 0o600 });
      try { fs.chmodSync(file, 0o600); } catch { /* append already succeeded */ }
      counts.set(event, (counts.get(event) || 0) + 1);
    } catch { /* best-effort only */ }
  };
}
