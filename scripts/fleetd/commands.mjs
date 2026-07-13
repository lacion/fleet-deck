// commands.mjs — the POST /command surface (broadcast / assign / assign auto /
// note). parseCommand is a pure helper; the deterministic auto-routing policy
// lives in q.autoCandidate. Threaded ctx state: q, mail, resolveTargets, tick,
// onMutate.

import { parseCommand } from './helpers.mjs';

export function createCommands(ctx) {
  const { q, mail, resolveTargets, tick, onMutate } = ctx;

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
    } else {
      tick(`📝 orchestrator note: ${parsed.text.slice(0, 60)}`);
    }
    logCommand();
    onMutate();
    return { ok: true, parsed, delivered };
  }

  return { command };
}
