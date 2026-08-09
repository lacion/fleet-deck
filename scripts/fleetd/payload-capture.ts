// payload-capture.ts — pin real CLI hook payload shapes for validation.
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
// Redaction rides that same single walk, in four layers: secret-looking KEYS
// (token/secret/password/api-key/authorization/… incl. camelCase) get a marker
// and their value is never descended into; string VALUES matching a known
// credential shape (Anthropic/GitHub/GitLab/Google/OpenAI-style/Hugging Face/
// DigitalOcean/Slack/AWS keys, JWTs, PEM private keys, Bearer tokens) are
// masked in place; credentialed URLs (userinfo, secret query params) are
// scrubbed positionally via scrubUrlCredentials; and the daemon's own access
// token is scrubbed verbatim from the finished line. What this canNOT catch
// is a secret with no telltale key name, no recognizable shape, and no URL
// structure sitting in arbitrary free text — which is exactly why capture
// stays opt-in and the file stays 0600.
//
// Two of those layers are exported for reuse by diagnostics that DO reach the
// board (a stalled spawn's pane excerpt, a failed clone's git stderr):
// redactDiagnosticText (shape scrub) and scrubUrlCredentials (positional URL
// userinfo scrub). See each one's own comment for why they stay separate.

import fs from 'node:fs';
import path from 'node:path';

// A JSON-serializable value — what boundedPayload projects any payload into
// before JSON.stringify ever sees it.
type Json = string | number | boolean | null | Json[] | { [key: string]: Json };

const MAX_FILE_BYTES = 1_000_000;
const MAX_PAYLOAD_BYTES = 64_000;
const PER_EVENT = 3;
const NOOP = () => {
  /* capture disabled: swallow every call */
};
const REDACTED = '[redacted]';

// SECRET_KEY_RE names keys whose VALUE we must never record. The
// (?:^|[_\-.])…(?:$|[_\-.]) boundaries are what stop it from firing on innocent
// words that merely contain a secret term — 'tokenizer', 'authored',
// 'monotonic' all survive. camelCase carries no such separator, so isSecretKey
// first rewrites humps to '_'; that is precisely what lets 'apiKey',
// 'authToken' and 'accessKeyId' redact while the negatives above still don't.
// The trailing `s?` admits the plural/container forms real env and tool JSON
// actually use — 'api_keys', 'tokens', 'credentials', 'client_secrets',
// 'apiKeys', 'TOKENS' — which carry the same live credentials as their
// singulars. The plural is not decorative: real payloads nest credentials
// under `api_keys`, `clientSecrets`, `tokens` (a hook event's `access_tokens`
// list), and a singular-only list recorded those verbatim. It cannot resurrect
// a negative: 'tokenizer' would need the `s` to sit mid-word, and no stem is
// one letter short of an innocent word.
const SECRET_KEY_RE =
  /(?:^|[_\-.])(token|secret|password|passwd|passphrase|api[_-]?key|apikey|auth(orization)?|bearer|cookie|credential|private[_-]?key|access[_-]?key|client[_-]?secret)s?(?:$|[_\-.])/i;

function isSecretKey(key: string): boolean {
  const normalized = key
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2') // apiKey → api_Key
    .replace(/([A-Z]+)([A-Z][a-z])/g, '$1_$2'); // ACCESSKey → ACCESS_Key (acronym run)
  return SECRET_KEY_RE.test(normalized);
}

// SECRET_VALUE_RES are known credential SHAPES, masked wherever they appear
// inside a recorded string. INVARIANT the byte accounting leans on: every one
// of these (and maskCompactTokens below) matches a run strictly longer than
// the 10-byte REDACTED marker, so masking can only shrink (never grow) an
// already-budgeted slice — see textWithinBudget. The PEM alternative tolerates a block the byte budget cut
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
//   - JWT is no longer a regex at all: see maskCompactTokens below. A regex had
//     exactly two ways to be wrong, and both shipped at some point — unbounded
//     runs were O(n^2) on adversarial text, and bounding them to {10,4096}
//     FAILED CLOSED the wrong way: a real JWT with a segment over the bound (a
//     large x5c certificate chain in the protected header) matched NOTHING and
//     crossed the redaction boundary verbatim. The scanner is a single forward
//     pass with no upper bound, so neither failure mode can recur.
//   - PEM was measured safe (lazy `[\s\S]*?`, anchored, `…|$` still tolerates a
//     truncated block). The two `[A-Z ]*` key-type labels are defensively
//     bounded to {0,40} — every real label ("RSA", "OPENSSH", "ENCRYPTED", …)
//     fits, and match semantics (incl. the truncated-block `…|$` fallback) are
//     unchanged.
// The `(?<![A-Za-z0-9_-])` left boundary on the 2026-08 additions is not
// decoration — it is the same guard exec.mjs's GIT_EXTRA_SECRET_RES documents:
// without it the generic `sk-` rule fires INSIDE ordinary words and
// `disk-quota-exceeded-for-user` becomes `di[redacted]`. Keep any future
// short-prefix shape behind the same lookbehind.
const SECRET_VALUE_RES: RegExp[] = [
  /sk-ant-[A-Za-z0-9_-]{10,}/g,
  /(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}/g,
  /github_pat_[A-Za-z0-9_]{20,}/g,
  /(?<![A-Za-z0-9_-])gl(?:pat|rt|dt|soat|cbt|ptt|feat|agent)-[A-Za-z0-9_-]{16,}/g, // GitLab PAT / runner / deploy / OAuth / CI job families
  /(?<![A-Za-z0-9_-])AIza[A-Za-z0-9_-]{30,}/g, // Google API key
  /(?<![A-Za-z0-9_-])sk-[A-Za-z0-9_-]{20,}/g, // OpenAI-style (and, harmlessly, sk-ant-* again)
  /(?<![A-Za-z0-9_-])hf_[A-Za-z0-9]{20,}/g, // Hugging Face
  /(?<![A-Za-z0-9_-])dop_v1_[A-Za-z0-9]{32,}/g, // DigitalOcean
  /xox[baprs]-[A-Za-z0-9-]{10,}/g,
  /AKIA[A-Z0-9]{16}/g,
  /-----BEGIN [A-Z ]{0,40}PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]{0,40}PRIVATE KEY-----|$)/g,
  /Bearer\s+[A-Za-z0-9._~+/=-]{16,}/g,
  // Forge/API prefixes the git path used to redact ONLY inside exec.mjs's
  // redactGitText (as GIT_EXTRA_SECRET_RES) — so a bare `glpat-…` / `sk-…` /
  // `AIza…` / `hf_…` / `dop_v1_…` relayed on a stalled spawn's pane excerpt
  // (`remote: the provided token (glpat-…) is incorrect`) or captured in a hook
  // payload's free-text field survived into stall_detail, the board drawer,
  // /state, every /ws frame and the durable SpawnStalled note. They live in the
  // SHARED list now so every consumer fails closed on the same shapes. ReDoS:
  // each is a fixed prefix plus ONE greedy trailing run with nothing required
  // after it (the lookbehind is zero-width and constant) — the same shape the
  // audit above certifies linear. All match runs longer than the 10-byte
  // `[redacted]` marker, preserving the shrink-only invariant.
  //
  // The `(?<![A-Za-z0-9_-])` left boundary is not decoration: without it the
  // generic `sk-` rule fires INSIDE ordinary words, and `disk-quota-exceeded`
  // becomes `di[redacted]` — destroying exactly the legibility these
  // diagnostics exist to deliver. A false redaction is cheap to write and
  // expensive to debug.
  /(?<![A-Za-z0-9_-])gl(?:pat|rt|dt|soat|cbt|ptt|feat|agent)-[A-Za-z0-9_-]{16,}/g, // GitLab PAT / runner / deploy / OAuth / CI job families
  /(?<![A-Za-z0-9_-])AIza[A-Za-z0-9_-]{30,}/g, // Google API key
  /(?<![A-Za-z0-9_-])sk-[A-Za-z0-9_-]{20,}/g, // OpenAI-style (and, harmlessly, sk-ant-* again)
  /(?<![A-Za-z0-9_-])hf_[A-Za-z0-9]{20,}/g, // Hugging Face
  /(?<![A-Za-z0-9_-])dop_v1_[A-Za-z0-9]{32,}/g, // DigitalOcean
];

// Compact-serialized JWTs (JOSE compact form, also used by PASETOv2) are
// recognized by maskCompactTokens — a linear single-pass scanner, NOT an entry
// in SECRET_VALUE_RES. It replaces what used to be a regex of three bounded
// runs, which had BOTH failure modes a credential-shape rule can have:
//   - UNBOUNDED runs were O(n^2) on adversarial text ('eyJ'.repeat(N) + '.' +
//     'a'.repeat(M) rescans to the lone dot at every 'eyJ' start — a measured
//     ~2s stall at 64KB, inside the synchronous hook handler).
//   - BOUNDING those runs to {10,4096} fixed the stall but failed CLOSED the
//     wrong way: a valid JWT whose header carries a large x5c certificate
//     chain blows past 4096 characters, matched nothing, and the full
//     credential crossed the redaction boundary into hook-payloads.jsonl and
//     board diagnostics VERBATIM. A bounded regex does not truncate an
//     over-long token — it simply never matches (the same lesson
//     URL_AUTHORITY_OVERLONG_RE exists to teach).
// The scanner has no upper bound, so no valid token can outgrow it, and it
// advances strictly forward from every candidate start (worst case one false
// start per 'eyJ' occurrence — each consumed in a single segment walk), so it
// stays linear on the same adversarial inputs.
//
// Shape: eyJ<base64url>{10,} '.' <base64url>{10,} '.' <base64url>{10,} —
// every real JWT header starts with the bytes `{"` (base64url `eyJ`), segments
// are joined by exactly two separators, and the minimum lengths (identical to
// the retired regex) keep ordinary prose from ever matching. The b64urlChars
// bitset makes each character test O(1) with no regex engine involved; a
// 6700-byte header segment masks in the same forward pass a 20-byte one does.
//
// LINEARITY: a NAIVE per-candidate walk is O(n^2) on ('eyJ'.repeat(N) + '.' +
// 'a'.repeat(M)) — every 'eyJ' start re-walks the same run to the lone dot.
// The skip-ahead on each failure branch is what keeps it linear: when the walk
// from a candidate fails, every LATER start inside a run it already crossed
// sees strictly fewer of the required separators ahead of it and fails
// identically, so the loop resumes past the crossed run instead of re-walking
// it (a candidate inside a run whose walk stopped at a '.' that followed a
// too-short run CAN still match, so those branches only skip to that run's
// start). Each run of the text is therefore walked O(1) times.
const B64URL_CHARS = (() => {
  const bits = new Uint8Array(128);
  for (let c = 48; c <= 57; c++) bits[c] = 1; // 0-9
  for (let c = 65; c <= 90; c++) bits[c] = 1; // A-Z
  for (let c = 97; c <= 122; c++) bits[c] = 1; // a-z
  bits[45] = 1; // -
  bits[95] = 1; // _
  return bits;
})();
const isB64url = (code: number): boolean => code < 128 && B64URL_CHARS[code] === 1;

function maskCompactTokens(text: string): string {
  const MIN_SEG = 10; // per segment, same floor as the retired regex
  let out: string | null = null; // lazy: untouched input is returned identity, no copy made
  let lastEnd = 0;
  for (let i = 0; (i = text.indexOf('eyJ', i)) !== -1 && i + 35 <= text.length; i++) {
    let j = i + 3;
    const seg1Start = j;
    while (j < text.length && isB64url(text.charCodeAt(j))) j++;
    // Any start inside this run ends at the same j and fails the same way.
    if (j - seg1Start < MIN_SEG || text.charCodeAt(j) !== 46 /* . */) {
      i = j - 1;
      continue;
    }
    j++;
    const seg2Start = j;
    while (j < text.length && isB64url(text.charCodeAt(j))) j++;
    if (j - seg2Start < MIN_SEG) {
      // Run ends at a '.' but is short: a start inside it can still match, so
      // only skip the runs proven dead (through seg2's start).
      i = seg2Start - 1;
      continue;
    }
    if (text.charCodeAt(j) !== 46 /* . */) {
      i = j - 1;
      continue;
    } // no separator → dead through j
    j++;
    const seg3Start = j;
    while (j < text.length && isB64url(text.charCodeAt(j))) j++;
    if (j - seg3Start < MIN_SEG) {
      i = seg3Start - 1;
      continue;
    }
    // Confirmed token at [i, j). Consume it so the loop never re-scans inside
    // an already-masked region.
    out = (out ?? '') + text.slice(lastEnd, i) + REDACTED;
    lastEnd = j;
    i = j - 1; // the for-loop's i++ moves past the token
  }
  return out === null ? text : out + text.slice(lastEnd);
}

function redactValue(text: string): string {
  let out = text;
  for (const re of SECRET_VALUE_RES) out = out.replace(re, REDACTED);
  return maskCompactTokens(out);
}

// Reuse the same known-credential shape scrubber for other bounded diagnostics
// (notably a stalled spawn's captured pane excerpt). It is intentionally only
// value-shape redaction — arbitrary free text can still contain an unrecognized
// secret, so callers must keep diagnostics small and behind the board's token.
export function redactDiagnosticText(text: string | null | undefined): string {
  return redactValue(text ?? '');
}

// URL_USERINFO_RE closes the hole redactDiagnosticText structurally cannot:
// SECRET_VALUE_RES is a list of credential SHAPES, and
// `https://luis:glpat-AbCdEf1234567890@gitlab.com/x/y.git` matches NONE of them
// (the GitLab PAT prefix is not in the list, a corporate password has no shape
// at all). git echoes exactly that string back in its own stderr —
// `fatal: unable to access 'https://user:token@host/x.git/'` — so the moment a
// git diagnostic became something the board can display, a shape-only scrubber
// stopped being sufficient. This one is positional: whatever sits between
// `scheme://` and the authority's last `@` is userinfo, and userinfo is by
// definition credential material.
//
// The WHOLE userinfo is replaced, not just the password half, because we cannot
// tell `https://alice@github.com` (a username) from
// `https://glpat-xxxxxxxx@gitlab.com` (a bare-token URL — GitLab, GitHub and
// Bitbucket all accept that form). Losing a legible username is the accepted
// cost of never shipping a token.
//
// scp-style `git@github.com:owner/repo.git` is deliberately LEFT ALONE: that
// form has no password slot, and mangling it would destroy precisely the
// legibility this whole change exists for — the Coder remedy block that a
// failed clone prints quotes ssh URLs of exactly that shape.
//
// The class excludes `/ ? #` and whitespace so a match can never cross out of
// the authority (a bare `https://github.com/settings/ssh/new` is untouched), and
// it is greedy so `https://user:p@ss@host/x` collapses on the LAST `@` before
// the path — a literal `@` inside a password is the realistic mangling case and
// must not survive as a hostname.
//
// ReDoS AUDIT (this pattern runs on the `remote:` text git relays from an
// attacker-influenceable server, on strings up to execFile's ~1 MB stderr
// buffer, so a super-linear pattern stalls the daemon — same standard as
// SECRET_VALUE_RES above). BOTH unbounded runs had to be bounded:
//   - the userinfo `{0,512}` is defensive only (real userinfo is orders of
//     magnitude shorter) and makes the backtrack after a failed `@` constant.
//   - the SCHEME run is the one that actually bit: `[a-z][a-z0-9+.-]*` on a long
//     alphanumeric run matches to the end and backtracks looking for `://` at
//     EVERY start position in the run → O(n^2), a measured 3.4s on 60 KB of
//     `a`. Bounded to {0,32} (the longest real scheme here is `https`), per-start
//     work is constant → linear; 60 KB now returns in single-digit ms.
// Do not relax either bound without repeating this measurement. The same
// standard applies to LAYERS 2-5 below: every added pattern either has a single
// greedy trailing run with nothing required after it, or bounded runs whose only
// backtrack positions fail in constant time, and the scheme run stays {0,32}
// throughout. tests/git-stderr-detail.test.mjs times the whole composed scrub on
// adversarial input and fails if it stops being effectively linear.
const URL_USERINFO_RE = /([a-z][a-z0-9+.-]{0,32}:\/\/)([^/?#\s]{0,512}@)/gi;

// LAYER 2 — the same shape, but tolerating WHITESPACE inside the PASSWORD half.
// `https://user:my pass@host/x.git` leaves the class above unable to reach the
// `@` at all, so the credential passed through untouched. fleetd's own clone
// validation (repos.mjs SPACE_OR_CONTROL_RE) rejects such an origin, but
// materializeBranch's fetch reads its remote from the CHECKOUT's .git/config,
// which fleetd never validated — so the case is reachable.
//
// TWO restrictions carry this rule's safety, and neither is decorative:
//   - the colon is REQUIRED, and the run BEFORE it still forbids whitespace. A
//     username with a space in it is not a real input; a password with one is.
//     Without those two, `https://gitlab.example for help: mail@example.com`
//     collapses into `https://[redacted]@example.com` — ordinary prose mangled,
//     on a line where nothing was ever secret.
//   - newlines stay excluded, so a match can never span lines and swallow a
//     multi-line remedy block.
// The password run is bounded generously (4096) rather than tightly: LAYER 3
// below cannot back this one up, because a userinfo containing whitespace is
// invisible to its whitespace-free class too. ACCEPTED RESIDUAL: a userinfo
// longer than 4096 characters that also contains whitespace is not covered.
const URL_USERINFO_SPACED_RE =
  /([a-z][a-z0-9+.-]{0,32}:\/\/)([^/?#\s]{0,256}:[^/?#\r\n]{0,4096}@)/gi;

// LAYER 3 — FAIL CLOSED past the bounds above. A bounded regex does not TRUNCATE
// an over-long userinfo: it simply fails to match, and the credential passes
// through verbatim. A 600-character userinfo therefore defeated LAYER 1 (512) and
// LAYER 2 (256+256) completely, and reached the card note, the ticker, the
// durable SpawnFailed event and /state — every surface the scrub exists to
// protect. So any authority run longer than the LAYER 1 bound is redacted
// WHOLESALE, host included. Over-redacting a 512-character hostname that never
// held a credential is a legibility cost; leaking a 600-character token is not a
// cost we are allowed to pay. Greedy run with nothing required after it, scheme
// bounded as above → linear.
const URL_AUTHORITY_OVERLONG_RE = /([a-z][a-z0-9+.-]{0,32}:\/\/)[^/?#\s]{512,}/gi;

// LAYER 4 — a SCHEMELESS `user:token@host/x.git`. git echoes remote strings in
// whatever form it was handed them, and none of the layers above fire without
// `scheme://`. The colon is required for the same reason it is in LAYER 2, and
// specifically so `git@github.com:owner/repo.git` — the scp-style form the whole
// expander exists to keep legible — is left completely alone. Anchored on a
// leading boundary so it cannot start mid-token (which is also what keeps it from
// re-firing on an already-scrubbed `https://[redacted]@host`: `/` is not a
// boundary character, and `[redacted]` holds no colon).
// Quotes and brackets are excluded from BOTH halves as well as required at the
// left edge: git quotes URLs in its own messages, and without that exclusion the
// leading `'` is swallowed into the username run and vanishes from the output —
// `read from 'user:tok@host'` became `read from [redacted]@host'`, quietly
// destroying the delimiter a human reads the line by.
const BARE_USERINFO_RE = /(^|[\s'"<([])([^\s:/@'"<>]{1,256}:[^\s/@'"<>]{1,512})@/g;

// LAYER 5 — a credential in the QUERY STRING or FRAGMENT, which every layer above
// structurally cannot see: their classes stop at `?` by design, so the authority
// is all they ever inspect. `?access_token=` (Gitea), `?private_token=` (GitLab),
// `?job_token=` (GitLab CI) are real bare-token URL forms, and a CI job token
// matches no entry in SECRET_VALUE_RES. git echoes the URL it was given —
// `fatal: unable to access 'https://host/o/r.git?access_token=…'` — so this was a
// live leak on both the note and the detail.
//
// Implemented as a replace CALLBACK rather than one regex on purpose: matching
// the parameter NAME as a single run and then testing it for secret words keeps
// the pattern free of the nested bounded runs (`{0,64}…{0,64}=`) that would make
// per-start-position work quadratic in the bounds on a 1 MB stderr. Start
// positions are limited to `? & #`; the name class excludes `=`, so a failed
// match backtracks over cheap, immediately-failing positions only.
const URL_PARAM_RE = /([?&#][A-Za-z0-9_.-]{1,128}=)([^&\s'"<>]+)/g;
const SECRET_PARAM_NAME_RE =
  /token|key|secret|password|passwd|passphrase|auth|credential|sig(?:nature)?|session/i;

// Deliberately NOT folded into redactDiagnosticText / SECRET_VALUE_RES: that
// shape scrubber's contract is value-shapes only, and callers that display git
// output compose the two explicitly. Hook payload capture composes them too —
// textWithinBudget below applies scrubUrlCredentials on top of redactValue so a
// credentialed URL in a hook string field cannot reach disk intact.
//
// IDEMPOTENT (all five layers): a second pass rewrites `[redacted]@` and
// `=[redacted]` to themselves, which is what lets callers scrub defensively at
// more than one layer without corrupting the text.
export function scrubUrlCredentials(text: string | null | undefined): string {
  return (text ?? '')
    .replace(URL_USERINFO_RE, `$1${REDACTED}@`)
    .replace(URL_USERINFO_SPACED_RE, `$1${REDACTED}@`)
    .replace(URL_AUTHORITY_OVERLONG_RE, `$1${REDACTED}`)
    .replace(BARE_USERINFO_RE, `$1${REDACTED}@`)
    .replace(URL_PARAM_RE, (whole: string, name: string) =>
      SECRET_PARAM_NAME_RE.test(name) ? `${name}${REDACTED}` : whole,
    );
}

// WHY this is a projection instead of `JSON.stringify(payload).slice(...)`:
// hook payloads can contain multi-megabyte file contents/tool inputs. The
// latter approach first materializes the very secret-bearing giant string we
// are trying to bound. This copier spends a byte budget while walking and
// therefore hands JSON.stringify only a small, acyclic value. The accounting
// is deliberately conservative; exact line size is still checked below.
function boundedPayload(value: unknown, maxBytes: number): Json {
  let remaining = Math.max(0, maxBytes);
  const seen = new WeakSet<object>();
  const marker = '[truncated]';

  function textWithinBudget(value: unknown): string {
    if (remaining <= 0) return marker;
    // Slice by characters first so Buffer.byteLength never has to inspect a
    // giant string. UTF-8 may use several bytes/character, hence the small
    // correction loop over an already-bounded slice.
    let out = String(value).slice(0, remaining);
    while (out && Buffer.byteLength(out) > remaining)
      out = out.slice(0, Math.floor(out.length * 0.75));
    remaining -= Buffer.byteLength(out);
    const truncated = out.length < String(value).length;
    // VALUE REDACTION rides here, AFTER the slice: a giant secret is bounded
    // first (never fully materialized) and only then masked. Because every
    // SECRET_VALUE_RES match is longer than the marker it becomes, shape
    // masking can only shrink `out`. scrubUrlCredentials can grow it slightly
    // (a short userinfo becomes the 10-byte marker), but the growth is bounded
    // per occurrence and the finished line's exact size is still checked
    // against the file cap before append, so the budget stays sound.
    // KNOWN RESIDUAL (accepted): masking runs on the post-slice string, so a
    // real credential that straddles the exact byte-budget boundary is cut to a
    // sub-min-length prefix its shape regex no longer recognizes, and that
    // prefix survives. We deliberately do NOT redact pre-slice — that would
    // re-materialize the multi-MB secret the budget exists to avoid. This is a
    // narrow leak of a partial token onto an opt-in, 0600 file; documented so
    // the next reader knows it is known and why it is tolerated.
    out = scrubUrlCredentials(redactValue(out));
    return truncated ? `${out}${marker}` : out;
  }

  function visit(current: unknown, depth = 0): Json {
    if (remaining <= 0) return marker;
    remaining -= 8; // WHY: reserve structural JSON punctuation per node.
    if (current === null || typeof current === 'boolean' || typeof current === 'number')
      return current;
    if (typeof current === 'string') return textWithinBudget(current);
    if (typeof current === 'bigint') return textWithinBudget(current);
    if (typeof current !== 'object') return textWithinBudget(current);
    if (depth >= 12) return '[max-depth]';
    if (seen.has(current)) return '[circular]';
    seen.add(current);

    if (Array.isArray(current)) {
      const arr = current as unknown[];
      const out: Json[] = [];
      // WHY: a hostile/surprising sparse array can advertise an enormous
      // length. The byte charge makes the walk finite even in that case.
      for (let i = 0; i < arr.length && remaining > 0; i++) out.push(visit(arr[i], depth + 1));
      if (out.length < arr.length) out.push(marker);
      return out;
    }

    const obj = current as Record<string, unknown>;
    const out: Record<string, Json> = {};
    for (const key in obj) {
      if (!Object.hasOwn(obj, key) || remaining <= 0) continue;
      remaining -= Math.min(remaining, Buffer.byteLength(key) + 4);
      // KEY-NAME REDACTION: a secret-looking key records a fixed marker and we
      // deliberately do NOT descend — the value (possibly a multi-MB token or
      // nested blob) is never walked or materialized. The marker is tiny, so we
      // charge nothing beyond the key name already charged above.
      if (isSecretKey(key)) {
        out[key] = REDACTED;
        continue;
      }
      out[key] = visit(obj[key], depth + 1);
    }
    return out;
  }

  return visit(value);
}

// The function every hook goes through — a no-op when capture is off, else the
// bounded/redacted append. Typed so a no-op and the real capture share a shape.
export type PayloadCapture = (event: string, payload: unknown) => void;

export interface PayloadCaptureOptions {
  maxBytes?: number;
  maxPayloadBytes?: number;
  perEvent?: number;
  // Values the daemon knows verbatim (its own token). Filtered to non-empty
  // strings before use; typed loose so a caller need not pre-clean the list.
  secrets?: readonly unknown[];
  enabled?: boolean;
}

export function createPayloadCapture(
  homeDir: string,
  {
    maxBytes = MAX_FILE_BYTES,
    maxPayloadBytes = MAX_PAYLOAD_BYTES,
    perEvent = PER_EVENT,
    secrets = [],
    enabled = process.env['FLEETDECK_CAPTURE_PAYLOADS']?.trim().toLowerCase() === 'on',
  }: PayloadCaptureOptions = {},
): PayloadCapture {
  // WHY return a function rather than null: fleetd/http can call capture on
  // every hook without a feature-flag branch or a wiring change.
  if (!enabled) return NOOP;

  // Values the daemon already knows verbatim (its own access token). Empty
  // entries are dropped so a tokenless daemon scrubs nothing. capture() applies
  // these via split/join — see there for why not a regex.
  const exactSecrets = secrets.filter((s): s is string => typeof s === 'string' && s.length > 0);

  const file = path.join(homeDir, 'hook-payloads.jsonl');
  const counts = new Map<string, number>();
  // mode on append only affects creation. Tighten a file left behind by an
  // older Fleet Deck before reading or appending any more sensitive records.
  try {
    fs.chmodSync(file, 0o600);
  } catch {
    /* absent/unwritable: best effort */
  }
  try {
    for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
      if (!line.trim()) continue;
      try {
        const rec: unknown = JSON.parse(line);
        const event =
          rec && typeof rec === 'object' ? (rec as { event?: unknown }).event : undefined;
        if (typeof event === 'string' && event) counts.set(event, (counts.get(event) ?? 0) + 1);
      } catch {
        /* truncated tail line */
      }
    }
  } catch {
    /* no capture file yet */
  }

  return function capture(event: string, payload: unknown): void {
    try {
      if (!event || (counts.get(event) ?? 0) >= perEvent) return;
      let size = 0;
      try {
        size = fs.statSync(file).size;
      } catch {
        size = 0;
      }
      const safePayload = boundedPayload(payload, maxPayloadBytes);
      let line =
        JSON.stringify({
          at: Date.now(),
          event,
          keys:
            safePayload && typeof safePayload === 'object' && !Array.isArray(safePayload)
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
      try {
        fs.chmodSync(file, 0o600);
      } catch {
        /* append already succeeded */
      }
      counts.set(event, (counts.get(event) ?? 0) + 1);
    } catch {
      /* best-effort only */
    }
  };
}
