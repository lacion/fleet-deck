import React from 'react';
import Sparkline from './Sparkline.jsx';
import { human, basename, prettyModel, modelShort, modelFamily } from '../util.js';

// How queued mail will reach this session (snapshot mail_meta[sid].route).
// watcher/pane deliver without the human doing anything; the other two wait.
const MAIL_HINT = {
  watcher: 'delivering — watcher live',
  pane: 'will be typed into the pane',
  'turn-boundary': 'queued — delivers at next turn',
  'offline-queued': 'queued — session offline, delivers on resume',
};

// One session card. Faithful to the design: pulse dot, mono callsign, model
// badge (--m-* family colors), branch/worktree line, note, conflict hazard
// edge + one-shot ripple, file chips, sparkline + age.
export default function SessionCard({
  s, now, compact, mailCount, mailMeta, conflictFiles, conflictPeers, ripple, priority, onOpen,
}) {
  const offline = s.col === 'offline';
  const needsyou = s.col === 'needsyou';
  const inConflict = conflictFiles.length > 0;
  const fam = modelFamily(s.model);
  const pulseClass =
    s.col === 'working' ? 'working'
    : s.col === 'verifying' ? 'verifying'
    : needsyou ? 'needsyou'
    : offline ? 'offline'
    : 'still';
  const files = (s.files || []).map(basename);
  const hot = new Set(conflictFiles.map(basename));
  const shown = files.slice(0, 4);
  // spawn watchdog (fix B): pane came up but the session never phoned home.
  // spawn.status stays 'spawning'; the daemon also flips the card to needsyou
  // with a descriptive note, so the chip is the compact echo of that state.
  const stalled = !!s.spawn?.stalled;
  const stalledTip = 'pane is up but never reached this daemon — likely env/port issue'
    + (s.spawn?.tmux_window ? `; check tmux window ${s.spawn.tmux_window}` : '');
  // mail route truth (fix C): snapshot mail_meta[sid] = {queued, oldest_at,
  // route}. Older daemons omit it — the badge falls back to a bare count.
  const mailHint = MAIL_HINT[mailMeta?.route];
  const mailStuck = mailMeta?.route === 'turn-boundary' || mailMeta?.route === 'offline-queued';
  // the daemon records worktree = toplevel of cwd even for the main tree —
  // only badge REAL secondary worktrees (toplevel differs from the repo name)
  const wt = s.worktree && basename(s.worktree) !== s.repo_name ? basename(s.worktree) : null;

  const cls = [
    'fd-card',
    inConflict && 'conflict',
    needsyou && 'needsyou',
    priority && !inConflict && 'priority',
    offline && 'offline',
    ripple && 'ripple',
  ].filter(Boolean).join(' ');

  return (
    <button type="button" className={cls} onClick={onOpen} title={s.task || s.note || s.callsign}>
      <span className="row1">
        <span className={`fd-pulse ${pulseClass}`} />
        <span className="callsign">{s.callsign || s.session_id}</span>
        {priority && <span className="star" title="priority">★</span>}
        {s.spawn && (
          <span className="fd-panechip" title={`board-owned pane · ${s.spawn.status || 'live'}`}>
            ⌗ {s.spawn.tmux_window}
          </span>
        )}
        {s.spawn?.skip_permissions && (
          <span className="fd-unsupchip" title="spawned with permissions bypassed — it will never ask before acting">
            unsupervised
          </span>
        )}
        {stalled && (
          <span className="fd-stalledchip" title={stalledTip}>never registered</span>
        )}
        {s.stale && (
          <span className="fd-stalechip" title="no events for a while; may be stuck">stale</span>
        )}
        <span className="fd-spacer" />
        {mailCount > 0 && (
          <span className="mailbadge" title={mailHint || `${mailCount} queued`}>
            ✉ {mailCount}{mailStuck ? ' ⧗' : ''}
          </span>
        )}
        <span className={`fd-mbadge ${fam}`} title={s.model || ''}>
          {compact ? modelShort(s.model) : prettyModel(s.model)}
        </span>
      </span>
      {!compact && (s.branch || wt) && (
        <span className="branch">
          <span className="b">⎇ {s.branch || '—'}</span>
          {wt ? `  ·  wt:${wt}` : ''}
        </span>
      )}
      <span style={{ display: 'flex', gap: 8, alignItems: 'baseline' }}>
        <span className="note" style={{ flex: 1 }}>{s.note || s.task || '—'}</span>
        {compact && <span className="age">{human(now - (s.lastSeen || s.startedAt || now))}</span>}
      </span>
      {inConflict && conflictPeers.length > 0 && (
        <span className="contested">▲ contested with {conflictPeers.join(', ')}</span>
      )}
      {!compact && (
        <span className="row-files">
          {shown.map((f, i) => (
            <span key={i} className={`fd-filechip${hot.has(f) ? ' hot' : ''}`}>{f}</span>
          ))}
          {files.length > shown.length && <span className="fd-filechip">+{files.length - shown.length}</span>}
          <span className="fd-spacer" />
          <Sparkline data={s.sparkline} />
          <span className="age">{human(now - (s.lastSeen || s.startedAt || now))}</span>
        </span>
      )}
    </button>
  );
}
