import { useCallback, useState } from 'react';
import { cleanup, killSpawn, reasonOf, renameSession, armUnsupervised } from '../api.js';
import { safeUrl, validSuffix } from '../util.js';

// The board-level fleet mutations that all report their outcome onto the shared
// feedback strip: Clear, revive (single + all), enable-remote, and the two-step
// kill (ASK opens the dialog; the POST fires only from the dialog's hazard
// button). revive/enable-remote POST + in-flight guards live in useSpawnActions
// — this hook only wraps them with the strip reporter — so the card chip and the
// drawer button can never each fire a second POST.
//
// showNote comes from useFeedbackStrip; the spawn actions come from
// useSpawnActions (App holds one instance and shares it with the drawer too).
export function useFleetActions({ showNote, revive, reviveAll, enableRemoteAction, adopt, spawnShellAction }) {
  const [clearing, setClearing] = useState(false);
  // v1.8 kill — the card chip and the drawer button both open ONE dialog; the
  // POST only fires from its hazard button. null | {spawnId, callsign, window, alive}
  const [killAsk, setKillAsk] = useState(null);
  const [killBusy, setKillBusy] = useState(false);
  // v2.0 Move-to-tmux — the card chip (offline 'now' OR live 'arm') opens ONE
  // dialog; the POST fires only from its confirm button. null | {sessionId,
  // callsign, live}. `live` picks the dialog's copy variant AND, on the daemon,
  // whether the click adopts now or arms a deferred move.
  const [armAsk, setArmAsk] = useState(null);
  const [armBusy, setArmBusy] = useState(false);
  // v2.1 Rename — the card's ✎ chip and the drawer's ✎ button both open ONE
  // dialog; the POST fires only from it. null | {sessionId, callsign, window}.
  // `window` is the pane's tmux window when the board owns one — it is ABSENT on
  // a card the board never spawned, which is fine: rename is a metadata action,
  // not a pane action, so EVERY live card can be renamed and the dialog simply
  // drops the "your tmux window keeps its name" line when there is no window.
  const [renameAsk, setRenameAsk] = useState(null);
  const [renameBusy, setRenameBusy] = useState(false);

  // Fix D — the daemon archives offline sessions, expires their mail/questions,
  // kills dead scoped panes, and LISTS (never deletes) orphaned worktrees for
  // the human to remove. Orphan paths need reading time — that strip stays until
  // dismissed.
  const doClear = async () => {
    if (clearing) return;
    setClearing(true);
    const res = await cleanup();
    if (res.ok && res.json?.ok !== false) {
      const j = res.json || {};
      const orphans = Array.isArray(j.orphan_worktrees) ? j.orphan_worktrees : [];
      const msg = `cleared ${j.archived ?? 0} offline · ${j.conflicts_cleared ?? 0} conflicts`
        + ` · ${(j.questions_purged ?? 0) + (j.questions_expired ?? 0)} questions`
        + ` · ${j.mail_expired ?? 0} mail · ${j.windows_killed ?? 0} windows · feed wiped`;
      showNote({ msg, orphans }, orphans.length ? 0 : 8000);
    } else {
      showNote({ err: reasonOf(res, `clear failed (${res.status})`) }, 8000);
    }
    setClearing(false);
  };

  // v1.5 — revive dead board-spawned agents (spawn.revivable). Success is
  // silent: the daemon moves the card to QUEUED ("reviving…") and it flips live
  // on the resumed session's first hook. Only failures hit the strip.
  const doRevive = useCallback((s) => {
    revive(s, (r) => {
      if (!r.ok) showNote({ hd: '✗ REVIVE', err: `${s.callsign || s.spawn?.spawn_id} — ${r.reason}` }, 8000);
    });
  }, [revive, showNote]);
  // Revive all (OFFLINE column head): sequential POSTs, one summary note.
  const doReviveAll = useCallback((list) => {
    reviveAll(list, ({ okN, total, fails }) => {
      if (fails.length === 0) {
        showNote({ hd: '✓ REVIVE', msg: `revived ${okN}/${total} — cards move to QUEUED` }, 8000);
      } else {
        // failure reasons need reading time — stays until dismissed
        showNote({ hd: '✗ REVIVE', err: `revived ${okN}/${total} — ${fails.join('  ·  ')}` }, 0);
      }
    });
  }, [reviveAll, showNote]);

  // v1.6 — put a board-spawned agent on remote control (card chip; the drawer's
  // OWNED PANE button reports inline instead, off the SAME shared POST). Success
  // surfaces on the strip — with the claude.ai link when the harvest beat the
  // response (and only when safeUrl vouches for it — M-S1). Failures (409
  // mid-turn races, dead pane) surface the reason.
  const doEnableRemote = useCallback((s) => {
    const label = s.callsign || s.spawn?.spawn_id;
    enableRemoteAction(s, (r) => {
      if (!r.ok) {
        showNote({ hd: '✗ REMOTE', err: `${label} — ${r.reason}` }, 8000);
        return;
      }
      const url = safeUrl(r.url);
      if (url) {
        // the link needs reading/tapping time — stays until dismissed
        showNote({ hd: '✓ REMOTE', msg: `${label} on remote control —`, url }, 0);
      } else {
        showNote({
          hd: '✓ REMOTE',
          msg: `${label} on remote control — ${r.pending
            ? 'still harvesting the claude.ai link; it lands on the card chip'
            : 'claude.ai link not captured — check the agent’s terminal (▣)'}`,
        }, 8000);
      }
    });
  }, [enableRemoteAction, showNote]);

  const doSpawnShell = useCallback((s) => {
    const label = s.callsign || s.session_id;
    spawnShellAction(s, (r) => {
      if (!r.ok) {
        showNote({ hd: '✗ SHELL', err: `${label} — ${r.reason}` }, 8000);
      } else {
        showNote({ hd: '✓ SHELL', msg: `${r.callsign || 'shell'} — terminal opened in ${s.worktree || s.cwd}` }, 8000);
      }
    });
  }, [spawnShellAction, showNote]);

  // v1.8 — kill a board-spawned agent. The card chip and the drawer button only
  // ASK (this opens the dialog); the POST fires from the dialog's hazard button
  // alone. Success is quiet on the board itself — the card goes OFFLINE on the
  // next snapshot — so the strip carries the confirmation, and every refusal
  // (409 not-offline, 410 gone, 404 unknown) reaches it verbatim.
  const askKill = useCallback((s) => {
    if (!s?.spawn?.spawn_id) return;
    setKillAsk({
      spawnId: s.spawn.spawn_id,
      callsign: s.callsign || s.session_id,
      window: s.spawn.tmux_window || '',
      alive: s.col !== 'offline',
    });
  }, []);
  const doKill = async () => {
    if (!killAsk || killBusy) return;
    const { spawnId, callsign, alive } = killAsk;
    setKillBusy(true);
    // force:true is REQUIRED for a card that isn't offline — the daemon 409s
    // otherwise. `alive` is exactly that condition (see the dialog's warning).
    const res = await killSpawn(spawnId, alive);
    if (res.ok && res.json?.ok !== false) {
      showNote({ hd: '✓ KILLED', msg: `${callsign} — pane killed · worktree and branch left on disk` }, 8000);
    } else {
      // res.reason is the daemon's reason (null when it sent none) — status
      // gives the fallback sentence; a network drop reads "daemon unreachable".
      const reason = res.reason;
      const msg =
        res.status === 409 ? (reason || 'refused — session is not offline (409)')
        : res.status === 410 ? (reason || 'window already gone (410)')
        : res.status === 404 ? (reason || 'unknown spawn (404)')
        : (reason || `kill failed (${res.status})`);
      showNote({ hd: '✗ KILL', err: `${callsign} — ${msg}` }, 8000);
    }
    setKillBusy(false);
    setKillAsk(null);
  };

  // v2.0 Move-to-tmux — report the branch the daemon took onto the shared strip.
  // A single POST can come back adopted (ended card → moved now) or armed (live
  // card → deferred until you exit the CLI), and either can fail; all three land
  // here so the copy stays in one place.
  const reportAdopt = useCallback((label, r) => {
    if (!r.ok) {
      showNote({ hd: '✗ MOVE', err: `${label} — ${r.reason}` }, 8000);
    } else if (r.armed) {
      // the move is deferred: nothing appears until the human exits the CLI
      showNote({ hd: '⧗ ARMED', msg: `${label} — armed; exit this session in your terminal to complete the move · the arm expires in ~30 min` }, 8000);
    } else {
      showNote({ hd: '✓ MOVE', msg: `${label} — moving to tmux; the card returns to QUEUED` }, 8000);
    }
  }, [showNote]);

  // Fire the adopt/arm POST through the shared single-owner action (per-session
  // lock in useSpawnActions). skip → dangerously_skip_permissions:true (the
  // dialog's two-step unsupervised gate); the safe default sends {}. 0.16.0: a
  // skip adopt must also echo a fresh arm token from the daemon — mint it here
  // and fold a refusal into the same strip reporter. Awaits the action's OWN
  // promise (which always resolves — even on an in-flight early-return) so the
  // dialog can close without ever hanging on the strip reporter, which only
  // fires when a POST actually went out.
  const doAdopt = useCallback(async (s, { skip } = {}) => {
    const body = {};
    if (skip) {
      body.dangerously_skip_permissions = true;
      try {
        const arm = await armUnsupervised();
        if (arm.ok && arm.json?.arm_token) body.arm_token = arm.json.arm_token;
      } catch { /* fall through: the daemon's 403 reason is the honest error */ }
    }
    await adopt(s, body, (r) => {
      reportAdopt(s.callsign || s.session_id, r);
    });
  }, [adopt, reportAdopt]);

  // Open the Move-to-tmux dialog. Both eligibility states funnel here: an ended
  // 'now' card and a live 'arm' card open the SAME dialog, differing only in the
  // copy `live` selects — the daemon, not the board, decides adopt-vs-arm.
  const askArm = useCallback((s) => {
    if (!s?.session_id) return;
    setArmAsk({
      sessionId: s.session_id,
      callsign: s.callsign || s.session_id,
      live: s.col !== 'offline',
    });
  }, []);
  // The dialog's confirm — mirrors doKill: hold the dialog (armBusy) through the
  // POST, then close on completion (success OR failure; the reason lands on the
  // strip either way). `skip` comes from the dialog's unsupervised gate.
  const doArm = useCallback(async (skip) => {
    const a = armAsk;
    if (!a || armBusy) return;
    setArmBusy(true);
    await doAdopt({ session_id: a.sessionId, callsign: a.callsign }, { skip });
    setArmBusy(false);
    setArmAsk(null);
  }, [armAsk, armBusy, doAdopt]);

  // The armed chip's click: cancel the deferred move immediately, no dialog
  // (nothing hazardous is being undone — worst case is a card that stays put).
  const doDisarm = useCallback((s) => {
    const label = s.callsign || s.session_id;
    adopt(s, { disarm: true }, (r) => {
      if (!r.ok) showNote({ hd: '✗ MOVE', err: `${label} — ${r.reason}` }, 8000);
      else showNote({ hd: '✓ DISARMED', msg: `${label} — move canceled; the card stays where it is` }, 8000);
    });
  }, [adopt, showNote]);

  // v2.1 Rename — open the dialog. Offered on ANY live card: a rename touches
  // only the session's name, so a session the board never spawned (no `s.spawn`)
  // is as renameable as a board-owned pane — hence the optional-chain on the
  // window rather than a spawn guard like askKill's.
  const askRename = useCallback((s) => {
    if (!s?.session_id) return;
    setRenameAsk({
      sessionId: s.session_id,
      callsign: s.callsign || s.session_id,
      window: s.spawn?.tmux_window || '',
    });
  }, []);

  // Both rename outcomes report through one place, because {suffix} and
  // {clear:true} answer in the SAME shape and their copy must not drift.
  // `renamed:false` is a success (the name simply did not change — you cleared a
  // name that was already automatic), so it reads as one, in green, not as a
  // silent no-op the human is left to wonder about.
  const reportRename = useCallback((label, res) => {
    if (!(res.ok && res.json?.ok !== false)) {
      showNote({ hd: '✗ NAME', err: `${label} — ${reasonOf(res, `rename failed (${res.status})`)}` }, 8000);
      return;
    }
    const j = res.json || {};
    showNote({
      hd: '✓ NAME',
      msg: j.renamed
        ? `${j.previous || label} is now ${j.callsign}`
        : `${j.callsign || label} — already its name; nothing changed`,
    }, 8000);
  }, [showNote]);

  // The dialog's confirm — mirrors doKill/doArm: hold the dialog (renameBusy)
  // through the POST, then close on completion, success OR failure (the daemon's
  // reason lands on the strip either way, so the dialog never has to carry it).
  const doRename = useCallback(async (suffix) => {
    const a = renameAsk;
    if (!a || renameBusy) return;
    // The dialog already disables the confirm on an invalid suffix — this is the
    // second latch, so a stray ⏎ can never POST a name the daemon must reject.
    // (The daemon validates it again regardless: it, not the board, is the
    // authority on names.)
    if (!validSuffix(suffix)) return;
    setRenameBusy(true);
    const res = await renameSession(a.sessionId, { suffix });
    reportRename(a.callsign, res);
    setRenameBusy(false);
    setRenameAsk(null);
  }, [renameAsk, renameBusy, reportRename]);

  // The dialog's quiet "reset to the automatic name" — {clear:true} reverts to
  // the ticket name when the session has a ticket, else to the hex name. No
  // second confirmation: nothing is destroyed, and the name it lands on is the
  // one the daemon would have given it anyway.
  const doResetName = useCallback(async () => {
    const a = renameAsk;
    if (!a || renameBusy) return;
    setRenameBusy(true);
    const res = await renameSession(a.sessionId, { clear: true });
    reportRename(a.callsign, res);
    setRenameBusy(false);
    setRenameAsk(null);
  }, [renameAsk, renameBusy, reportRename]);

  return {
    clearing, doClear,
    killAsk, setKillAsk, killBusy, askKill, doKill,
    armAsk, setArmAsk, armBusy, askArm, doArm, doDisarm,
    renameAsk, setRenameAsk, renameBusy, askRename, doRename, doResetName,
    doRevive, doReviveAll, doEnableRemote, doSpawnShell,
  };
}
