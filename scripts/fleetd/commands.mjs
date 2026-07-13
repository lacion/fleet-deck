// commands.mjs — the POST /command surface (broadcast / assign / assign auto /
// note). parseCommand is a pure helper; the deterministic auto-routing policy
// lives in q.autoCandidate. Threaded ctx state: q, mail, resolveTargets, tick,
// onMutate.

import { parseCommand } from './helpers.mjs';
import { normalizeTicket } from './tickets.mjs';

export function createCommands(ctx) {
  const { q, mail, resolveTargets, tick, onMutate, applyTicket, updateSession } = ctx;

  // `ticket <target> clear`: drop the ticket and pin the auto path off. Setting
  // ticket_source='manual' (never NULL) is what makes "no ticket" stick — a
  // later branch switch must not re-ticket a session a human deliberately
  // cleared. Revert to the birth callsign when one was recorded AND is still
  // free; if it was reissued to a newer session, keep the current name rather
  // than collide. On revert the DROPPED ticketed name moves into prev_callsign
  // (never nulled): peers' briefs and the ticker still reference it, so it must
  // stay mail-routable — the row holds the same two-name set as before the
  // clear (columns merely swap), so no other session's uniqueness changes.
  function clearTicket(sid) {
    const c = q.getSession.get(sid);
    if (!c || c.ended_at != null) return { ok: false, reason: 'no live session for that target' };
    const upd = { ticket: null, ticket_source: 'manual' };
    let result = { ok: true, renamed: false, callsign: c.callsign, ticket: null };
    if (c.prev_callsign && !q.callsignTaken.get(c.prev_callsign, c.prev_callsign, sid)) {
      upd.callsign = c.prev_callsign;
      upd.prev_callsign = c.callsign;
      tick(`🎫 ${c.callsign} reverted to ${c.prev_callsign} (ticket cleared)`);
      result = { ok: true, renamed: true, callsign: c.prev_callsign, ticket: null, previous: c.callsign };
    } else {
      tick(`🎫 ${c.callsign} ticket cleared`);
    }
    updateSession(sid, upd);
    return result;
  }

  // Resolve a manual `ticket` target to exactly one live (non-ended,
  // non-archived) session by session_id | current callsign | birth callsign
  // (prev_callsign — the stale name a human may still be typing). Returns
  // { sid } on a unique hit, or { error } (0 → none, >1 → ambiguous).
  function resolveTicketTarget(target) {
    const matches = q.visibleSessions.all().filter(s =>
      s.ended_at == null
      && (s.session_id === target || s.callsign === target || s.prev_callsign === target));
    if (matches.length === 0) return { error: `no live session matching "${target}"` };
    if (matches.length > 1) return { error: `"${target}" is ambiguous — use the session id` };
    return { sid: matches[0].session_id };
  }

  // ------------------------------------------------------------- commands
  function command(text) {
    const parsed = parseCommand(text);
    const logCommand = extra =>
      q.insertCommand.run(Date.now(), String(text ?? ''), JSON.stringify(extra ? { ...parsed, ...extra } : parsed));
    let delivered = 0;
    if (parsed.cmd === 'broadcast') {
      const targets = resolveTargets('all');
      targets.forEach(sid => mail(sid, 'orchestrator', parsed.text));
      delivered = targets.length;
      tick(`📣 orchestrator broadcast → ${delivered} session(s)`);
    } else if (parsed.cmd === 'assign_auto') {
      // v1.1 deterministic auto-routing (POST /command contract). The
      // candidate/ranking policy lives entirely in q.autoCandidate above —
      // zero model calls, one SQL round. The same repo key feeds all three
      // placeholders (NULL = unscoped, else matched against repo_id OR
      // repo_name).
      const repo = parsed.repo ?? null;
      const winner = q.autoCandidate.get(repo, repo, repo);
      if (!winner) {
        logCommand({ unrouted: true });
        tick('⚠ assign auto: no available session — task logged');
        onMutate();
        // v1.2 unrouted CTA: carry the task text so the board can render a
        // "spawn a session for this" button with the prompt prefilled.
        // Routing itself NEVER spawns.
        return { ok: false, unrouted: true, text: parsed.text };
      }
      const assigned_to = { session_id: winner.session_id, callsign: winner.callsign };
      mail(winner.session_id, 'orchestrator', `[FLEETDECK ASSIGNMENT] ${parsed.text}`);
      tick(`⚡ orchestrator → ${winner.callsign}: ${parsed.text.slice(0, 60)}`);
      logCommand({ assigned_to });
      onMutate();
      return { ok: true, assigned_to };
    } else if (parsed.cmd === 'assign') {
      const targets = resolveTargets(parsed.target);
      // Same frame as auto-routing (v1.1): every routed task carries
      // [FLEETDECK ASSIGNMENT] so the wake path / doctrine skill can treat
      // assignments uniformly regardless of how they were targeted.
      targets.forEach(sid => mail(sid, 'orchestrator', `[FLEETDECK ASSIGNMENT] ${parsed.text}`));
      delivered = targets.length;
      tick(`📌 orchestrator assign → ${parsed.target}${delivered ? '' : ' (no such session)'}`);
    } else if (parsed.cmd === 'ticket') {
      // The manual `ticket` surface. EVERY exit here returns {ok:false, reason}
      // or {session_id, ...applyTicketResult} — it must NEVER fall through to
      // the note handler (a malformed rename is an error to show, not a note to
      // file). A bare/malformed command arrives already carrying parsed.error.
      if (parsed.error) { logCommand(); onMutate(); return { ok: false, reason: parsed.error }; }
      // Per-session only: `all` / `repo:*` are broadcast scopes, meaningless for
      // a rename.
      if (parsed.target === 'all' || /^repo:/.test(parsed.target)) {
        logCommand();
        onMutate();
        return { ok: false, reason: 'ticket targets one session — not all/repo:*' };
      }
      const resolved = resolveTicketTarget(parsed.target);
      if (resolved.error) { logCommand(); onMutate(); return { ok: false, reason: resolved.error }; }
      let result;
      if (/^clear$/i.test(parsed.ticket)) {
        result = clearTicket(resolved.sid);
      } else {
        // Strict whole-string key (proj-55 → PROJ-55; PROJ-007 → null). An
        // invalid key is loud, never a silent no-op. Manual may fire any number
        // of times — ticket_source='manual' also permanently blocks auto-detect.
        const key = normalizeTicket(parsed.ticket);
        if (!key) {
          logCommand();
          onMutate();
          return { ok: false, reason: `invalid ticket key "${parsed.ticket}" — expected e.g. PROJ-123 or clear` };
        }
        result = applyTicket(resolved.sid, key, 'manual');
      }
      logCommand({ result });
      onMutate();
      return { session_id: resolved.sid, ...result };
    } else {
      tick(`📝 orchestrator note: ${parsed.text.slice(0, 60)}`);
    }
    logCommand();
    onMutate();
    return { ok: true, parsed, delivered };
  }

  return { command };
}
