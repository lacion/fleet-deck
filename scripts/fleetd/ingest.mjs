// ingest.mjs — agents-cli ingest (F1): the merge step for the secondary
// session source. Threaded ctx state: q, assignCallsign, updateSession, tick,
// onMutate. deriveRepo/branchOf resolve repo identity; pidAlive/colFromAgentState
// are pure helpers.

import { deriveRepo, branchOf } from './repo-identity.mjs';
import { pidAlive, colFromAgentState } from './helpers.mjs';

export function createIngest(ctx) {
  const { q, assignCallsign, updateSession, tick, onMutate } = ctx;

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
  //   2. An interactive entry must have a LIVE pid (kill(pid, 0)) — the
  //      registry can outlive the process.
  //   3. Absence tombstones agents-cli cards ONLY: a card this poller
  //      created, that hooks never claimed, and that the (filtered) poll no
  //      longer reports, is marked offline — the poller is the only
  //      lifecycle those cards have. Hook-sourced cards are untouched;
  //      SessionEnd remains their only tombstone.
  // (pidAlive + colFromAgentState are pure helpers now — see helpers.mjs.)
  function ingestAgentsPoll(records) {
    if (!Array.isArray(records)) return;
    // Trust rules 1+2: interactive entries with a live pid are the only
    // records that count — for creation, update AND the absence sweep below.
    const live = records.filter(rec =>
      rec && typeof rec === 'object' && rec.sessionId
      && rec.kind === 'interactive' && pidAlive(rec.pid));

    for (const rec of live) {
      const sid = rec.sessionId;
      const rawState = rec.state ?? rec.status;
      const existing = q.getSession.get(sid);
      if (!existing) {
        const callsign = assignCallsign(sid);
        const cwd = rec.cwd || null;
        const repo = cwd ? deriveRepo(cwd) : { repo_id: null, repo_name: null, worktree: null };
        const branch = cwd ? branchOf(cwd) : null;
        const now = Date.now();
        const startedAt = Number.isFinite(rec.startedAt) ? rec.startedAt : now;
        q.insertAgentSession.run(
          sid, callsign, cwd, repo.repo_id ?? null, repo.repo_name ?? null,
          branch ?? null, repo.worktree ?? null, colFromAgentState(rawState, true),
          'seen via agents CLI', rec.name ?? null, startedAt, now,
        );
        tick(`${callsign} joined the fleet (agents CLI)`);
        onMutate();
      } else if (existing.source === 'agents-cli') {
        updateSession(sid, {
          col: colFromAgentState(rawState, false),
          note: 'seen via agents CLI',
          last_seen: Date.now(),
          ended_at: null, // reappearance revives an absence-tombstoned card
        });
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
      });
      tick(`${s.callsign} left the fleet (agents CLI)`);
      onMutate();
    }
  }

  return { ingestAgentsPoll };
}
