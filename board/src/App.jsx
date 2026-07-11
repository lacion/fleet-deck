import React, { useEffect, useMemo, useRef, useState } from 'react';
import { useFleetState } from './useFleetState.js';
import { hhmmss, basename, spawnTermable } from './util.js';
import { sendMail, markPlan, cleanup, reviveSpawn, enableRemote, killSpawn } from './api.js';
import { useAuth, saveToken } from './token.js';
import BoardLanes from './components/BoardLanes.jsx';
import Inbox from './components/Inbox.jsx';
import Feed from './components/Feed.jsx';
import Drawer from './components/Drawer.jsx';
import Compose from './components/Compose.jsx';
import SpawnForm from './components/SpawnForm.jsx';
import PlanLibrary from './components/PlanLibrary.jsx';
import LanPanel from './components/LanPanel.jsx';
import KillConfirm from './components/KillConfirm.jsx';
import TokenGate from './components/TokenGate.jsx';

// v1.4 — lazy: xterm.js (~300 kB) loads only when a terminal is opened
const TermModal = React.lazy(() => import('./components/TermModal.jsx'));

const WS_LABEL = { live: 'LIVE', reconnecting: 'RECONNECTING', offline: 'OFFLINE' };

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
  // v1.4 live terminal — ONE at a time; identity captured at open so the
  // stream survives the card mutating (or vanishing) mid-view.
  const [term, setTerm] = useState(null); // null | { spawnId, callsign, window }
  // v1.8 kill — the card chip and the drawer button both open ONE dialog; the
  // POST only fires from its hazard button. null | {spawnId, callsign, window, alive}
  const [killAsk, setKillAsk] = useState(null);
  const [killBusy, setKillBusy] = useState(false);
  const [priorities, setPriorities] = useState(() => new Set());
  const [threads, setThreads] = useState({}); // sid -> [{text, at}] (this tab only)
  const [ripples, setRipples] = useState(() => new Map()); // sid -> until(ms)
  const [clearing, setClearing] = useState(false);
  // shared feedback strip (Clear + revive + remote):
  // {hd?, msg, orphans?, url?} | {hd?, err}
  const [clearNote, setClearNote] = useState(null);
  // v1.5 revive — spawn_ids with a revive POST in flight, + the bulk action
  const [reviving, setReviving] = useState(() => new Set());
  const [revivingAll, setRevivingAll] = useState(false);
  // v1.6 remote control — spawn_ids with an enable POST in flight (the
  // daemon types /rc and harvests the claude.ai link: ~3-6 s round-trip)
  const [enablingRemote, setEnablingRemote] = useState(() => new Set());
  const clearTimer = useRef(null);
  const prevConflicts = useRef({ keys: null, sawData: false });
  const termOpen = useRef(false); // mirrors `term` for the keydown handler
  useEffect(() => { termOpen.current = !!term; }, [term]);
  const killOpen = useRef(false); // mirrors `killAsk` for the keydown handler
  useEffect(() => { killOpen.current = !!killAsk; }, [killAsk]);

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

  // one-shot conflict ripple: fire only when a conflict row FIRST appears
  // (never on the initial snapshot — history doesn't ripple)
  useEffect(() => {
    const store = prevConflicts.current;
    const list = snap.conflicts || [];
    const keyOf = (c) => `${c.at}:${c.rel_path || c.file}:${(c.sessions || []).join(',')}`;
    const keys = new Set(list.map(keyOf));
    const isData = (snap.up_ms || 0) > 0 || (snap.sessions || []).length > 0 || list.length > 0;
    if (store.keys && store.sawData) {
      const until = Date.now() + 2000;
      const add = new Map();
      for (const c of list) {
        if (!store.keys.has(keyOf(c))) for (const sid of c.sessions || []) add.set(sid, until);
      }
      if (add.size) {
        setRipples((prev) => new Map([...prev, ...add]));
        setTimeout(() => setRipples((prev) => {
          const m = new Map(prev);
          for (const [sid, u] of add) if (m.get(sid) === u) m.delete(sid);
          return m;
        }), 2200);
      }
    }
    store.keys = keys;
    if (isData) store.sawData = true;
  }, [snap]);

  const sessions = snap.sessions || [];
  const questions = snap.questions || [];
  const pendingQs = useMemo(() => questions.filter((q) => q.status === 'pending'), [questions]);

  // keep a valid rail selection
  useEffect(() => {
    if (!pendingQs.some((q) => q.id === selQ)) setSelQ(pendingQs[0]?.id ?? null);
  }, [pendingQs, selQ]);

  // keyboard: j/k rail nav · y/n permission · 1-9 choice · c compose · Esc close
  useEffect(() => {
    const onKey = (e) => {
      const tag = (e.target.tagName || '').toLowerCase();
      const typing = tag === 'input' || tag === 'textarea' || tag === 'select';
      // Esc NEVER touches the live terminal — the agent's TUI needs it. The
      // modal stops propagation itself; this guard covers stray focus too.
      if (e.key === 'Escape') {
        if (termOpen.current) return;
        // the kill dialog is modal over everything else: Esc cancels IT, and
        // leaves the drawer it may have been opened from standing
        if (killOpen.current) { setKillAsk(null); return; }
        setDrawerSid(null); setCompose(null); setSpawnForm(null); setLanOpen(false);
        return;
      }
      if (typing) return;
      const idx = pendingQs.findIndex((q) => q.id === selQ);
      if (e.key === 'j' || e.key === 'ArrowDown') {
        e.preventDefault();
        if (pendingQs.length) setSelQ(pendingQs[Math.min(pendingQs.length - 1, Math.max(0, idx) + (idx < 0 ? 0 : 1))].id);
      } else if (e.key === 'k' || e.key === 'ArrowUp') {
        e.preventDefault();
        if (pendingQs.length) setSelQ(pendingQs[Math.max(0, (idx < 0 ? 0 : idx) - 1)].id);
      } else if (e.key === 'c') {
        e.preventDefault();
        setCompose({ target: 'all' });
      } else {
        const q = pendingQs[idx];
        if (!q) return;
        const card = document.querySelector(`.fd-q[data-qid="${q.id}"]`);
        if (q.kind === 'permission' && (e.key === 'y' || e.key === 'n')) {
          card?.querySelector(e.key === 'y' ? '.fd-allow' : '.fd-deny')?.click();
        } else if (q.kind === 'choice' && /^[1-9]$/.test(e.key)) {
          card?.querySelectorAll('.fd-opt')[Number(e.key) - 1]?.click();
        } else if (q.kind === 'freeform' && e.key === 'Enter') {
          card?.querySelector('textarea')?.focus();
          e.preventDefault();
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [pendingQs, selQ]);

  const stale = status !== 'live';
  const liveN = sessions.filter((s) => s.col !== 'offline').length;
  // v1.2 spawn capability — ALL spawn UI hides when unavailable
  const spawnCap = snap.spawn || null;
  const spawnAvailable = !!spawnCap?.available;
  const conflicts = snap.conflicts || [];
  const byId = new Map(sessions.map((s) => [s.session_id, s]));
  const csOf = (sid) => byId.get(sid)?.callsign || sid;

  // Prefer the daemon's callsigns: it resolves them from EVERY session, so a
  // conflict whose participants have since been archived still reads
  // `comet-2d9d × ember-fc8e` instead of shouting two raw uuids at you.
  const conflictMsg = conflicts.length
    ? conflicts.map((c) => {
      const who = c.callsigns?.length ? c.callsigns : (c.sessions || []).map(csOf);
      return `${basename(c.file || c.rel_path)} — ${who.join(' × ')}`;
    }).join('   ·   ')
    : null;

  const drawerSession = drawerSid ? byId.get(drawerSid) : null;
  const drawerConflictFiles = drawerSession
    ? conflicts.filter((c) => (c.sessions || []).includes(drawerSid)).map((c) => c.file || c.rel_path)
    : [];
  const drawerTimeline = drawerSession
    ? (snap.ticker || []).filter((e) => String(e.msg || '').includes(drawerSession.callsign)).slice(0, 12)
    : [];

  const recordThread = (target, text) => {
    if (byId.has(target)) {
      setThreads((prev) => ({ ...prev, [target]: [...(prev[target] || []), { text, at: Date.now() }] }));
    }
  };

  // v1.3 plan library — hidden entirely when the daemon doesn't send `plans`
  const plans = Array.isArray(snap.plans) ? snap.plans : null;

  // Fix D — manual cleanup. The button only appears when there is something
  // to clear (an offline card); the daemon archives offline sessions, expires
  // their mail/questions, kills dead scoped panes, and LISTS (never deletes)
  // orphaned worktrees for the human to remove.
  const hasOffline = sessions.some((s) => s.col === 'offline');
  // one strip, many reporters (Clear, revive): ms=0 stays until dismissed
  const showNote = (note, ms) => {
    clearTimeout(clearTimer.current);
    setClearNote(note);
    if (ms) clearTimer.current = setTimeout(() => setClearNote(null), ms);
  };
  const doClear = async () => {
    if (clearing) return;
    setClearing(true);
    try {
      const res = await cleanup();
      if (res.ok && res.json?.ok !== false) {
        const j = res.json || {};
        const orphans = Array.isArray(j.orphan_worktrees) ? j.orphan_worktrees : [];
        const msg = `cleared ${j.archived ?? 0} offline · ${j.conflicts_cleared ?? 0} conflicts`
          + ` · ${(j.questions_purged ?? 0) + (j.questions_expired ?? 0)} questions`
          + ` · ${j.mail_expired ?? 0} mail · ${j.windows_killed ?? 0} windows · feed wiped`;
        // orphan paths need reading time — that strip stays until dismissed
        showNote({ msg, orphans }, orphans.length ? 0 : 8000);
      } else {
        showNote({ err: res.json?.err || `clear failed (${res.status})` }, 8000);
      }
    } catch {
      showNote({ err: 'daemon unreachable' }, 8000);
    } finally {
      setClearing(false);
    }
  };
  useEffect(() => () => clearTimeout(clearTimer.current), []);

  // v1.5 — revive dead board-spawned agents (spawn.revivable). Success is
  // silent: the daemon moves the card to QUEUED ("reviving…") and it flips
  // live on the resumed session's first hook. Only failures hit the strip.
  const markReviving = (ids, on) => setReviving((prev) => {
    const next = new Set(prev);
    for (const id of ids) { if (on) next.add(id); else next.delete(id); }
    return next;
  });
  const reviveReason = (res) => res.json?.reason || res.json?.err || `HTTP ${res.status}`;
  const doRevive = async (s) => {
    const id = s.spawn?.spawn_id;
    if (!id || reviving.has(id)) return;
    markReviving([id], true);
    try {
      const res = await reviveSpawn(id);
      if (!res.ok || res.json?.ok === false) {
        showNote({ hd: '✗ REVIVE', err: `${s.callsign || id} — ${reviveReason(res)}` }, 8000);
      }
    } catch {
      showNote({ hd: '✗ REVIVE', err: `${s.callsign || id} — daemon unreachable` }, 8000);
    } finally {
      markReviving([id], false);
    }
  };
  // Revive all (OFFLINE column head): sequential POSTs, one summary note.
  const doReviveAll = async (list) => {
    if (revivingAll || !list.length) return;
    setRevivingAll(true);
    markReviving(list.map((s) => s.spawn.spawn_id), true);
    let okN = 0;
    const fails = [];
    for (const s of list) {
      const label = s.callsign || s.spawn.spawn_id;
      try {
        const res = await reviveSpawn(s.spawn.spawn_id);
        if (res.ok && res.json?.ok !== false) okN += 1;
        else fails.push(`${label}: ${reviveReason(res)}`);
      } catch {
        fails.push(`${label}: daemon unreachable`);
      }
      markReviving([s.spawn.spawn_id], false);
    }
    setRevivingAll(false);
    if (fails.length === 0) {
      showNote({ hd: '✓ REVIVE', msg: `revived ${okN}/${list.length} — cards move to QUEUED` }, 8000);
    } else {
      // failure reasons need reading time — stays until dismissed
      showNote({ hd: '✗ REVIVE', err: `revived ${okN}/${list.length} — ${fails.join('  ·  ')}` }, 0);
    }
  };

  // v1.6 — put a board-spawned agent on remote control (card chip; the
  // drawer's OWNED PANE button reports inline instead). Success surfaces on
  // the shared strip — with the claude.ai link when the harvest beat the
  // response — and the card chip flips to the permanent door on the next
  // snapshot. Failures (409 mid-turn races, dead pane) surface the reason.
  const doEnableRemote = async (s) => {
    const id = s.spawn?.spawn_id;
    if (!id || enablingRemote.has(id)) return;
    const label = s.callsign || id;
    setEnablingRemote((prev) => new Set(prev).add(id));
    try {
      const res = await enableRemote(id);
      if (res.ok && res.json?.ok !== false) {
        const url = res.json?.url || null;
        if (url) {
          // the link needs reading/tapping time — stays until dismissed
          showNote({ hd: '✓ REMOTE', msg: `${label} on remote control —`, url }, 0);
        } else {
          showNote({
            hd: '✓ REMOTE',
            msg: `${label} on remote control — ${res.json?.pending
              ? 'still harvesting the claude.ai link; it lands on the card chip'
              : 'claude.ai link not captured — check the agent’s terminal (▣)'}`,
          }, 8000);
        }
      } else {
        showNote({ hd: '✗ REMOTE', err: `${label} — ${res.json?.reason || res.json?.err || `HTTP ${res.status}`}` }, 8000);
      }
    } catch {
      showNote({ hd: '✗ REMOTE', err: `${label} — daemon unreachable` }, 8000);
    } finally {
      setEnablingRemote((prev) => { const next = new Set(prev); next.delete(id); return next; });
    }
  };

  // v1.8 — kill a board-spawned agent. The card chip and the drawer button
  // only ASK (this opens the dialog); the POST fires from the dialog's hazard
  // button alone. Success is quiet on the board itself — the card goes OFFLINE
  // on the next snapshot — so the strip carries the confirmation, and every
  // refusal (409 not-offline, 410 gone, 404 unknown) reaches it verbatim.
  const askKill = (s) => {
    if (!s?.spawn?.spawn_id) return;
    setKillAsk({
      spawnId: s.spawn.spawn_id,
      callsign: s.callsign || s.session_id,
      window: s.spawn.tmux_window || '',
      alive: s.col !== 'offline',
    });
  };
  const doKill = async () => {
    if (!killAsk || killBusy) return;
    const { spawnId, callsign, alive } = killAsk;
    setKillBusy(true);
    try {
      // force:true is REQUIRED for a card that isn't offline — the daemon 409s
      // otherwise. `alive` is exactly that condition (see the dialog's warning).
      const res = await killSpawn(spawnId, alive);
      if (res.ok && res.json?.ok !== false) {
        showNote({ hd: '✓ KILLED', msg: `${callsign} — pane killed · worktree and branch left on disk` }, 8000);
      } else {
        const reason = res.json?.reason || res.json?.err;
        const msg =
          res.status === 409 ? (reason || 'refused — session is not offline (409)')
          : res.status === 410 ? (reason || 'window already gone (410)')
          : res.status === 404 ? (reason || 'unknown spawn (404)')
          : (reason || `kill failed (${res.status})`);
        showNote({ hd: '✗ KILL', err: `${callsign} — ${msg}` }, 8000);
      }
    } catch {
      showNote({ hd: '✗ KILL', err: `${callsign} — daemon unreachable` }, 8000);
    } finally {
      setKillBusy(false);
      setKillAsk(null);
    }
  };

  // v1.4 — open the live terminal for a board-spawned session
  const openTerm = (s) => {
    if (!spawnTermable(s)) return;
    setTerm({
      spawnId: s.spawn.spawn_id,
      callsign: s.callsign || s.session_id,
      window: s.spawn.tmux_window,
    });
  };

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
    const reason = res.json?.err || res.json?.reason;
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
    <div className={`fd${compact ? ' compact' : ''}${stale ? ' stale' : ''}`}>
      {/* ============ header ============ */}
      <div className="fd-header">
        <div className="fd-wordmark">FLEET&nbsp;DECK&nbsp;⚡</div>
        <div className={`fd-wspill ${status}`}>
          <span className="dot" />
          {WS_LABEL[status]}
        </div>
        {stale && <div className="fd-stale">showing last known state</div>}
        {pendingQs.length > 0 && (
          <button
            type="button"
            className="fd-needschip"
            title="Jump to the inbox"
            onClick={() => {
              document.querySelector(`.fd-q[data-qid="${(pendingQs[0] || {}).id}"]`)?.scrollIntoView({ block: 'nearest' });
            }}
          >
            NEEDS YOU · {pendingQs.length}
          </button>
        )}
        <div className="fd-spacer" />
        <div className="fd-fleetline">
          {liveN} session{liveN === 1 ? '' : 's'} · {conflicts.length} conflict{conflicts.length === 1 ? '' : 's'}
        </div>
        <div className="fd-clock">{hhmmss(now)}</div>
        <button type="button" className="fd-hbtn" onClick={() => setCompose({ target: 'all' })}>
          ✉ Compose <span className="fd-kbd">c</span>
        </button>
        {spawnAvailable && (
          <button type="button" className="fd-hbtn" onClick={() => setSpawnForm({})}>
            + Spawn
            {(spawnCap.active || 0) > 0 && (
              <span className="fd-spawncount">{spawnCap.active}/{spawnCap.max}</span>
            )}
          </button>
        )}
        {hasOffline && (
          <button type="button" className="fd-hbtn" disabled={clearing} onClick={doClear}>
            ⌫ {clearing ? 'Clearing…' : 'Clear'}
          </button>
        )}
        {/* v1.7 — always offered: when LAN is off the panel is where you learn
            how to turn it on, so it must not hide precisely when it's needed */}
        <button
          type="button"
          className="fd-hbtn"
          title="Open this board on another device"
          onClick={() => setLanOpen(true)}
        >
          ⇄ Share
          {snap.lan?.enabled && <span className="fd-landot" aria-label="LAN mode on" />}
        </button>
        <button type="button" className="fd-hbtn dim" aria-label="Toggle density" onClick={() => setCompact(!compact)}>
          {compact ? '▤ Cozy' : '▦ Compact'}
        </button>
        <button type="button" className="fd-hbtn dim" aria-label="Toggle theme" onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}>
          {theme === 'dark' ? '☀ Light' : '● Dark'}
        </button>
      </div>

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
          {clearNote.url && (
            <a className="lnk" href={clearNote.url} target="_blank" rel="noopener noreferrer">
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
              repos={snap.repos || []}
              conflicts={conflicts}
              mailPending={snap.mail_pending || {}}
              mailMeta={snap.mail_meta || {}}
              now={now}
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
              enablingRemote={enablingRemote}
              onEnableRemote={doEnableRemote}
              onKill={askKill}
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
          thread={threads[drawerSid] || []}
          onSendThread={(text) => {
            sendMail(drawerSid, text).then((res) => {
              if (res.ok) recordThread(drawerSid, text);
            }).catch(() => { /* surfaced by the LIVE pill */ });
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

      {/* ============ spawn (v1.2 — explicit human click only) ============ */}
      {spawnForm && (
        <SpawnForm
          sessions={sessions}
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

      {/* ============ live terminal (v1.4 — closes via ✕ ONLY) ============ */}
      {term && (
        <React.Suspense fallback={null}>
          <TermModal
            key={term.spawnId}
            spawnId={term.spawnId}
            callsign={term.callsign}
            tmuxWindow={term.window}
            onClose={() => setTerm(null)}
          />
        </React.Suspense>
      )}
    </div>
  );
}
