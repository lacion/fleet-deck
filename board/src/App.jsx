import React, { useEffect, useMemo, useRef, useState } from 'react';
import { useFleetState } from './useFleetState.js';
import { hhmmss, basename } from './util.js';
import { sendMail, markPlan } from './api.js';
import BoardLanes from './components/BoardLanes.jsx';
import Inbox from './components/Inbox.jsx';
import Feed from './components/Feed.jsx';
import Drawer from './components/Drawer.jsx';
import Compose from './components/Compose.jsx';
import SpawnForm from './components/SpawnForm.jsx';
import PlanLibrary from './components/PlanLibrary.jsx';

const WS_LABEL = { live: 'LIVE', reconnecting: 'RECONNECTING', offline: 'OFFLINE' };

export default function App() {
  const { snap, status } = useFleetState();
  const [now, setNow] = useState(Date.now());
  const [theme, setTheme] = useState(() => localStorage.getItem('fd-theme') || 'dark');
  const [compact, setCompact] = useState(() => localStorage.getItem('fd-compact') === '1');
  const [repoFilter, setRepoFilter] = useState('all');
  const [selQ, setSelQ] = useState(null);
  const [drawerSid, setDrawerSid] = useState(null);
  const [compose, setCompose] = useState(null); // null | { target }
  const [spawnForm, setSpawnForm] = useState(null); // null | { prompt?, cwd?, planId? }
  const [priorities, setPriorities] = useState(() => new Set());
  const [threads, setThreads] = useState({}); // sid -> [{text, at}] (this tab only)
  const [ripples, setRipples] = useState(() => new Map()); // sid -> until(ms)
  const prevConflicts = useRef({ keys: null, sawData: false });

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
      if (e.key === 'Escape') { setDrawerSid(null); setCompose(null); setSpawnForm(null); return; }
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

  const conflictMsg = conflicts.length
    ? conflicts.map((c) => `${basename(c.file || c.rel_path)} — ${(c.sessions || []).map(csOf).join(' × ')}`).join('   ·   ')
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
              now={now}
              compact={compact}
              stale={stale}
              repoFilter={repoFilter}
              onRepoFilter={setRepoFilter}
              ripples={ripples}
              priorities={priorities}
              onOpenSession={setDrawerSid}
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
    </div>
  );
}
