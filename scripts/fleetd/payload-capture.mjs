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
//
// Redaction rides that same single walk, in three layers: secret-looking KEYS
// (token/secret/password/api-key/authorization/… incl. camelCase) get a marker
// and their value is never descended into; string VALUES matching a known
// credential shape (Anthropic/GitHub/Slack/AWS keys, JWTs, PEM private keys,
// Bearer tokens) are masked in place; and the daemon's own access token is
// scrubbed verbatim from the finished line. What this canNOT catch is a secret
// with no telltale key name and no recognizable shape sitting in arbitrary free
// text — which is exactly why capture stays opt-in and the file stays 0600.

import fs from 'node:fs';
import path from 'node:path';

const MAX_FILE_BYTES = 1_000_000;
const MAX_PAYLOAD_BYTES = 64_000;
const PER_EVENT = 3;
const NOOP = () => {};
const REDACTED = '[redacted]';

// SECRET_KEY_RE names keys whose VALUE we must never record. The
// (?:^|[_\-.])…(?:$|[_\-.]) boundaries are what stop it from firing on innocent
// words that merely contain a secret term — 'tokenizer', 'authored',
// 'monotonic' all survive. camelCase carries no such separator, so isSecretKey
// first rewrites humps to '_'; that is precisely what lets 'apiKey',
// 'authToken' and 'accessKeyId' redact while the negatives above still don't.
const SECRET_KEY_RE = /(?:^|[_\-.])(token|secret|password|passwd|passphrase|api[_-]?key|apikey|auth(orization)?|bearer|cookie|credential|private[_-]?key|access[_-]?key|client[_-]?secret)(?:$|[_\-.])/i;

function isSecretKey(key) {
  const normalized = String(key)
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')     // apiKey → api_Key
    .replace(/([A-Z]+)([A-Z][a-z])/g, '$1_$2'); // ACCESSKey → ACCESS_Key (acronym run)
  return SECRET_KEY_RE.test(normalized);
}

// SECRET_VALUE_RES are known credential SHAPES, masked wherever they appear
// inside a recorded string. INVARIANT the byte accounting leans on: every one
// of these matches a run strictly longer than the 10-byte REDACTED marker, so
// masking can only shrink (never grow) an already-budgeted slice — see
// textWithinBudget. The PEM alternative tolerates a block the byte budget cut
// off mid-key (…|$), so a half-captured private key still masks.
//
// ReDoS AUDIT (capture runs synchronously in the hook handler, so a pattern
// that backtracks super-linearly on attacker-influenced text stalls the
// daemon). Each pattern below was checked against adversarial input:
//   - sk-ant / ghp / github_pat / xox / Bearer each have a SINGLE trailing
//     unbounded run with no required token after it (and the class never
//     overlaps its own prefix separator), so a greedy match either succeeds in
//     one forward pass or fails locally — linear, left as-is. Bounding their
//     tails would risk under-masking a legitimately long token.
//   - AKIA is fixed-width — trivially linear.
//   - JWT had THREE unbounded runs joined by literal dots; on a string like
//     ('eyJ'.repeat(N) + '.' + 'a'.repeat(M)) the first run scans to the lone
//     dot at every one of the N 'eyJ' start positions → O(n^2), a measured
//     ~2s stall at 64KB. Bounding each segment to {10,4096} (real JWT segments
//     are far shorter) makes per-start work constant → linear, and a normal JWT
//     still matches.
//   - PEM was measured safe (lazy `[\s\S]*?`, anchored, `…|$` still tolerates a
//     truncated block). The two `[A-Z ]*` key-type labels are defensively
//     bounded to {0,40} — every real label ("RSA", "OPENSSH", "ENCRYPTED", …)
//     fits, and match semantics (incl. the truncated-block `…|$` fallback) are
//     unchanged.
const SECRET_VALUE_RES = [
  /sk-ant-[A-Za-z0-9_-]{10,}/g,
  /(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}/g,
  /github_pat_[A-Za-z0-9_]{20,}/g,
  /xox[baprs]-[A-Za-z0-9-]{10,}/g,
  /AKIA[A-Z0-9]{16}/g,
  /eyJ[A-Za-z0-9_-]{10,4096}\.[A-Za-z0-9_-]{10,4096}\.[A-Za-z0-9_-]{10,4096}/g,
  /-----BEGIN [A-Z ]{0,40}PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]{0,40}PRIVATE KEY-----|$)/g,
  /Bearer\s+[A-Za-z0-9._~+/=-]{16,}/g,
];

function redactValue(text) {
  let out = text;
  for (const re of SECRET_VALUE_RES) out = out.replace(re, REDACTED);
  return out;
}

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
    const truncated = out.length < String(value).length;
    // VALUE REDACTION rides here, AFTER the slice: a giant secret is bounded
    // first (never fully materialized) and only then masked. Because every
    // SECRET_VALUE_RES match is longer than the marker it becomes, this can only
    // shrink `out`; `remaining` was already charged for the pre-mask bytes, so
    // we never emit more than was accounted for and the budget stays sound.
    // KNOWN RESIDUAL (accepted): masking runs on the post-slice string, so a
    // real credential that straddles the exact byte-budget boundary is cut to a
    // sub-min-length prefix its shape regex no longer recognizes, and that
    // prefix survives. We deliberately do NOT redact pre-slice — that would
    // re-materialize the multi-MB secret the budget exists to avoid. This is a
    // narrow leak of a partial token onto an opt-in, 0600 file; documented so
    // the next reader knows it is known and why it is tolerated.
    out = redactValue(out);
    return truncated ? `${out}${marker}` : out;
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
      // KEY-NAME REDACTION: a secret-looking key records a fixed marker and we
      // deliberately do NOT descend — the value (possibly a multi-MB token or
      // nested blob) is never walked or materialized. The marker is tiny, so we
      // charge nothing beyond the key name already charged above.
      if (isSecretKey(key)) { out[key] = REDACTED; continue; }
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
  secrets = [],
  enabled = process.env.FLEETDECK_CAPTURE_PAYLOADS?.trim().toLowerCase() === 'on',
} = {}) {
  // WHY return a function rather than null: fleetd/http can call capture on
  // every hook without a feature-flag branch or a wiring change.
  if (!enabled) return NOOP;

  // Values the daemon already knows verbatim (its own access token). Empty
  // entries are dropped so a tokenless daemon scrubs nothing. capture() applies
  // these via split/join — see there for why not a regex.
  const exactSecrets = secrets.filter(s => typeof s === 'string' && s.length > 0);

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
      let line = JSON.stringify({
        at: Date.now(),
        event,
        keys: safePayload && typeof safePayload === 'object' && !Array.isArray(safePayload)
          ? Object.keys(safePayload)
          : [],
        payload: safePayload,
      }) + '\n';
      // EXACT-SECRET SCRUB: strip any known daemon secret from the FINISHED line
      // via split/join, not a regex — a token can contain regex metacharacters.
      // Runs before the size cap so the cap measures exactly what hits disk.
      // The line is JSON, so a secret containing " \ or a control char appears
      // ONLY in JSON-escaped form and the raw split would never match — so scrub
      // BOTH the raw secret AND its escaped inner form (stringify then drop the
      // surrounding quotes). Generated tokens are hex (escaped === raw, so the
      // second pass is skipped), but an operator-set FLEETDECK_TOKEN may contain
      // those chars and must not leak verbatim.
      for (const secret of exactSecrets) {
        line = line.split(secret).join(REDACTED);
        const escaped = JSON.stringify(secret).slice(1, -1); // inner form, no quotes
        if (escaped && escaped !== secret) line = line.split(escaped).join(REDACTED);
      }
      if (size + Buffer.byteLength(line) > maxBytes) return; // size cap
      // WHY both mode and chmod: mode protects first creation against ambient
      // umask; chmod also repairs an existing legacy 0644 capture file.
      fs.appendFileSync(file, line, { encoding: 'utf8', mode: 0o600 });
      try { fs.chmodSync(file, 0o600); } catch { /* append already succeeded */ }
      counts.set(event, (counts.get(event) || 0) + 1);
    } catch { /* best-effort only */ }
  };
}
