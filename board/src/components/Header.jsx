import React from 'react';
import { hhmmss } from '../util.js';
import { getQuestion } from '../qbus.js';

const WS_LABEL = { live: 'LIVE', reconnecting: 'RECONNECTING', offline: 'OFFLINE' };

// The board header: wordmark, WS status pill, the NEEDS YOU jump-chip, the fleet
// line + clock, and the action buttons (Compose · Terminals · Spawn · Clear ·
// Worktrees · Share · density · theme). Pure presentation over App's state —
// every button is a callback prop, so the header holds no business logic beyond
// the scroll-to-inbox handle it reads from qbus.
export default function Header({
  status, stale, pendingQs, liveN, conflictCount, now,
  onCompose,
  termableSessions, watchable, termBtnRef, onOpenGrid,
  spawnAvailable, spawnActive, onSpawn,
  hasOffline, clearing, onClear,
  wtSupported, wtCount, wtHazard, onOpenWorktrees,
  lanEnabled, onShare,
  compact, onToggleCompact,
  theme, onToggleTheme,
}) {
  return (
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
          onClick={() => { getQuestion((pendingQs[0] || {}).id)?.scrollIntoView?.(); }}
        >
          NEEDS YOU · {pendingQs.length}
        </button>
      )}
      <div className="fd-spacer" />
      <div className="fd-fleetline">
        {liveN} session{liveN === 1 ? '' : 's'} · {conflictCount} conflict{conflictCount === 1 ? '' : 's'}
      </div>
      <div className="fd-clock">{hhmmss(now)}</div>
      <button type="button" className="fd-hbtn" onClick={onCompose}>
        ✉ Compose <span className="fd-kbd">c</span>
      </button>
      {/* v1.9 the wall of screens. Ticked agents if you ticked any, otherwise
          every live pane — so "just show me everything" is one click. */}
      {termableSessions.length > 0 && (
        <button
          type="button"
          ref={termBtnRef}
          className="fd-hbtn"
          title={watchable.length
            ? `Watch the ${watchable.length} selected agent${watchable.length === 1 ? '' : 's'}`
            : `Watch all ${termableSessions.length} live agent${termableSessions.length === 1 ? '' : 's'}`}
          onClick={() => onOpenGrid(watchable)}
        >
          ▦ Terminals
          <span className="fd-spawncount">{watchable.length || termableSessions.length}</span>
        </button>
      )}
      {spawnAvailable && (
        <button type="button" className="fd-hbtn" onClick={onSpawn}>
          + Spawn
          {spawnActive > 0 && (
            // A count, not a budget — there is no cap on the fleet size.
            <span className="fd-spawncount" title={`${spawnActive} board-spawned agents live`}>{spawnActive} live</span>
          )}
        </button>
      )}
      {hasOffline && (
        <button type="button" className="fd-hbtn" disabled={clearing} onClick={onClear}>
          ⌫ {clearing ? 'Clearing…' : 'Clear'}
        </button>
      )}
      {/* v1.9 — worktrees left behind by spawns. Always offered (an empty modal
          explains where they come from); hidden only on a daemon whose
          /api/worktrees 404s. The badge turns hazard when any row holds work
          nobody has pushed: that is the fact you want to see from the header. */}
      {wtSupported && (
        <button
          type="button"
          className="fd-hbtn"
          title="Git worktrees left behind by spawns"
          onClick={onOpenWorktrees}
        >
          ⑂ Worktrees
          {wtCount > 0 && (
            <span className={`fd-wtbadge${wtHazard ? ' haz' : ''}`}>{wtCount}</span>
          )}
        </button>
      )}
      {/* v1.7 — always offered: when LAN is off the panel is where you learn how
          to turn it on, so it must not hide precisely when it's needed */}
      <button
        type="button"
        className="fd-hbtn"
        title="Open this board on another device"
        onClick={onShare}
      >
        ⇄ Share
        {lanEnabled && <span className="fd-landot" aria-label="LAN mode on" />}
      </button>
      <button type="button" className="fd-hbtn dim" aria-label="Toggle density" onClick={onToggleCompact}>
        {compact ? '▤ Cozy' : '▦ Compact'}
      </button>
      <button type="button" className="fd-hbtn dim" aria-label="Toggle theme" onClick={onToggleTheme}>
        {theme === 'dark' ? '☀ Light' : '● Dark'}
      </button>
    </div>
  );
}
