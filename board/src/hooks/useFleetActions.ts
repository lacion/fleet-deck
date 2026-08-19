import { useCallback, useRef, useState } from 'react';
import {
  cleanup,
  killSpawn,
  reasonOf,
  renameSession,
  armUnsupervised,
  dismissSession,
} from '../api.ts';
import type { ApiResult } from '../api.ts';
import { safeUrl, validSuffix } from '../util.ts';
import type {
  AdoptBody,
  AdoptResult,
  EnableResult,
  ReviveAllResult,
  ReviveResult,
  ShellResult,
} from '../useSpawnActions.ts';
import type { FeedbackNote } from './useFeedbackStrip.ts';

// The defensively-accessed slice of a session these mutations read. Callers pass
// a raw session (a real SessionEntry) and each field is reached behind a `?.` (an
// older daemon may omit `spawn`, and the ask* handlers accept a possibly-absent
// card), so this structural view keeps every guard honest while the real type
// still flows in unchanged.
interface FleetSpawnLike {
  spawn_id: string;
  tmux_window: string | null;
  status?: string;
}
interface Session {
  session_id: string;
  callsign: string;
  col?: string;
  worktree?: string | null;
  cwd?: string | null;
  spawn?: FleetSpawnLike;
}
// doAdopt only ever needs the session's identity — doArm builds it from armAsk.
interface AdoptTarget {
  session_id: string;
  callsign: string;
}

type ShowNote = (note: FeedbackNote, ms: number) => void;

// The daemon response fields these mutations read that api.ts's ApiJson does not
// (yet) name — read through a structural cast so each stays honest without
// widening the shared ApiJson (the daemon, not the browser, is the shape
// authority; see api.ts).
interface ClearJson {
  ok?: boolean;
  orphan_worktrees?: string[];
  archived?: number;
  conflicts_cleared?: number;
  questions_purged?: number;
  questions_expired?: number;
  mail_expired?: number;
  windows_killed?: number;
}
interface ArmJson {
  arm_token?: string;
}
interface RenameJson {
  ok?: boolean;
  renamed?: boolean;
  previous?: string | null;
  callsign?: string | null;
}

// The two-step dialogs' pending state (null when closed) — each ask* fills one.
interface KillAsk {
  spawnId: string;
  callsign: string;
  window: string;
  alive: boolean;
  provisioning: boolean;
}
interface ArmAsk {
  sessionId: string;
  callsign: string;
  live: boolean;
}
interface RenameAsk {
  sessionId: string;
  callsign: string;
  window: string;
}

// The spawn actions (from useSpawnActions) plus showNote (from useFeedbackStrip)
// this hook wraps. They are all async (useSpawnActions returns Promises); this
// hook fires revive/reviveAll/enable/shell and forgets them (their Promise is
// discarded with `void`), while adopt is awaited (→ Promise).
interface UseFleetActionsArgs {
  showNote: ShowNote;
  revive: (s: Session, onResult?: (r: ReviveResult) => void) => Promise<void>;
  reviveAll: (list: readonly Session[], onResult?: (r: ReviveAllResult) => void) => Promise<void>;
  enableRemoteAction: (s: Session, onResult?: (r: EnableResult) => void) => Promise<void>;
  adopt: (s: AdoptTarget, body: AdoptBody, onResult?: (r: AdoptResult) => void) => Promise<void>;
  spawnShellAction: (s: Session, onResult?: (r: ShellResult) => void) => Promise<void>;
}

// The board-level fleet mutations that all report their outcome onto the shared
// feedback strip: Clear, revive (single + all), enable-remote, and the two-step
// kill (ASK opens the dialog; the POST fires only from the dialog's hazard
// button). revive/enable-remote POST + in-flight guards live in useSpawnActions
// — this hook only wraps them with the strip reporter — so the card chip and the
// drawer button can never each fire a second POST.
//
// showNote comes from useFeedbackStrip; the spawn actions come from
// useSpawnActions (App holds one instance and shares it with the drawer too).
export function useFleetActions({
  showNote,
  revive,
  reviveAll,
  enableRemoteAction,
  adopt,
  spawnShellAction,
}: UseFleetActionsArgs) {
  const [clearing, setClearing] = useState(false);
  const clearingRef = useRef(false);
  // v1.8 kill — the card chip and the drawer button both open ONE dialog; the
  // POST only fires from its hazard button. null | {spawnId, callsign, window, alive}
  const [killAsk, setKillAsk] = useState<KillAsk | null>(null);
  const [killBusy, setKillBusy] = useState(false);
  const killBusyRef = useRef(false);
  // v2.0 Move-to-tmux — the card chip (offline 'now' OR live 'arm') opens ONE
  // dialog; the POST fires only from its confirm button. null | {sessionId,
  // callsign, live}. `live` picks the dialog's copy variant AND, on the daemon,
  // whether the click adopts now or arms a deferred move.
  const [armAsk, setArmAsk] = useState<ArmAsk | null>(null);
  const [armBusy, setArmBusy] = useState(false);
  const armBusyRef = useRef(false);
  // v2.1 Rename — the card's ✎ chip and the drawer's ✎ button both open ONE
  // dialog; the POST fires only from it. null | {sessionId, callsign, window}.
  // `window` is the pane's tmux window when the board owns one — it is ABSENT on
  // a card the board never spawned, which is fine: rename is a metadata action,
  // not a pane action, so EVERY live card can be renamed and the dialog simply
  // drops the "your tmux window keeps its name" line when there is no window.
  const [renameAsk, setRenameAsk] = useState<RenameAsk | null>(null);
  const [renameBusy, setRenameBusy] = useState(false);
  const renameBusyRef = useRef(false);
  // Item 3 dismiss — a per-session in-flight guard (ref = synchronous source of
  // truth at click time; the state Set mirrors it so the chip can show/disable
  // "dismissing…"). Keyed on session_id: the card being dismissed is offline and
  // may have no live spawn row, so the lock is the session, like adopt's.
  const dismissRef = useRef(new Set<string>());
  const [dismissing, setDismissing] = useState(() => new Set<string>());

  // Fix D — the daemon archives offline sessions, expires their mail/questions,
  // kills dead scoped panes, and LISTS (never deletes) orphaned worktrees for
  // the human to remove. Orphan paths need reading time — that strip stays until
  // dismissed.
  const doClear = async () => {
    if (clearingRef.current) return;
    clearingRef.current = true;
    setClearing(true);
    try {
      const res = await cleanup();
      if (res.ok && res.json?.ok !== false) {
        const j = (res.json ?? {}) as ClearJson;
        const orphans = Array.isArray(j.orphan_worktrees) ? j.orphan_worktrees : [];
        const msg =
          `cleared ${j.archived ?? 0} offline · ${j.conflicts_cleared ?? 0} conflicts` +
          ` · ${(j.questions_purged ?? 0) + (j.questions_expired ?? 0)} questions` +
          ` · ${j.mail_expired ?? 0} mail · ${j.windows_killed ?? 0} windows · feed wiped`;
        showNote({ msg, orphans }, orphans.length ? 0 : 8000);
      } else {
        showNote({ err: reasonOf(res, `clear failed (${res.status})`) }, 8000);
      }
    } finally {
      clearingRef.current = false;
      setClearing(false);
    }
  };

  // v1.5 — revive dead board-spawned agents (spawn.revivable). Success is
  // silent: the daemon moves the card to QUEUED ("reviving…") and it flips live
  // on the resumed session's first hook. Only failures hit the strip.
  const doRevive = useCallback(
    (s: Session) => {
      void revive(s, (r) => {
        if (!r.ok)
          showNote(
            { hd: '✗ REVIVE', err: `${s.callsign || (s.spawn?.spawn_id ?? '')} — ${r.reason}` },
            8000,
          );
      });
    },
    [revive, showNote],
  );
  // Revive all (OFFLINE column head): sequential POSTs, one summary note.
  const doReviveAll = useCallback(
    (list: readonly Session[]) => {
      void reviveAll(list, ({ okN, total, fails }) => {
        if (fails.length === 0) {
          showNote({ hd: '✓ REVIVE', msg: `revived ${okN}/${total} — cards move to QUEUED` }, 8000);
        } else {
          // failure reasons need reading time — stays until dismissed
          showNote({ hd: '✗ REVIVE', err: `revived ${okN}/${total} — ${fails.join('  ·  ')}` }, 0);
        }
      });
    },
    [reviveAll, showNote],
  );

  // v1.6 — put a board-spawned agent on remote control (card chip; the drawer's
  // OWNED PANE button reports inline instead, off the SAME shared POST). Success
  // surfaces on the strip — with the claude.ai link when the harvest beat the
  // response (and only when safeUrl vouches for it — M-S1). Failures (409
  // mid-turn races, dead pane) surface the reason.
  const doEnableRemote = useCallback(
    (s: Session) => {
      const label = s.callsign || (s.spawn?.spawn_id ?? '');
      void enableRemoteAction(s, (r) => {
        if (!r.ok) {
          showNote({ hd: '✗ REMOTE', err: `${label} — ${r.reason}` }, 8000);
          return;
        }
        const url = safeUrl(r.url);
        if (url) {
          // the link needs reading/tapping time — stays until dismissed
          showNote({ hd: '✓ REMOTE', msg: `${label} on remote control —`, url }, 0);
        } else {
          showNote(
            {
              hd: '✓ REMOTE',
              msg: `${label} on remote control — ${
                r.pending
                  ? 'still harvesting the claude.ai link; it lands on the card chip'
                  : 'claude.ai link not captured — check the agent’s terminal (▣)'
              }`,
            },
            8000,
          );
        }
      });
    },
    [enableRemoteAction, showNote],
  );

  const doSpawnShell = useCallback(
    (s: Session) => {
      const label = s.callsign || s.session_id;
      void spawnShellAction(s, (r) => {
        if (!r.ok) {
          showNote({ hd: '✗ SHELL', err: `${label} — ${r.reason}` }, 8000);
        } else {
          showNote(
            {
              hd: '✓ SHELL',
              msg: `${(r.callsign ?? '') || 'shell'} — terminal opened in ${(s.worktree ?? '') || (s.cwd ?? '')}`,
            },
            8000,
          );
        }
      });
    },
    [spawnShellAction, showNote],
  );

  // v1.8 — kill a board-spawned agent. The card chip and the drawer button only
  // ASK (this opens the dialog); the POST fires from the dialog's hazard button
  // alone. Success is quiet on the board itself — the card goes OFFLINE on the
  // next snapshot — so the strip carries the confirmation, and every refusal
  // (409 not-offline, 410 gone, 404 unknown) reaches it verbatim.
  const askKill = useCallback((s: Session | null | undefined) => {
    if (!s?.spawn?.spawn_id) return;
    setKillAsk({
      spawnId: s.spawn.spawn_id,
      callsign: s.callsign || s.session_id,
      window: s.spawn.tmux_window ?? '',
      alive: s.col !== 'offline',
      provisioning: s.spawn.status === 'provisioning',
    });
  }, []);
  const doKill = async () => {
    if (!killAsk || killBusyRef.current) return;
    const { spawnId, callsign, alive } = killAsk;
    killBusyRef.current = true;
    setKillBusy(true);
    // force:true is REQUIRED for a card that isn't offline — the daemon 409s
    // otherwise. `alive` is exactly that condition (see the dialog's warning).
    const res = await killSpawn(spawnId, alive);
    if (res.ok && res.json?.ok !== false) {
      const cancelling = res.json?.status === 'cancelling';
      const cancelled = res.json?.status === 'cancelled';
      showNote(
        cancelling
          ? { hd: '◌ CANCELING', msg: `${callsign} — stopping repository provisioning…` }
          : cancelled
            ? { hd: '✓ CANCELED', msg: `${callsign} — repository provisioning canceled` }
            : {
                hd: '✓ KILLED',
                msg: `${callsign} — pane killed · worktree and branch left on disk`,
              },
        8000,
      );
    } else {
      // res.reason is the daemon's reason (null when it sent none) — status
      // gives the fallback sentence; a network drop reads "daemon unreachable".
      const reason = res.reason ?? '';
      const msg =
        res.status === 409
          ? reason || 'refused — session is not offline (409)'
          : res.status === 410
            ? reason || 'window already gone (410)'
            : res.status === 404
              ? reason || 'unknown spawn (404)'
              : reason || `kill failed (${res.status})`;
      showNote({ hd: '✗ KILL', err: `${callsign} — ${msg}` }, 8000);
    }
    killBusyRef.current = false;
    setKillBusy(false);
    setKillAsk(null);
  };

  // v2.0 Move-to-tmux — report the branch the daemon took onto the shared strip.
  // A single POST can come back adopted (ended card → moved now) or armed (live
  // card → deferred until you exit the CLI), and either can fail; all three land
  // here so the copy stays in one place.
  const reportAdopt = useCallback(
    (label: string, r: AdoptResult) => {
      if (!r.ok) {
        showNote({ hd: '✗ MOVE', err: `${label} — ${r.reason}` }, 8000);
      } else if (r.armed) {
        // the move is deferred: nothing appears until the human exits the CLI
        showNote(
          {
            hd: '⧗ ARMED',
            msg: `${label} — armed; exit this session in your terminal to complete the move · the arm expires in ~30 min`,
          },
          8000,
        );
      } else {
        showNote(
          { hd: '✓ MOVE', msg: `${label} — moving to tmux; the card returns to QUEUED` },
          8000,
        );
      }
    },
    [showNote],
  );

  // Fire the adopt/arm POST through the shared single-owner action (per-session
  // lock in useSpawnActions). skip → dangerously_skip_permissions:true (the
  // dialog's two-step unsupervised gate); the safe default sends {}. 0.16.0: a
  // skip adopt must also echo a fresh arm token from the daemon — mint it here
  // and fold a refusal into the same strip reporter. Awaits the action's OWN
  // promise (which always resolves — even on an in-flight early-return) so the
  // dialog can close without ever hanging on the strip reporter, which only
  // fires when a POST actually went out.
  const doAdopt = useCallback(
    async (s: AdoptTarget, { skip }: { skip?: boolean } = {}) => {
      const body: AdoptBody = {};
      if (skip) {
        body.dangerously_skip_permissions = true;
        try {
          const arm = await armUnsupervised();
          const j = arm.json as ArmJson | null;
          if (arm.ok && j?.arm_token) body.arm_token = j.arm_token;
        } catch {
          /* fall through: the daemon's 403 reason is the honest error */
        }
      }
      await adopt(s, body, (r) => {
        reportAdopt(s.callsign || s.session_id, r);
      });
    },
    [adopt, reportAdopt],
  );

  // Open the Move-to-tmux dialog. Both eligibility states funnel here: an ended
  // 'now' card and a live 'arm' card open the SAME dialog, differing only in the
  // copy `live` selects — the daemon, not the board, decides adopt-vs-arm.
  const askArm = useCallback((s: Session | null | undefined) => {
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
  const doArm = useCallback(
    async (skip: boolean) => {
      const a = armAsk;
      if (!a || armBusyRef.current) return;
      armBusyRef.current = true;
      setArmBusy(true);
      try {
        await doAdopt({ session_id: a.sessionId, callsign: a.callsign }, { skip });
      } finally {
        armBusyRef.current = false;
        setArmBusy(false);
        setArmAsk(null);
      }
    },
    [armAsk, doAdopt],
  );

  // The armed chip's click: cancel the deferred move immediately, no dialog
  // (nothing hazardous is being undone — worst case is a card that stays put).
  const doDisarm = useCallback(
    (s: Session) => {
      const label = s.callsign || s.session_id;
      void adopt(s, { disarm: true }, (r) => {
        if (!r.ok) showNote({ hd: '✗ MOVE', err: `${label} — ${r.reason}` }, 8000);
        else
          showNote(
            { hd: '✓ DISARMED', msg: `${label} — move canceled; the card stays where it is` },
            8000,
          );
      });
    },
    [adopt, showNote],
  );

  // Item 3 — dismiss ONE offline card now (cleanup scoped to a single session).
  // The card's ✕ chip owns the two-step confirm for a revivable card; this only
  // fires the POST. Success is quiet on the board — the card leaves the offline
  // lane on the next snapshot — so the strip carries the confirmation, and every
  // refusal (409 not-offline / already dismissed / stalled, 404 unknown) reaches
  // it verbatim.
  const dismissImpl = useCallback(
    async (s: Session) => {
      const id = s.session_id;
      // Single-flight: the card disables its chip on `dismissing`, but the ref is
      // the real guard — a double click can never fire a second POST.
      if (!id || dismissRef.current.has(id)) return;
      dismissRef.current.add(id);
      setDismissing(new Set(dismissRef.current));
      const label = s.callsign || id;
      try {
        const res = await dismissSession(id);
        // A 409 "already dismissed" is a benign race (the card left the board a
        // beat ago, or two clicks slipped through) — report it as success, not an
        // alarming error the human has to reason about.
        const alreadyGone =
          res.status === 409 && (res.json?.reason ?? '').includes('already dismissed');
        if ((res.ok && res.json?.ok !== false) || alreadyGone) {
          showNote(
            {
              hd: '✓ DISMISSED',
              msg: `${label} — card cleared from the board · the worktree is left on disk`,
            },
            8000,
          );
        } else {
          showNote(
            {
              hd: '✗ DISMISS',
              err: `${label} — ${reasonOf(res, `dismiss failed (${res.status})`)}`,
            },
            8000,
          );
        }
      } finally {
        dismissRef.current.delete(id);
        setDismissing(new Set(dismissRef.current));
      }
    },
    [showNote],
  );
  // Fire-and-forget wrapper so the board can pass a plain `(s) => void` handler:
  // BoardLanes is memoized (M-P4) and must receive a stable, non-Promise callback
  // — the impl's single-flight guard already makes a double click a no-op.
  const doDismiss = useCallback(
    (s: Session) => {
      void dismissImpl(s);
    },
    [dismissImpl],
  );

  // v2.1 Rename — open the dialog. Offered on ANY live card: a rename touches
  // only the session's name, so a session the board never spawned (no `s.spawn`)
  // is as renameable as a board-owned pane — hence the optional-chain on the
  // window rather than a spawn guard like askKill's.
  const askRename = useCallback((s: Session | null | undefined) => {
    if (!s?.session_id) return;
    setRenameAsk({
      sessionId: s.session_id,
      callsign: s.callsign || s.session_id,
      window: s.spawn?.tmux_window ?? '',
    });
  }, []);

  // Both rename outcomes report through one place, because {suffix} and
  // {clear:true} answer in the SAME shape and their copy must not drift.
  // `renamed:false` is a success (the name simply did not change — you cleared a
  // name that was already automatic), so it reads as one, in green, not as a
  // silent no-op the human is left to wonder about.
  const reportRename = useCallback(
    (label: string, res: ApiResult) => {
      if (!(res.ok && res.json?.ok !== false)) {
        showNote(
          { hd: '✗ NAME', err: `${label} — ${reasonOf(res, `rename failed (${res.status})`)}` },
          8000,
        );
        return;
      }
      const j = (res.json ?? {}) as RenameJson;
      showNote(
        {
          hd: '✓ NAME',
          msg: j.renamed
            ? `${(j.previous ?? '') || label} is now ${j.callsign ?? ''}`
            : `${(j.callsign ?? '') || label} — already its name; nothing changed`,
        },
        8000,
      );
    },
    [showNote],
  );

  // Both rename commands share one single-flight lifecycle. Their request bodies
  // differ, but busy state, reporting, and close-on-completion must not drift.
  const runRename = useCallback(
    async (body: { suffix: string } | { clear: true }) => {
      const a = renameAsk;
      if (!a || renameBusyRef.current) return;
      renameBusyRef.current = true;
      setRenameBusy(true);
      try {
        const res = await renameSession(a.sessionId, body);
        reportRename(a.callsign, res);
      } finally {
        renameBusyRef.current = false;
        setRenameBusy(false);
        setRenameAsk(null);
      }
    },
    [renameAsk, reportRename],
  );

  // The dialog's confirm validates locally before entering the shared lifecycle;
  // the daemon validates again because it remains the authority on names.
  const doRename = useCallback(
    async (suffix: string) => {
      if (!validSuffix(suffix)) return;
      await runRename({ suffix });
    },
    [runRename],
  );

  // The dialog's quiet "reset to the automatic name" — {clear:true} reverts to
  // the ticket name when the session has a ticket, else to the hex name. No
  // second confirmation: nothing is destroyed, and the name it lands on is the
  // one the daemon would have given it anyway.
  const doResetName = useCallback(() => runRename({ clear: true }), [runRename]);

  return {
    clearing,
    doClear,
    killAsk,
    setKillAsk,
    killBusy,
    askKill,
    doKill,
    armAsk,
    setArmAsk,
    armBusy,
    askArm,
    doArm,
    doDisarm,
    renameAsk,
    setRenameAsk,
    renameBusy,
    askRename,
    doRename,
    doResetName,
    doRevive,
    doReviveAll,
    doEnableRemote,
    doDismiss,
    dismissing,
    doSpawnShell,
  };
}
