// commands.ts — the POST /command surface (broadcast / assign / assign auto /
// ticket / name / note). parseCommand is a pure helper; the deterministic
// auto-routing policy lives in q.autoCandidate. Threaded ctx state: q, mail,
// resolveTargets, tick, onMutate, applyTicket, applyCustomName.

import { asText, parseCommand, validateNameSuffix } from './helpers.ts';
import { normalizeTicket } from './tickets.ts';
import { MAIL_MAX_LEN } from './mail.ts';
import type { Statements } from './statements.ts';
import type { SqlValue } from './sqlite.ts';

// The rename receipt returned by clearTicket / applyTicket / applyCustomName.
// commands.ts only ever SPREADS it into the /command response and never reads a
// field back, so Record<string, unknown> is the honest boundary — the callbacks
// live in derive.mjs (not yet converted), and a narrower shape would be fiction.
type RenameResult = Record<string, unknown>;

// resolveTicketTarget's discriminated result: a unique live session, or a reason.
type TicketTarget = { sid: string } | { error: string };

interface CommandsCtx {
  // The prepared-statement bundle only (the daemon threads q onto ctx), same
  // idiom as mail.ts / questions.ts — Statements['q'], not the whole Statements.
  q: Statements['q'];
  mail: (sid: string, from: string, text: string) => unknown;
  resolveTargets: (target: string) => string[];
  tick: (msg: string) => void;
  onMutate: () => void;
  applyTicket: (sid: string, key: string, source: string) => RenameResult;
  updateSession: Statements['updateSession'];
  applyCustomName: (sid: string, suffix: string | null) => RenameResult; // 0.7.1 `name <target> <suffix|clear>`
}

export function createCommands(ctx: CommandsCtx) {
  const {
    q,
    mail,
    resolveTargets,
    tick,
    onMutate,
    applyTicket,
    updateSession,
    applyCustomName, // 0.7.1 `name <target> <suffix|clear>`
  } = ctx;

  // `ticket <target> clear`: drop the ticket and pin the auto path off. Setting
  // ticket_source='manual' (never NULL) is what makes "no ticket" stick — a
  // later branch switch must not re-ticket a session a human deliberately
  // cleared. Revert to the birth callsign when one was recorded AND is still
  // free; if it was reissued to a newer session, keep the current name rather
  // than collide. BUG-107: on revert the birth name moves back into CALLSIGN
  // and prev_callsign becomes NULL — it is the write-once stale-ref ANCHOR,
  // never a scratch slot. Keeping the dropped ticketed name there (the old
  // "columns merely swap" behaviour) meant the very next rename re-anchored on
  // that intermediate alias and permanently forgot the SessionStart callsign.
  // The dropped name stays routable via the alias table instead.
  function clearTicket(sid: string): RenameResult {
    const c = q.getSession.get(sid);
    if (!c || c.ended_at != null) return { ok: false, reason: 'no live session for that target' };
    const upd: Record<string, SqlValue> = { ticket: null, ticket_source: 'manual' };
    let result: RenameResult = { ok: true, renamed: false, callsign: c.callsign, ticket: null };
    if (c.prev_callsign && !q.callsignTaken.get(c.prev_callsign, c.prev_callsign, sid)) {
      upd['callsign'] = c.prev_callsign;
      upd['prev_callsign'] = null;
      q.rememberAlias.run(sid, c.callsign, Date.now());
      tick(`🎫 ${c.callsign} reverted to ${c.prev_callsign} (ticket cleared)`);
      result = {
        ok: true,
        renamed: true,
        callsign: c.prev_callsign,
        ticket: null,
        previous: c.callsign,
      };
    } else {
      tick(`🎫 ${c.callsign} ticket cleared`);
    }
    updateSession(sid, upd);
    return result;
  }

  // Resolve a manual `ticket` target to exactly one live (non-ended,
  // non-archived) session by session_id | current callsign | birth callsign
  // (prev_callsign — the stale name a human may still be typing), then by the
  // full alias history (BUG-107: a name the card wore and dropped at any
  // point). Returns { sid } on a unique hit, or { error } (0 → none, >1 →
  // ambiguous).
  function resolveTicketTarget(target: string): TicketTarget {
    const matches = q.visibleSessions
      .all()
      .filter(
        (s) =>
          s.ended_at == null &&
          (s.session_id === target || s.callsign === target || s.prev_callsign === target),
      );
    if (matches.length > 1) return { error: `"${target}" is ambiguous — use the session id` };
    const found = matches.length
      ? matches
      : q.aliasesMatch.all(target, target).filter((s) => s.ended_at == null);
    if (found.length === 0) return { error: `no live session matching "${target}"` };
    if (found.length > 1) return { error: `"${target}" is ambiguous — use the session id` };
    const only = found[0];
    if (!only) return { error: `no live session matching "${target}"` };
    return { sid: only.session_id };
  }

  function resolveRenameTarget(command: 'ticket' | 'name', target: string): TicketTarget {
    // Renames are per-session only: all/repo:* are broadcast scopes.
    if (target === 'all' || target.startsWith('repo:')) {
      return { error: `${command} targets one session — not all/repo:*` };
    }
    return resolveTicketTarget(target);
  }

  // ------------------------------------------------------------- commands
  function command(text: unknown) {
    const parsed = parseCommand(text);
    const logCommand = (extra?: Record<string, unknown>) =>
      q.insertCommand.run(
        Date.now(),
        asText(text),
        JSON.stringify(extra ? { ...parsed, ...extra } : parsed),
      );
    const rejectCommand = (reason: string) => {
      logCommand();
      onMutate();
      return { ok: false, reason };
    };
    let delivered = 0;
    if (parsed.cmd === 'broadcast' || parsed.cmd === 'assign_auto' || parsed.cmd === 'assign') {
      // BUG-021: the mail() clamp reports {truncated, original_length}, but the
      // command path used to ignore that receipt — /command returned ok:true
      // while agents received a body with its tail silently cut (acceptance
      // criteria, safety constraints, diagnostics lost). Validate the FULLY
      // FRAMED body (assignments prepend [FLEETDECK ASSIGNMENT], which itself
      // counts against the cap) against MAIL_MAX_LEN BEFORE inserting any
      // recipient row, and reject the command atomically: nothing is stored,
      // nothing half-delivered, and the operator is told to shorten or split
      // instead of believing a partial instruction landed intact.
      const frame = parsed.cmd === 'broadcast' ? '' : '[FLEETDECK ASSIGNMENT] ';
      const framed = `${frame}${parsed.text}`;
      if (framed.length > MAIL_MAX_LEN) {
        const reason = `message too long (${framed.length} > ${MAIL_MAX_LEN} code units) — shorten it or split it into multiple commands`;
        logCommand({ rejected: true, reason });
        tick(`⚠ command rejected: ${reason}`);
        onMutate();
        return { ok: false, reason, max_length: MAIL_MAX_LEN, original_length: framed.length };
      }
    }
    if (parsed.cmd === 'broadcast') {
      const targets = resolveTargets('all');
      targets.forEach((sid) => mail(sid, 'orchestrator', parsed.text));
      delivered = targets.length;
      tick(`📣 orchestrator broadcast → ${delivered} session(s)`);
    } else if (parsed.cmd === 'assign_auto') {
      // v1.1 deterministic auto-routing (POST /command contract). The
      // candidate/ranking policy lives entirely in q.autoCandidate above —
      // zero model calls, one SQL round. The same repo key feeds all three
      // placeholders (NULL = unscoped, else matched against repo_id OR
      // repo_name).
      const repo = parsed.repo;
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
      // Singular targeting must never fan out: a duplicated callsign resolves
      // to two sessions, and silently assigning BOTH means two agents
      // independently execute a task meant for one (duplicate compute,
      // conflicting edits). Fan-out stays reserved for the explicit `all` and
      // `repo:*` scopes; a multi-hit direct target is refused loudly, like
      // `ticket`'s resolver above.
      if (parsed.target !== 'all' && !parsed.target.startsWith('repo:') && targets.length > 1) {
        logCommand({ refused: 'ambiguous' });
        onMutate();
        return {
          ok: false,
          reason: `"${parsed.target}" matches ${targets.length} sessions — use the session id`,
        };
      }
      // Same frame as auto-routing (v1.1): every routed task carries
      // [FLEETDECK ASSIGNMENT] so the wake path / doctrine skill can treat
      // assignments uniformly regardless of how they were targeted.
      targets.forEach((sid) => mail(sid, 'orchestrator', `[FLEETDECK ASSIGNMENT] ${parsed.text}`));
      delivered = targets.length;
      tick(`📌 orchestrator assign → ${parsed.target}${delivered ? '' : ' (no such session)'}`);
    } else if (parsed.cmd === 'ticket') {
      // The manual `ticket` surface. EVERY exit here returns {ok:false, reason}
      // or {session_id, ...applyTicketResult} — it must NEVER fall through to
      // the note handler (a malformed rename is an error to show, not a note to
      // file). A bare/malformed command arrives already carrying parsed.error.
      if ('error' in parsed) return rejectCommand(parsed.error);
      const resolved = resolveRenameTarget(parsed.cmd, parsed.target);
      if ('error' in resolved) return rejectCommand(resolved.error);
      let result: RenameResult;
      if (/^clear$/i.test(parsed.ticket)) {
        result = clearTicket(resolved.sid);
      } else {
        // Strict whole-string key (proj-55 → PROJ-55; PROJ-007 → null). An
        // invalid key is loud, never a silent no-op. Manual may fire any number
        // of times — ticket_source='manual' also permanently blocks auto-detect.
        const key = normalizeTicket(parsed.ticket);
        if (!key) {
          return rejectCommand(
            `invalid ticket key "${parsed.ticket}" — expected e.g. PROJ-123 or clear`,
          );
        }
        result = applyTicket(resolved.sid, key, 'manual');
      }
      logCommand({ result });
      onMutate();
      return { session_id: resolved.sid, ...result };
    } else if (parsed.cmd === 'name') {
      // 0.7.1 custom names — the `ticket` branch's twin, same contract: every
      // exit is {ok:false, reason} or {session_id, ...renameResult}, and a
      // malformed rename is NEVER filed as a note. The human owns the suffix;
      // the animal is not theirs to choose, so it is not in the grammar.
      if ('error' in parsed) return rejectCommand(parsed.error);
      const resolved = resolveRenameTarget(parsed.cmd, parsed.target);
      if ('error' in resolved) return rejectCommand(resolved.error);
      const clearing = /^clear$/i.test(parsed.suffix);
      if (!clearing) {
        const bad = validateNameSuffix(parsed.suffix);
        if (bad) {
          return rejectCommand(bad);
        }
      }
      const result = applyCustomName(resolved.sid, clearing ? null : parsed.suffix);
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
