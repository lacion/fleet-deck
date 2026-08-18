// worktrees.ts — worktree custody: the bounded, real-git-state inspector
// behind GET /api/worktrees and the allow-listed destruction behind
// POST /api/worktrees/remove. Threaded ctx state: q (the spawns allow-list +
// session lookups), tick and onMutate. The subprocess/fs primitives are pure
// helpers.

import fs from 'node:fs';
import path from 'node:path';
import {
  mapLimit,
  chmodWritableWhereOwned,
  blockedPaths,
  shellQuote,
  canonicalPathKey,
} from './helpers.ts';
import { execFileP, baseBranch } from './exec.ts';
// A local `git worktree prune` should never print a credential — but its stderr
// goes straight into an HTTP body below, and repos.mjs now asserts that no git
// stderr reaches a note or a response unscrubbed. Uniformity is the point: a
// control applied to some git call sites and not others is read by the next
// reader as an intentional posture rather than the gap it actually is.
import { scrubUrlCredentials } from './payload-capture.ts';
import type { Statements, WorktreeSpawnRow } from './statements.ts';
import type { SqliteHandle } from './sqlite.ts';

// The last-commit tuple the board renders when a worktree has any history.
interface WorktreeLastCommit {
  sha: string;
  subject: string;
  at: number;
}

// One worktree inspection record. Every "info" field below the identity block
// starts null/empty in worktreeShell and is filled only once its evidence is
// read; base_is_local is set solely on the no-remote fallback path, so it is
// optional rather than nullable.
interface WorktreeItem {
  path: string | null;
  exists: boolean;
  callsign: string | null;
  session_id: string;
  session_alive: boolean;
  spawn_status: string | null;
  branch: string | null;
  dirty: number | null;
  dirty_files: string[];
  ahead: number | null;
  base: string | null;
  base_is_local?: boolean;
  upstream: string | null;
  unpushed: number | null;
  merged: boolean | null;
  last_commit: WorktreeLastCommit | null;
  note: string | null; // why we cannot vouch for it — the board shows this verbatim
  verdict: string;
}

// refreshRemoteKnowledge's outcome: ok, or ok:false carrying the git stderr
// that explains why the remote cache could not be refreshed.
type RefreshResult = { ok: true } | { ok: false; err: string };

// The parsed POST /api/worktrees/remove body — untrusted JSON, so every field
// stays unknown until guarded (path must be a string; force/delete_branch are
// compared strictly against `true`).
interface RemoveBody {
  path?: unknown;
  force?: unknown;
  delete_branch?: unknown;
}

// Every removal path returns an HTTP status plus a JSON body; the body's shape
// varies per outcome, so it is a plain bag the http layer serializes verbatim.
interface RemoveResult {
  status: number;
  body: Record<string, unknown>;
}

interface WorktreesCtx {
  q: Statements['q'];
  // The atomic purge wrapper needs the raw handle; direct-drive tests omit it
  // and run the same statements outside an explicit transaction.
  db?: SqliteHandle;
  tick: (msg: string) => void;
  onMutate: () => void;
  // Per-path removal/revive mutex + custody lease; derive wires both per-core,
  // and tests driving this module directly opt in (so both are optional).
  acquireWorktreePathLock?: (key: string) => Promise<() => void>;
  claimWorktreeCustody?: (path: string, reason: string) => (() => void) | null | undefined;
}

// Pure path canonicalization (same rule as repo-identity.mjs canon()).
function canonical(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}

// Canonicalized `git rev-parse --git-common-dir` for a directory — the one
// value a repository and ALL of its worktrees share (repo-identity's repo_id
// rule). null when git cannot read the directory as a repository at all.
async function gitCommonDir(dir: string): Promise<string | null> {
  const result = await execFileP('git', ['-C', dir, 'rev-parse', '--git-common-dir'], {
    timeout: 5_000,
  });
  if (!result.ok) return null;
  const raw = result.out.trim();
  if (!raw) return null;
  // May be relative (usually ".git"); resolve against the directory itself.
  return canonical(path.isAbsolute(raw) ? raw : path.resolve(dir, raw));
}

// BUG-059 (data-loss): `repo` below is derived from a REMEMBERED spawn cwd, and a
// remembered path proves nothing — the directory can have been deleted and
// recreated as a DIFFERENT repository since the spawn, while the fleet
// worktree still belongs to the original one. Every repo-scoped argv built
// from it (`worktree remove`/`prune`, and above all `branch -D`, which erases
// unique commits) would then be aimed at an unrelated repository. So before
// any destructive step, prove the candidate repository OWNS the target
// worktree with a witness that does not consult the remembered cwd:
//  - worktree on disk and git-readable: canonical common-dirs must be equal.
//    (This still works for a half-removed tree — rev-parse follows the
//    worktree's .git link without consulting the repo's admin registry.)
//  - worktree already gone from disk: the repo's OWN worktree registry names
//    the path only if the worktree was created from this repository.
async function repoOwnsWorktree(
  repo: string,
  worktreePath: string,
  worktreeExists: boolean,
): Promise<boolean> {
  if (worktreeExists) {
    const [repoCommon, worktreeCommon] = await Promise.all([
      gitCommonDir(repo),
      gitCommonDir(worktreePath),
    ]);
    return repoCommon != null && worktreeCommon != null && repoCommon === worktreeCommon;
  }
  const list = await execFileP('git', ['-C', repo, 'worktree', 'list', '--porcelain'], {
    timeout: 5_000,
  });
  if (!list.ok) return false;
  const target = canonical(worktreePath);
  return list.out
    .split('\n')
    .some((line) => line.startsWith('worktree ') && canonical(line.slice(9).trim()) === target);
}

export function createWorktrees(ctx: WorktreesCtx) {
  const { q, db, tick, onMutate, acquireWorktreePathLock, claimWorktreeCustody } = ctx;

  // ------------------------------------------------------- worktree custody
  // CONTRACT: inspection is deliberately real git state, not remembered
  // spawn metadata. Every subprocess is execFile(cmd, argv): paths, branches,
  // and refs are inert argv values even when fleetd is reachable from the LAN.
  // Four worktrees at a time bounds process pressure while preserving modal
  // latency; independent probes within one worktree run concurrently.
  // (mapLimit itself is a pure helper now — see helpers.ts.)

  function worktreeRows(): WorktreeSpawnRow[] {
    const seen = new Set<string | null>();
    return q.worktreeSpawns.all().filter((row) => {
      if (seen.has(row.worktree_path)) return false;
      seen.add(row.worktree_path);
      return true;
    });
  }

  function worktreeShell(row: WorktreeSpawnRow, exists: boolean): WorktreeItem {
    return {
      path: row.worktree_path,
      exists,
      callsign: row.callsign,
      session_id: row.session_id,
      session_alive: row.session_ended_at == null && q.getSession.get(row.session_id) != null,
      spawn_status: row.status,
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
  //
  // Remote-tracking refs are a CACHE of the remote's state, and a stale cache
  // lies in the dangerous direction: a commit that was pushed, fetched, and
  // then force-reset away on the server still exists on the local
  // refs/remotes/*, so `rev-list HEAD --not --remotes` would certify it as
  // safely copied while the alleged remote copy is gone. Before any verdict a
  // human can destroy on, refresh that cache from the actual remote —
  // prompting disabled so an unreachable or auth-gated remote fails fast
  // instead of hanging the board — and prune the refs the remote no longer
  // has. A refresh that cannot complete means the cache is of unknown vintage,
  // and UNKNOWN is then the only honest verdict: never 'safe'.
  async function refreshRemoteKnowledge(worktreePath: string): Promise<RefreshResult> {
    const remotes = await execFileP('git', ['-C', worktreePath, 'remote'], { timeout: 5_000 });
    if (!remotes.ok) return { ok: false, err: remotes.err };
    const names = remotes.out
      .split(/\r?\n/)
      .map((name) => name.trim())
      .filter(Boolean);
    if (!names.length) return { ok: false, err: 'no remote is configured to refresh against' };
    for (const name of names) {
      const fetched = await execFileP('git', ['-C', worktreePath, 'fetch', '--prune', name], {
        timeout: 30_000,
        env: { GIT_TERMINAL_PROMPT: '0' },
      });
      if (!fetched.ok) return { ok: false, err: fetched.err };
    }
    return { ok: true };
  }

  async function inspectWorktree(row: WorktreeSpawnRow): Promise<WorktreeItem> {
    // worktreeSpawns' SQL drops worktree_path NULL rows, so a null here is
    // dead-defensive — treat it exactly as an absent tree (existsSync failing).
    const worktreePath = row.worktree_path;
    let exists = false;
    if (worktreePath != null) {
      try {
        exists = fs.existsSync(worktreePath);
      } catch {
        /* unknown path state stays gone */
      }
    }
    const item = worktreeShell(row, exists);
    if (worktreePath == null || !exists) return item;

    const [branch, status, upstream, log, base] = await Promise.all([
      execFileP('git', ['-C', worktreePath, 'rev-parse', '--abbrev-ref', 'HEAD'], {
        timeout: 5_000,
      }),
      execFileP('git', ['-C', worktreePath, 'status', '--porcelain'], { timeout: 5_000 }),
      execFileP('git', ['-C', worktreePath, 'rev-parse', '--abbrev-ref', '@{u}'], {
        timeout: 5_000,
      }),
      execFileP('git', ['-C', worktreePath, 'log', '-1', '--format=%h%x00%s%x00%ct'], {
        timeout: 5_000,
      }),
      baseBranch(worktreePath),
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
        ? 'git no longer recognises this directory as a worktree — a previous removal was interrupted. ' +
          'Whatever is inside cannot be checked from here; removal will report exactly what blocks it.'
        : 'git could not read this worktree.';
      return item;
    }

    item.branch = branch.out.trim() || null;
    const lines = status.out.split(/\r?\n/).filter(Boolean);
    item.dirty = lines.length;
    item.dirty_files = lines.slice(0, 10).map((line) => line.slice(3).trim());
    item.base = base.ref;
    item.base_is_local = base.local; // no remote to check against — say so
    item.upstream = upstream.ok ? upstream.out.trim() || null : null;
    if (log.ok && log.out.trim()) {
      const [sha = '', subject = '', at] = log.out.trimEnd().split('\0');
      item.last_commit = { sha, subject, at: Number(at) };
    }

    // Refresh the remote knowledge BEFORE the verdict is computed, every time:
    // without a fresh fetch+prune the reachability answers below are as stale
    // as the last fetch, and a force-reset on the server turns a vanished
    // remote copy into a cached 'safe' that destroys the last local branch
    // holding the commits. An unreachable remote is not proof of danger — but
    // it is proof of ignorance, so the verdict stays UNKNOWN (which still
    // requires force to remove) rather than a 'safe' we cannot stand behind.
    if (!base.local) {
      const refreshed = await refreshRemoteKnowledge(worktreePath);
      if (!refreshed.ok) {
        item.note =
          'could not refresh the remote before judging this worktree — ' +
          'its remote knowledge may be stale, so nothing here can be certified as safe. ' +
          'Check the remote and refresh again.';
        return item;
      }
    }

    const [ahead, unpushed, merged] = await Promise.all([
      execFileP('git', ['-C', worktreePath, 'rev-list', '--count', `${base.ref}..HEAD`], {
        timeout: 5_000,
      }),
      // THE question, and the only one that decides whether deleting this
      // destroys anything: are these commits on ANY remote-tracking ref? Not
      // "ahead of my upstream", not "ahead of my local main" — both of those
      // say yes to work that is already safely merged on the server. `--not
      // --remotes` asks git for commits that exist on no remote we know of.
      // The refs were just fetched and pruned above, so "we know of" is as of
      // THIS inspection, not the last time somebody happened to fetch.
      execFileP('git', ['-C', worktreePath, 'rev-list', '--count', 'HEAD', '--not', '--remotes'], {
        timeout: 5_000,
      }),
      execFileP('git', ['-C', worktreePath, 'merge-base', '--is-ancestor', 'HEAD', base.ref], {
        timeout: 5_000,
      }),
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
  // see helpers.ts; they carry their own contract comments there.)
  // A worktree_path counts as LIVE — and must never be removed or purged — when
  // any of its spawn rows is launching or running. A revive (/api/spawn/:id/revive)
  // inserts its durable row 'provisioning' → 'spawning' SYNCHRONOUSLY, but leaves
  // sessions.ended_at SET until the resumed child's async SessionStart hook lands
  // seconds later — so a liveness test keyed on session_ended_at is BLIND to a
  // revive in flight (the exact revive-during-removal race). Spawn STATUS is the
  // synchronous signal the revive does set; a not-yet-ended session is the other.
  // 'stalled' is deliberately NOT here: it is set later by the watchdog, never by
  // a revive, and the ended-session branch already governs it as before.
  //
  // Two claim sets, because worktreeSpawns alone is BLIND to cwd-only rows
  // (its WHERE clause drops worktree_path NULL): a live shell spawned INTO a
  // fleet worktree, or an adopted Claude resumed in one, carries
  // `worktree_path NULL, cwd = <that tree>` and would be deleted underneath its
  // running process. liveWorktreeClaims covers them: any launching/live spawn
  // whose EFFECTIVE directory — `worktree_path ?? cwd`, the same coalesce the
  // launch paths use — IS the target or lies INSIDE it (a shell cd'd into a
  // subdirectory still loses its ground when the tree goes) blocks removal.
  const LAUNCHING_OR_LIVE = new Set(['provisioning', 'spawning', 'live']);
  // Lexical containment on normalised absolute paths; the separator anchor
  // keeps '/repo/tree' from claiming '/repo/tree-evil'. Rows are daemon-written
  // absolute paths, and the one tree a symlinked /tmp would confuse this way is
  // also protected by the first claim set (its owning row IS worktree-keyed).
  function claimsPath(candidate: WorktreeSpawnRow, target: string): boolean {
    const effective = candidate.worktree_path ?? candidate.cwd;
    if (typeof effective !== 'string' || !effective) return false;
    const dir = path.resolve(effective);
    return dir === target || dir.startsWith(target + path.sep);
  }
  function worktreePathIsLive(worktreePath: string): boolean {
    const target = path.resolve(worktreePath);
    return (
      q.worktreeSpawns
        .all()
        .some(
          (candidate) =>
            candidate.worktree_path === worktreePath &&
            (LAUNCHING_OR_LIVE.has(candidate.status) ||
              (candidate.session_ended_at == null &&
                q.getSession.get(candidate.session_id) != null)),
        ) || q.liveWorktreeClaims.all().some((candidate) => claimsPath(candidate, target))
    );
  }

  // The exact OID refs/heads/<branch> points at in <repo> right now, or null
  // when it cannot be read. This is the compare-and-swap witness for branch
  // deletion: a safety verdict is measured against ONE tip, so deleting the
  // ref must prove the tip has not moved since the measurement.
  async function branchTipOid(repo: string, branch: string): Promise<string | null> {
    const tip = await execFileP(
      'git',
      ['-C', repo, 'rev-parse', '--verify', `refs/heads/${branch}`],
      { timeout: 5_000 },
    );
    return tip.ok ? tip.out.trim() : null;
  }

  async function pruneWorktreeMetadata(repo: string): Promise<RemoveResult | null> {
    const pruned = await execFileP('git', ['-C', repo, 'worktree', 'prune'], {
      timeout: 30_000,
    });
    if (pruned.ok) return null;
    return {
      status: 409,
      body: {
        ok: false,
        reason: `git worktree prune failed: ${scrubUrlCredentials(pruned.err)}`.slice(0, 300),
      },
    };
  }

  async function removeWorktree(body: RemoveBody | null = {}): Promise<RemoveResult> {
    if (typeof body?.path !== 'string') {
      return { status: 400, body: { ok: false, reason: 'not a fleet worktree' } };
    }
    // BUG-060: hold this path's canonical claim for the WHOLE removal — the
    // liveness gates, the awaited inspect/git-remove/prune/branch ops, and the
    // DB purge. The pre-remove recheck below was never enough on its own: a
    // revive could pass its own cwd/transcript validation and insert+launch
    // AFTER that gate but while `git worktree remove` was still deleting the
    // checkout, reporting success into a pane whose cwd had just disappeared.
    // Every launch/revive into this directory acquires the same claim first,
    // so it either queues behind the removal (and then fails its own
    // "cwd no longer exists" validation) or lands before it (and the liveness
    // gates then refuse the removal). Released on every exit path.
    const worktreePath = body.path;
    const releasePath = acquireWorktreePathLock
      ? await acquireWorktreePathLock(canonicalPathKey(worktreePath))
      : () => {
          /* no path lock wired (direct-drive tests) */
        };
    try {
      const rows = q.worktreeSpawns.all().filter((row) => row.worktree_path === worktreePath);
      const row = rows[0];
      if (!row) return { status: 400, body: { ok: false, reason: 'not a fleet worktree' } };
      if (worktreePathIsLive(worktreePath))
        return { status: 409, body: { ok: false, reason: 'session is still alive' } };

      const state = await inspectWorktree(row);
      // The OID the verdict below was measured against. Worktree HEAD == its
      // branch ref while the tree exists, so inspecting the tree IS inspecting
      // the branch tip — capture it HERE, at inspection time. (Reading it later,
      // beside the delete, would bless a commit that landed mid-request: exactly
      // the stale-verdict loss this exists to prevent.)
      const inspected_tip =
        state.exists && state.branch ? await branchTipOid(worktreePath, state.branch) : null;
      if ((state.verdict === 'has-work' || state.verdict === 'unknown') && body.force !== true) {
        return {
          status: 409,
          body: {
            ok: false,
            reason:
              state.verdict === 'has-work'
                ? 'worktree has uncommitted or unpushed work'
                : 'worktree safety is unknown',
            verdict: state.verdict,
            dirty: state.dirty,
            unpushed: state.unpushed,
          },
        };
      }

      // A worktree spawn always records its main-repo cwd; a null here means the
      // row is unusable, the same "main repository unavailable" the rev-parse
      // below reports when the recorded cwd no longer resolves (execFileP catches
      // the invalid-argv throw and returns ok:false, so this guard is faithful).
      if (row.cwd == null)
        return { status: 409, body: { ok: false, reason: 'main repository unavailable' } };
      const repoResult = await execFileP('git', ['-C', row.cwd, 'rev-parse', '--show-toplevel'], {
        timeout: 5_000,
      });
      if (!repoResult.ok)
        return { status: 409, body: { ok: false, reason: 'main repository unavailable' } };
      const repo = repoResult.out.trim();

      // BUG-059 (data-loss): `repo` came from a remembered spawn cwd — prove it
      // actually OWNS this worktree before building any repo-scoped destructive
      // argv from it. Without this check, a recorded cwd that has been deleted
      // and recreated as a different repository B silently redirects the fallback
      // directory removal's prune and, worst of all, `git branch -D` into B —
      // deleting an unrelated repository's branch and its unique commits. Refuse
      // everything destructive when ownership cannot be proven; the spawn rows
      // stay, so the worktree can be cleaned up once the path resolves again.
      if (!(await repoOwnsWorktree(repo, worktreePath, state.exists))) {
        return {
          status: 409,
          body: {
            ok: false,
            reason:
              'recorded cwd now resolves to a different repository than this worktree belongs to — refusing to remove or delete branches in it',
            repo: canonical(repo),
          },
        };
      }

      // CONTRACT: re-validate liveness HERE, after every awaited probe and right
      // before the first destructive step. The gate above was decided before
      // inspectWorktree's multi-second git probes AND the repo rev-parse yielded the
      // event loop; a concurrent /api/spawn/:id/revive of this offline spawn during
      // that window relaunches Claude in THIS worktree and writes a fresh
      // 'provisioning'/'spawning' spawn row. worktreePathIsLive re-queries the DB
      // (authoritative and synchronous) by spawn STATUS — not session_ended_at,
      // which the revive leaves set until a later hook — so it actually sees that
      // row and aborts before we chmod/`git worktree remove` (which with force:true
      // would erase the just-revived pane's tree). Closes the pre-remove window.
      if (worktreePathIsLive(worktreePath)) {
        return { status: 409, body: { ok: false, reason: 'session became live during removal' } };
      }

      // Claim custody of this path with NO await between the liveness re-check
      // and the claim, and hold it through the ENTIRE destructive tail below
      // (git worktree remove → rm/prune fallback → optional branch -D → row
      // purge). Every re-check in this function is a snapshot; without the lease
      // a revive could still slip in after THIS check — validate the
      // still-existing cwd, insert its 'provisioning' row and launch its pane in
      // the very directory the awaited `git worktree remove` was about to
      // delete, leaving a successful revive (and a live card) pointing at a
      // nonexistent checkout. revive() claims the same per-path lease before its
      // first await, so exactly one operation wins — and a winning revive keeps
      // its directory. Derive wires claimWorktreeCustody per-core; tests driving
      // this module directly opt in.
      const releaseCustody = claimWorktreeCustody?.(worktreePath, 'remove');
      if (!releaseCustody && claimWorktreeCustody) {
        return { status: 409, body: { ok: false, reason: 'session became live during removal' } };
      }
      try {
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
          chmodWritableWhereOwned(worktreePath); // sync: only fs.chmodSync/readdirSync

          const args = ['-C', repo, 'worktree', 'remove'];
          if (body.force === true) args.push('--force');
          args.push(worktreePath);
          const removed = await execFileP('git', args, { timeout: 30_000 });

          if (!removed.ok) {
            // Anything left belongs to somebody else. This daemon runs as you and
            // does NOT escalate to root — so it names the paths and their owner,
            // hands over the exact command, and stops.
            const blocked = blockedPaths(worktreePath);
            const firstBlocked = blocked[0];
            if (firstBlocked) {
              return {
                status: 409,
                body: {
                  ok: false,
                  reason:
                    `blocked by ${blocked.length} path(s) this daemon may not delete — owned by ` +
                    `${[...new Set(blocked.map((b) => b.owner))].join(', ')}. Fleet Deck runs as you and never escalates to root.`,
                  blocked_paths: blocked.map((b) => b.path),
                  blocked_owner: firstBlocked.owner,
                  fix_command:
                    `sudo rm -rf ${blocked.map((b) => shellQuote(b.path)).join(' ')} ` +
                    `&& git -C ${shellQuote(repo)} worktree prune`,
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
              const porcelain = await execFileP(
                'git',
                ['-C', worktreePath, 'status', '--porcelain'],
                { timeout: 5_000 },
              );
              if (porcelain.ok && porcelain.out.trim() !== '') {
                return {
                  status: 409,
                  body: {
                    ok: false,
                    reason:
                      'git refused to remove this worktree and it still has uncommitted changes — pass force to delete',
                    verdict: 'has-work',
                    dirty: porcelain.out.split(/\r?\n/).filter(Boolean).length,
                  },
                };
              }
            }
            try {
              fs.rmSync(worktreePath, { recursive: true, force: true });
            } catch (err) {
              const detail =
                err instanceof Error
                  ? ((err as NodeJS.ErrnoException).code ?? err.message)
                  : String(err);
              return {
                status: 409,
                body: { ok: false, reason: `could not remove worktree: ${detail}` },
              };
            }
            const pruneError = await pruneWorktreeMetadata(repo);
            if (pruneError) return pruneError;
          }
        } else {
          const pruneError = await pruneWorktreeMetadata(repo);
          if (pruneError) return pruneError;
        }

        let branch_deleted = false;
        const branch = state.branch ?? q.getSession.get(row.session_id)?.branch ?? null;
        if (body.delete_branch === true && branch) {
          // Compare-and-swap against the INSPECTED tip. The safety verdict was
          // measured against inspected_tip; a commit made DURING this request —
          // another process landing clean unpushed work on the branch while the
          // awaited probes above yielded the event loop — was never part of it,
          // and an unconditional `git branch -D` would silently discard that
          // commit even though the operator approved deletion when nothing would
          // have been lost. So re-read the tip NOW and delete only when the ref
          // still points at exactly what was vouched for, handing git the expected
          // old value so a move in the final microseconds also refuses. A kept
          // branch is reported (branch_deleted:false), not a 409 — the tree is
          // already gone either way. (force is no exception: force overrides the
          // VERDICT, it never was a licence to discard commits made mid-request.)
          //
          // When the tree was already gone there is no inspection to compare
          // against (the branch name came from the session row) — and no loss is
          // possible: everything the branch ever was survived the earlier tree
          // removal, so deleting the ref is the unconditional `branch -D` this
          // endpoint has always done there, checked-out guard included. `git
          // update-ref` carries no checked-out guard, but by the time it runs the
          // only worktree that could check this branch out — THIS one — is gone.
          if (inspected_tip == null) {
            const deleted = await execFileP('git', ['-C', repo, 'branch', '-D', branch], {
              timeout: 30_000,
            });
            branch_deleted = deleted.ok;
          } else if ((await branchTipOid(repo, branch)) === inspected_tip) {
            const deleted = await execFileP(
              'git',
              ['-C', repo, 'update-ref', '-d', `refs/heads/${branch}`, inspected_tip],
              { timeout: 30_000 },
            );
            branch_deleted = deleted.ok;
          }
        }

        // Final liveness gate before the DB purge. The awaited git remove/prune/branch
        // ops above yielded the event loop for up to ~90s; a revive that landed in THAT
        // window has a fresh launching/live spawn row for this path. The tree may
        // already be gone, but purging that row would vanish a LIVE session from the
        // board (a lost-terminal bug) — so keep the rows and report rows_purged:0.
        if (worktreePathIsLive(worktreePath)) {
          onMutate();
          return {
            status: 200,
            body: {
              ok: true,
              removed: true,
              branch_deleted,
              rows_purged: 0,
              spawn_became_live: true,
              path: worktreePath,
            },
          };
        }
        const sessionIds = [
          ...new Set(rows.map((candidate) => candidate.session_id).filter(Boolean)),
        ];
        // One transaction, and dependents settle BEFORE their routing parents go:
        // a bare session delete would orphan pending mail and pending questions —
        // freeform questions deliberately survive SessionEnd (the session is
        // resumable), so an ended spawned session can still own a pending queue.
        // With the session row gone the board shows a ghost question, its answer
        // and the original mail route to a callsign that no longer exists, and no
        // retention query (they all locate targets THROUGH sessions) ever sweeps
        // the rows. Hold-kind rows for an ENDED session have no parked socket or
        // re-arm timer left to keep consistent, so the statement-level expiry is
        // sufficient here — expireAllForSession's hold machinery has nothing to
        // release for a session whose hooks already disconnected.
        const now = Date.now();
        let spawnsPurged = 0;
        let sessionsPurged = 0;
        const purgeRows = () => {
          for (const sessionId of sessionIds) {
            q.expireMailForSession.run(now, sessionId);
            q.expireQuestionsForSession.run(sessionId);
          }
          spawnsPurged = Number(q.deleteWorktreeSpawns.run(worktreePath).changes);
          for (const sessionId of sessionIds)
            sessionsPurged += Number(q.deleteEndedSession.run(sessionId).changes);
        };
        // The atomic wrapper needs the raw handle; production and the full-core
        // tests wire `db`, while the direct-drive createWorktrees tests do not —
        // there the same statements run outside an explicit transaction (a
        // single-threaded test needs no isolation), so the expiry-before-delete
        // ordering is preserved either way.
        if (db) {
          db.exec('BEGIN IMMEDIATE');
          try {
            purgeRows();
            db.exec('COMMIT');
          } catch (err) {
            try {
              db.exec('ROLLBACK');
            } catch {
              /* the transaction is already gone */
            }
            const detail = err instanceof Error ? err.message : String(err);
            return {
              status: 500,
              body: { ok: false, reason: `could not purge worktree rows: ${detail}` },
            };
          }
        } else {
          purgeRows();
        }
        const rows_purged = spawnsPurged + sessionsPurged;
        tick(
          `⌫ removed worktree ${worktreePath}${branch_deleted && branch ? ` and branch ${branch}` : ''}`,
        );
        onMutate();
        return {
          status: 200,
          body: { ok: true, removed: true, branch_deleted, rows_purged, path: worktreePath },
        };
      } finally {
        // Release on EVERY exit path (success, refusal, or throw) so a failed
        // removal can never wedge the path against future removes or revives.
        releaseCustody?.();
      }
    } finally {
      releasePath();
    }
  }

  return { worktrees, removeWorktree };
}
