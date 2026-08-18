// run-nonce.ts — ONE run nonce per CLI PROCESS, shared by every hook that
// process fires.
//
// WHY THIS EXISTS. SessionEnd is an async hook while SessionStart is
// synchronous, so a `claude --resume` (a NEW process reusing the SAME session
// id) can register and go live BEFORE the previous process's SessionEnd lands.
// The daemon refuses to tombstone on a SessionEnd whose nonce is not the card's
// active run, which is what stops a delayed end from killing a live resumed
// session. That guard is only as good as the nonce's identity.
//
// THE BUG THIS FIXES. The nonce used to be keyed on `process.ppid`, on the
// assumption that the hook's parent IS the CLI. It is not: Claude Code runs
// each hook through a fresh intermediate shell, so every hook invocation sees a
// DIFFERENT parent pid, minted a brand-new nonce, and wrote another
// `run-<pid>` file. Measured on one real session: 510 nonce files, 510 distinct
// values, none of them equal to the card's recorded run.
//
// The consequence was systematic rather than occasional. SessionStart stored
// nonce A; the SessionEnd that followed minted B; B !== A, so EVERY tagged
// SessionEnd looked like it came from a dead previous run and was discarded.
// Hook-only sessions therefore never tombstoned on exit (they lingered until
// the silence sweep hours later), an armed move-to-tmux could never fire (the
// arm is consumed further down the same function the guard returns from), and
// HOME accumulated one stray file per hook.
//
// THE KEY. Measured Claude Code 2.1.206 does NOT export CLAUDE_PID. Fleet
// Deck's fixed `/bin/sh` hook launcher is directly parented by Claude, so it
// overwrites and exports CLAUDE_PID=$PPID before starting Bun. SessionStart's
// compatibility probe authenticates that exact process executable, version,
// and generation. The derived identity is stable for every hook of one CLI
// process and DIFFERENT for `--resume`, which is a new process and must get a
// new nonce or the guard above stops working.
//
// Fallback order, most to least trustworthy:
//   1. CLAUDE_PID           — the launcher's derived CLI pid, what we want
//   2. the nearest `claude` ancestor via /proc (Linux)  — same answer, derived
//   3. process.ppid         — the historical behaviour; wrong when a shell sits
//                             in between, but never worse than before
//
// Every failure path returns null, which leaves the event UNTAGGED — the
// daemon's historical unconditional-tombstone path. Fail open; never break the
// session.

import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { errCode } from './errors.ts';

// Type predicate so `walked` (number | null) narrows to number at the call
// sites. `typeof v === 'number'` is redundant at runtime with Number.isInteger
// for the numbers every caller passes, but it lets the predicate accept unknown.
const isPid = (v: unknown): v is number => typeof v === 'number' && Number.isInteger(v) && v > 0;

/** The nearest ancestor that is the Claude CLI, read from /proc (Linux only). */
function claudeAncestor(startPid: number): number | null {
  let pid = startPid;
  for (let hops = 0; hops < 12 && isPid(pid) && pid > 1; hops += 1) {
    let comm: string;
    try {
      comm = fs.readFileSync(`/proc/${pid}/comm`, 'utf8').trim();
    } catch {
      return null;
    }
    if (comm === 'claude') return pid;
    // stat field 4 is ppid; the comm field (2) may contain spaces or parens, so
    // parse from the LAST ')' rather than splitting the whole line.
    let stat: string;
    try {
      stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    } catch {
      return null;
    }
    const tail = stat
      .slice(stat.lastIndexOf(')') + 1)
      .trim()
      .split(/\s+/);
    const parent = Number(tail[1]);
    if (!isPid(parent)) return null;
    pid = parent;
  }
  return null;
}

interface RunKey {
  key: number;
  source: 'CLAUDE_PID' | 'proc-ancestor' | 'ppid';
}

/**
 * The identity the nonce file is keyed on. Exported for tests and diagnostics.
 */
export function runKey(env: NodeJS.ProcessEnv = process.env, ppid: number = process.ppid): RunKey {
  const declared = Number(env['CLAUDE_PID']);
  if (isPid(declared)) return { key: declared, source: 'CLAUDE_PID' };
  const walked = process.platform === 'linux' ? claudeAncestor(ppid) : null;
  if (isPid(walked)) return { key: walked, source: 'proc-ancestor' };
  return { key: ppid, source: 'ppid' };
}

/** Path of the nonce file for a given key — one per CLI process. */
export const runFileFor = (home: string, key: number): string => path.join(home, `run-${key}`);

/**
 * Read this CLI process's nonce, minting and persisting it on first use.
 * Returns null when HOME is unusable — the caller then leaves the event
 * untagged rather than failing.
 */
export function runNonce(
  home: string,
  env: NodeJS.ProcessEnv = process.env,
  ppid: number = process.ppid,
): string | null {
  if (!home) return null;
  try {
    const file = runFileFor(home, runKey(env, ppid).key);
    try {
      const existing = fs.readFileSync(file, 'utf8').trim();
      if (existing) return existing;
    } catch {
      /* first hook of this CLI process */
    }
    const minted = randomUUID();
    // 0600 like the rest of HOME's state (token, log, db).
    fs.writeFileSync(file, minted, { mode: 0o600 });
    return minted;
  } catch {
    return null;
  }
}

/**
 * Remove nonce files whose CLI process is gone. One file per CLI process is
 * small, but nothing ever cleaned them up: a long-lived HOME accumulates one
 * per session forever (and, before the keying fix, one per HOOK). A file is
 * only removed when its pid is provably dead AND it is older than `minAgeMs`,
 * so a live CLI's nonce is never pulled out from under it and a just-minted
 * file cannot lose a race with the process that is about to use it.
 * Returns how many were removed.
 */
export function pruneRunNonces(
  home: string,
  { minAgeMs = 3_600_000, now = Date.now() }: { minAgeMs?: number; now?: number } = {},
): number {
  if (!home) return 0;
  let removed = 0;
  let names: string[];
  try {
    names = fs.readdirSync(home);
  } catch {
    return 0;
  }
  for (const name of names) {
    const m = /^run-(\d+)$/.exec(name);
    if (!m) continue;
    const pid = Number(m[1]);
    if (!isPid(pid)) continue;
    const file = path.join(home, name);
    try {
      if (now - fs.statSync(file).mtimeMs < minAgeMs) continue;
      // Alive (or not ours to judge — EPERM means SOMETHING holds the pid): keep.
      try {
        process.kill(pid, 0);
        continue;
      } catch (err) {
        if (errCode(err) !== 'ESRCH') continue;
      }
      fs.unlinkSync(file);
      removed += 1;
    } catch {
      /* raced with another prune, or unreadable — leave it */
    }
  }
  return removed;
}
