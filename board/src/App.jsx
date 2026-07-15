import React, { useEffect, useMemo, useRef, useState } from 'react';
import { useFleetState } from './useFleetState.js';
import { useSpawnActions } from './useSpawnActions.js';
import { useConflictRipples } from './hooks/useConflictRipples.js';
import { useFeedbackStrip } from './hooks/useFeedbackStrip.js';
import { useTermWindows } from './hooks/useTermWindows.js';
import { useWorktrees } from './hooks/useWorktrees.js';
import { useBoardHotkeys } from './hooks/useBoardHotkeys.js';
import { useFleetActions } from './hooks/useFleetActions.js';
import { ClockContext } from './clock.jsx';
import { basename, safeUrl, spawnTermable, sessionsById, callsignOf, sessionTicker } from './util.js';
import { sendMail, markPlan, reasonOf } from './api.js';
import { useAuth, saveToken } from './token.js';
import Header from './components/Header.jsx';
import BoardLanes from './components/BoardLanes.jsx';
import Inbox from './components/Inbox.jsx';
import Feed from './components/Feed.jsx';
import Drawer from './components/Drawer.jsx';
import Compose from './components/Compose.jsx';
import SpawnForm from './components/SpawnForm.jsx';
import PlanLibrary from './components/PlanLibrary.jsx';
import LanPanel from './components/LanPanel.jsx';
import KillConfirm from './components/KillConfirm.jsx';
import ArmMoveConfirm from './components/ArmMoveConfirm.jsx';
import RenameDialog from './components/RenameDialog.jsx';
import WorktreesModal from './components/WorktreesModal.jsx';
import FileViewer from './components/FileViewer.jsx';
import TokenGate from './components/TokenGate.jsx';

// v1.4 — lazy: xterm.js (~300 kB) loads only when a terminal is opened.
// v1.9 — the grid shares the same chunk: both are TermPane in a different box.
const TermModal = React.lazy(() => import('./components/TermModal.jsx'));
const TermGrid = React.lazy(() => import('./components/TermGrid.jsx'));

// Stable empty singletons for absent snapshot fields — a fresh `[]`/`{}` per
// render would break the memoized board (M-P4) on every 1 s clock tick.
const EMPTY_ARR = [];
const EMPTY_OBJ = {};

export default function App() {
  const { snap, status } = useFleetState();
  const { unauthorized, attempts } = useAuth(); // v1.7 LAN token
  const [now, setNow] = useState(Date.now());
  const [theme, setTheme] = useState(() => localStorage.getItem('fd-theme') || 'dark');
  const [compact, setCompact] = useState(() => localStorage.getItem('fd-compact') === '1');
  const [repoFilter, setRepoFilter] = useState('all');
  const [selQ, setSelQ] = useState(null);
  const [drawerSid, setDrawerSid] = useState(null);
  const [compose, setCompose] = useState(null); // null | { target }
  const [spawnForm, setSpawnForm] = useState(null); // null | { prompt?, cwd?, planId? }
  const [lanOpen, setLanOpen] = useState(false); // v1.7 LAN share panel
  const [wtOpen, setWtOpen] = useState(false); // v1.9 worktrees modal
  const [fsView, setFsView] = useState(null); // v2.2 file viewer — null | {sid, callsign, root, path?}
  const [priorities, setPriorities] = useState(() => new Set());
  const [threads, setThreads] = useState({}); // sid -> [{text, at}] (this tab only)

  const sessions = snap.sessions || EMPTY_ARR;
  const questions = snap.questions || EMPTY_ARR;
  const conflicts = snap.conflicts || EMPTY_ARR;
  const pendingQs = useMemo(() => questions.filter((q) => q.status === 'pending'), [questions]);
  const byId = useMemo(() => sessionsById(sessions), [sessions]);

  // ---- subsystem hooks ------------------------------------------------------
  // one-shot conflict ripple (fires only when a conflict row FIRST appears)
  const ripples = useConflictRipples(snap);
  // shared feedback strip (Clear + revive + remote), {hd?,msg,orphans?,url?}|{hd?,err}
  const { clearNote, setClearNote, showNote } = useFeedbackStrip();
  // M-F2 — ONE owner of the revive + enable-remote POSTs and their per-spawn
  // in-flight sets, shared by the card chips (via useFleetActions below) and the
  // drawer's OWNED PANE. A card chip and its drawer button can't each fire a POST.
  const { reviving, enabling, revivingAll, adopting, revive, reviveAll, enableRemote: enableRemoteAction, adopt } = useSpawnActions();
  // board-level mutations that report onto the strip: Clear, revive, enable-remote,
  // the two-step kill, the two-step Move-to-tmux (ASK opens the dialog; the POST
  // is the dialog's alone — disarm is the one direct click), and the two-step
  // rename (v2.1 — same shape: ✎ asks, the dialog POSTs).
  const {
    clearing, doClear,
    killAsk, setKillAsk, killBusy, askKill, doKill,
    armAsk, setArmAsk, armBusy, askArm, doArm, doDisarm,
    renameAsk, setRenameAsk, renameBusy, askRename, doRename, doResetName,
    doRevive, doReviveAll, doEnableRemote,
  } = useFleetActions({ showNote, revive, reviveAll, enableRemoteAction, adopt });
  // terminal / grid / watch windows — killAsk + armAsk + renameAsk are threaded in
  // for the keydown mirrors (Esc cancels the topmost dialog, leaves the drawer
  // standing)
  const {
    term, setTerm, grid, setGrid, watch,
    termableSessions, watchable, openTerm, toggleWatch, openGrid,
    termOpen, killOpen, armOpen, renameOpen,
  } = useTermWindows(sessions, killAsk, armAsk, renameAsk);
  // worktrees list — reloads on boot and whenever the fleet gains/loses a session
  const {
    worktrees, wtLoading, wtErr, wtSupported, loadWorktrees, removeWorktree: doRemoveWorktree,
    wtCount, wtHazard,
  } = useWorktrees(sessions.length);

  // R3-4 — the header "▦ Terminals" button is a persistent focus target: when
  // the grid promotes a tile to the full modal, the grid (and the ⤢ button that
  // opened the modal) unmount, so the modal has no live opener to restore to on
  // close. This ref is its safety net (handed to TermModal + TermGrid).
  const termBtnRef = useRef(null);

  // 1 s clock: ages, countdown rings, header clock
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  // theme (direction 1c survives as the light theme — tokens [data-theme="light"])
  useEffect(() => {
    if (theme === 'light') document.documentElement.dataset.theme = 'light';
    else delete document.documentElement.dataset.theme;
    localStorage.setItem('fd-theme', theme);
  }, [theme]);
  useEffect(() => { localStorage.setItem('fd-compact', compact ? '1' : '0'); }, [compact]);

  // keep a valid rail selection
  useEffect(() => {
    if (!pendingQs.some((q) => q.id === selQ)) setSelQ(pendingQs[0]?.id ?? null);
  }, [pendingQs, selQ]);

  // keyboard: j/k rail nav · y/n permission · 1-9 choice · c compose · Esc close
  useBoardHotkeys({
    pendingQs, selQ, setSelQ, termOpen, killOpen, armOpen, renameOpen, fsOpen: !!fsView,
    setKillAsk, setArmAsk, setRenameAsk, setDrawerSid, setCompose, setSpawnForm, setLanOpen, setWtOpen, setFsView,
  });

  const stale = status !== 'live';
  const liveN = sessions.filter((s) => s.col !== 'offline').length;
  // v1.2 spawn capability — ALL spawn UI hides when unavailable
  const spawnCap = snap.spawn || null;
  const spawnAvailable = !!spawnCap?.available;
  // Fix D — manual cleanup: the Clear button only appears when there is an
  // offline card to clear.
  const hasOffline = sessions.some((s) => s.col === 'offline');

  // Prefer the daemon's callsigns: it resolves them from EVERY session, so a
  // conflict whose participants have since been archived still reads
  // `comet-2d9d × ember-fc8e` instead of shouting two raw uuids at you.
  const conflictMsg = conflicts.length
    ? conflicts.map((c) => {
      const who = c.callsigns?.length ? c.callsigns : (c.sessions || []).map((sid) => callsignOf(byId, sid));
      return `${basename(c.file || c.rel_path)} — ${who.join(' × ')}`;
    }).join('   ·   ')
    : null;

  const drawerSession = drawerSid ? byId.get(drawerSid) : null;
  const drawerConflictFiles = drawerSession
    ? conflicts.filter((c) => (c.sessions || []).includes(drawerSid)).map((c) => c.file || c.rel_path)
    : [];
  // Memoized off the 1 Hz clock re-render: the ticker filter (a RegExp build +
  // full scan in sessionTicker) reruns only when the ticker or the drawer's
  // callsign changes, like byId/pendingQs above.
  const drawerTimeline = useMemo(
    () => (drawerSession ? sessionTicker(snap.ticker || EMPTY_ARR, drawerSession.callsign) : EMPTY_ARR),
    [snap.ticker, drawerSession?.callsign],
  );

  const recordThread = (target, text) => {
    if (byId.has(target)) {
      setThreads((prev) => ({ ...prev, [target]: [...(prev[target] || []), { text, at: Date.now() }] }));
    }
  };

  // v1.3 plan library — hidden entirely when the daemon doesn't send `plans`
  const plans = Array.isArray(snap.plans) ? snap.plans : null;

  // Execute plan → spawn form prefilled per contract; cwd from a live
  // session worktree of the plan's repo when one exists.
  const openExecutePlan = (p) => {
    const peer = sessions.find((s) => s.col !== 'offline' && s.repo_id === p.repo_id && (s.worktree || s.cwd));
    setSpawnForm({
      prompt: 'Execute this approved plan exactly. Custom instructions: \n\n---\n' + (p.plan_md || ''),
      cwd: peer ? (peer.worktree || peer.cwd) : '',
      planId: p.plan_id,
    });
  };

  // After a successful plan-execute spawn: mark executed via:'spawn:<id>'.
  // The SpawnForm shows whatever this returns — a 409 stays on screen.
  const onSpawnedForPlan = async (json) => {
    if (!spawnForm?.planId) return null;
    const res = await markPlan(spawnForm.planId, { status: 'executed', via: `spawn:${json.spawn_id}` });
    if (res.ok) return { ok: true, text: 'plan marked executed' };
    const reason = res.reason;
    return {
      ok: false,
      text: res.status === 409
        ? `spawned, but the plan mark hit 409 — ${reason || 'bad transition'}`
        : `spawned, but marking the plan failed (${reason || res.status})`,
    };
  };

  // v1.7 — a 401 means this board is on the network and we don't hold its key.
  // The board behind the gate is dead (every call would bounce), so the gate
  // REPLACES it rather than floating over a screen full of dead buttons. All
  // hooks above have already run — this early return is safe.
  if (unauthorized) {
    return <TokenGate attempts={attempts} onSubmit={saveToken} />;
  }

  return (
    // M-P4 — the 1 s `now` reaches only the leaves that read this context (the
    // card <Age> spans). App re-renders each second for the header clock, but
    // the memoized board below skips because none of its props changed.
    <ClockContext.Provider value={now}>
    <div className={`fd${compact ? ' compact' : ''}${stale ? ' stale' : ''}`}>
      <Header
        status={status}
        stale={stale}
        pendingQs={pendingQs}
        liveN={liveN}
        conflictCount={conflicts.length}
        version={snap.version}
        now={now}
        onCompose={() => setCompose({ target: 'all' })}
        termableSessions={termableSessions}
        watchable={watchable}
        termBtnRef={termBtnRef}
        onOpenGrid={openGrid}
        spawnAvailable={spawnAvailable}
        spawnActive={spawnCap?.active || 0}
        onSpawn={() => setSpawnForm({})}
        hasOffline={hasOffline}
        clearing={clearing}
        onClear={doClear}
        wtSupported={wtSupported}
        wtCount={wtCount}
        wtHazard={wtHazard}
        onOpenWorktrees={() => { setWtOpen(true); loadWorktrees(); }}
        lanEnabled={snap.lan?.enabled}
        onShare={() => setLanOpen(true)}
        compact={compact}
        onToggleCompact={() => setCompact(!compact)}
        theme={theme}
        onToggleTheme={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
      />

      {/* ============ conflict strip ============ */}
      {conflictMsg && (
        <div className="fd-confstrip">
          <span className="hd">▲ CONFLICT</span>
          <span className="msg">{conflictMsg}</span>
        </div>
      )}

      {/* ============ feedback strip (Clear + revive + remote results) ============ */}
      {clearNote && (
        <div className={`fd-clearstrip${clearNote.err ? ' err' : ''}`}>
          <span className="hd">{clearNote.hd || (clearNote.err ? '✗ CLEAR' : '✓ CLEARED')}</span>
          <span className="msg">{clearNote.err || clearNote.msg}</span>
          {safeUrl(clearNote.url) && (
            <a className="lnk" href={safeUrl(clearNote.url)} target="_blank" rel="noopener noreferrer">
              📱 open on claude.ai ↗
            </a>
          )}
          {(clearNote.orphans || []).length > 0 && (
            <span className="orph">
              orphan worktrees — remove manually: {clearNote.orphans.join('  ·  ')}
            </span>
          )}
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Dismiss" onClick={() => setClearNote(null)}>✕</button>
        </div>
      )}

      {/* ============ main ============ */}
      <div className="fd-main">
        <div className="fd-board">
          {sessions.length === 0 ? (
            <div className="fd-empty">
              <div className="t1">NO SESSIONS REPORTING</div>
              <div className="t2">
                The deck is quiet. Start a session anywhere on this machine and it will appear here within a heartbeat.
              </div>
              <div className="cmd">$ fleetd up &amp;&amp; claude <span className="c"># hooks auto-attach</span></div>
            </div>
          ) : (
            <BoardLanes
              sessions={sessions}
              repos={snap.repos || EMPTY_ARR}
              conflicts={conflicts}
              mailPending={snap.mail_pending || EMPTY_OBJ}
              mailMeta={snap.mail_meta || EMPTY_OBJ}
              compact={compact}
              stale={stale}
              repoFilter={repoFilter}
              onRepoFilter={setRepoFilter}
              ripples={ripples}
              priorities={priorities}
              onOpenSession={setDrawerSid}
              onOpenTerm={openTerm}
              reviving={reviving}
              revivingAll={revivingAll}
              onRevive={doRevive}
              onReviveAll={doReviveAll}
              enablingRemote={enabling}
              onEnableRemote={doEnableRemote}
              onKill={askKill}
              onToggleWatch={toggleWatch}
              watch={watch}
              onArmMove={askArm}
              onDisarm={doDisarm}
              adopting={adopting}
              onRename={askRename}
            />
          )}
          {/* v1.3 — PLANS library, between the lanes and the feed (never in
              the rail: needs-you keeps the human's full attention) */}
          {plans && (
            <PlanLibrary
              plans={plans}
              sessions={sessions}
              now={now}
              spawnAvailable={spawnAvailable}
              onExecute={openExecutePlan}
            />
          )}
          <Feed ticker={snap.ticker || []} orphans={snap.spawn_orphans || []} />
        </div>

        {/* the inbox stays GLOBAL: never filtered by repo */}
        <Inbox
          questions={questions}
          sessions={sessions}
          now={now}
          selQ={selQ}
          onSelect={setSelQ}
        />
      </div>

      {/* ============ drawer ============ */}
      {drawerSession && (
        <Drawer
          s={{ ...drawerSession, timeline: drawerTimeline }}
          now={now}
          conflictFiles={drawerConflictFiles}
          mailCount={(snap.mail_pending || {})[drawerSid] || 0}
          priority={priorities.has(drawerSid)}
          onTogglePriority={() => {
            setPriorities((prev) => {
              const s = new Set(prev);
              if (s.has(drawerSid)) s.delete(drawerSid); else s.add(drawerSid);
              return s;
            });
          }}
          onClose={() => setDrawerSid(null)}
          onCompose={() => { setCompose({ target: drawerSid }); setDrawerSid(null); }}
          onOpenTerm={spawnTermable(drawerSession)
            ? () => { openTerm(drawerSession); setDrawerSid(null); }
            : undefined}
          onKill={() => askKill(drawerSession)}
          // M-F2 — the drawer's OWNED PANE drives revive/enable-remote through
          // the SAME shared hook the card chip uses; a second click can't fire a
          // second POST. It renders its OWN inline result off the callback.
          onRevive={revive}
          onEnableRemote={enableRemoteAction}
          reviving={!!(drawerSession.spawn && reviving.has(drawerSession.spawn.spawn_id))}
          enablingRemote={!!(drawerSession.spawn && enabling.has(drawerSession.spawn.spawn_id))}
          // v2.0 — Move-to-tmux, same shared owner as the card chip (M-F2)
          onArmMove={askArm}
          onDisarm={doDisarm}
          adopting={adopting.has(drawerSid)}
          // v2.1 — rename: the drawer's ✎ and the card's chip open the SAME
          // dialog, which owns the POST and reports on the shared strip
          onRename={askRename}
          // v2.2 — the read-only file viewer opens OVER the drawer (relPath
          // null = at the root; a FILES chip passes the file to reveal)
          onBrowseFiles={(sess, relPath) => setFsView({
            sid: sess.session_id,
            callsign: sess.callsign || sess.session_id,
            root: sess.worktree || sess.cwd || '',
            path: relPath || null,
          })}
          thread={threads[drawerSid] || EMPTY_ARR}
          // M-F5 — await the result: clear the draft only on success, and let
          // the drawer surface a failure instead of swallowing it.
          onSendThread={async (text) => {
            const res = await sendMail(drawerSid, text);
            if (res.ok && res.json?.ok !== false) { recordThread(drawerSid, text); return { ok: true }; }
            return { ok: false, reason: reasonOf(res, `send failed (${res.status})`) };
          }}
        />
      )}

      {/* ============ compose ============ */}
      {compose && (
        <Compose
          initialTarget={compose.target}
          sessions={sessions}
          repos={snap.repos || []}
          onClose={() => setCompose(null)}
          onSent={recordThread}
          spawnAvailable={spawnAvailable}
          onSpawnFor={(text) => { setCompose(null); setSpawnForm({ prompt: text }); }}
        />
      )}

      {/* ============ LAN share (v1.7) ============ */}
      {lanOpen && <LanPanel lan={snap.lan} onClose={() => setLanOpen(false)} />}

      {/* ============ worktrees (v1.9 — the ONLY door to removeWorktree) ============ */}
      {wtOpen && (
        <WorktreesModal
          worktrees={worktrees}
          loading={wtLoading}
          error={wtErr}
          now={now}
          onReload={loadWorktrees}
          onRemove={doRemoveWorktree}
          onClose={() => setWtOpen(false)}
        />
      )}

      {/* ============ file viewer (v2.2 — read-only, per session) ============ */}
      {fsView && (
        <FileViewer
          key={fsView.sid}
          sid={fsView.sid}
          callsign={fsView.callsign}
          root={fsView.root}
          initialPath={fsView.path}
          onClose={() => setFsView(null)}
        />
      )}

      {/* ============ spawn (v1.2 — explicit human click only) ============ */}
      {spawnForm && (
        <SpawnForm
          sessions={sessions}
          repoCatalog={snap.repo_catalog || EMPTY_ARR}
          settings={snap.settings || EMPTY_OBJ}
          prefillPrompt={spawnForm.prompt || ''}
          prefillCwd={spawnForm.cwd || ''}
          planMode={!!spawnForm.planId}
          onSpawned={spawnForm.planId ? onSpawnedForPlan : undefined}
          onClose={() => setSpawnForm(null)}
        />
      )}

      {/* ============ kill confirmation (v1.8 — the ONLY door to killSpawn) ============ */}
      {killAsk && (
        <KillConfirm
          callsign={killAsk.callsign}
          tmuxWindow={killAsk.window}
          alive={killAsk.alive}
          busy={killBusy}
          onCancel={() => setKillAsk(null)}
          onConfirm={doKill}
        />
      )}

      {/* ======= move-to-tmux confirmation (v2.0 — the door to an armed/immediate adopt) ======= */}
      {armAsk && (
        <ArmMoveConfirm
          callsign={armAsk.callsign}
          live={armAsk.live}
          busy={armBusy}
          onCancel={() => setArmAsk(null)}
          onConfirm={doArm}
        />
      )}

      {/* ======= rename (v2.1 — the ONLY door to renameSession) ======= */}
      {renameAsk && (
        <RenameDialog
          callsign={renameAsk.callsign}
          tmuxWindow={renameAsk.window}
          busy={renameBusy}
          onCancel={() => setRenameAsk(null)}
          onConfirm={doRename}
          onReset={doResetName}
        />
      )}

      {/* ============ live terminal (v1.4 — closes via ✕ ONLY) ============ */}
      {term && (
        <React.Suspense fallback={null}>
          <TermModal
            key={term.spawnId}
            spawnId={term.spawnId}
            callsign={term.callsign}
            tmuxWindow={term.window}
            fallbackFocusRef={termBtnRef}
            onClose={() => setTerm(null)}
          />
        </React.Suspense>
      )}

      {/* ============ the wall of screens (v1.9 — ✕ ONLY, same as above) ===== */}
      {grid && (
        <React.Suspense fallback={null}>
          <TermGrid
            tiles={grid}
            fallbackFocusRef={termBtnRef}
            onClose={() => setGrid(null)}
            onExpand={(t) => { setGrid(null); setTerm(t); }}
          />
        </React.Suspense>
      )}
    </div>
    </ClockContext.Provider>
  );
}
