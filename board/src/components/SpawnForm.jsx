import React, { useEffect, useRef, useState } from 'react';
import { spawnSession } from '../api.js';

// v1.2 spawn form — POST /api/spawn on an explicit human click, never any
// other path. Fail-loud: a {ok:false, reason} renders inline, no silent
// retry; success shows "spawning — <callsign>" then closes.
//
// v1.3 additions:
//   - "run unsupervised" is hazard-styled with a TWO-STEP confirm: checking
//     it only reveals the confirm row; a second explicit arm checkbox is what
//     puts dangerously_skip_permissions:true on the POST body. Un-arming (or
//     unchecking step one) drops the flag. While revealed-but-unarmed the
//     Spawn button stays disabled — the form never quietly downgrades.
//   - plan execution prefill (planMode): prompt arrives prefilled, the
//     textarea grows, and the caret parks right after "Custom instructions: "
//     so the human edits exactly where the contract expects; after a
//     successful POST the onSpawned callback (App) marks the plan executed
//     and any failure of THAT surfaces here instead of auto-closing over it.
export default function SpawnForm({ sessions, prefillPrompt, prefillCwd, planMode, onClose, onSpawned }) {
  const [cwd, setCwd] = useState(prefillCwd || '');
  const [prompt, setPrompt] = useState(prefillPrompt || '');
  const [model, setModel] = useState('');
  const [permissionMode, setPermissionMode] = useState('default');
  const [worktree, setWorktree] = useState(false);
  const [unsup, setUnsup] = useState(false);   // step 1: reveal the confirm
  const [armed, setArmed] = useState(false);   // step 2: actually send the flag
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  const [note, setNote] = useState(null); // "spawning — <callsign>"
  const cwdRef = useRef(null);
  const promptRef = useRef(null);
  const closeTimer = useRef(null);

  useEffect(() => {
    if (planMode && promptRef.current) {
      // park the caret after "Custom instructions: " — the one editable spot
      const el = promptRef.current;
      el.focus();
      const marker = 'Custom instructions: ';
      const idx = (prefillPrompt || '').indexOf(marker);
      const pos = idx >= 0 ? idx + marker.length : 0;
      try { el.setSelectionRange(pos, pos); } catch { /* older engines */ }
      el.scrollTop = 0;
    } else {
      cwdRef.current?.focus();
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => () => clearTimeout(closeTimer.current), []);

  // distinct places the fleet has been seen working → cwd suggestions
  const suggestions = [...new Set(
    (sessions || []).flatMap((s) => [s.worktree, s.cwd]).filter(Boolean),
  )];

  // hazard checkbox checked but not armed → refuse to spawn either way
  const blocked = unsup && !armed;

  const submit = async () => {
    if (!cwd.trim() || busy || note || blocked) return;
    setBusy(true);
    setErr(null);
    const body = { cwd: cwd.trim() };
    if (prompt.trim()) body.prompt = prompt.trim();
    if (model.trim()) body.model = model.trim();
    if (permissionMode !== 'default') body.permission_mode = permissionMode;
    if (worktree) body.worktree = true;
    if (unsup && armed) body.dangerously_skip_permissions = true;
    try {
      const res = await spawnSession(body);
      if (res.ok && res.json?.ok) {
        setNote(`spawning — ${res.json.callsign || res.json.session_id || 'new session'}`);
        // plan execution: mark the plan executed (via:'spawn:<id>'); a mark
        // failure keeps the form open and says so — never close over a 409
        let extra = null;
        if (onSpawned) {
          try { extra = await onSpawned(res.json); } catch { extra = { ok: false, text: 'plan mark failed — daemon unreachable' }; }
        }
        if (extra && !extra.ok) {
          setErr(extra.text);
        } else {
          if (extra?.text) setNote((n) => `${n} · ${extra.text}`);
          closeTimer.current = setTimeout(onClose, 1400);
        }
        // busy stays true: the form is closing (or the error owns it), no re-submit
      } else {
        setErr(res.json?.reason || `spawn failed (${res.status})`);
        setBusy(false);
      }
    } catch {
      setErr('daemon unreachable');
      setBusy(false);
    }
  };

  const onCtrlEnter = (e) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submit();
  };

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div className="fd-compose fd-spawn" role="dialog" aria-label="Spawn a session" onClick={(e) => e.stopPropagation()}>
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">{planMode ? 'SPAWN SESSION — EXECUTE PLAN' : 'SPAWN SESSION'}</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>✕</button>
        </div>
        <div className="fd-form">
          <div className="frow">
            <span className="fl">cwd *</span>
            <input
              ref={cwdRef}
              className="fd-input"
              list="fd-cwd-suggest"
              placeholder="/path/to/repo"
              value={cwd}
              onChange={(e) => { setCwd(e.target.value); if (err) setErr(null); }}
              onKeyDown={onCtrlEnter}
            />
            <datalist id="fd-cwd-suggest">
              {suggestions.map((p) => <option key={p} value={p} />)}
            </datalist>
          </div>
          <div className="frow top">
            <span className="fl">prompt</span>
            <textarea
              ref={promptRef}
              className="fd-input"
              rows={planMode ? 12 : 3}
              placeholder="Initial prompt (optional)"
              value={prompt}
              onChange={(e) => { setPrompt(e.target.value); if (err) setErr(null); }}
              onKeyDown={onCtrlEnter}
            />
          </div>
          <div className="frow">
            <span className="fl">model</span>
            <input
              className="fd-input"
              placeholder="default"
              value={model}
              onChange={(e) => { setModel(e.target.value); if (err) setErr(null); }}
              onKeyDown={onCtrlEnter}
            />
          </div>
          <div className="frow">
            <span className="fl">permission-mode</span>
            <select
              className="fd-input"
              value={permissionMode}
              onChange={(e) => setPermissionMode(e.target.value)}
            >
              <option value="default">default</option>
              <option value="acceptEdits">acceptEdits</option>
              <option value="plan">plan</option>
              <option value="bypassPermissions">bypassPermissions</option>
            </select>
          </div>
          <div className="frow">
            <span className="fl">worktree</span>
            <label className="fd-check">
              <input
                type="checkbox"
                checked={worktree}
                onChange={(e) => setWorktree(e.target.checked)}
              />
              work in a fresh git worktree
            </label>
          </div>
          {/* v1.3 — unsupervised (two-step: reveal, then arm) */}
          <div className="frow">
            <span className="fl">unsupervised</span>
            <label className="fd-check hazard">
              <input
                type="checkbox"
                checked={unsup}
                onChange={(e) => { setUnsup(e.target.checked); if (!e.target.checked) setArmed(false); if (err) setErr(null); }}
              />
              run unsupervised
            </label>
          </div>
          {unsup && (
            <div className="frow top">
              <span className="fl" />
              <div className="fd-hazardconfirm">
                <div className="warn">⚠ this session will never ask permission for anything</div>
                <div className="sub">no permission cards will ever reach this board for it</div>
                <label className="fd-check hazard">
                  <input
                    type="checkbox"
                    checked={armed}
                    onChange={(e) => { setArmed(e.target.checked); if (err) setErr(null); }}
                  />
                  I understand — arm it
                </label>
              </div>
            </div>
          )}
        </div>
        {err && <div className="fd-spawnerr">✗ {err}</div>}
        <div className="foot">
          {note
            ? <span className="note" style={{ color: 'var(--ok)' }}>{note}</span>
            : blocked
              ? <span className="note" style={{ color: 'var(--hazard)' }}>arm the unsupervised confirm — or uncheck it</span>
              : <span className="note">starts a new billed Claude session</span>}
          <span className="fd-spacer" />
          <button type="button" className="send" onClick={submit} disabled={!cwd.trim() || busy || blocked}>
            Spawn ⏎
          </button>
        </div>
      </div>
    </div>
  );
}
