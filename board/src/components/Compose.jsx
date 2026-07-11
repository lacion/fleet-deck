import React, { useEffect, useRef, useState } from 'react';
import { sendMail, sendCommand } from '../api.js';
import { basename } from '../util.js';

// Honest per-target feedback from POST /mail `targets` ({session_id, callsign,
// route}). watcher/pane routes deliver without the human doing anything;
// turn-boundary/offline-queued wait for the session itself.
function deliveryNote(targets) {
  if (!targets.length) return '→ sent — no matching recipients';
  const names = (list) => list.map((t) => t.callsign || t.session_id).join(', ');
  const now = targets.filter((t) => t.route === 'watcher' || t.route === 'pane');
  const later = targets.filter((t) => t.route !== 'watcher' && t.route !== 'pane');
  if (!later.length) return `→ delivering now to ${names(now)}`;
  if (!now.length) return `→ queued for ${names(later)} — delivers at next turn boundary`;
  return `→ delivering now to ${names(now)} · queued: ${names(later)} (next turn boundary)`;
}

// Compose modal: mail to a session / all / repo:<name>, or free text to the
// orchestrator (POST /command — broadcast / assign / note).
export default function Compose({ initialTarget, sessions, repos, onClose, onSent, spawnAvailable, onSpawnFor }) {
  const [target, setTarget] = useState(initialTarget || 'all');
  const [text, setText] = useState('');
  const [err, setErr] = useState(null);
  const [note, setNote] = useState(null); // brief confirmation after a daemon command
  const [unroutedText, setUnroutedText] = useState(null); // v1.2: task text of an unrouted command → spawn CTA
  const [busy, setBusy] = useState(false);
  const taRef = useRef(null);

  useEffect(() => { taRef.current?.focus(); }, []);

  const targets = [
    { id: 'all', label: 'ALL' },
    { id: 'daemon', label: 'ORCHESTRATOR' },
    ...repos
      .filter((r) => r.repo_name || r.repo_id)
      .map((r) => {
        const name = r.repo_name || basename(r.repo_id);
        return { id: `repo:${name}`, label: `repo:${name}` };
      }),
    ...sessions
      .filter((s) => s.col !== 'offline')
      .map((s) => ({ id: s.session_id, label: s.callsign || s.session_id })),
  ];

  const send = async () => {
    if (!text.trim() || busy) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    const sent = text.trim();
    try {
      const res = target === 'daemon'
        ? await sendCommand(sent)
        : await sendMail(target, sent);
      // v1.1 auto-routing: /command may answer {ok:true, assigned_to:{callsign}}
      // (routed) or {ok:false, unrouted:true} (no candidate, task still logged)
      // — both count as "sent", neither is an error to show in red.
      const callsign = target === 'daemon' ? res.json?.assigned_to?.callsign : null;
      const unrouted = target === 'daemon' && res.json?.unrouted === true;
      if (res.ok || unrouted) {
        onSent?.(target, sent);
        if (callsign) {
          setNote(`→ routed to ${callsign}`);
          setText('');
        } else if (unrouted) {
          setNote('no available session — task logged');
          setUnroutedText(res.json?.text ?? sent);
          setText('');
        } else if (target !== 'daemon' && Array.isArray(res.json?.targets)) {
          // fix C: the daemon now says HOW each recipient gets the mail —
          // confirm inline (same affordance as daemon-command notes).
          setNote(deliveryNote(res.json.targets));
          setText('');
        } else {
          onClose();
        }
      } else {
        setErr(res.json?.err || `send failed (${res.status})`);
      }
    } catch {
      setErr('daemon unreachable');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div className="fd-compose" role="dialog" aria-label="Compose" onClick={(e) => e.stopPropagation()}>
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">COMPOSE</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>✕</button>
        </div>
        <div className="fd-targets">
          {targets.map((t) => (
            <button
              key={t.id}
              type="button"
              className={`fd-target${target === t.id ? ' on' : ''}`}
              onClick={() => { setTarget(t.id); setNote(null); setErr(null); setUnroutedText(null); }}
            >
              {t.label}
            </button>
          ))}
        </div>
        <textarea
          ref={taRef}
          rows={4}
          placeholder={target === 'daemon'
            ? 'Instruct the orchestrator…  (broadcast <text> · assign <callsign> <text> · assign auto <text> · assign auto:<repo> <text> · note)'
            : 'Write to the fleet…'}
          value={text}
          onChange={(e) => { setText(e.target.value); if (note) setNote(null); if (unroutedText) setUnroutedText(null); }}
          onKeyDown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) send(); }}
        />
        <div className="foot">
          {note ? (
            <span className="note" style={{ color: 'var(--ok)' }}>{note}</span>
          ) : (
            <span className="note">
              {target === 'daemon' ? 'runs in the daemon immediately' : 'delivers at the agent’s next turn boundary — idle sessions usually wake within seconds'}
            </span>
          )}
          {unroutedText != null && spawnAvailable && (
            <button type="button" className="fd-ctabtn" onClick={() => onSpawnFor?.(unroutedText)}>
              spawn a session for this
            </button>
          )}
          {err && <span className="note" style={{ color: 'var(--hazard)' }}>{err}</span>}
          <span className="fd-spacer" />
          <button type="button" className="send" onClick={send} disabled={!text.trim() || busy}>
            Send ⏎
          </button>
        </div>
      </div>
    </div>
  );
}
