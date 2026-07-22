// events.mjs — the hook state machine. applyEvent is the faithful port of the
// spike's derivation switch; the hook endpoints (SessionStart brief,
// UserPromptSubmit, Pre/PostToolUse whisper, Stop mail-block, SessionEnd
// tombstone, and the F3a/b/c hold-open intake with v1.3 plan capture) wrap it.
// Threaded ctx state: q, db, card, updateSession, tick, logEvent, onMutate,
// port, the questions relay, the spawn hooks (scheduleRegistrationRemoteHarvest,
// forgetSpawn), the model-tracking pair, the ledger (recordFile/whisperText),
// mail (drainMail), notifyWatchers and modelMemo.

import path from 'node:path';
import os from 'node:os';
import { deriveRepo, branchOf } from './repo-identity.mjs';
import { ticketFromBranch } from './tickets.mjs';
import { lastAssistantText } from './transcript.mjs';
import { detectTrailingQuestion } from './questions.mjs';
import { mungeClaudeProjectCwd } from './helpers.mjs';

const EDIT_TOOLS = ['Edit', 'Write', 'MultiEdit', 'NotebookEdit'];
const TEST_RUNNER_RE = /\b(pytest|jest|vitest|go test|cargo test|npm (run )?test)\b/; // spike regex, verbatim

// 0.16.0 succession corroboration: the directory a session working in `cwd`
// writes its transcripts to (~/.claude/projects/<munged-cwd>/). The heir of a
// /clear shares the predecessor's cwd, so its transcript_path MUST live here.
function expectedTranscriptDir(cwd, homeDir = os.homedir()) {
  return path.join(homeDir, '.claude', 'projects', mungeClaudeProjectCwd(cwd));
}

export function createEvents(ctx) {
  const {
    q, db, card, updateSession, tick, logEvent, onMutate, port, questions,
    scheduleRegistrationRemoteHarvest, stampTranscriptFloor, readTranscriptModel,
    recordFile, whisperText, drainMail, notifyWatchers, modelMemo, forgetSpawn,
    applyTicket,
    // 0.7.0 Move-to-tmux (adopt): the armed auto-adopt trigger in hookSessionEnd
    // fires adoptSession after ADOPT_DELAY_MS. createSpawns(ctx) runs before
    // createEvents(ctx) in derive.mjs, so ctx.adoptSession is already bound here.
    adoptSession, ADOPT_DELAY_MS,
    // 0.7.1 /clear succession: recognise the heir a /clear just minted, before
    // card() can deal it a fresh identity — and, when the hooks arrive out of
    // order, recognise an heir that is already waiting.
    findClearedPredecessor, succeedSession, succeedForwardFromClear,
    touchRepo,
  } = ctx;

  // ---------------------------------------------- hook event -> card state
  // Faithful port of the spike's applyEvent switch.
  function applyEvent(ev) {
    const sid = ev.session_id || 'unknown';
    let c = card(sid, ev.cwd);
    // Retention tombstones are reversible: a late hook proves the process was
    // alive (or resumed). Clear both timestamps so an archived presumed-dead
    // card becomes visible again before normal derivation continues.
    //
    // M-B5: resurrection must also lift the card OUT of the offline column.
    // The Pre/PostToolUse column rule is "queued|needsyou → working, else keep
    // col"; with col still 'offline' from the tombstone, "keep col" would leave
    // an actively-working resurrected session stranded in the offline lane
    // (UserPromptSubmit forces 'working' and hid this, but tool hooks and the
    // resolved-Notification path do not). Reset the base column to 'queued' so
    // the switch below re-derives a live lane from the activity it just saw.
    // 0.7.1: a SUPERSEDED row is the one tombstone that must NEVER come back.
    // Its lineage did not die, it CONTINUED under a new id — and the heir is
    // already wearing this row's callsign and driving its pane. Async hooks
    // (Notification, FileChanged, an in-flight PostToolUse) can still land for
    // the retired id a moment after the /clear, and resurrecting on one would
    // put a second card on the board under the SAME callsign, holding no pane
    // and never updating again: the exact split this release exists to end.
    // Let the event fall through and update the archived row's counters (it is
    // invisible either way — visibleSessions filters archived rows out); just
    // never un-retire it.
    const superseded = c.succeeded_by != null;
    if (!superseded && (c.ended_at != null || c.archived_at != null)) {
      // 0.7.0: a session that comes back alive is no longer ended, so its
      // end_reason (the hook reason, or retention's 'presumed' guess) is stale —
      // clear it so adopt-now's presumed-dead guard reads the fresh liveness.
      // The adopt_armed_* columns are deliberately KEPT: the human armed this
      // card, and the session being alive again is exactly the armed state's
      // precondition (the auto-adopt still waits for its next real SessionEnd).
      updateSession(sid, { ended_at: null, archived_at: null, col: 'queued', end_reason: null });
      c = { ...c, ended_at: null, archived_at: null, col: 'queued', end_reason: null };
    }
    // Precedence rule (handoff F1): hook-derived state ALWAYS wins. The
    // moment a real hook event arrives for a session — even one first
    // discovered via the agents-cli poller — its source flips to 'hooks' for
    // good, and the poller must never touch its column again after this.
    if (c.source !== 'hooks') {
      updateSession(sid, { source: 'hooks' });
      c = { ...c, source: 'hooks' };
    }
    // A first hook is bring-up proof for both a fresh spawn and a revive.
    // Revived cards already have source='hooks', so this check deliberately
    // lives outside the provenance flip. Terminal historical rows stay put;
    // spawnBySession returns only the newest row and only active rows move.
    const sp = q.spawnBySession.get(sid);
    if (sp && (sp.status === 'spawning' || sp.status === 'stalled')) {
      q.setSpawnStatus.run('live', sp.spawn_id);
      tick(`🛰 ${c.callsign} pane is live (first hook event)`);
      if (sp.remote_control) scheduleRegistrationRemoteHarvest(sp.spawn_id);
    }
    const upd = { last_seen: Date.now(), events: c.events + 1 };
    let serverBranch = null; // the SERVER-derived branch — the ONLY value ticket detection is allowed to trust
    let changedRepo = null;
    if (ev.cwd) {
      upd.cwd = ev.cwd;
      const repo = deriveRepo(ev.cwd);
      upd.repo_id = repo.repo_id;
      upd.repo_name = repo.repo_name;
      upd.worktree = repo.worktree;
      serverBranch = branchOf(ev.cwd);
      const branch = serverBranch || ev.git_branch || null; // display column; payload value only as fallback
      if (branch) upd.branch = branch;
      if (repo.is_git && repo.repo_id !== c.repo_id) changedRepo = repo;
    }
    // The payload only ever carries a model on SessionStart (a bare id string;
    // the object form is defensive — a future CLI may send the statusline
    // shape). That value is the CLI's live truth at launch, and it beats the
    // transcript when an old session is resumed under a different --model. Every
    // OTHER event has to go to the transcript, because that is the only place a
    // mid-session /model switch is written down.
    const payloadModel = ev.model?.display_name || ev.model?.id
      || (typeof ev.model === 'string' && ev.model ? ev.model : null);
    if (ev.hook_event_name === 'SessionStart') {
      stampTranscriptFloor(sid, ev.transcript_path);
      if (payloadModel) upd.model = payloadModel;
    } else if (payloadModel) {
      upd.model = payloadModel;
    } else if (ev.transcript_path) {
      const model = readTranscriptModel(sid, ev.transcript_path);
      if (model && model !== c.model) {
        upd.model = model;
        if (c.model) tick(`🔀 ${c.callsign} switched model → ${model}`);
      }
    }
    updateSession(sid, upd);
    if (changedRepo) {
      touchRepo({
        repo_id: changedRepo.repo_id,
        repo_name: changedRepo.repo_name,
        root: changedRepo.main_tree,
        source: 'hooks',
      });
    }
    c = { ...c, ...upd };

    // Rename-once (late ticket detection): a card born ticketless — or one whose
    // branch only just resolved — that is now seen on a ticket branch is renamed
    // a single time. Guarded on ticket IS NULL AND ticket_source IS NULL (a
    // manual ticket_source or an earlier branch detection permanently closes
    // this path; applyTicket re-checks the same). Detection consumes ONLY the
    // server-derived branch, never the unauthenticated ev.git_branch. Re-read c
    // so THIS event's later ticks and (on SessionStart) composeBrief already
    // speak the renamed callsign + ticket.
    // 0.7.1: a human-named card is off the auto path for good, and the check
    // belongs HERE, not only inside applyTicket. applyTicket refusing a branch
    // rename writes nothing, so the gate below would never close — and the
    // "one extra synchronous git exec per session LIFETIME" below would become
    // one per hook event, forever, on the hottest path in the daemon.
    if (upd.branch && c.ticket == null && c.ticket_source == null && c.custom_suffix == null) {
      let tk = ticketFromBranch(serverBranch);
      if (tk) {
        // The cached read above is the cheap per-event trigger, but the rename
        // is one-shot — a ≤20s-stale cache hit (e.g. a shared-cwd peer warmed
        // it just before a checkout) would mis-ticket the session PERMANENTLY.
        // Re-verify against the live branch at the single moment of first
        // detection: one extra synchronous git exec per session lifetime,
        // still inside applyEvent's no-await block.
        tk = ticketFromBranch(branchOf(ev.cwd, { fresh: true }));
      }
      if (tk && applyTicket(sid, tk, 'branch').ok) c = q.getSession.get(sid);
    }

    let conflict = null;
    const set = {};
    switch (ev.hook_event_name) {
      case 'SessionStart':
        set.col = 'queued';
        set.note = `session ${ev.source || 'startup'}`;
        break;
      case 'UserPromptSubmit':
        set.col = 'working';
        set.task = c.task || (ev.prompt || '').slice(0, 80);
        set.note = 'prompt: ' + (ev.prompt || '').slice(0, 60);
        set.notification_type = null; // activity clears the needs-you reason (F3e)
        tick(`${c.callsign} got a prompt`);
        break;
      case 'PreToolUse':
      case 'PostToolUse': {
        set.col = c.col === 'needsyou' ? 'working' : (c.col === 'queued' ? 'working' : c.col);
        set.notification_type = null; // activity clears the needs-you reason (F3e)
        set.last_tool = ev.tool_name ?? null;
        const input = ev.tool_input || {};
        const file = input.file_path || input.notebook_path;
        if (EDIT_TOOLS.includes(ev.tool_name) && file) {
          conflict = recordFile(sid, file, c);
          set.note = `editing ${path.basename(file)}`;
        } else if (ev.tool_name === 'Bash' && input.command) {
          const cmd = String(input.command);
          if (TEST_RUNNER_RE.test(cmd)) {
            set.col = 'verifying';
            set.note = 'running tests';
          } else {
            set.note = 'sh: ' + cmd.slice(0, 50);
          }
        } else {
          set.note = ev.tool_name;
        }
        break;
      }
      case 'FileChanged': {
        // Bash-side edits net (F4): feed the ledger, do not move the column.
        const file = ev.file_path || ev.tool_input?.file_path || ev.path || null;
        if (file) {
          conflict = recordFile(sid, file, c);
          set.note = `changed ${path.basename(file)}`;
        }
        break;
      }
      case 'Notification': {
        // F3e safety net: the board must always SHOW a stuck session, with
        // the reason. notification_type values (docs §8): permission_prompt,
        // idle_prompt, elicitation_dialog, agent_needs_input are needs-you
        // situations; auth_success / elicitation_complete /
        // elicitation_response / agent_completed are progress reports, not
        // requests for attention — those update the note but don't move the
        // column. Unknown/absent types keep the Phase 1 behavior (needsyou).
        const ntype = ev.notification_type ?? null;
        const RESOLVED_TYPES = ['auth_success', 'elicitation_complete', 'elicitation_response', 'agent_completed'];
        set.notification_type = ntype;
        set.note = (ev.message || ntype || 'needs attention').slice(0, 80);
        if (!RESOLVED_TYPES.includes(ntype)) {
          set.col = 'needsyou';
          tick(`🖐 ${c.callsign} needs you${ntype ? ` (${ntype})` : ''}: ${(ev.message || '').slice(0, 50)}`);
        }
        break;
      }
      case 'PermissionRequest':
        // F3a: the session is waiting on a human decision (relay card is
        // created by hookHoldQuestion; this is the telemetry side).
        // F3c side effect (validated): AskUserQuestion rides the permission
        // machinery — its PermissionRequest fires right before the native
        // terminal chooser renders (and only on the {} path: a board answer
        // denies at PreToolUse and short-circuits this event entirely). The
        // http layer already answered {} without holding; this is telemetry.
        set.col = 'needsyou';
        if (ev.tool_name === 'AskUserQuestion') {
          set.note = 'question open in the terminal';
          tick(`🖐 ${c.callsign} has a question open in the terminal`);
        } else {
          set.note = `permission: ${ev.tool_name || 'tool'}`;
          tick(`🖐 ${c.callsign} awaits permission: ${ev.tool_name || 'tool'}`);
        }
        break;
      case 'AskUserQuestion': {
        // F3c: the model asked a structured question (AskUserQuestion
        // PreToolUse, held by hookHoldQuestion). The terminal chooser renders
        // only AFTER the held hook responds, so during the hold the board is
        // the only place it can be answered.
        const first = (Array.isArray(ev.tool_input?.questions) && ev.tool_input.questions[0]?.question)
          || 'structured question';
        set.col = 'needsyou';
        set.note = ('choice: ' + first).slice(0, 80);
        tick(`🖐 ${c.callsign} asks: ${String(first).slice(0, 50)}`);
        break;
      }
      case 'Elicitation':
        // F3b: an MCP server is waiting on form input.
        set.col = 'needsyou';
        set.note = `elicitation: ${ev.message || ev.matcher || 'MCP input requested'}`.slice(0, 80);
        tick(`🖐 ${c.callsign} awaits input (elicitation)`);
        break;
      case 'Stop':
        set.col = 'idle';
        set.note = 'turn finished, waiting';
        tick(`${c.callsign} finished a turn`);
        break;
      case 'SessionEnd':
        // BUG 1 (0.2.0): a /clear is NOT a session end. Treating it as one
        // tombstones a live card (and hookSessionEnd would condemn its live pane
        // 'pane-dead'), so the board loses the terminal button for an agent that
        // is right there working. Leave col/ended_at untouched — last_seen was
        // already bumped above, and the clear proves liveness, which also resets
        // BUG 2's silence clock. Every real end reason still ends.
        //
        // 0.7.1 CORRECTION to that comment's model: the current CLI does NOT
        // keep the same session_id across a /clear. It mints a NEW one — this
        // SessionEnd is immediately followed by a SessionStart(source='clear')
        // under a brand-new id in the same cwd. So the card does keep living
        // (this branch is still right), but it lives on under a DIFFERENT id:
        // stamp cleared_at to open the correlation window that hookSessionStart
        // uses to recognise the heir (see succeedSession in derive.mjs). If no
        // heir ever arrives — an older CLI that really does keep the id — the
        // stamp simply expires and this stays the plain "still live" case.
        if (ev.reason === 'clear') {
          set.note = 'context cleared (/clear) — still live';
          set.cleared_at = Date.now();
          tick(`🧹 ${c.callsign} ran /clear — context reset, session still live`);
        } else {
          set.col = 'offline';
          set.ended_at = Date.now();
          // 0.7.0: record the hook-PROVEN end reason. adopt-now trusts this to
          // distinguish a real end (safe to `claude --resume`) from retention's
          // silence guess ('presumed', set in retention.mjs) — resuming a still-
          // live CLI would be a duplicate billed session. Default 'end' when the
          // CLI sent no reason so the column is never NULL after a proven end.
          set.end_reason = ev.reason || 'end';
          set.note = 'session ended' + (ev.reason ? ` (${ev.reason})` : '');
          tick(`${c.callsign} left the fleet`);
        }
        break;
      default:
        set.note = ev.hook_event_name;
    }
    updateSession(sid, set);
    c = { ...c, ...set };
    logEvent(sid, ev.hook_event_name, ev.tool_name, c.note);
    onMutate();
    return { card: c, conflict };
  }

  // ------------------------------------------------------ hook endpoints
  function hookSessionStart(ev) {
    // 0.7.1 /clear succession, intercepted HERE because applyEvent → card()
    // births a card on first touch: by the time applyEvent runs, an unrecognised
    // heir would already have been dealt a fresh animal, a fresh hex suffix and
    // a "joined the fleet" tick — a second card for what the human considers the
    // same session. So before any of that: if this is an id we have never seen,
    // arriving with source='clear' (or 'compact', handled defensively — if that
    // one keeps its id, this branch simply never fires), look for the session in
    // this cwd that just cleared, and continue IT instead of starting a stranger.
    const sid = ev.session_id || 'unknown';
    if (ev.source === 'clear' || ev.source === 'compact') {
      const existing = q.getSession.get(sid);
      // Usually the heir is brand-new. But the agents-cli poller can beat its
      // SessionStart hook and pre-create the row, and a re-delivered hook would
      // find it too — so "no card yet" is not the test. The test is "this card
      // has done NOTHING yet and continues nobody": a placeholder, which we
      // adopt into the lineage (rename mode) instead of leaving it stranded.
      const placeholder = existing
        && existing.events === 0
        && existing.succeeded_by == null
        && !q.successorClaimed.get(sid)
        && !q.spawnBySession.get(sid);
      if (!existing || placeholder) {
        const prev = findClearedPredecessor(sid, ev.cwd, Date.now());
        // 0.16.0 corroboration: hooks now arrive authenticated through the
        // command shims, but cheap defense in depth costs nothing — when the
        // heir DOES present a transcript_path, it must live in the transcript
        // directory Claude derives from the shared cwd before we hand it the
        // predecessor's identity, pane, mail and questions. An absent
        // transcript_path falls back to the historical (cwd, time) inference —
        // reconcileClearForks' boot heal depends on payloads that lack it.
        if (prev && ev.transcript_path
          && path.dirname(String(ev.transcript_path)) !== expectedTranscriptDir(ev.cwd)) {
          logEvent(sid, 'ClearSuccessionRefused', null,
            `transcript ${String(ev.transcript_path).slice(0, 160)} does not match cwd ${String(ev.cwd).slice(0, 160)}`);
        } else if (prev) {
          succeedSession(prev, sid, { rename: !!existing });
        }
      }
    }
    const { card: c } = applyEvent({ ...ev, hook_event_name: 'SessionStart' });
    return { ok: true, callsign: c.callsign, brief: composeBrief(c) };
  }

  // 0.16.0: fleet-sessionstart.mjs reports the takeover it performed, so the
  // brief for THAT session can tell the human the rest of the fleet needs
  // restarts too. Counts come from http.mjs's legacy-tracking sets, but
  // derive owns the brief, so the daemon hands a probe through createCore's
  // options (see fleetd.mjs).
  function takeoverBriefLines(replacedVersion, legacy) {
    const lines = [`[FLEETDECK] The fleet daemon was just upgraded (replacing v${replacedVersion}).`];
    const n = legacy?.sessions?.length ?? 0;
    if (n > 0) {
      const names = legacy.sessions.slice(0, 8).map(s => String(s).slice(0, 8)).join(', ');
      lines.push(`[FLEETDECK] ${n} session(s) are still running pre-0.16.0 hooks (${names}${n > 8 ? ', …' : ''}) — they are dark on the board until restarted. Tell the human: restart those sessions when convenient; the board tracks which are left.`);
    }
    return lines;
  }

  function composeBrief(c) {
    const others = q.allSessions.all()
      .filter(s => s.session_id !== c.session_id && s.ended_at == null);
    const sameRepo = others.filter(s => (s.repo_id ?? null) === (c.repo_id ?? null));
    const elsewhere = others.filter(s => (s.repo_id ?? null) !== (c.repo_id ?? null));
    const otherRepos = new Set(elsewhere.map(s => s.repo_id ?? '(none)')).size;
    const repoLabel = c.repo_name ? ` in ${c.repo_name}` : '';
    const lines = [
      `[FLEETDECK] You are on the fleet board as "${c.callsign}"${c.ticket ? ` (ticket ${c.ticket})` : ''} — live at http://127.0.0.1:${port}`,
      sameRepo.length
        ? `Other active sessions${repoLabel} (${sameRepo.length}):`
        : `No other sessions active${repoLabel} right now.`,
      ...sameRepo.map(s =>
        `  - ${s.callsign} [${s.col}] ${s.note}${s.branch ? ' — ' + s.branch : ''}${s.worktree && s.worktree !== c.worktree ? ' @ ' + s.worktree : ''}`),
    ];
    if (elsewhere.length) {
      lines.push(`${elsewhere.length} more session${elsewhere.length === 1 ? '' : 's'} across ${otherRepos} other repo${otherRepos === 1 ? '' : 's'}.`);
    }
    lines.push('Fleetdeck will warn you in-context if you touch files another session is editing. Take those warnings seriously: coordinate, don’t clobber.');
    return lines.join('\n');
  }

  function hookUserPromptSubmit(ev) {
    const sid = ev.session_id || 'unknown';
    applyEvent({ ...ev, hook_event_name: 'UserPromptSubmit' });
    q.setBlocked.run(0, sid); // new turn started — clear the one-block-per-turn flag
    // F3e auto-resolution: activity settles this session's pending
    // permission/elicitation questions (live holds fail open with {};
    // freeform questions stay pending — they're the human's queue).
    questions.expireOnActivity(sid);
    const box = drainMail(sid);
    if (!box.length) return {};
    onMutate();
    return {
      hookSpecificOutput: {
        hookEventName: 'UserPromptSubmit',
        additionalContext: '[FLEETDECK]\n' + box.map(m => `✉ from ${m.from}: ${m.text}`).join('\n'),
      },
    };
  }

  // http.mjs routes BOTH /hook/PreToolUse and /hook/PostToolUse here (same
  // derivation branch as the spike). The conflict whisper must therefore
  // declare the caller's ACTUAL event name — a PreToolUse client that receives
  // hookSpecificOutput.hookEventName:'PostToolUse' may drop the mismatched
  // whisper (M-B2). The event's own hook_event_name is authoritative (Claude
  // sends it in every hook payload); fall back to 'PostToolUse' only when a
  // client omitted it.
  function hookPostToolUse(ev) {
    const eventName = ev.hook_event_name || 'PostToolUse';
    const { conflict } = applyEvent({ ...ev, hook_event_name: eventName });
    // F3e auto-resolution, correlated (M-B1): a single completed tool call must
    // settle ONLY its own permission hold, never a sibling parallel hold still
    // awaiting the human. Real PostToolUse payloads carry NO tool_use_id, so we
    // correlate on the identity they DO share with the held PermissionRequest —
    // (tool_name, tool_input). A hold for a DIFFERENT tool call, and every
    // freeform row, is left untouched; the turn-boundary UserPromptSubmit path
    // (hookUserPromptSubmit above) stays session-wide.
    questions.expireOnActivity(ev.session_id || 'unknown', { toolName: ev.tool_name, toolInput: ev.tool_input });
    if (!conflict) return {};
    return {
      hookSpecificOutput: {
        hookEventName: eventName,
        additionalContext: whisperText(conflict),
      },
    };
  }

  // Turn boundary: deliver queued mail by refusing to stop — AT MOST ONCE per
  // turn per session, enforced server-side via blocked_this_turn. The flag
  // clears on the next UserPromptSubmit or the next Stop that passes with no
  // mail. NEVER reads stop_hook_active. Stop is never a tombstone.
  function hookStop(ev) {
    const sid = ev.session_id || 'unknown';
    const c = card(sid);
    if (!c.blocked_this_turn) {
      const box = drainMail(sid);
      if (box.length) {
        q.setBlocked.run(1, sid);
        // telemetry for the blocked stop (card stays in-turn; no idle transition)
        updateSession(sid, { last_seen: Date.now(), events: c.events + 1, col: 'working', note: 'processing fleet mail' });
        logEvent(sid, 'Stop', null, 'mail delivered via block');
        tick(`✉ ${c.callsign} got fleet mail at the turn boundary`);
        onMutate();
        const msgs = box.map(m => `from ${m.from}: ${m.text}`).join(' | ');
        return {
          decision: 'block',
          reason: `[FLEETDECK MAIL] ${msgs} — Act on this if it affects your work (briefly), then finish your turn. Do not start unrelated work.`,
        };
      }
    }
    // Stop passes.
    applyEvent({ ...ev, hook_event_name: 'Stop' });
    const stillPending = q.pendingMail.all(sid).length > 0;
    if (!stillPending) q.setBlocked.run(0, sid); // cleared on a Stop that passes with no mail
    detectFreeform(ev); // F3d — only on a PASSING Stop (a block continues the turn)
    return {};
  }

  // ------------------------------------------------ F3d free-text questions
  // Runs on every Stop that PASSES (a Stop answered with a mail block skips
  // detection — the turn continues and a later Stop will look again).
  // Input preference: payload.last_assistant_message when the CLI sends one
  // (docs §6 say 2.1.206 does NOT — hook-payloads.jsonl capture pins the
  // truth), else the transcript tail at payload.transcript_path.
  function detectFreeform(ev) {
    const sid = ev.session_id || 'unknown';
    try {
      const fromPayload = typeof ev.last_assistant_message === 'string' && ev.last_assistant_message.trim()
        ? ev.last_assistant_message : null;
      const text = fromPayload ?? (ev.transcript_path ? lastAssistantText(ev.transcript_path) : null);
      const question = detectTrailingQuestion(text);
      if (!question) return;
      // one pending card per distinct question text per session (a re-Stop
      // with the same trailing question must not spam the queue)
      const dup = questions.pendingOf(sid).some(r => {
        if (r.kind !== 'freeform') return false;
        try { return JSON.parse(r.payload_json || '{}').text === question; } catch { return false; }
      });
      if (dup) return;
      questions.create('freeform', sid, { text: question });
      const c = card(sid);
      updateSession(sid, { col: 'needsyou', note: ('Q: ' + question).slice(0, 80) });
      logEvent(sid, 'Stop', null, 'trailing question → needsyou');
      tick(`❓ ${c.callsign} asked: ${question.slice(0, 60)}`);
      onMutate();
    } catch { /* detection is best-effort — never disturb the Stop response */ }
  }

  // ----------------------------------------- F3a/F3b/F3c hold-open intake
  // Creates the durable question row and applies telemetry (card → needsyou).
  // The HTTP layer parks the response and registers the hold via
  // questions.attachHold; all resolution paths live in questions.mjs.
  //
  // v1.3 plan CAPTURE (CONTRACT "B. Plan library"): an ExitPlanMode
  // PermissionRequest carries the full plan markdown in tool_input.plan.
  // Capture it BEFORE the hold, in the SAME synchronous tick as the question
  // row insert — a daemon crash mid-hold still has the plan. Capture is
  // unconditional and survives whatever happens to the planner/hold; the
  // question then holds NORMALLY (unlike the AskUserQuestion instant-{}
  // guard in http.mjs, which stays untouched).
  function hookHoldQuestion(ev, eventName) {
    const kind = eventName === 'Elicitation' ? 'elicitation'
      : eventName === 'AskUserQuestion' ? 'choice'
      : 'permission';
    applyEvent({ ...ev, hook_event_name: eventName });
    const sid = ev.session_id || 'unknown';
    const isPlan = eventName === 'PermissionRequest' && ev?.tool_name === 'ExitPlanMode';
    if (!isPlan) {
      const row = questions.create(kind, sid, ev);
      onMutate();
      return row;
    }
    // M-B6: the ExitPlanMode question row and its captured plan row must both
    // persist or NEITHER. They used to be two independent inserts with the
    // plan insert's errors swallowed — a crash/SQLITE error in between left a
    // held question with no linked plan, so the board's `capture` answer path
    // (planByQuestion → null) could never satisfy it. Wrap both inserts in one
    // transaction; on any failure roll BOTH back and fail the hook OPEN (return
    // null → http.mjs answers {} and the terminal resumes normally), rather
    // than relaying a hold the library can never honour.
    let row = null;
    let planRowId = null;
    let callsign = null;
    db.exec('BEGIN IMMEDIATE');
    try {
      row = questions.create(kind, sid, ev);
      const c = q.getSession.get(sid); // applyEvent ensured the card exists
      callsign = c?.callsign ?? sid;
      const planMd = typeof ev.tool_input?.plan === 'string'
        ? ev.tool_input.plan
        : String(ev.tool_input?.plan ?? '');
      const info = q.insertPlan.run(sid, c?.callsign ?? null, c?.repo_id ?? null,
        c?.repo_name ?? null, row.id, planMd, Date.now());
      planRowId = Number(info.lastInsertRowid);
      db.exec('COMMIT');
    } catch (err) {
      try { db.exec('ROLLBACK'); } catch { /* keep the original error */ }
      console.error('fleetd plan capture error (question + plan rolled back, hook fails open):', err);
      onMutate();
      return null;
    }
    tick(`📋 ${callsign} proposed a plan — captured to the library (#${planRowId})`);
    onMutate();
    return row;
  }

  // SessionEnd: THE tombstone — pending hold-kind questions die with it;
  // freeform questions outlive the session (answer deliverable on --resume).
  function hookSessionEnd(ev) {
    const sid = ev.session_id || 'unknown';
    applyEvent({ ...ev, hook_event_name: 'SessionEnd' });
    // BUG 1: a /clear (reason='clear') is NOT a session end — see the guarded
    // SessionEnd case in applyEvent above, which keeps the card live. Mirror
    // that here: do NOT mark the pane 'pane-dead' and do NOT drop the model
    // memo (same session_id, same transcript, still running). The terminal
    // wiped whatever hold-kind question was drawn there, so those live holds
    // are expired (they'd otherwise wait forever on a prompt that is gone);
    // freeform rows survive — they are the human's queue.
    questions.expireAllForSession(sid);
    if (ev.reason === 'clear') {
      // 0.7.1: applyEvent has just stamped cleared_at. Normally the heir's
      // SessionStart arrives next and claims this card. But SessionEnd is an
      // ASYNC hook while SessionStart is not, so the heir can already be here,
      // sitting on its own orphan card — look forward and claim it now, or the
      // pair stays split until the next boot heal.
      succeedForwardFromClear(sid, ev.cwd);
      // F3d-2: still wake watchers so a poll re-checks, but the session is
      // deliberately left live — pane and card untouched.
      notifyWatchers(sid);
      return {};
    }
    // 0.7.0 Move-to-tmux armed auto-adopt (CONTRACT): the human armed this
    // card ("move to tmux") while the CLI was live — two processes can't drive
    // one conversation, so the adopt waited for the CLI to exit, which is NOW.
    // This sits AFTER the reason==='clear' early-return above: a /clear is not
    // an end and KEEPS the arm (the session is still live, still armed). Read
    // the durable arm columns (applyEvent leaves them untouched) and fire the
    // adopt fire-and-forget after a short grace (ADOPT_DELAY_MS, default
    // 750 ms, 0 in tests) that lets the CLI flush the final transcript lines
    // the resume will read.
    const armed = q.getSession.get(sid);
    if (armed && armed.adopt_armed_until != null && armed.adopt_armed_until > Date.now()) {
      // The arm is NOT cleared here: it is durable intent, consumed by
      // adoptSession itself inside its single-flight claim. That buys three
      // things the old clear-first scheme lost: a disarm click landing inside
      // the grace window genuinely cancels the move (the deferred call finds
      // the arm gone and stands down); a daemon death inside the window leaves
      // the arm in SQLite for the retention sweep to complete at next boot; and
      // one-shot still holds — whoever consumes the arm first wins, nothing
      // ever retries a failed launch.
      const skip = !!armed.adopt_armed_skip;
      const timer = setTimeout(() => {
        adoptSession(sid, { dangerously_skip_permissions: skip }, { deferred: true })
          .then(out => {
            // One loud ticker line on REAL failures only. 409s are benign
            // races — a manual adopt-now click (or a revive) beat the timer and
            // the move actually happened; shouting "failed" over a success was
            // the confirmed false-alarm bug. canceled:true outcomes already
            // ticked (or deliberately stayed silent) inside adoptSession.
            if (!out || (out.status >= 400 && out.status !== 409)) {
              const c = q.getSession.get(sid);
              tick(`✗ move-to-tmux failed for ${c?.callsign ?? sid}: ${(out?.body?.reason ?? 'unknown')}`.slice(0, 100));
            }
          })
          .catch(err => {
            const c = q.getSession.get(sid);
            tick(`✗ move-to-tmux failed for ${c?.callsign ?? sid}: ${String(err?.message || err)}`.slice(0, 100));
          });
      }, ADOPT_DELAY_MS);
      timer.unref?.(); // never keep the daemon alive for a deferred adopt
    }
    modelMemo.delete(sid); // a revive re-stamps the floor at its SessionStart
    // v1.2: SessionEnd on a spawned session does NOT kill its pane — the
    // human may want the scrollback (CONTRACT). It just updates the row: the
    // pane no longer hosts a live claude session, so it stops counting as live
    // right now (the ~10 s liveness tick would reach the same verdict once the
    // pane's claude exits, but under the FLEETDECK_SPAWN_CMD override there is
    // no pane to observe at all — this direct update is the only path there).
    const sp = q.spawnBySession.get(sid);
    if (sp && (sp.status === 'spawning' || sp.status === 'stalled' || sp.status === 'live')) {
      q.setSpawnStatus.run('pane-dead', sp.spawn_id);
      forgetSpawn(sp.spawn_id); // M-G2: terminal-ish — drop nudge/harvest ephemera
    }
    // F3d-2: wake any /api/watch long-poll so its watcher sees
    // session_alive:false and exits now instead of at its hold timeout.
    notifyWatchers(sid);
    return {};
  }

  return {
    applyEvent, hookSessionStart, hookUserPromptSubmit, hookPostToolUse,
    hookStop, hookSessionEnd, hookHoldQuestion, takeoverBriefLines,
  };
}
