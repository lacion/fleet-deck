import React from 'react';
import Sparkline from './Sparkline.jsx';
import { human, basename, prettyModel, modelShort, modelFamily, spawnTermable, spawnKillable, spawnRemoteAvailable } from '../util.js';

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
  s, now, compact, mailCount, mailMeta, conflictFiles, conflictPeers, ripple, priority, onOpen, onOpenTerm,
  onRevive, reviving, onEnableRemote, enablingRemote, onKill, onToggleWatch, watched,
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
  // v1.8 — the card's own action row. The terminal was reachable before, but
  // only by clicking what looked like a metadata chip; killing was reachable
  // only from inside the drawer. Both are ACTIONS, so both now read as buttons,
  // side by side, on the card itself. (Spans with role=button: the card is
  // itself a <button> and buttons don't nest — same trick as the chips above.)
  const canTerm = !!onOpenTerm && spawnTermable(s);
  const canKill = !!onKill && spawnKillable(s);
  // v1.9 — tick an agent into the wall of screens. Same eligibility as the
  // terminal: only a pane the daemon owns can be watched at all.
  const canWatch = !!onToggleWatch && spawnTermable(s);
  const hasActs = canTerm || canKill || canWatch;

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
          // the window name is METADATA — it says which pane this card owns.
          // The door into that pane is the ▣ terminal button in the action row
          // below (v1.8: the chip used to be the door, and nobody found it).
          <span className="fd-panechip" title={`board-owned pane · ${s.spawn.status || 'live'}`}>
            ⌗ {s.spawn.tmux_window}
          </span>
        )}
        {s.spawn?.skip_permissions && (
          <span className="fd-unsupchip" title="spawned with permissions bypassed — it will never ask before acting">
            unsupervised
          </span>
        )}
        {s.spawn?.remote?.enabled && (
          // v1.6: remote control is on. With a harvested link the chip is the
          // door to claude.ai (span, not <a> — the card itself is a button);
          // without one it just states the fact and points at the terminal.
          s.spawn.remote.url ? (
            <span
              className="fd-remotechip link"
              role="link"
              tabIndex={0}
              title={`remote control on — open ${s.spawn.remote.url}`}
              onClick={(e) => { e.stopPropagation(); window.open(s.spawn.remote.url, '_blank', 'noopener'); }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); window.open(s.spawn.remote.url, '_blank', 'noopener'); }
              }}
            >
              📱 remote ↗
            </span>
          ) : (
            <span
              className="fd-remotechip"
              title="remote control on, but the claude.ai URL wasn't captured — open the agent's terminal (▣) to find it"
            >
              📱 remote
            </span>
          )
        )}
        {spawnRemoteAvailable(s) && onEnableRemote && (
          // v1.6: the enable door — live pane, not yet on remote control,
          // session at a turn boundary (span, not <button> — the card is a
          // button). Results land on the header feedback strip via App.
          <span
            className={`fd-remoteofferchip${enablingRemote ? ' busy' : ''}`}
            role="button"
            tabIndex={enablingRemote ? -1 : 0}
            aria-disabled={enablingRemote || undefined}
            title="put this agent on remote control — drive it from claude.ai on web or phone"
            onClick={(e) => { e.stopPropagation(); if (!enablingRemote) onEnableRemote(); }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); if (!enablingRemote) onEnableRemote(); }
            }}
          >
            {enablingRemote ? 'enabling…' : '📱 enable remote'}
          </span>
        )}
        {offline && s.spawn?.revivable && onRevive && (
          // v1.5: dead agent, surviving worktree + transcript — the chip is
          // the resurrection door (span, not <button> — the card is a button).
          <span
            className={`fd-revivechip${reviving ? ' busy' : ''}`}
            role="button"
            tabIndex={reviving ? -1 : 0}
            aria-disabled={reviving || undefined}
            title="worktree + transcript survived — revive this agent (card moves to QUEUED)"
            onClick={(e) => { e.stopPropagation(); if (!reviving) onRevive(); }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); if (!reviving) onRevive(); }
            }}
          >
            {reviving ? 'reviving…' : '⟲ revive'}
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
      {hasActs && (
        // v1.8 — card actions. Present in BOTH densities: they are the two
        // things you want at 2am without hunting through a drawer.
        <span className="fd-cardacts">
          {canTerm && (
            <span
              className="fd-actbtn term"
              role="button"
              tabIndex={0}
              title="open live terminal — your keystrokes go straight to the agent"
              onClick={(e) => { e.stopPropagation(); onOpenTerm(); }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); onOpenTerm(); }
              }}
            >
              ▣ terminal
            </span>
          )}
          {canWatch && (
            <span
              className={`fd-actbtn watch${watched ? ' on' : ''}`}
              role="button"
              aria-pressed={!!watched}
              tabIndex={0}
              title={watched ? 'remove from the terminal wall' : 'add to the terminal wall'}
              onClick={(e) => { e.stopPropagation(); onToggleWatch(); }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); onToggleWatch(); }
              }}
            >
              {watched ? '▦ watching' : '▦ watch'}
            </span>
          )}
          {canKill && (
            // opens the confirmation dialog — NEVER kills on this click
            <span
              className="fd-actbtn kill"
              role="button"
              tabIndex={0}
              title="kill this agent — asks first; the worktree and branch are left alone"
              onClick={(e) => { e.stopPropagation(); onKill(); }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); onKill(); }
              }}
            >
              ☠ kill
            </span>
          )}
        </span>
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
