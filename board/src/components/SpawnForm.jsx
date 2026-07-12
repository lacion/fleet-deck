import React, { useEffect, useMemo, useRef, useState } from 'react';
import { spawnSession } from '../api.js';
import { batchTotal, expandBatchTasks, parseBatchTasks } from '../util.js';

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
//
// v1.6: "remote control" checkbox → remote_control:true on the POST body —
// the session comes up remote-controllable from claude.ai (web/phone) from
// birth; the claude.ai link lands on the card once the daemon harvests it.
//
// BATCH: ticking "batch" turns the prompt box into a task list — one agent per
// line, "3x <task>" to run a line several times — and fans the whole list out
// across the repo in one submit. Two rules make it safe:
//
//   • It is opt-in. A multi-line prompt is still ONE prompt otherwise, which
//     plan execution depends on absolutely (the plan IS the prompt).
//   • Every agent in a batch gets its OWN git worktree, forced, not offered.
//     N agents sharing one working tree overwrite each other's edits, and the
//     only thing that would tell you is a conflict warning after the fact.
//
// Since the daemon no longer caps how many agents may be live, the preview
// below — the exact list, counted, before you click — IS the guardrail.
export default function SpawnForm({ sessions, prefillPrompt, prefillCwd, planMode, onClose, onSpawned }) {
  const [cwd, setCwd] = useState(prefillCwd || '');
  const [prompt, setPrompt] = useState(prefillPrompt || '');
  const [model, setModel] = useState('');
  const [permissionMode, setPermissionMode] = useState('default');
  const [worktree, setWorktree] = useState(false);
  const [batch, setBatch] = useState(false);   // prompt box becomes a task list
  const [remote, setRemote] = useState(false); // v1.6: remote control from birth
  const [unsup, setUnsup] = useState(false);   // step 1: reveal the confirm
  const [armed, setArmed] = useState(false);   // step 2: actually send the flag
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState(null);
  const [note, setNote] = useState(null); // "spawning — <callsign>"
  const [progress, setProgress] = useState(null); // batch: {done, total, failed[]}
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

  // Batch is meaningless for plan execution: there, the prompt IS the plan, and
  // splitting it on newlines would fan a single brief out into dozens of agents.
  const canBatch = !planMode;
  const batching = batch && canBatch;
  const tasks = useMemo(() => (batching ? parseBatchTasks(prompt) : []), [batching, prompt]);
  const total = batching ? batchTotal(tasks) : 1;

  // hazard checkbox checked but not armed → refuse to spawn either way
  const blocked = unsup && !armed;
  // a batch needs at least one task, and each agent needs its own worktree
  const emptyBatch = batching && total === 0;

  /** The POST body shared by every agent in this submit; `prompt` varies. */
  const baseBody = () => {
    const body = { cwd: cwd.trim() };
    if (model.trim()) body.model = model.trim();
    if (permissionMode !== 'default') body.permission_mode = permissionMode;
    if (worktree || batching) body.worktree = true; // forced for a batch
    if (remote) body.remote_control = true;
    if (unsup && armed) body.dangerously_skip_permissions = true;
    return body;
  };

  // One agent — the original path, byte for byte, including the plan-mark hook.
  const submitOne = async () => {
    const body = baseBody();
    if (prompt.trim()) body.prompt = prompt.trim();
    const res = await spawnSession(body);
    if (!(res.ok && res.json?.ok)) {
      setErr(res.json?.reason || `spawn failed (${res.status})`);
      setBusy(false);
      return;
    }
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
  };

  // N agents, one at a time. Sequential on purpose: each spawn shells out to
  // `git worktree add`, and a failure part-way through must leave the agents
  // that DID launch alone and say plainly which ones didn't.
  const submitBatch = async () => {
    const prompts = expandBatchTasks(tasks);
    const launched = [];
    const failed = [];
    setProgress({ done: 0, total: prompts.length, failed });
    for (const [i, p] of prompts.entries()) {
      let res;
      try {
        res = await spawnSession({ ...baseBody(), prompt: p });
      } catch {
        res = null;
      }
      if (res?.ok && res.json?.ok) launched.push(res.json.callsign || res.json.session_id);
      else failed.push({ prompt: p, reason: res?.json?.reason || (res ? `spawn failed (${res.status})` : 'daemon unreachable') });
      setProgress({ done: i + 1, total: prompts.length, failed: [...failed] });
    }
    if (failed.length) {
      // Partial success, handled honestly. The agents that came up are up and
      // are NOT re-spawned by a retry: the task list is rewritten to hold only
      // the ones that failed, so the button now means "try these again" and the
      // preview above shows exactly that. Never close over a failure.
      setPrompt(failed.map((f) => f.prompt).join('\n'));
      setProgress(null);
      setErr(launched.length
        ? `spawned ${launched.length} of ${prompts.length} (${launched.join(', ')}) — the ${failed.length} left above failed: ${failed[0].reason}`
        : `none of the ${prompts.length} spawned: ${failed[0].reason}`);
      setBusy(false);
      return;
    }
    setNote(`spawning ${launched.length} — ${launched.join(', ')}`);
    closeTimer.current = setTimeout(onClose, 1800);
  };

  const submit = async () => {
    if (!cwd.trim() || busy || note || blocked || emptyBatch) return;
    setBusy(true);
    setErr(null);
    try {
      if (batching) await submitBatch();
      else await submitOne();
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
            <span className="fl">{batching ? 'tasks' : 'prompt'}</span>
            <textarea
              ref={promptRef}
              className="fd-input"
              rows={planMode ? 12 : (batching ? 6 : 3)}
              placeholder={batching
                ? 'One agent per line:\n  fix the flaky worktree test\n  3x audit the spawn path'
                : 'Initial prompt (optional)'}
              value={prompt}
              onChange={(e) => { setPrompt(e.target.value); if (err) setErr(null); }}
              onKeyDown={onCtrlEnter}
            />
          </div>
          {canBatch && (
            <div className="frow">
              <span className="fl">batch</span>
              <label className="fd-check">
                <input
                  type="checkbox"
                  checked={batch}
                  onChange={(e) => { setBatch(e.target.checked); if (err) setErr(null); }}
                />
                one agent per line — prefix <code>3x</code> to repeat a line
              </label>
            </div>
          )}
          {batching && total > 0 && (
            <div className="frow top">
              <span className="fl" />
              <div className="fd-batchpreview">
                <div className="hd">{total} agent{total === 1 ? '' : 's'}, each in its own worktree of <b>{cwd.trim() || 'cwd'}</b></div>
                <ol>
                  {tasks.map((t, i) => (
                    <li key={i}>
                      {t.count > 1 && <span className="mult">{t.count}×</span>}
                      {t.prompt}
                    </li>
                  ))}
                </ol>
              </div>
            </div>
          )}
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
                checked={worktree || batching}
                disabled={batching}
                onChange={(e) => setWorktree(e.target.checked)}
              />
              {batching
                ? 'each agent gets its own worktree — required for a batch'
                : 'work in a fresh git worktree'}
            </label>
          </div>
          {/* v1.6 — remote control from birth (/rc) */}
          <div className="frow">
            <span className="fl">remote control</span>
            <label className="fd-check">
              <input
                type="checkbox"
                checked={remote}
                onChange={(e) => setRemote(e.target.checked)}
              />
              📱 drive it from claude.ai (web / phone)
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
          {busy && progress && !note
            ? <span className="note">spawning {progress.done} of {progress.total}…</span>
            : note
              ? <span className="note" style={{ color: 'var(--ok)' }}>{note}</span>
              : blocked
                ? <span className="note" style={{ color: 'var(--hazard)' }}>arm the unsupervised confirm — or uncheck it</span>
                : emptyBatch
                  ? <span className="note">write at least one task, one per line</span>
                  : <span className="note">starts {total > 1 ? `${total} new billed Claude sessions` : 'a new billed Claude session'}</span>}
          <span className="fd-spacer" />
          <button type="button" className="send" onClick={submit} disabled={!cwd.trim() || busy || blocked || emptyBatch}>
            {total > 1 ? `Spawn ${total} ⏎` : 'Spawn ⏎'}
          </button>
        </div>
      </div>
    </div>
  );
}
