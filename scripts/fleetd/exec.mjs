// exec.mjs — the daemon's shared async subprocess primitive.
//
// `execFileP` runs a command by ARGV (never a shell string, so `;`/`$()`/quotes
// in an argument arrive as literal bytes) and resolves a result object instead of
// rejecting: `{ ok: true, out }` on success, `{ ok: false, code, err }` on any
// failure (non-zero exit, timeout, missing binary, or a synchronous throw). Every
// async execFile caller in the daemon funnels through this one shape — worktrees'
// git probes, the tmux adapter (spawn.mjs) which maps it to its own
// null-or-stdout convention at the boundary, and the agents-cli poller
// (agents-poll.mjs), which tokenizes the operator-supplied FLEETDECK_AGENTS_CMD
// on whitespace and runs it by argv. There is therefore NO shell execution
// anywhere in the daemon — the no-shell security boundary holds without exception.
//
// One sibling wrapper deliberately does NOT share this and is not moved here:
//   - repo-identity.mjs `git()` is SYNCHRONOUS (execFileSync) on purpose — its
//     caller (derive.mjs) consumes results inline while building SQL, and making
//     it async would thread Promises into session state (see its own comment).
import { execFile } from 'node:child_process';
// Both are pure string functions (payload-capture itself imports only
// node:fs/node:path, so this stays acyclic). NOTHING added below runs a
// subprocess, let alone a shell: the no-shell boundary declared above is a
// security guarantee, and a richer diagnostic is never worth probing git for.
import { redactDiagnosticText, scrubUrlCredentials } from './payload-capture.mjs';

export function execFileP(cmd, args, { timeout = 30_000, env } = {}) {
  return new Promise((resolve) => {
    try {
      execFile(cmd, args, {
        timeout,
        windowsHide: true,
        // When an env is supplied it is MERGED over the daemon's own (never
        // replacing it), so PATH and the rest survive while a caller adds e.g.
        // GIT_TERMINAL_PROMPT=0 to make an unauthenticated clone fail fast
        // instead of hanging on a credential prompt.
        ...(env ? { env: { ...process.env, ...env } } : {}),
      }, (err, stdout, stderr) => {
        if (err) return resolve({ ok: false, code: err.code, err: String(stderr || err.message || err).trim() });
        resolve({ ok: true, out: stdout });
      });
    } catch (err) {
      resolve({ ok: false, err: String(err.message || err) });
    }
  });
}

// Distil a git subprocess's stderr down to the one line a human needs on a
// tombstone or ticker: git's own `fatal:`/`error:` verdict. git narrates before
// it fails ("Cloning into '…'"), so the FIRST stderr line — the one an 80-char
// note clamp used to show — routinely hid the actual cause (e.g. `fatal: could
// not read Username for 'https://gitlab.com': terminal prompts disabled`). We
// take the LAST matching verdict line (the final word wins when git prints
// several), else the last non-empty line, trimmed and capped so it stays a
// note, not a log. The full stderr still goes to fleetd.log, and a bounded,
// redacted excerpt now rides the card too (gitStderrDetail below) so the REMEDY
// — a workspace public key, a `https://github.com/settings/ssh/new` to paste it
// into — is one glance away instead of hours: on a Coder workspace this note
// read only "fatal: Could not read from remote repository." while the two lines
// git printed directly above it, discarded here, were the entire fix.
export function distillGitStderr(text) {
  const lines = String(text ?? '').split('\n');
  let verdict = null;
  let lastNonEmpty = null;
  for (const line of lines) {
    if (line.trim() !== '') lastNonEmpty = line;
    if (/^\s*(fatal|error):/i.test(line)) verdict = line;
  }
  const chosen = (verdict ?? lastNonEmpty ?? '').trim();
  return chosen.length > 300 ? chosen.slice(0, 300) : chosen;
}

// The same budget and posture as a stalled spawn's stall_detail (spawns.mjs
// stallDiagnosticExcerpt) — deliberately its sibling, because the two land in
// the same register of the UI and must not be arguable about separately.
const GIT_DETAIL_LINES = 20;
const GIT_DETAIL_MAX = 2000;

// Credential SHAPES that SECRET_VALUE_RES does not carry, kept LOCAL to this
// function on purpose: payload-capture's list governs the on-disk
// hook-payloads.jsonl format, and this change owns no blast radius there (see the
// comment on scrubUrlCredentials). git stderr has its own population of forges,
// and the fetch path in repos.mjs has no origin URL in scope to derive an
// exact-secret needle from, so a BARE forge token relayed on a `remote:` line —
// `remote: the provided token (glpat-…) is incorrect` — had no covering layer at
// all. ReDoS: each is a fixed prefix plus ONE greedy trailing run with nothing
// required after it, the same shape the audit in payload-capture.mjs certifies
// linear (the lookbehind is zero-width and constant). All match runs longer than
// the 10-byte `[redacted]` marker.
//
// The `(?<![A-Za-z0-9_-])` left boundary is not decoration: without it the generic
// `sk-` rule fires INSIDE ordinary words, and `disk-quota-exceeded-for-user`
// becomes `di[redacted]` — destroying exactly the legibility this whole change
// exists to deliver. A false redaction is cheap to write and expensive to debug.
const GIT_EXTRA_SECRET_RES = [
  /(?<![A-Za-z0-9_-])gl(?:pat|rt|dt|soat|cbt|ptt|feat|agent)-[A-Za-z0-9_-]{16,}/g, // GitLab PAT / runner / deploy / OAuth / CI job families
  /(?<![A-Za-z0-9_-])AIza[A-Za-z0-9_-]{30,}/g,                                     // Google API key
  /(?<![A-Za-z0-9_-])sk-[A-Za-z0-9_-]{20,}/g,                                      // OpenAI-style (and, harmlessly, sk-ant-* again)
  /(?<![A-Za-z0-9_-])hf_[A-Za-z0-9]{20,}/g,                                        // Hugging Face
  /(?<![A-Za-z0-9_-])dop_v1_[A-Za-z0-9]{32,}/g,                                    // DigitalOcean
];

// THE single hardening pass for git output, exported so that the NOTE and the
// DETAIL derived from one stderr can never disagree about it. That was a real
// defect and not a hypothetical: the note was given only the positional URL
// scrub, while the shape scrub and the caller's exact secrets were applied inside
// gitStderrDetail — so `fatal: helper rejected token ghp_…` masked the token in
// the expander and printed it verbatim in the note six characters away. The note
// is the strictly WORSE sink of the two: it lands on the card, in the 120-char
// ticker line, in the HTTP 409 body, and in the DURABLE SpawnFailed event that
// outlives the archived card. Callers harden ONCE and derive both.
//
// Order within the pass mirrors gitStderrDetail's contract: positional first
// (a credentialed URL is invisible to a shape list), then the shape lists, then
// the caller's exact needles. Every step is idempotent, so composing this with
// gitStderrDetail — which runs it again over its own input — is safe by design.
export function redactGitText(text, secrets = []) {
  let out = redactDiagnosticText(scrubUrlCredentials(text));
  for (const re of GIT_EXTRA_SECRET_RES) out = out.replace(re, '[redacted]');
  for (const secret of secrets) {
    if (typeof secret === 'string' && secret) out = out.split(secret).join('[redacted]');
  }
  return out;
}

// Keep the fuller git stderr the note above throws away, in a form that is safe
// to put in SQLite, /state and the /ws broadcast. distillGitStderr answers "what
// went wrong"; this answers "what do I do about it" — the failure that motivated
// it printed the workspace's public key and the exact URL to register it in the
// lines ABOVE its `fatal:` verdict, and only the verdict survived.
//
// THIS TEXT IS PARTLY REMOTE-AUTHORED and must never be treated as trusted: every
// `remote:` line is written by whoever controls the far end of the clone (or, on
// an http:// remote, by anyone in the middle). It is bounded, control-stripped and
// rendered as a React text node, so it cannot script the board — but it CAN print
// a plausible-looking instruction, and the drawer offers it with a copy button.
// The UI labels it as remote output for exactly that reason; do not relabel it as
// authoritative advice, and do not feed it to anything that acts on text.
//
// Pipeline order is load-bearing:
//   1. controls stripped BEFORE anything else — an escape sequence must never
//      reach the board's <pre> or an operator's clipboard.
//   2. REDACTION ON THE FULL TEXT, before any truncation. This was originally
//      done after the line tail, which quietly defeated the multi-line PEM rule:
//      SECRET_VALUE_RES anchors on `-----BEGIN … PRIVATE KEY-----` and tolerates a
//      missing END (the byte cap cutting the tail), but a key block longer than
//      GIT_DETAIL_LINES loses its BEGIN marker in the dropped HEAD, and the
//      surviving base64 body lines were emitted verbatim. Redacting first
//      collapses the whole block to one marker. The same applies to any pattern
//      that can span a newline.
//   3. scrubUrlCredentials, then the shape scrubs, then the caller's exact
//      secrets — positional first, because a credentialed URL is the one leak
//      shape the shape lists provably cannot see.
//   4. TAIL, not head: git narrates first ("Cloning into '…'") and the remedy
//      sits immediately above the final verdict.
//   5. the byte cap LAST: capping after redaction can only remove bytes, never
//      re-expose a masked secret by cutting a marker.
// `secrets` is for credential material the caller already holds verbatim (see
// repos.mjs): it is what catches a corporate password or a PAT that matches no
// known shape and appears BARE, e.g. in `remote: HTTP Basic: Access denied for
// user '…'`.
export function gitStderrDetail(text, { secrets = [] } = {}) {
  if (typeof text !== 'string' || !text) return null;
  // The class is C0 + DEL + C1 + the two Unicode line separators, and it
  // deliberately does NOT include TAB. C1 (U+0080-U+009F, U+009B being CSI) and
  // U+2028/2029 are inert inside HTML but travel through a clipboard into a
  // terminal with bracketed paste off, and this text is copyable by design. TAB
  // is kept because server-relayed `remote:` remedy blocks are routinely
  // tab-indented and deleting it joins words ("hint\tuse ssh" → "hintuse ssh") —
  // the board renders the result in a <pre>, where the indentation is meaningful
  // and a literal tab is correct.
  const redacted = redactGitText(
    String(text).replace(/\r/g, '').replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u2028\u2029]/g, ''),
    secrets);
  const lines = redacted.split('\n').map(line => line.replace(/\s+$/g, ''));
  while (lines.length && !lines[0]) lines.shift();
  while (lines.length && !lines[lines.length - 1]) lines.pop();
  if (!lines.length) return null;
  let tail = lines.slice(-GIT_DETAIL_LINES).join('\n');
  // Bytes, not JS code units: a stderr full of CJK/emoji must still respect the
  // 2KB SQLite/snapshot budget, which is also the ONLY bound on this field — a
  // spawns row is never pruned by age, and every mutation re-stringifies the
  // whole snapshot to every /ws peer under a per-peer buffer cap that terminates
  // slow ones. Start from the byte tail, drop a partial leading UTF-8 character
  // (U+FFFD), then correct any replacement expansion.
  const bytes = Buffer.from(tail);
  if (bytes.length > GIT_DETAIL_MAX) {
    tail = bytes.subarray(bytes.length - GIT_DETAIL_MAX).toString('utf8').replace(/^�+/, '');
    while (Buffer.byteLength(tail) > GIT_DETAIL_MAX) tail = tail.slice(1);
  }
  return tail || null;
}

// Resolve the repository's primary integration ref, built on execFileP above.
// Prefer origin/HEAD, then conventional remote main/master, and only fall back
// to a local branch when the repo has no matching remote-tracking ref (a repo
// with no remote) — the caller flags that as local-only. Shared by the worktree
// inspector and repo-mode spawns so the base is computed exactly one way.
export async function baseBranch(worktree) {
  const head = await execFileP('git', ['-C', worktree, 'symbolic-ref', '--short', 'refs/remotes/origin/HEAD'], { timeout: 5_000 });
  if (head.ok && head.out.trim()) return { ref: head.out.trim(), local: false };
  for (const name of ['main', 'master']) {
    const remote = await execFileP('git', ['-C', worktree, 'show-ref', '--verify', '--quiet', `refs/remotes/origin/${name}`], { timeout: 5_000 });
    if (remote.ok) return { ref: `origin/${name}`, local: false };
  }
  for (const name of ['main', 'master']) {
    const local = await execFileP('git', ['-C', worktree, 'show-ref', '--verify', '--quiet', `refs/heads/${name}`], { timeout: 5_000 });
    if (local.ok) return { ref: name, local: true };
  }
  return null;
}
