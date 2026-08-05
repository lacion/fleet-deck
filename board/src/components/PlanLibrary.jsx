import React, { useEffect, useMemo, useState } from 'react';
import { human, TURN_BOUNDARY_HINT } from '../util.js';
import { renderMarkdown, planTitle } from '../markdown.js';
import { assignPlan, markPlan, reasonOf } from '../api.js';

// v1.3 PLANS library — collapsible strip between the lanes and the feed
// (contract offered rail-under-questions as the alternative; the rail is the
// human's scarcest surface, so the library lives on the board side and never
// crowds needs-you). Cards come from /state `plans` (non-archived, newest
// first); no `plans` field at all → App hides this component entirely.
//
// Actions by status: Spawn executor + Assign on EXECUTABLE statuses; Archive
// on anything non-archived. Spawn executor is spawn UI, so it hides with the
// spawn capability. Every daemon 409 renders verbatim-honest, never swallowed.

// Statuses a fresh executor session can be spawned for. Status-agnostic on
// purpose: a new status (e.g. Phase 2.2's handled-in-terminal) joins or skips
// the actions by editing THIS set, never the copy.
const EXECUTABLE = new Set(['proposed', 'approved', 'captured']);

function markErr(res, did) {
  const reason = res.reason; // raw daemon reason (null when it sent none)
  if (res.status === 409) return `${did ? did + ', but ' : ''}409 — ${reason || 'bad transition'}`;
  if (res.status === 404) return `${did ? did + ', but ' : ''}404 — ${reason || 'unknown plan'}`;
  return `${did ? did + ', but ' : ''}${reasonOf(res, `mark failed (${res.status})`)}`;
}

function PlanCard({ p, now, liveSessions, spawnAvailable, onExecute }) {
  const [open, setOpen] = useState(false);
  const [assigning, setAssigning] = useState(false);
  const [target, setTarget] = useState(null); // session_id
  const [instr, setInstr] = useState('');
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState(null); // {cls, text}

  // M-P7 — parse the plan body once per text, not on every render/second.
  // M-P2 — a snapshot may ship a plan row without its body; keep both the title
  // and the expanded view honest about that rather than rendering an empty box.
  const md = p.plan_md || '';
  const title = useMemo(() => planTitle(md), [md]);
  const html = useMemo(() => (md.trim() ? renderMarkdown(md) : null), [md]);

  const executable = EXECUTABLE.has(p.status);
  // Duplicate-worker tell: the session that proposed this plan may still be
  // live (possibly mid-execution) — spawning a second executor is the footgun
  // the "still live" annotation exists to catch.
  const proposer = liveSessions.find((s) => s.session_id === p.session_id);

  const archive = async () => {
    setBusy(true);
    setNote(null);
    try {
      const res = await markPlan(p.plan_id, { status: 'archived' });
      if (res.ok) setNote({ cls: 'ok', text: '✓ archived — it leaves the library on the next snapshot' });
      else setNote({ cls: 'hazard', text: markErr(res, null) });
    } catch {
      setNote({ cls: 'hazard', text: 'daemon unreachable' });
    }
    setBusy(false);
  };

  const assign = async () => {
    if (!target || busy) return;
    setBusy(true);
    setNote(null);
    const cs = liveSessions.find((s) => s.session_id === target)?.callsign || target;
    // BUG-039 — the [FLEETDECK ASSIGNMENT] frame is daemon-reserved (POST /mail
    // 422s it), so the daemon composes and mails it: one request assigns the
    // plan and marks it executed, never a client-composed reserved frame.
    try {
      const res = await assignPlan(p.plan_id, { to: target, instructions: instr.trim() });
      if (res.ok) {
        setNote({ cls: 'ok', text: `✓ assigned to ${res.json?.callsign || cs} — marked executed` });
        setAssigning(false);
        setInstr('');
        setTarget(null);
      } else {
        setNote({ cls: 'hazard', text: markErr(res, null) });
      }
    } catch {
      setNote({ cls: 'hazard', text: 'daemon unreachable' });
    }
    setBusy(false);
  };

  return (
    <div className="fd-plan" data-planid={p.plan_id}>
      <button type="button" className="phead" onClick={() => setOpen(!open)} title={open ? 'collapse' : 'expand the full plan'}>
        <span className="tri">{open ? '▾' : '▸'}</span>
        <span className="ttl">{title}</span>
        <span className="callsign">{p.callsign || p.session_id}</span>
        {p.repo_name && <span className="repo">{p.repo_name}</span>}
        <span className={`fd-pstatus ${p.status}`}>{p.status}</span>
        {proposer && (
          <span className="fd-planlive">proposed by {p.callsign || proposer.callsign || proposer.session_id} — still live</span>
        )}
        <span className="fd-spacer" />
        <span className="age">{human(now - (p.created_at || now))}</span>
      </button>
      {open && (
        <div className="pbody">
          {html
            ? <div className="fd-md" dangerouslySetInnerHTML={{ __html: html }} />
            : <div className="fd-md"><em>plan text not included in this snapshot</em></div>}
        </div>
      )}
      <div className="pactions">
        {executable && spawnAvailable && (
          <span className="execwrap">
            <button
              type="button"
              className="fd-planbtn exec"
              disabled={busy}
              title="starts a NEW session with this plan as its prompt"
              onClick={() => onExecute(p)}
            >
              Spawn executor…
            </button>
            <span className="micro">starts a NEW session with this plan as its prompt</span>
          </span>
        )}
        {executable && (
          <button
            type="button"
            className="fd-planbtn"
            disabled={busy}
            onClick={() => { setAssigning(!assigning); setNote(null); }}
          >
            Assign
          </button>
        )}
        <button type="button" className="fd-planbtn arch" disabled={busy} onClick={archive}>
          Archive
        </button>
        {note && <span className={`status ${note.cls}`}>{note.text}</span>}
      </div>
      {assigning && (
        <div className="passign">
          {liveSessions.length === 0 ? (
            <span className="none">no live sessions to assign to</span>
          ) : (
            <>
              <div className="fd-targets">
                {liveSessions.map((s) => (
                  <button
                    key={s.session_id}
                    type="button"
                    className={`fd-target${target === s.session_id ? ' on' : ''}`}
                    onClick={() => setTarget(target === s.session_id ? null : s.session_id)}
                  >
                    {s.callsign || s.session_id}
                  </button>
                ))}
              </div>
              <div className="fd-freerow">
                <input
                  className="fd-input"
                  placeholder="Custom instructions (optional)…"
                  value={instr}
                  onChange={(e) => setInstr(e.target.value)}
                  onKeyDown={(e) => { if (e.key === 'Enter') assign(); }}
                />
                <button type="button" className="fd-send" disabled={busy || !target} onClick={assign}>
                  Send
                </button>
              </div>
              <span className="micro">{`mails the framed plan — delivered at ${TURN_BOUNDARY_HINT}`}</span>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default function PlanLibrary({ plans, sessions, now, spawnAvailable, onExecute }) {
  // Never-toggled (no stored pref) defaults OPEN when the library is populated
  // — an invisible plan library is a dead one. Once the user toggles, the
  // stored '0'/'1' is their word and we keep it. The rail stays sacred: the
  // strip still lives below the lanes and never touches needs-you.
  const [open, setOpen] = useState(() => {
    const stored = localStorage.getItem('fd-plans-open');
    return stored === null ? plans.length > 0 : stored === '1';
  });
  const toggle = () => {
    setOpen((o) => {
      localStorage.setItem('fd-plans-open', o ? '0' : '1');
      return !o;
    });
  };
  const liveSessions = (sessions || []).filter((s) => s.col !== 'offline');

  // Snapshots stream in after mount — if the first one was empty and plans
  // arrive later, apply the same never-toggled default: open once, only while
  // the user still hasn't touched the toggle.
  useEffect(() => {
    if (!open && plans.length > 0 && localStorage.getItem('fd-plans-open') === null) setOpen(true);
  }, [open, plans.length]);

  return (
    <div className="fd-plans">
      <button type="button" className="fd-planshead" onClick={toggle} aria-expanded={open}>
        <span className="tri">{open ? '▾' : '▸'}</span>
        <span className="lbl">PLANS</span>
        {plans.length > 0 && <span className="count">{plans.length}</span>}
        <span className="fd-spacer" />
        <span className="hint">the fleet plan library — spawn a NEW executor session, assign to a live one, or archive</span>
      </button>
      {open && (
        <div className="fd-planlist">
          {plans.length === 0 && (
            <div className="none">
              no plans yet — spawn with permission-mode: plan, then Approve or Capture its exit-plan question in NEEDS YOU and it lands here
            </div>
          )}
          {plans.map((p) => (
            <PlanCard
              key={p.plan_id}
              p={p}
              now={now}
              liveSessions={liveSessions}
              spawnAvailable={spawnAvailable}
              onExecute={onExecute}
            />
          ))}
        </div>
      )}
    </div>
  );
}
