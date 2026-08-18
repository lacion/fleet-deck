import { useEffect, useRef, useState } from 'react';
import {
  human,
  hhmmss,
  basename,
  prettyModel,
  modelFamily,
  safeUrl,
  spawnKillable,
  spawnRemoteAvailable,
  colPulse,
  worktreeLabel,
  adoptableNow,
  adoptArmable,
  adoptArmed,
  relToRoot,
  TURN_BOUNDARY_HINT,
  copyText,
} from '../util.ts';
import { sendMail, reasonOf } from '../api.ts';
import { useModal } from '../useModal.ts';
import type { SessionEntry, SessionSpawn, TickerEntry } from '../../../contracts/index.ts';

// The drawer reads a session with a per-tab TIMELINE spliced in (App injects
// the ticker lines; the snapshot row itself carries none) and reads files /
// events defensively, so a local structural shape relaxes those while the real
// snapshot row flows in structurally.
type DrawerSession = Omit<SessionEntry, 'files' | 'events'> & {
  files?: string[] | null;
  events?: number | null;
  timeline?: TickerEntry[] | null;
};

// OwnedPane renders only inside {s.spawn && …} (spawn present) but still reads
// spawn.remote defensively, so remote is relaxed to an optional key while the
// rest of the spawn shape stays intact.
type OwnedSpawn = Omit<SessionSpawn, 'remote'> & {
  remote?: { enabled: boolean; url: string | null };
};
type OwnedPaneSession = Omit<DrawerSession, 'spawn'> & { spawn: OwnedSpawn };

// The shared useSpawnActions result the revive / enable-remote callbacks report.
interface SpawnActionResult {
  ok: boolean;
  reason?: string;
  url?: string | null;
}

interface RevState {
  done: boolean;
  err: string | null | undefined;
  ok: string | null;
}

interface RcState {
  done: boolean;
  err: string | null | undefined;
  ok: string | null;
  url: string | null | undefined;
}

interface NudgeState {
  ok?: string;
  err?: string;
}

interface ThreadMsg {
  text: string;
  at: number;
}

interface SendThreadResult {
  ok: boolean;
  reason?: string;
}

interface OwnedPaneProps {
  s: OwnedPaneSession;
  onOpenTerm: (() => void) | undefined;
  onKill: (() => void) | undefined;
  onRevive: (s: OwnedPaneSession, cb: (r: SpawnActionResult) => void) => void;
  onEnableRemote: (s: OwnedPaneSession, cb: (r: SpawnActionResult) => void) => void;
  reviving: boolean;
  enablingRemote: boolean;
}

interface DrawerProps {
  s: DrawerSession;
  now: number;
  conflictFiles: string[];
  mailCount: number;
  priority: boolean;
  onTogglePriority: () => void;
  onClose: () => void;
  onCompose: () => void;
  onOpenTerm: (() => void) | undefined;
  onKill: () => void;
  thread: ThreadMsg[];
  onSendThread: (text: string) => Promise<SendThreadResult | null>;
  onRevive: (s: OwnedPaneSession, cb: (r: SpawnActionResult) => void) => void;
  onEnableRemote: (s: OwnedPaneSession, cb: (r: SpawnActionResult) => void) => void;
  reviving: boolean;
  enablingRemote: boolean;
  onArmMove?: (s: DrawerSession) => void;
  onDisarm?: (s: DrawerSession) => void;
  adopting: boolean;
  onRename?: (s: DrawerSession) => void;
  onBrowseFiles?: (sess: DrawerSession, relPath?: string | null) => void;
}

// Owned-pane block (v1.2): attach hint + kill.
// v1.4 adds the live-terminal door (onOpenTerm — App passes it only while
// the pane is viewable).
// v1.6 adds remote control: an enable button on RUNNING sessions (POST
// /api/spawn/<id>/rc) and the harvested claude.ai link once it exists.
// v1.8: kill no longer lives here alone (the card carries it too) and no
// longer arms in place — both doors open App's KillConfirm dialog, which owns
// the POST and reports the outcome on the shared header strip.
//
// M-F2 — revive + enable-remote no longer POST from here directly: onRevive /
// onEnableRemote come from App's shared useSpawnActions hook (the SAME one the
// card chips use), so the card and this pane can't each fire a POST. The busy
// state (reviving / enablingRemote) is shared too; only the inline result text
// is local to this surface.
function OwnedPane({
  s,
  onOpenTerm,
  onKill,
  onRevive,
  onEnableRemote,
  reviving,
  enablingRemote,
}: OwnedPaneProps) {
  // v1.5 revive — offline + spawn.revivable only; one click, no confirm
  // (worst case is a fresh QUEUED card, not data loss). Local = the result line.
  const [rev, setRev] = useState<RevState>({ done: false, err: null, ok: null });
  // v1.6 remote control — the daemon types /rc into the pane and harvests the
  // claude.ai link, so the round-trip runs ~3-6 s; a 409 (mid-turn / pane not
  // live) surfaces honestly.
  const [rc, setRc] = useState<RcState>({ done: false, err: null, ok: null, url: null });
  const [diagCopied, setDiagCopied] = useState(false);
  const diagTimer = useRef<number | null>(null);
  // Its OWN latch, deliberately not shared with the stall diag above: both blocks
  // can be on one card (a clone that failed, a pane that then never registered),
  // and a shared latch would flash "✓ copied" on the block you did not copy.
  const [failCopied, setFailCopied] = useState(false);
  const failTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      clearTimeout(diagTimer.current ?? undefined);
      clearTimeout(failTimer.current ?? undefined);
    },
    [],
  );
  const alive = s.col !== 'offline';
  const revivable = !alive && s.spawn.revivable;
  // snapshot truth first; the fresh POST response fills the gap until the
  // next snapshot lands (WS pushes on every mutation, so it's brief)
  const remoteOn = !!s.spawn.remote?.enabled || rc.done;
  const remoteUrl = safeUrl((s.spawn.remote?.url ?? '') || (rc.url ?? '')); // M-S1 — https/claude.ai only
  const win = s.spawn.tmux_window ?? '';
  // the window is named fd<port>-<callsign> — recover the daemon port for
  // the attach hint from it (falls back to how this board was reached)
  const m = /^fd(\d+)-/.exec(win);
  const tmuxSession = `fleetdeck-${m ? (m[1] ?? '') : location.port || '4711'}`;

  const doRevive = () => {
    setRev({ done: false, err: null, ok: null });
    onRevive(s, (r) => {
      if (r.ok) setRev({ done: true, err: null, ok: 'reviving — card moves to QUEUED' });
      else setRev({ done: false, err: r.reason, ok: null });
    });
  };

  const doRemote = () => {
    setRc({ done: false, err: null, ok: null, url: null });
    onEnableRemote(s, (r) => {
      if (r.ok) {
        setRc({
          done: true,
          err: null,
          ok: safeUrl(r.url)
            ? 'remote control on'
            : 'remote control on — harvesting the claude.ai link',
          url: r.url,
        });
      } else {
        setRc({ done: false, err: r.reason, ok: null, url: null });
      }
    });
  };

  return (
    <div className="fd-sect">
      <div className="sl" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        OWNED PANE · {(s.spawn.status || 'live').toUpperCase()}
        {s.spawn.skip_permissions && (
          <span
            className="fd-unsupchip"
            title="spawned with permissions bypassed — it will never ask before acting"
          >
            unsupervised
          </span>
        )}
        {remoteOn && (
          <span
            className="fd-remotechip"
            title="remote control on — this session can be driven from claude.ai"
          >
            📱 remote
          </span>
        )}
      </div>
      <div className="fd-attach">
        $ tmux attach -t {tmuxSession}
        {'\n'}
        {'  '}window {win}
      </div>
      {s.spawn.stalled && (
        <div className="fd-stalldiag">
          <div className="hd">
            <span>CAPTURED WHEN REGISTRATION STALLED</span>
            {s.spawn.stall_detail && (
              <button
                type="button"
                className="fd-ghostbtn"
                onClick={() =>
                  void (async () => {
                    const ok = await copyText(s.spawn.stall_detail);
                    setDiagCopied(ok);
                    clearTimeout(diagTimer.current ?? undefined);
                    diagTimer.current = setTimeout(() => {
                      setDiagCopied(false);
                    }, 1800);
                  })()
                }
              >
                {diagCopied ? '✓ copied' : 'copy'}
              </button>
            )}
          </div>
          {s.spawn.stall_detail ? (
            <pre>{s.spawn.stall_detail}</pre>
          ) : (
            <div className="none">
              No pane excerpt was captured. Open the live terminal to inspect the stalled process.
            </div>
          )}
        </div>
      )}
      {s.spawn.fail_detail && (
        // The card carries a ▸ disclosure of this same text; here it gets room to
        // breathe AND a copy button, because the thing you need out of a failed
        // clone is usually a URL to open or a public key to paste into a settings
        // page — retyping either off a screenshot is how the hours get spent.
        // No `.none` fallback, unlike the stall diag: the daemon writes this field
        // only when it captured something, so the block simply does not render.
        <div className="fd-faildiag">
          <div className="hd">
            <span>GIT OUTPUT WHEN THE SPAWN FAILED</span>
            {/* Every `remote:` line in here was written by whoever runs the far
                end of the clone (or, on an http:// remote, by anyone in the
                middle) — git only relays it. It is bounded, control-stripped and
                rendered as a text node, so it cannot script the board, but it CAN
                print a plausible-looking instruction, and the button beside it
                trains you to copy a URL or a key straight out of it. Say whose
                words they are; do not present them as fleetdeck's advice. */}
            <span className="src">relayed from the remote server</span>
            <button
              type="button"
              className="fd-ghostbtn"
              onClick={() =>
                void (async () => {
                  const ok = await copyText(s.spawn.fail_detail);
                  setFailCopied(ok);
                  clearTimeout(failTimer.current ?? undefined);
                  failTimer.current = setTimeout(() => {
                    setFailCopied(false);
                  }, 1800);
                })()
              }
            >
              {failCopied ? '✓ copied' : 'copy'}
            </button>
          </div>
          <pre>{s.spawn.fail_detail}</pre>
        </div>
      )}
      {/* v1.6 — the claude.ai door (or its pending placeholder) */}
      {remoteOn &&
        (remoteUrl ? (
          <div className="fd-remoterow">
            <a href={remoteUrl} target="_blank" rel="noopener noreferrer" title={remoteUrl}>
              📱 open on claude.ai ↗
            </a>
          </div>
        ) : (
          <div className="fd-remoterow pending">
            📱 remote on — claude.ai link not captured yet; if it never lands here, find it in the
            live terminal (▣)
          </div>
        ))}
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
        {spawnRemoteAvailable(s) && !rc.done && (
          // offered only when the daemon would say yes (live pane, turn
          // boundary) — a mid-click race still 409s honestly below
          <button
            type="button"
            className="fd-ghostbtn"
            disabled={enablingRemote}
            title="put this agent on remote control — drive it from claude.ai on web or phone (types /rc into the pane; takes a few seconds)"
            onClick={doRemote}
          >
            📱 {enablingRemote ? 'Enabling remote… (~5 s)' : 'Enable remote'}
          </button>
        )}
        {revivable && !rev.done && (
          <button
            type="button"
            className="fd-ghostbtn"
            disabled={reviving}
            title="worktree + transcript survived — resume this agent (card moves to QUEUED)"
            onClick={doRevive}
          >
            ⟲ {reviving ? 'Reviving…' : 'Revive'}
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
  s,
  now,
  conflictFiles,
  mailCount,
  priority,
  onTogglePriority,
  onClose,
  onCompose,
  onOpenTerm,
  onKill,
  thread,
  onSendThread,
  onRevive,
  onEnableRemote,
  reviving,
  enablingRemote,
  onArmMove,
  onDisarm,
  adopting,
  onRename,
  onBrowseFiles,
}: DrawerProps) {
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const [sendErr, setSendErr] = useState<string | null>(null); // M-F5 — a swallowed send now shows
  const [nudge, setNudge] = useState<NudgeState | null>(null); // {ok} | {err}
  const [nudging, setNudging] = useState(false);
  // State disables the controls after React commits. The refs close the smaller
  // same-render window where two clicks/Enter presses could otherwise issue two
  // messages before that commit lands.
  const sendingRef = useRef(false);
  const nudgingRef = useRef(false);
  const ref = useRef<HTMLDivElement | null>(null);
  useModal(ref); // M-A2 — focus in, Tab trapped, focus restored on close
  const fam = modelFamily(s.model);
  const pulseClass = colPulse(s.col);
  const wt = worktreeLabel(s);
  // v2.2 — keep the absolute ledger path alongside the display basename: a chip
  // whose file resolves inside the browse root opens the viewer AT that file;
  // one that doesn't (a /tmp scratch path) opens at the root instead — the
  // title says which of the two a click will do.
  const files = (s.files ?? []).map((abs) => ({
    abs,
    name: basename(abs),
    rel: relToRoot(s, abs),
  }));
  const hot = new Set(conflictFiles.map(basename));
  const canBrowse = !!onBrowseFiles && !!((s.worktree ?? '') || (s.cwd ?? ''));
  const offline = s.col === 'offline';
  // v2.0 Move-to-tmux — mirror the card chip here (the drawerbtns row is shown
  // for every session, so it drops in cleanly). Same shared owner as the card,
  // so a click here and a click on the chip can't each fire a POST (M-F2).
  const canMove = !!onArmMove && (adoptableNow(s) || adoptArmable(s));
  const isArmed = !!onDisarm && adoptArmed(s);

  // M-F5 — await the result: clear the draft ONLY on success, surface a failure
  // rather than dropping it (the old code cleared before the POST and swallowed
  // 4xx/network errors entirely).
  const send = async () => {
    const text = draft.trim();
    if (!text || sendingRef.current) return;
    sendingRef.current = true;
    setSending(true);
    setSendErr(null);
    try {
      const res = await onSendThread(text);
      if (res?.ok) setDraft('');
      else setSendErr((res?.reason ?? '') || 'send failed');
    } catch {
      setSendErr('daemon unreachable');
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  };

  // M-F5 — Nudge was fire-and-forget (unhandled rejection, no feedback). Same
  // result handling as the thread send now: awaited, with an ok/err line.
  const doNudge = async () => {
    if (nudgingRef.current) return;
    nudgingRef.current = true;
    setNudging(true);
    setNudge(null);
    try {
      const res = await sendMail(s.session_id, 'nudge: status check-in please');
      if (res.ok && res.json?.ok !== false) setNudge({ ok: 'nudge sent' });
      else setNudge({ err: reasonOf(res, `nudge failed (${res.status})`) });
    } catch {
      setNudge({ err: 'daemon unreachable' });
    } finally {
      nudgingRef.current = false;
      setNudging(false);
    }
  };

  const busy = sending || nudging;
  const requestClose = () => {
    // Keep an unsent draft/result surface mounted until its bounded request
    // resolves. Closing mid-flight used to lose the only copy of a failed send.
    if (!sendingRef.current && !nudgingRef.current) onClose();
  };

  return (
    <>
      <div className="fd-scrim" onClick={requestClose} />
      <div
        className="fd-drawer"
        role="dialog"
        aria-modal="true"
        aria-label={`Session ${s.callsign}`}
        aria-busy={busy}
        ref={ref}
        onKeyDown={(e) => {
          if ((sendingRef.current || nudgingRef.current) && e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
          }
        }}
      >
        <div className="fd-drawerhead">
          <span className={`fd-pulse ${pulseClass}`} style={{ width: 9, height: 9 }} />
          <span className="callsign">{s.callsign || s.session_id}</span>
          {/* v2.1 — rename, right where the name is: the ✎ sits ON the callsign it
              renames rather than down among the action buttons, which are all
              things done TO the session. Same door as the card chip (App's
              RenameDialog owns the POST); same gate — any live card, spawn or not. */}
          {onRename && !offline && (
            <button
              type="button"
              className="fd-namebtn"
              aria-label={`Rename ${s.callsign || s.session_id}`}
              title="rename this session — the animal stays, the rest is yours"
              onClick={() => {
                onRename(s);
              }}
            >
              ✎
            </button>
          )}
          <span className={`fd-mbadge ${fam}`}>{prettyModel(s.model)}</span>
          <span className="col">{(s.col || '').toUpperCase()}</span>
          <span className="fd-spacer" />
          <button
            type="button"
            className="fd-x"
            aria-label="Close"
            disabled={busy}
            onClick={requestClose}
          >
            ✕
          </button>
        </div>
        <div className="fd-drawerbody">
          <div className="fd-sect">
            <div className="sl">TASK</div>
            <div className="task">{(s.task ?? '') || (s.note ?? '') || '—'}</div>
          </div>
          <div className="fd-meta">
            <div>
              repo{' '}
              <span title={s.cwd ?? ''}>
                {(s.repo_name ?? '') || basename(s.repo_id ?? '') || '—'}
                {wt ? ` (wt: ${wt})` : ''}
              </span>
            </div>
            <div>
              branch <span>⎇ {(s.branch ?? '') || '—'}</span>
            </div>
            <div>
              started <span>{s.startedAt ? human(now - s.startedAt) + ' ago' : '—'}</span>
            </div>
            <div>
              events{' '}
              <span>
                {s.events ?? 0}
                {s.lastTool ? ` · last ${s.lastTool}` : ''}
              </span>
            </div>
            <div>
              seen <span>{s.lastSeen ? human(now - s.lastSeen) + ' ago' : '—'}</span>
            </div>
            <div>
              mail <span>{mailCount} queued</span>
            </div>
          </div>
          <div className="fd-drawerbtns">
            <button type="button" className="fd-primary" disabled={busy} onClick={onCompose}>
              Message
            </button>
            <button
              type="button"
              className="fd-ghostbtn"
              disabled={nudging}
              onClick={() => void doNudge()}
            >
              {nudging ? 'Nudging…' : 'Nudge'}
            </button>
            <button
              type="button"
              className="fd-ghostbtn"
              style={priority ? { color: 'var(--act)' } : undefined}
              aria-pressed={priority}
              onClick={onTogglePriority}
            >
              {priority ? '★ Priority' : '☆ Priority'}
            </button>
            {/* v2.0 — move-to-tmux, mirroring the card chip. Armed → disarm on
                click; otherwise → opens App's ArmMoveConfirm dialog over the drawer. */}
            {isArmed ? (
              <button
                type="button"
                className="fd-ghostbtn"
                style={{ color: 'var(--m-comet)' }}
                disabled={adopting}
                title="a move is armed — exit this session in your terminal and fleetdeck resumes it in a board-owned pane; click to cancel the move"
                onClick={() => {
                  onDisarm(s);
                }}
              >
                ⧗ {adopting ? 'Canceling…' : 'Armed — exit CLI to move'}
              </button>
            ) : canMove ? (
              <button
                type="button"
                className="fd-ghostbtn"
                disabled={adopting}
                title="move this session into a board-owned tmux pane so you can drive it from the board"
                onClick={() => {
                  onArmMove(s);
                }}
              >
                ⇥ {adopting ? 'Moving…' : 'Move to tmux'}
              </button>
            ) : null}
            {nudge?.ok && (
              <span className="fd-killok" role="status">
                ✓ {nudge.ok}
              </span>
            )}
            {nudge?.err && (
              <span className="fd-killwarn" role="alert">
                ✗ {nudge.err}
              </span>
            )}
          </div>
          {s.spawn && (
            <OwnedPane
              s={s as OwnedPaneSession}
              onOpenTerm={onOpenTerm}
              onKill={onKill}
              onRevive={onRevive}
              onEnableRemote={onEnableRemote}
              reviving={reviving}
              enablingRemote={enablingRemote}
            />
          )}
          <div className="fd-sect">
            <div className="sl" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              FILES
              {canBrowse && (
                <button
                  type="button"
                  className="fd-fsopenbtn"
                  title="browse this session's working tree — read-only, with search"
                  onClick={() => {
                    onBrowseFiles(s);
                  }}
                >
                  ⌸ browse
                </button>
              )}
            </div>
            <div className="fd-filewrap">
              {files.length === 0 && <span className="fd-filechip">none yet</span>}
              {files.map((f, i) =>
                canBrowse ? (
                  <button
                    key={i}
                    type="button"
                    className={`fd-filechip clickable${hot.has(f.name) ? ' hot' : ''}`}
                    title={
                      f.rel != null
                        ? `open ${f.abs} in the viewer`
                        : `${f.abs} is outside this session's working tree — opens the viewer at the root`
                    }
                    onClick={() => {
                      onBrowseFiles(s, f.rel);
                    }}
                  >
                    {f.name}
                    {hot.has(f.name) ? '  ▲ contested' : ''}
                  </button>
                ) : (
                  <span key={i} className={`fd-filechip${hot.has(f.name) ? ' hot' : ''}`}>
                    {f.name}
                    {hot.has(f.name) ? '  ▲ contested' : ''}
                  </span>
                ),
              )}
            </div>
          </div>
          <div className="fd-sect">
            <div className="sl">TIMELINE</div>
            <div className="fd-timeline">
              {(s.timeline ?? []).length === 0 && (
                <div className="ev">
                  <span className="m">nothing in the recent ticker for this session</span>
                </div>
              )}
              {(s.timeline ?? []).map((ev, i) => (
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
                <div className="none">{`No messages yet. Anything you send lands at the agent’s ${TURN_BOUNDARY_HINT}.`}</div>
              )}
              {thread.map((m, i) => (
                <div className="fd-msg" key={i}>
                  <div className="bubble">{m.text}</div>
                  <div className="state">
                    {offline
                      ? `⧗ sent ${hhmmss(m.at)} — delivers on resume`
                      : `⧗ sent ${hhmmss(m.at)} — delivers at ${TURN_BOUNDARY_HINT}`}
                  </div>
                </div>
              ))}
              <div className="fd-freerow">
                <input
                  className="fd-input"
                  style={{ fontFamily: 'var(--font-ui)', fontSize: '12.5px' }}
                  placeholder={`Message ${s.callsign || 'session'}…`}
                  aria-label={`Message ${s.callsign || 'session'}`}
                  value={draft}
                  onChange={(e: React.ChangeEvent<HTMLInputElement>) => {
                    setDraft(e.target.value);
                    if (sendErr) setSendErr(null);
                  }}
                  onKeyDown={(e: React.KeyboardEvent) => {
                    if (e.key === 'Enter') void send();
                  }}
                />
                <button
                  type="button"
                  className="fd-send"
                  disabled={!draft.trim() || sending}
                  onClick={() => void send()}
                >
                  {sending ? 'Sending…' : 'Send'}
                </button>
              </div>
              {sendErr && (
                <div className="status hazard" role="alert">
                  ✗ {sendErr} — your message is still here, try again
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
