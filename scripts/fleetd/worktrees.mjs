// worktrees.mjs — worktree custody: the bounded, real-git-state inspector
// behind GET /api/worktrees and the allow-listed destruction behind
// POST /api/worktrees/remove. Threaded ctx state: q (the spawns allow-list +
// session lookups), tick and onMutate. The subprocess/fs primitives are pure
// helpers.

import fs from 'node:fs';
import path from 'node:path';
import {
  execFileP, mapLimit, chmodWritableWhereOwned, blockedPaths, shellQuote, baseBranch,
} from './helpers.mjs';

export function createWorktrees(ctx) {
  const { q, tick, onMutate } = ctx;

  // ------------------------------------------------------- worktree custody
  // CONTRACT: inspection is deliberately real git state, not remembered
  // spawn metadata. Every subprocess is execFile(cmd, argv): paths, branches,
  // and refs are inert argv values even when fleetd is reachable from the LAN.
  // Four worktrees at a time bounds process pressure while preserving modal
  // latency; independent probes within one worktree run concurrently.
  // (mapLimit itself is a pure helper now — see helpers.mjs.)

  function worktreeRows() {
    const seen = new Set();
    return q.worktreeSpawns.all().filter(row => {
      if (seen.has(row.worktree_path)) return false;
      seen.add(row.worktree_path);
      return true;
    });
  }

  function worktreeShell(row, exists) {
    return {
      path: row.worktree_path,
      exists,
      callsign: row.callsign ?? null,
      session_id: row.session_id ?? null,
      session_alive: row.session_ended_at == null && q.getSession.get(row.session_id) != null,
      spawn_status: row.status ?? null,
      branch: null,
      dirty: null,
      dirty_files: [],
      ahead: null,
      base: null,
      upstream: null,
      unpushed: null,
      merged: null,
      last_commit: null,
      note: null, // why we cannot vouch for it — the board shows this verbatim
      verdict: exists ? 'unknown' : 'gone',
    };
  }

  // The base we measure against is the REMOTE one (origin/main), not the local
  // branch of the same name — and that distinction is the whole feature.
  //
  // A local `main` is a cache, and a stale one lies. Measured against a local
  // main that was ten commits behind, a worktree whose work had ALREADY been
  // merged upstream read as "9 commits that exist nowhere else" — the exact
  // false alarm that pushes a human toward force-deleting, or (worse) toward
  // not trusting the warning the one time it is real. Verified the hard way.
  //
  // Falls back to the local branch only when no remote-tracking ref exists at
  // all (a repo with no remote); the caller flags that as base_is_local so the
  // board can say its knowledge is local-only.
  async function inspectWorktree(row) {
    let exists = false;
    try { exists = fs.existsSync(row.worktree_path); } catch { /* unknown path state stays gone */ }
    const item = worktreeShell(row, exists);
    if (!exists) return item;

    const [branch, status, upstream, log, base] = await Promise.all([
      execFileP('git', ['-C', row.worktree_path, 'rev-parse', '--abbrev-ref', 'HEAD'], { timeout: 5_000 }),
      execFileP('git', ['-C', row.worktree_path, 'status', '--porcelain'], { timeout: 5_000 }),
      execFileP('git', ['-C', row.worktree_path, 'rev-parse', '--abbrev-ref', '@{u}'], { timeout: 5_000 }),
      execFileP('git', ['-C', row.worktree_path, 'log', '-1', '--format=%h%x00%s%x00%ct'], { timeout: 5_000 }),
      baseBranch(row.worktree_path),
    ]);
    // Missing upstream and an empty log are valid repository states. Branch,
    // status, and a resolvable base are the minimum evidence needed to make a
    // destructive verdict; absent any of them, UNKNOWN is the only safe word.
    if (!branch.ok || !status.ok || !base) {
      // The commonest cause, and it has a story: an interrupted removal. git
      // unlinks its worktree admin entry BEFORE it deletes the files, so a
      // removal that dies on an undeletable path (a root-owned directory a
      // container left behind) leaves the directory standing and orphaned —
      // git no longer recognises it, and nothing can be verified about what is
      // inside. Never call that 'safe'.
      item.note = !branch.ok
        ? 'git no longer recognises this directory as a worktree — a previous removal was interrupted. '
          + 'Whatever is inside cannot be checked from here; removal will report exactly what blocks it.'
        : 'git could not read this worktree.';
      return item;
    }

    item.branch = branch.out.trim() || null;
    const lines = status.out.split(/\r?\n/).filter(Boolean);
    item.dirty = lines.length;
    item.dirty_files = lines.slice(0, 10).map(line => line.slice(3).trim());
    item.base = base.ref;
    item.base_is_local = base.local; // no remote to check against — say so
    item.upstream = upstream.ok ? (upstream.out.trim() || null) : null;
    if (log.ok && log.out.trim()) {
      const [sha, subject, at] = log.out.trimEnd().split('\0');
      item.last_commit = { sha, subject, at: Number(at) };
    }

    const [ahead, unpushed, merged] = await Promise.all([
      execFileP('git', ['-C', row.worktree_path, 'rev-list', '--count', `${base.ref}..HEAD`], { timeout: 5_000 }),
      // THE question, and the only one that decides whether deleting this
      // destroys anything: are these commits on ANY remote-tracking ref? Not
      // "ahead of my upstream", not "ahead of my local main" — both of those
      // say yes to work that is already safely merged on the server. `--not
      // --remotes` asks git for commits that exist on no remote we know of.
      // (Knowledge is as of the last fetch; the board says so, and `?fetch=1`
      // refreshes it.)
      execFileP('git', ['-C', row.worktree_path, 'rev-list', '--count', 'HEAD', '--not', '--remotes'], { timeout: 5_000 }),
      execFileP('git', ['-C', row.worktree_path, 'merge-base', '--is-ancestor', 'HEAD', base.ref], { timeout: 5_000 }),
    ]);
    if (!ahead.ok || !unpushed.ok || (!merged.ok && merged.code !== 1)) return item;
    item.ahead = Number(ahead.out.trim());
    item.unpushed = Number(unpushed.out.trim());
    item.merged = merged.ok;
    // A repo with no remote at all cannot prove anything lives elsewhere, so
    // "merged into the local base" is the strongest safety it can offer.
    if (base.local) item.unpushed = item.merged ? 0 : item.ahead;
    item.verdict = item.dirty > 0 || item.unpushed > 0 ? 'has-work' : 'safe';
    return item;
  }

  async function worktrees() {
    return { ok: true, worktrees: await mapLimit(worktreeRows(), 4, inspectWorktree) };
  }

  // CONTRACT: removal reuses the inspector's daemon verdict, but the DB
  // allow-list and liveness gates come first. UNKNOWN also requires force:
  // inability to prove safety must never become permission to destroy data.
  // (chmodWritableWhereOwned / blockedPaths / shellQuote are pure helpers now —
  // see helpers.mjs; they carry their own contract comments there.)
  async function removeWorktree(body = {}) {
    if (typeof body?.path !== 'string') {
      return { status: 400, body: { ok: false, reason: 'not a fleet worktree' } };
    }
    const rows = q.worktreeSpawns.all().filter(row => row.worktree_path === body.path);
    const row = rows[0];
    if (!row) return { status: 400, body: { ok: false, reason: 'not a fleet worktree' } };
    const alive = rows.some(candidate => candidate.session_ended_at == null && q.getSession.get(candidate.session_id));
    if (alive) return { status: 409, body: { ok: false, reason: 'session is still alive' } };

    const state = await inspectWorktree(row);
    if ((state.verdict === 'has-work' || state.verdict === 'unknown') && body.force !== true) {
      return {
        status: 409,
        body: {
          ok: false,
          reason: state.verdict === 'has-work' ? 'worktree has uncommitted or unpushed work' : 'worktree safety is unknown',
          verdict: state.verdict,
          dirty: state.dirty,
          unpushed: state.unpushed,
        },
      };
    }

    const repoResult = await execFileP('git', ['-C', row.cwd, 'rev-parse', '--show-toplevel'], { timeout: 5_000 });
    if (!repoResult.ok) return { status: 409, body: { ok: false, reason: 'main repository unavailable' } };
    const repo = repoResult.out.trim();
    if (state.exists) {
      // "Permission denied" is a diagnosis, not an answer. A worktree is a
      // WORKING directory: build tooling leaves read-only files in it, and a
      // container run from inside it leaves paths owned by ROOT (the real case
      // that found this: a Zitadel init wrote secrets/ as root:root). git then
      // refuses the whole removal with one opaque line and no way forward.
      //
      // Clear what we legitimately can FIRST — a failed `worktree remove` can
      // leave the tree half-dismantled (git unlinks its .git file before it
      // hits the undeletable file), and the retry then fails with a *different*
      // and even less useful error.
      chmodWritableWhereOwned(row.worktree_path); // sync: only fs.chmodSync/readdirSync

      const args = ['-C', repo, 'worktree', 'remove'];
      if (body.force === true) args.push('--force');
      args.push(row.worktree_path);
      const removed = await execFileP('git', args, { timeout: 30_000 });

      if (!removed.ok) {
        // Anything left belongs to somebody else. This daemon runs as you and
        // does NOT escalate to root — so it names the paths and their owner,
        // hands over the exact command, and stops.
        const blocked = blockedPaths(row.worktree_path);
        if (blocked.length) {
          return {
            status: 409,
            body: {
              ok: false,
              reason: `blocked by ${blocked.length} path(s) this daemon may not delete — owned by `
                + `${[...new Set(blocked.map(b => b.owner))].join(', ')}. Fleet Deck runs as you and never escalates to root.`,
              blocked_paths: blocked.map(b => b.path),
              blocked_owner: blocked[0].owner,
              fix_command: `sudo rm -rf ${blocked.map(b => shellQuote(b.path)).join(' ')} `
                + `&& git -C ${shellQuote(repo)} worktree prune`,
            },
          };
        }
        // Nothing FOREIGN in the way — but "not foreign" is not the same as
        // "safe to erase". H-R1: blockedPaths() only ever reports paths owned by
        // ANOTHER user; your own uncommitted/untracked files are invisible to
        // it. git refuses a DIRTY worktree, and reaching here on a request that
        // never set force (verdict was 'safe' at inspect, or a TOCTOU write
        // landed after it) would mean rmSync(force:true) silently destroys that
        // work. So before we take the directory down ourselves, re-read the
        // working tree: only rm when the human forced it, OR a fresh
        // `git status --porcelain` proves the tree is clean (git was refusing
        // for a benign admin reason — a half-removed tree it no longer
        // recognises, where `git status` itself fails). A tree with real
        // uncommitted changes and no force is refused, loudly.
        if (body.force !== true) {
          const porcelain = await execFileP('git', ['-C', row.worktree_path, 'status', '--porcelain'], { timeout: 5_000 });
          if (porcelain.ok && porcelain.out.trim() !== '') {
            return {
              status: 409,
              body: {
                ok: false,
                reason: 'git refused to remove this worktree and it still has uncommitted changes — pass force to delete',
                verdict: 'has-work',
                dirty: porcelain.out.split(/\r?\n/).filter(Boolean).length,
              },
            };
          }
        }
        try {
          fs.rmSync(row.worktree_path, { recursive: true, force: true });
        } catch (err) {
          return { status: 409, body: { ok: false, reason: `could not remove worktree: ${err.code || err.message}` } };
        }
        const pruned = await execFileP('git', ['-C', repo, 'worktree', 'prune'], { timeout: 30_000 });
        if (!pruned.ok) return { status: 409, body: { ok: false, reason: `git worktree prune failed: ${pruned.err}`.slice(0, 300) } };
      }
    } else {
      const pruned = await execFileP('git', ['-C', repo, 'worktree', 'prune'], { timeout: 30_000 });
      if (!pruned.ok) return { status: 409, body: { ok: false, reason: `git worktree prune failed: ${pruned.err}`.slice(0, 300) } };
    }

    let branch_deleted = false;
    const branch = state.branch ?? q.getSession.get(row.session_id)?.branch ?? null;
    if (body.delete_branch === true && branch) {
      const deleted = await execFileP('git', ['-C', repo, 'branch', '-D', branch], { timeout: 30_000 });
      branch_deleted = deleted.ok;
    }

    const sessionIds = [...new Set(rows.map(candidate => candidate.session_id).filter(Boolean))];
    const spawnsPurged = Number(q.deleteWorktreeSpawns.run(row.worktree_path).changes);
    let sessionsPurged = 0;
    for (const sessionId of sessionIds) sessionsPurged += Number(q.deleteEndedSession.run(sessionId).changes);
    const rows_purged = spawnsPurged + sessionsPurged;
    tick(`⌫ removed worktree ${row.worktree_path}${branch_deleted ? ` and branch ${branch}` : ''}`);
    onMutate();
    return { status: 200, body: { ok: true, removed: true, branch_deleted, rows_purged, path: row.worktree_path } };
  }

  return { worktrees, removeWorktree };
}
