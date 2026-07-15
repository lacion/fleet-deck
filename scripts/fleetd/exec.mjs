// exec.mjs — the daemon's shared async subprocess primitive.
//
// `execFileP` runs a command by ARGV (never a shell string, so `;`/`$()`/quotes
// in an argument arrive as literal bytes) and resolves a result object instead of
// rejecting: `{ ok: true, out }` on success, `{ ok: false, code, err }` on any
// failure (non-zero exit, timeout, missing binary, or a synchronous throw). Every
// async execFile caller in the daemon funnels through this one shape — worktrees'
// git probes, and the tmux adapter (spawn.mjs), which maps it to its own
// null-or-stdout convention at the boundary.
//
// Two sibling wrappers deliberately do NOT share this and are not moved here:
//   - repo-identity.mjs `git()` is SYNCHRONOUS (execFileSync) on purpose — its
//     caller (derive.mjs) consumes results inline while building SQL, and making
//     it async would thread Promises into session state (see its own comment).
//   - agents-poll.mjs `runOnce()` uses `exec` (a SHELL) because FLEETDECK_AGENTS_CMD
//     is an operator-supplied command STRING; keeping that the lone shell caller,
//     apart from this argv-only primitive, preserves the no-shell security boundary
//     the rest of the daemon relies on.
import { execFile } from 'node:child_process';

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
