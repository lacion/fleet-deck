// ingest.mjs — agents-cli ingest (F1): the merge step for the secondary
// session source. Threaded ctx state: q, assignCallsign, updateSession, tick,
// onMutate. deriveRepo/branchOf resolve repo identity; pidOwnedBy/colFromAgentState
// are pure helpers.

import { deriveRepo, branchOf } from './repo-identity.ts';
import { ticketFromBranch } from './tickets.ts';
import { pidOwnedBy, colFromAgentState } from './helpers.ts';

export function createIngest(ctx) {
  const { q, assignCallsign, updateSession, tick, onMutate, touchRepo } = ctx;

  // ------------------------------------------- agents-cli ingest (F1)
  // Secondary session source: `claude agents --json` catches sessions that
  // predate plugin install — no hook ever fired for them, so they'd
  // otherwise never appear on the board. Polled by scripts/fleetd/agents-poll.mjs
  // (~10s cadence); this function is the merge step.
  //
  // Precedence rule (critical, see also the source flip in applyEvent
  // above): hook-derived state ALWAYS wins. This may only:
  //   (a) create a card for a sessionId never seen before at all — marked
  //       source='agents-cli', callsign assigned normally, cwd/repo identity
  //       derived as usual, col mapped from state/status, note "seen via
  //       agents CLI", name -> task.
  //   (b) update col/note/lastSeen on a card whose source is STILL
  //       'agents-cli'. The instant a real hook event lands for a session,
  //       applyEvent flips its source to 'hooks' and this function leaves it
  //       completely alone from then on — including lastSeen.
  // Trust rules (learned on install day — the CLI's agent registry lies):
  //   1. Only `kind: "interactive"` entries are fleet sessions. Background
  //      entries are subagents living INSIDE a parent session, and the
  //      registry keeps them for hours after completion (observed: two
  //      "blocked" background agents from that morning's work rendered as
  //      phantom WORKING cards). They never belong on the board.
  //   2. An interactive entry must own its pid: live AND with a process
  //      start matching the record's startedAt (kill(pid, 0) alone proves
  //      only that SOME process holds the pid — the registry outlives the
  //      process, and the OS can hand a dead Claude's pid to an unrelated
  //      process whose mere existence would then create/revive a phantom
  //      routable card. Unverifiable ownership counts as absent).
  //   3. Absence tombstones agents-cli cards ONLY: a card this poller
  //      created, that hooks never claimed, and that the (filtered) poll no
  //      longer reports, is marked offline — the poller is the only
  //      lifecycle those cards have. Hook-sourced cards are untouched;
  //      SessionEnd remains their only tombstone.
  // (pidOwnedBy + colFromAgentState are pure helpers now — see helpers.mjs.)
  function ingestAgentsPoll(records) {
    if (!Array.isArray(records)) return;
    // Trust rules 1+2: interactive entries with VERIFIED pid ownership are
    // the only records that count — for creation, update AND the absence
    // sweep below (so a reused pid also tombstones the stale card).
    const live = records.filter(rec =>
      rec && typeof rec === 'object' && rec.sessionId
      && rec.kind === 'interactive' && pidOwnedBy(rec.pid, rec.startedAt));

    for (const rec of live) {
      const sid = rec.sessionId;
      const rawState = rec.state ?? rec.status;
      const existing = q.getSession.get(sid);
      const cwd = rec.cwd || null;
      const repo = cwd ? deriveRepo(cwd) : { repo_id: null, repo_name: null, worktree: null, main_tree: null, is_git: false };
      if (!existing) {
        // Naming moment (same rules as a hook birth): fresh branch (bypass the
        // 20s cache) so the ticket key matches the checkout the agent is on now;
        // ticket-first callsign; ticket + source recorded right after the insert.
        const branch = cwd ? branchOf(cwd, { fresh: true }) : null;
        const ticket = ticketFromBranch(branch);
        const callsign = assignCallsign(sid, ticket);
        const now = Date.now();
        const startedAt = Number.isFinite(rec.startedAt) ? rec.startedAt : now;
        q.insertAgentSession.run(
          sid, callsign, cwd, repo.repo_id ?? null, repo.repo_name ?? null,
          branch ?? null, repo.worktree ?? null, colFromAgentState(rawState, true),
          'seen via agents CLI', rec.name ?? null, startedAt, now,
        );
        if (repo.is_git) {
          touchRepo({
            repo_id: repo.repo_id,
            repo_name: repo.repo_name,
            root: repo.main_tree,
            source: 'hooks',
          });
        }
        if (ticket) updateSession(sid, { ticket, ticket_source: 'branch' });
        tick(`${callsign} joined the fleet (agents CLI)`);
        onMutate();
      } else if (existing.source === 'agents-cli') {
        const repoChanged = repo.is_git && repo.repo_id !== existing.repo_id;
        // Mutable identity fields refresh outside the repoChanged gate: an
        // in-place checkout (same worktree, another branch) leaves repo_id
        // stable, and a gated branch would stay stale for the card's whole
        // lifetime — agents-cli cards have no hook telemetry to correct it.
        // cwd/worktree move when a session's working directory does (e.g. a
        // worktree-to-worktree move inside one repo). All three reads are
        // TTL-cached in repo-identity.mjs, so the extra probes cost at most
        // one git round per cwd per ~20s (branch) / ~5min (identity).
        const cwdChanged = !!cwd && cwd !== existing.cwd;
        const worktreeChanged = !!cwd && repo.worktree !== existing.worktree;
        // Cached (20s TTL) read — a real checkout is picked up within a few
        // poll cycles; `fresh` is reserved for naming moments.
        const branch = cwd ? branchOf(cwd) : existing.branch;
        updateSession(sid, {
          col: colFromAgentState(rawState, false),
          note: 'seen via agents CLI',
          last_seen: Date.now(),
          ended_at: null, // reappearance revives an absence-tombstoned card
          end_reason: null, // and clears the absence guess stamped below
          ...(repoChanged || cwdChanged ? { cwd } : {}),
          ...(repoChanged ? { repo_id: repo.repo_id, repo_name: repo.repo_name } : {}),
          ...(repoChanged || worktreeChanged ? { worktree: repo.worktree } : {}),
          ...(branch !== existing.branch ? { branch } : {}),
        });
        if (repoChanged) {
          touchRepo({
            repo_id: repo.repo_id,
            repo_name: repo.repo_name,
            root: repo.main_tree,
            source: 'hooks',
          });
        }
        onMutate();
      }
      // existing.source === 'hooks': hook-derived state always wins here —
      // leave the card completely alone.
    }

    // Trust rule 3: absence sweep, agents-cli cards only.
    const liveSids = new Set(live.map(r => r.sessionId));
    for (const s of q.allSessions.all()) {
      if (s.source !== 'agents-cli' || s.ended_at != null) continue;
      if (liveSids.has(s.session_id)) continue;
      updateSession(s.session_id, {
        col: 'offline',
        note: 'no longer reported by agents CLI',
        ended_at: Date.now(),
        // Absence from one registry poll is a GUESS, not proof of death (the
        // registry is documented-unreliable, trust rules above). Stamp it like
        // retention's silence guess so move-to-tmux's adopt-now allowlist
        // refuses it — resuming a still-live CLI is a duplicate billed session.
        end_reason: 'presumed',
      });
      tick(`${s.callsign} left the fleet (agents CLI)`);
      onMutate();
    }
  }

  return { ingestAgentsPoll };
}
