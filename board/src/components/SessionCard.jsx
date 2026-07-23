import React from 'react';
import Sparkline from './Sparkline.jsx';
import { Age } from '../clock.jsx';
import { basename, prettyModel, modelShort, modelFamily, safeUrl, spawnTermable, spawnKillable, spawnRemoteAvailable, colPulse, worktreeLabel, adoptableNow, adoptArmable, adoptArmed } from '../util.js';

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
//
// M-P4 — React.memo'd (see export below) with a stable prop set: no `now`
// (the age is an <Age> leaf that reads the 1 s clock from context), and every
// action prop takes the session `s` so BoardLanes can pass ONE stable function
// per action instead of minting a fresh closure per card per render. A 1 s tick
// therefore re-renders only the <Age> spans, not the whole wall of cards.
function SessionCard({
  s, compact, mailCount, mailMeta, conflictFiles, conflictPeers, ripple, priority, onOpen, onOpenTerm,
  onRevive, reviving, onEnableRemote, enablingRemote, onKill, onToggleWatch, watched,
  onArmMove, onDisarm, adopting, onRename, onDismiss, dismissing, legacy,
  onSpawnShell, spawningShell,
}) {
  const shell = s.spawn?.kind === 'shell' || s.source === 'shell';
  const offline = s.col === 'offline';
  const needsyou = !shell && s.col === 'needsyou';
  const inConflict = conflictFiles.length > 0;
  const fam = modelFamily(s.model);
  const pulseClass = colPulse(s.col);
  const files = (s.files || []).map(basename);
  const hot = new Set(conflictFiles.map(basename));
  const shown = files.slice(0, 4);
  // Spawn watchdog: pane came up but the session never phoned home. The daemon
  // captures the pane tail when it declares the stall; put the excerpt in the
  // chip tooltip (and the full readable block in Drawer) so "never registered"
  // says WHAT was on screen — trust dialog, crash, auth error — not just where.
  const stalled = !!s.spawn?.stalled;
  const stalledTip = [
    'pane is up but never reached this daemon',
    s.spawn?.tmux_window ? `window ${s.spawn.tmux_window}` : null,
    s.spawn?.stall_detail ? `captured screen:\n${s.spawn.stall_detail}` : 'no pane excerpt was captured — open the live terminal',
  ].filter(Boolean).join('\n\n');
  // mail route truth (fix C): snapshot mail_meta[sid] = {queued, oldest_at,
  // route}. Older daemons omit it — the badge falls back to a bare count.
  const mailHint = MAIL_HINT[mailMeta?.route];
  const mailStuck = mailMeta?.route === 'turn-boundary' || mailMeta?.route === 'offline-queued';
  // only badge REAL secondary worktrees (see worktreeLabel — the daemon records
  // worktree = toplevel of cwd even for the main tree)
  const wt = worktreeLabel(s);
  // v1.8 — the card's own action row. The terminal was reachable before, but
  // only by clicking what looked like a metadata chip; killing was reachable
  // only from inside the drawer. Both are ACTIONS, so both now read as buttons,
  // side by side, on the card itself. (R3-3: they are real <button>s — the card
  // is no longer an interactive element, so nesting is no longer a concern.)
  const canTerm = !!onOpenTerm && spawnTermable(s);
  const canKill = !!onKill && spawnKillable(s);
  // M-S1 — the harvested remote URL only becomes a live link if it is https on
  // claude.ai; anything else (a `javascript:` a hostile agent printed into its
  // terminal) collapses to null and renders as the plain, unclickable chip.
  const remoteUrl = safeUrl(s.spawn?.remote?.url);
  // v1.9 — tick an agent into the wall of screens. Same eligibility as the
  // terminal: only a pane the daemon owns can be watched at all.
  const canWatch = !!onToggleWatch && spawnTermable(s);
  // v2.0 Move-to-tmux — the FIRST action a non-`s.spawn` card ever shows (a plain
  // `claude` in your own terminal had zero card actions before this). `canMove`
  // opens the confirm dialog (offline 'now' → immediate copy, live 'arm' →
  // deferred copy); `isArmed` means a deferred move is waiting and the chip
  // becomes a one-click disarm (no dialog — nothing hazardous is undone).
  const canMove = !shell && !!onArmMove && (adoptableNow(s) || adoptArmable(s));
  const isArmed = !!onDisarm && adoptArmed(s);
  const canAdopt = canMove || isArmed;
  // v2.1 Rename — the ONE action with no spawn gate at all. Renaming touches the
  // session's name, never its pane, so it is offered on EVERY live card:
  // board-owned or not, revivable or not. Offline cards are excluded on purpose —
  // they are on their way to being archived (and their callsign released), so
  // naming one buys nothing and would only invite a refusal.
  const canRename = !!onRename && !offline;
  // Item 3 — ✕ dismiss: retire ONE offline card now (the daemon archives it),
  // rather than waiting for 24h retention or the bulk Clear. Only offline cards
  // carry it. A REVIVABLE card (same gate the ⟲ revive button uses above) would
  // lose its resurrection door, so it takes an inline two-step: the first click
  // arms a hazard-styled "sure?" that reverts after ~4s, the second dismisses.
  // A non-revivable offline card dismisses on a single click.
  const canDismiss = !!onDismiss && offline;
  const revivable = !!s.spawn?.revivable;
  const [dismissArmed, setDismissArmed] = React.useState(false);
  const dismissTimer = React.useRef(null);
  React.useEffect(() => () => { if (dismissTimer.current) clearTimeout(dismissTimer.current); }, []);
  const handleDismiss = () => {
    if (dismissing) return; // in-flight: the POST guard also holds, this just avoids re-arming
    if (revivable && !dismissArmed) {
      setDismissArmed(true);
      dismissTimer.current = setTimeout(() => { dismissTimer.current = null; setDismissArmed(false); }, 4000);
      return;
    }
    if (dismissTimer.current) { clearTimeout(dismissTimer.current); dismissTimer.current = null; }
    setDismissArmed(false);
    onDismiss(s);
  };
  const canSpawnShell = !!onSpawnShell && !!(s.worktree || s.cwd);
  const hasActs = canTerm || canKill || canWatch || canAdopt || canRename || canDismiss || canSpawnShell;

  const cls = [
    'fd-card',
    inConflict && 'conflict',
    needsyou && 'needsyou',
    priority && !inConflict && 'priority',
    offline && 'offline',
    ripple && 'ripple',
  ].filter(Boolean).join(' ');

  return (
    // M-A1 / R3-3 — the card is wall-to-wall action chips (terminal/kill/revive/
    // remote), so it can't itself be an interactive control: a role="button"
    // may not contain interactive descendants, and nesting real buttons inside
    // one is just as illegal. Instead the card is a NON-interactive container
    // (role="group"), the single "open the drawer" control is the full-bleed
    // <button className="fd-cardopen"> below — keyboard-focusable, covering the
    // whole card so a click anywhere still opens the drawer — and every action
    // chip is a real <button>/<a> that rides ABOVE that overlay (z-index), a
    // sibling control, never nested inside another interactive element.
    <div role="group" aria-label={s.callsign || s.session_id} className={cls}>
      <button
        type="button"
        className="fd-cardopen"
        aria-label={`Open ${s.callsign || s.session_id}`}
        title={s.task || s.note || s.callsign}
        onClick={() => onOpen(s)}
      />
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
        {legacy && (
          // 0.16.0: this session is running pre-upgrade hooks — its card will
          // not update (and looks deceptively idle) until the session is
          // restarted. Clears itself the moment its first authenticated hook
          // lands (i.e. after the restart).
          <span className="fd-legacytag" title="running pre-0.16.0 hooks — dark on this board until the session is restarted">
            ⬆ restart me
          </span>
        )}
        {s.spawn?.gateway && (
          // v0.15: this pane's API traffic goes to the configured gateway, not
          // to Anthropic. Which provider is serving a session is exactly the
          // kind of thing that should never be invisible on the board — the
          // whole feature exists so it is a decision, not an accident.
          <span className="fd-gwchip" title="routed through the configured LLM gateway, not Anthropic">
            🛰 gateway
          </span>
        )}
        {s.spawn?.setup_cmd && (
          <span className="fd-setupchip" title={s.spawn.setup_cmd}>⚙ setup</span>
        )}
        {!shell && s.spawn?.remote?.enabled && (
          // v1.6: remote control is on. With a harvested (and vouched-for) link
          // the chip is the door to claude.ai; without a safe one it just states
          // the fact and points at the terminal.
          remoteUrl ? (
            // real <a> (safeUrl vouched it https/claude.ai) — rides above the
            // card's open-drawer overlay, so it opens claude.ai, not the drawer.
            <a
              className="fd-remotechip link"
              href={remoteUrl}
              target="_blank"
              rel="noopener noreferrer"
              title={`remote control on — open ${remoteUrl}`}
            >
              📱 remote ↗
            </a>
          ) : (
            <span
              className="fd-remotechip"
              title="remote control on, but no claude.ai URL was captured — open the agent's terminal (▣) to find it"
            >
              📱 remote
            </span>
          )
        )}
        {!shell && spawnRemoteAvailable(s) && onEnableRemote && (
          // v1.6: the enable door — live pane, not yet on remote control,
          // session at a turn boundary. R3-3: a real <button> (raised above the
          // card overlay); `disabled` while in flight replaces the old aria/guard.
          // Results land on the header feedback strip via App.
          <button
            type="button"
            className={`fd-remoteofferchip${enablingRemote ? ' busy' : ''}`}
            disabled={enablingRemote}
            title="put this agent on remote control — drive it from claude.ai on web or phone"
            onClick={() => onEnableRemote(s)}
          >
            {enablingRemote ? 'enabling…' : '📱 enable remote'}
          </button>
        )}
        {!shell && offline && s.spawn?.revivable && onRevive && (
          // v1.5: dead agent, surviving worktree + transcript — the resurrection
          // door. R3-3: a real <button> (raised above the card overlay);
          // `disabled` while in flight replaces the old aria/guard.
          <button
            type="button"
            className={`fd-revivechip${reviving ? ' busy' : ''}`}
            disabled={reviving}
            title="worktree + transcript survived — revive this agent (card moves to QUEUED)"
            onClick={() => onRevive(s)}
          >
            {reviving ? 'reviving…' : '⟲ revive'}
          </button>
        )}
        {!shell && stalled && (
          <span className="fd-stalledchip" title={stalledTip}>never registered</span>
        )}
        {!shell && s.stale && (
          <span className="fd-stalechip" title="no events for a while; may be stuck">stale</span>
        )}
        <span className="fd-spacer" />
        {mailCount > 0 && (
          <span className="mailbadge" title={mailHint || `${mailCount} queued`}>
            ✉ {mailCount}{mailStuck ? ' ⧗' : ''}
          </span>
        )}
        {shell ? (
          <span className="fd-shellchip" title="shell-only terminal — no Claude conversation">&gt;_ shell</span>
        ) : (
          <span className={`fd-mbadge ${fam}`} title={s.model || ''}>
            {compact ? modelShort(s.model) : prettyModel(s.model)}
          </span>
        )}
      </span>
      {!compact && (s.branch || wt) && (
        <span className="branch">
          <span className="b">⎇ {s.branch || '—'}</span>
          {wt ? `  ·  wt:${wt}` : ''}
        </span>
      )}
      <span style={{ display: 'flex', gap: 8, alignItems: 'baseline' }}>
        <span className="note" style={{ flex: 1 }}>{s.note || s.task || '—'}</span>
        {compact && <Age className="age" from={s.lastSeen || s.startedAt} />}
      </span>
      {inConflict && conflictPeers.length > 0 && (
        <span className="contested">▲ contested with {conflictPeers.join(', ')}</span>
      )}
      {hasActs && (
        // v1.8 — card actions. Present in BOTH densities: they are the two
        // things you want at 2am without hunting through a drawer. R3-3: real
        // <button>s now (raised above the card's open-drawer overlay), so they
        // no longer need role/tabIndex/onKeyDown or a stopPropagation guard.
        <span className="fd-cardacts">
          {canAdopt && (
            // v2.0 — move-to-tmux, the first action on a non-board card. R3-3:
            // a real <button> raised above the card's open-drawer overlay (the
            // .fd-actbtn z-index rule already covers it). Armed → click disarms
            // immediately; otherwise → opens the confirm dialog.
            isArmed ? (
              <button
                type="button"
                className={`fd-actbtn move armed${adopting ? ' busy' : ''}`}
                disabled={adopting}
                title="a move is armed — exit this session in your terminal and fleetdeck resumes it in a board-owned pane; click to cancel the move"
                onClick={() => onDisarm(s)}
              >
                {adopting ? 'canceling…' : '⧗ armed — exit CLI to move'}
              </button>
            ) : (
              <button
                type="button"
                className={`fd-actbtn move${adopting ? ' busy' : ''}`}
                disabled={adopting}
                title="move this session into a board-owned tmux pane so you can drive it from the board"
                onClick={() => onArmMove(s)}
              >
                {adopting ? 'moving…' : '⇥ move to tmux'}
              </button>
            )
          )}
          {canTerm && (
            <button
              type="button"
              className="fd-actbtn term"
              title="open live terminal — your keystrokes go straight to the agent"
              onClick={() => onOpenTerm(s)}
            >
              ▣ terminal
            </button>
          )}
          {canWatch && (
            <button
              type="button"
              className={`fd-actbtn watch${watched ? ' on' : ''}`}
              aria-pressed={!!watched}
              title={watched ? 'remove from the terminal wall' : 'add to the terminal wall'}
              onClick={() => onToggleWatch(s)}
            >
              {watched ? '▦ watching' : '▦ watch'}
            </button>
          )}
          {canRename && (
            // v2.1 — ✎ rename. R3-3: a real <button>, a SIBLING of the full-bleed
            // .fd-cardopen overlay and never nested inside it, raised above it by
            // the existing `.fd-card .fd-cardacts .fd-actbtn { z-index: 2 }` rule
            // — so this click renames, and a click anywhere else on the card still
            // opens the drawer. It only OPENS the dialog; the POST is that
            // dialog's alone (so no busy state belongs on the chip).
            <button
              type="button"
              className="fd-actbtn rename"
              title="rename this session — the animal stays, the rest is yours"
              onClick={() => onRename(s)}
            >
              ✎ rename
            </button>
          )}
          {canSpawnShell && (
            <button
              type="button"
              className={`fd-actbtn shell${spawningShell ? ' busy' : ''}`}
              disabled={spawningShell}
              title={`open a shell-only terminal in ${s.worktree || s.cwd}`}
              onClick={() => onSpawnShell(s)}
            >
              {spawningShell ? 'opening…' : '⌨ shell'}
            </button>
          )}
          {canKill && (
            // opens the confirmation dialog — NEVER kills on this click
            <button
              type="button"
              className="fd-actbtn kill"
              title="kill this agent — asks first; the worktree and branch are left alone"
              onClick={() => onKill(s)}
            >
              ☠ kill
            </button>
          )}
          {canDismiss && (
            // Item 3 — ✕ dismiss an offline card. A revivable card arms a
            // two-step confirm first (it would lose ⟲ revive); everything else
            // dismisses on one click. The worktree on disk is left alone.
            <button
              type="button"
              className={`fd-actbtn dismiss${dismissArmed ? ' hazard' : ''}${dismissing ? ' busy' : ''}`}
              disabled={dismissing}
              title={revivable
                ? 'dismiss this dead card — it is still revivable, so click again to confirm (you lose ⟲ revive)'
                : 'dismiss this dead card — remove it from the board now (the worktree is left alone)'}
              onClick={handleDismiss}
            >
              {dismissing ? '✕ dismissing…' : dismissArmed ? '✕ sure? loses ⟲' : '✕ dismiss'}
            </button>
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
          {!shell && <Sparkline data={s.sparkline} />}
          <Age className="age" from={s.lastSeen || s.startedAt} />
        </span>
      )}
    </div>
  );
}

// M-P4 — memoized so a 1 s clock tick (or an unrelated App re-render) skips the
// whole card when its props are referentially equal; only the <Age> leaves,
// which read the clock from context, update each second.
export default React.memo(SessionCard);
