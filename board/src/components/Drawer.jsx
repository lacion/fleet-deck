import React, { useState } from 'react';
import { human, hhmmss, basename, prettyModel, modelFamily, spawnKillable, spawnRemoteAvailable } from '../util.js';
import { sendMail, reviveSpawn, enableRemote } from '../api.js';

// Owned-pane block (v1.2): attach hint + kill.
// v1.4 adds the live-terminal door (onOpenTerm — App passes it only while
// the pane is viewable).
// v1.6 adds remote control: an enable button on RUNNING sessions (POST
// /api/spawn/<id>/rc) and the harvested claude.ai link once it exists.
// v1.8: kill no longer lives here alone (the card carries it too) and no
// longer arms in place — both doors open App's KillConfirm dialog, which owns
// the POST and reports the outcome on the shared header strip.
function OwnedPane({ s, onOpenTerm, onKill }) {
  // v1.5 revive — offline + spawn.revivable only; one click, no confirm
  // (worst case is a fresh QUEUED card, not data loss)
  const [rev, setRev] = useState({ state: 'idle', err: null, ok: null }); // idle | busy | done
  // v1.6 remote control — the daemon types /rc into the pane and harvests the
  // claude.ai link, so the round-trip runs ~3-6 s; the button owns a busy
  // state and a 409 (mid-turn / pane not live) surfaces honestly.
  const [rc, setRc] = useState({ state: 'idle', err: null, ok: null, url: null }); // idle | busy | done
  const alive = s.col !== 'offline';
  const revivable = !alive && !!s.spawn.revivable;
  // snapshot truth first; the fresh POST response fills the gap until the
  // next snapshot lands (WS pushes on every mutation, so it's brief)
  const remoteOn = !!s.spawn.remote?.enabled || rc.state === 'done';
  const remoteUrl = s.spawn.remote?.url || rc.url;
  const win = s.spawn.tmux_window || '';
  // the window is named fd<port>-<callsign> — recover the daemon port for
  // the attach hint from it (falls back to how this board was reached)
  const m = /^fd(\d+)-/.exec(win);
  const tmuxSession = `fleetdeck-${m ? m[1] : (location.port || '4711')}`;

  const doRevive = async () => {
    setRev({ state: 'busy', err: null, ok: null });
    try {
      const res = await reviveSpawn(s.spawn.spawn_id);
      if (res.ok && res.json?.ok !== false) {
        setRev({ state: 'done', err: null, ok: 'reviving — card moves to QUEUED' });
      } else {
        const reason = res.json?.reason || res.json?.err;
        setRev({ state: 'idle', err: reason || `revive failed (${res.status})`, ok: null });
      }
    } catch {
      setRev({ state: 'idle', err: 'daemon unreachable', ok: null });
    }
  };

  const doRemote = async () => {
    setRc({ state: 'busy', err: null, ok: null, url: null });
    try {
      const res = await enableRemote(s.spawn.spawn_id);
      if (res.ok && res.json?.ok !== false) {
        const url = res.json?.url || null;
        setRc({
          state: 'done',
          err: null,
          ok: url ? 'remote control on' : 'remote control on — harvesting the claude.ai link',
          url,
        });
      } else {
        const reason = res.json?.reason || res.json?.err;
        const msg = res.status === 409
          ? (reason || 'refused — session is mid-turn (409), retry when it goes idle')
          : (reason || `remote control failed (${res.status})`);
        setRc({ state: 'idle', err: msg, ok: null, url: null });
      }
    } catch {
      setRc({ state: 'idle', err: 'daemon unreachable', ok: null, url: null });
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
        {remoteOn && (
          <span className="fd-remotechip" title="remote control on — this session can be driven from claude.ai">
            📱 remote
          </span>
        )}
      </div>
      <div className="fd-attach">
        $ tmux attach -t {tmuxSession}{'\n'}
        {'  '}window {win}
      </div>
      {/* v1.6 — the claude.ai door (or its pending placeholder) */}
      {remoteOn && (
        remoteUrl ? (
          <div className="fd-remoterow">
            <a href={remoteUrl} target="_blank" rel="noopener noreferrer" title={remoteUrl}>
              📱 open on claude.ai ↗
            </a>
          </div>
        ) : (
          <div className="fd-remoterow pending">
            📱 remote on — claude.ai link not captured yet; if it never lands here, find it in the live terminal (▣)
          </div>
        )
      )}
      <div className="fd-killrow">
        {onOpenTerm && (
          <button
            type="button"
            className="fd-ghostbtn"
            title="open a live terminal onto this pane — keystrokes go to the agent"
            onClick={onOpenTerm}
          >
            ▣ Live terminal
          </button>
        )}
        {spawnRemoteAvailable(s) && rc.state !== 'done' && (
          // offered only when the daemon would say yes (live pane, turn
          // boundary) — a mid-click race still 409s honestly below
          <button
            type="button"
            className="fd-ghostbtn"
            disabled={rc.state === 'busy'}
            title="put this agent on remote control — drive it from claude.ai on web or phone (types /rc into the pane; takes a few seconds)"
            onClick={doRemote}
          >
            📱 {rc.state === 'busy' ? 'Enabling remote… (~5 s)' : 'Enable remote'}
          </button>
        )}
        {revivable && rev.state !== 'done' && (
          <button
            type="button"
            className="fd-ghostbtn"
            disabled={rev.state === 'busy'}
            title="worktree + transcript survived — resume this agent (card moves to QUEUED)"
            onClick={doRevive}
          >
            ⟲ {rev.state === 'busy' ? 'Reviving…' : 'Revive'}
          </button>
        )}
        {onKill && spawnKillable(s) && (
          // one click OPENS THE DIALOG — the kill itself needs a second,
          // deliberate confirmation there (and the outcome lands on the strip)
          <button
            type="button"
            className="fd-ghostbtn fd-killbtn"
            title="kill this agent — asks first; the worktree and branch are left alone"
            onClick={onKill}
          >
            ☠ {alive ? 'Kill pane…' : 'Kill window…'}
          </button>
        )}
        {rc.ok && <span className="fd-killok">✓ {rc.ok}</span>}
        {rc.err && <span className="fd-killwarn">✗ {rc.err}</span>}
        {rev.ok && <span className="fd-killok">✓ {rev.ok}</span>}
        {rev.err && <span className="fd-killwarn">✗ {rev.err}</span>}
      </div>
    </div>
  );
}

// Session detail drawer. TIMELINE approximates the design's per-session log
// with the ticker lines that mention this callsign (the snapshot carries no
// per-session event log). THREAD is board→session mail sent from this tab;
// delivery is turn-boundary, so entries stay marked queued.
export default function Drawer({
  s, now, conflictFiles, mailCount, priority, onTogglePriority, onClose, onCompose, onOpenTerm, onKill,
  thread, onSendThread,
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
          {s.spawn && <OwnedPane s={s} onOpenTerm={onOpenTerm} onKill={onKill} />}
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
