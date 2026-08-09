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
import { redactDiagnosticText, scrubUrlCredentials } from './payload-capture.ts';

// Grace between the timeout's SIGTERM and the SIGKILL escalation below.
// 1s is enough for tmux/git/agents-cli to exit cleanly on TERM, and bounds the
// worst-case overshoot of any advertised deadline to timeout + 1s.
const KILL_GRACE_MS = 1_000;

export function execFileP(cmd, args, { timeout = 30_000, env } = {}) {
  return new Promise((resolve) => {
    try {
      let child;
      let done = false;
      let killTimer = null;
      // Settle EXACTLY once, on whatever happens first — exit, error, or our
      // own wall-clock deadline. execFile's `timeout` only SIGTERMs the child;
      // the CALLBACK still waits for the pipes to close, so a child that
      // ignores TERM (or that leaves a grandchild holding an inherited
      // stdout/stderr pipe open) would keep the callback — and this promise —
      // pending forever, silently wedging agents-poll's whole scheduling loop.
      // The deadline timer therefore OWNS settlement: it kills the child, kills
      // the attempt, and resolves regardless of what the child does later.
      const settle = (fn) => {
        if (done) return;
        done = true;
        if (killTimer) clearTimeout(killTimer);
        resolve(fn());
      };
      const deadline = setTimeout(() => {
        // Settle FIRST: settle() clears any armed killTimer, and only this
        // timeout path can leave a child alive needing a KILL (every other
        // settle means execFile's callback ran, i.e. the child already
        // exited), so the escalation below is armed AFTER settlement on
        // purpose.
        settle(() => ({ ok: false, code: 'ETIMEDOUT', err: `timed out after ${timeout}ms` }));
        if (child && !child.killed) {
          try { child.kill('SIGTERM'); } catch { /* already gone */ }
          // Escalate: a process may survive SIGTERM indefinitely (and on
          // platforms where kill() is only advisory, so may SIGKILL — the
          // resolve above is what actually bounds the attempt). child.killed
          // is already true from the SIGTERM just sent, so re-check aliveness
          // with signal 0 instead.
          killTimer = setTimeout(() => {
            let alive = true;
            try { process.kill(child.pid, 0); } catch { alive = false; }
            if (alive) { try { child.kill('SIGKILL'); } catch { /* already gone */ } }
          }, KILL_GRACE_MS);
          killTimer.unref?.();
        }
      }, timeout);
      // The deadline (and the escalation grace below) exist only to bound THIS
      // attempt; they must not keep the daemon's event loop alive when they are
      // the only handles left.
      deadline.unref?.();
      child = execFile(cmd, args, {
        // 0 disables execFile's own SIGTERM-only timeout: the deadline above is
        // strictly stronger (same TERM, then KILL, then settle), and a second
        // timer race inside execFile would change the shape of the callback
        // error without changing settlement.
        timeout: 0,
        windowsHide: true,
        // When an env is supplied it is MERGED over the daemon's own (never
        // replacing it), so PATH and the rest survive while a caller adds e.g.
        // GIT_TERMINAL_PROMPT=0 to make an unauthenticated clone fail fast
        // instead of hanging on a credential prompt.
        ...(env ? { env: { ...process.env, ...env } } : {}),
      }, (err, stdout, stderr) => {
        clearTimeout(deadline);
        if (err) return settle(() => ({ ok: false, code: err.code, err: String(stderr || err.message || err).trim() }));
        settle(() => ({ ok: true, out: stdout }));
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
// (a credentialed URL is invisible to a shape list), then the shape list, then
// the caller's exact needles. The forge/API shapes (glpat/AIza/sk-/hf_/dop_v1_)
// this pass once applied from a LOCAL extra list now live in payload-capture's
// shared SECRET_VALUE_RES, so redactDiagnosticText covers them and the two
// halves of this file can never drift apart again. Every step is idempotent, so
// composing this with gitStderrDetail — which runs it again over its own
// input — is safe by design.
export function redactGitText(text, secrets = []) {
  let out = redactDiagnosticText(scrubUrlCredentials(text));
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
// with no remote) — the caller flags that as local-only. For the local fallback
// the primary branch is DERIVED, not guessed: `git worktree list --porcelain`
// lists the main worktree first, and its `branch refs/heads/<name>` entry names
// the integration branch whatever it is called — trunk, develop, any custom
// default — so a no-remote repo on a non-conventional name still resolves
// instead of losing branch/dirty/ahead evidence. Conventional main/master stay
// as the last resort for a main worktree in detached HEAD. Shared by the
// worktree inspector and repo-mode spawns so the base is computed exactly one
// way.
export async function baseBranch(worktree) {
  const head = await execFileP('git', ['-C', worktree, 'symbolic-ref', '--short', 'refs/remotes/origin/HEAD'], { timeout: 5_000 });
  if (head.ok && head.out.trim()) return { ref: head.out.trim(), local: false };
  for (const name of ['main', 'master']) {
    const remote = await execFileP('git', ['-C', worktree, 'show-ref', '--verify', '--quiet', `refs/remotes/origin/${name}`], { timeout: 5_000 });
    if (remote.ok) return { ref: `origin/${name}`, local: false };
  }
  const trees = await execFileP('git', ['-C', worktree, 'worktree', 'list', '--porcelain'], { timeout: 5_000 });
  if (trees.ok) {
    // Entries are blank-line-separated; the main worktree is always first.
    // A bare or detached main worktree has no `branch` line and falls through.
    const first = trees.out.split('\n\n', 1)[0];
    const branch = /^branch refs\/heads\/(.+)$/m.exec(first);
    if (branch && branch[1].trim()) return { ref: branch[1].trim(), local: true };
  }
  for (const name of ['main', 'master']) {
    const local = await execFileP('git', ['-C', worktree, 'show-ref', '--verify', '--quiet', `refs/heads/${name}`], { timeout: 5_000 });
    if (local.ok) return { ref: name, local: true };
  }
  return null;
}
