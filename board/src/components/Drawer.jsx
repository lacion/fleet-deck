import React, { useState } from 'react';
import { human, hhmmss, basename, prettyModel, modelFamily } from '../util.js';
import { sendMail, killSpawn } from '../api.js';

// Owned-pane block (v1.2): attach hint + kill. Kill lives in the drawer
// ONLY — first click arms a confirm; a non-offline card takes the warning
// (force) path; 409/410 from the daemon surface honestly.
function OwnedPane({ s }) {
  const [kill, setKill] = useState({ state: 'idle', err: null, ok: null }); // idle | armed | busy | done
  const alive = s.col !== 'offline';
  const win = s.spawn.tmux_window || '';
  // the window is named fd<port>-<callsign> — recover the daemon port for
  // the attach hint from it (falls back to how this board was reached)
  const m = /^fd(\d+)-/.exec(win);
  const tmuxSession = `fleetdeck-${m ? m[1] : (location.port || '4711')}`;

  const doKill = async () => {
    setKill({ state: 'busy', err: null, ok: null });
    try {
      const res = await killSpawn(s.spawn.spawn_id, alive);
      if (res.ok && res.json?.ok !== false) {
        setKill({ state: 'done', err: null, ok: 'pane killed' });
      } else {
        const reason = res.json?.reason || res.json?.err;
        const msg =
          res.status === 409 ? (reason || 'refused — session is not offline (409)')
          : res.status === 410 ? (reason || 'window already gone (410)')
          : res.status === 404 ? (reason || 'unknown spawn (404)')
          : (reason || `kill failed (${res.status})`);
        setKill({ state: 'idle', err: msg, ok: null });
      }
    } catch {
      setKill({ state: 'idle', err: 'daemon unreachable', ok: null });
    }
  };

  return (
    <div className="fd-sect">
      <div className="sl" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        OWNED PANE · {String(s.spawn.status || 'live').toUpperCase()}
        {s.spawn.skip_permissions && (
          <span className="fd-unsupchip" title="spawned with permissions bypassed — it will never ask before acting">
            unsupervised
          </span>
        )}
      </div>
      <div className="fd-attach">
        $ tmux attach -t {tmuxSession}{'\n'}
        {'  '}window {win}
      </div>
      <div className="fd-killrow">
        {kill.state !== 'armed' && kill.state !== 'done' && (
          <button
            type="button"
            className="fd-ghostbtn fd-killbtn"
            disabled={kill.state === 'busy'}
            onClick={() => setKill({ state: 'armed', err: null, ok: null })}
          >
            {kill.state === 'busy' ? 'Killing…' : 'Kill pane'}
          </button>
        )}
        {kill.state === 'armed' && (
          <>
            <span className="fd-killwarn">
              {alive ? 'session appears alive — force kill the pane?' : 'kill the pane?'}
            </span>
            <button type="button" className="fd-ghostbtn fd-killbtn armed" onClick={doKill}>
              {alive ? 'Force kill' : 'Kill'}
            </button>
            <button
              type="button"
              className="fd-ghostbtn"
              onClick={() => setKill({ state: 'idle', err: null, ok: null })}
            >
              Cancel
            </button>
          </>
        )}
        {kill.ok && <span className="fd-killok">✓ {kill.ok}</span>}
        {kill.err && <span className="fd-killwarn">✗ {kill.err}</span>}
      </div>
    </div>
  );
}

// Session detail drawer. TIMELINE approximates the design's per-session log
// with the ticker lines that mention this callsign (the snapshot carries no
// per-session event log). THREAD is board→session mail sent from this tab;
// delivery is turn-boundary, so entries stay marked queued.
export default function Drawer({
  s, now, conflictFiles, mailCount, priority, onTogglePriority, onClose, onCompose, thread, onSendThread,
}) {
  const [draft, setDraft] = useState('');
  const fam = modelFamily(s.model);
  const pulseClass =
    s.col === 'working' ? 'working'
    : s.col === 'verifying' ? 'verifying'
    : s.col === 'needsyou' ? 'needsyou'
    : s.col === 'offline' ? 'offline'
    : 'still';
  const files = (s.files || []).map(basename);
  const hot = new Set(conflictFiles.map(basename));
  const offline = s.col === 'offline';

  const send = () => {
    const text = draft.trim();
    if (!text) return;
    onSendThread(text);
    setDraft('');
  };

  return (
    <>
      <div className="fd-scrim" onClick={onClose} />
      <div className="fd-drawer" role="dialog" aria-label={`Session ${s.callsign}`}>
        <div className="fd-drawerhead">
          <span className={`fd-pulse ${pulseClass}`} style={{ width: 9, height: 9 }} />
          <span className="callsign">{s.callsign || s.session_id}</span>
          <span className={`fd-mbadge ${fam}`}>{prettyModel(s.model)}</span>
          <span className="col">{String(s.col || '').toUpperCase()}</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>✕</button>
        </div>
        <div className="fd-drawerbody">
          <div className="fd-sect">
            <div className="sl">TASK</div>
            <div className="task">{s.task || s.note || '—'}</div>
          </div>
          <div className="fd-meta">
            <div>repo <span title={s.cwd || ''}>{s.repo_name || basename(s.repo_id || '') || '—'}{s.worktree && basename(s.worktree) !== s.repo_name ? ` (wt: ${basename(s.worktree)})` : ''}</span></div>
            <div>branch <span>⎇ {s.branch || '—'}</span></div>
            <div>started <span>{s.startedAt ? human(now - s.startedAt) + ' ago' : '—'}</span></div>
            <div>events <span>{s.events ?? 0}{s.lastTool ? ` · last ${s.lastTool}` : ''}</span></div>
            <div>seen <span>{s.lastSeen ? human(now - s.lastSeen) + ' ago' : '—'}</span></div>
            <div>mail <span>{mailCount} queued</span></div>
          </div>
          <div className="fd-drawerbtns">
            <button type="button" className="fd-primary" onClick={onCompose}>Message</button>
            <button
              type="button"
              className="fd-ghostbtn"
              onClick={() => sendMail(s.session_id, 'nudge: status check-in please')}
            >
              Nudge
            </button>
            <button
              type="button"
              className="fd-ghostbtn"
              style={priority ? { color: 'var(--act)' } : undefined}
              onClick={onTogglePriority}
            >
              {priority ? '★ Priority' : '☆ Priority'}
            </button>
          </div>
          {s.spawn && <OwnedPane s={s} />}
          <div className="fd-sect">
            <div className="sl">FILES</div>
            <div className="fd-filewrap">
              {files.length === 0 && <span className="fd-filechip">none yet</span>}
              {files.map((f, i) => (
                <span key={i} className={`fd-filechip${hot.has(f) ? ' hot' : ''}`}>
                  {f}{hot.has(f) ? '  ▲ contested' : ''}
                </span>
              ))}
            </div>
          </div>
          <div className="fd-sect">
            <div className="sl">TIMELINE</div>
            <div className="fd-timeline">
              {(s.timeline || []).length === 0 && (
                <div className="ev"><span className="m">nothing in the recent ticker for this session</span></div>
              )}
              {(s.timeline || []).map((ev, i) => (
                <div className="ev" key={i}>
                  <span className="t">{hhmmss(ev.at)}</span>
                  <span className="m">{ev.msg}</span>
                </div>
              ))}
            </div>
          </div>
          <div className="fd-sect">
            <div className="sl">THREAD</div>
            <div className="fd-thread">
              {thread.length === 0 && (
                <div className="none">No messages yet. Anything you send lands at the agent’s next turn boundary — idle sessions usually wake within seconds.</div>
              )}
              {thread.map((m, i) => (
                <div className="fd-msg" key={i}>
                  <div className="bubble">{m.text}</div>
                  <div className="state">
                    {offline
                      ? `⧗ sent ${hhmmss(m.at)} — delivers on resume`
                      : `⧗ sent ${hhmmss(m.at)} — delivers at next turn boundary — idle sessions usually wake within seconds`}
                  </div>
                </div>
              ))}
              <div className="fd-freerow">
                <input
                  className="fd-input"
                  style={{ fontFamily: 'var(--font-ui)', fontSize: '12.5px' }}
                  placeholder={`Message ${s.callsign || 'session'}…`}
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => { if (e.key === 'Enter') send(); }}
                />
                <button type="button" className="fd-send" disabled={!draft.trim()} onClick={send}>Send</button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
