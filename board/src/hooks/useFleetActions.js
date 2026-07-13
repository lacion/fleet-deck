import { useCallback, useState } from 'react';
import { cleanup, killSpawn, reasonOf } from '../api.js';
import { safeUrl } from '../util.js';

// The board-level fleet mutations that all report their outcome onto the shared
// feedback strip: Clear, revive (single + all), enable-remote, and the two-step
// kill (ASK opens the dialog; the POST fires only from the dialog's hazard
// button). revive/enable-remote POST + in-flight guards live in useSpawnActions
// — this hook only wraps them with the strip reporter — so the card chip and the
// drawer button can never each fire a second POST.
//
// showNote comes from useFeedbackStrip; the spawn actions come from
// useSpawnActions (App holds one instance and shares it with the drawer too).
export function useFleetActions({ showNote, revive, reviveAll, enableRemoteAction }) {
  const [clearing, setClearing] = useState(false);
  // v1.8 kill — the card chip and the drawer button both open ONE dialog; the
  // POST only fires from its hazard button. null | {spawnId, callsign, window, alive}
  const [killAsk, setKillAsk] = useState(null);
  const [killBusy, setKillBusy] = useState(false);

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

  return {
    clearing, doClear,
    killAsk, setKillAsk, killBusy, askKill, doKill,
    doRevive, doReviveAll, doEnableRemote,
  };
}
