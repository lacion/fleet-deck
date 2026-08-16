import { useEffect, useRef, useState } from 'react';
import { sendMail, sendCommand, reasonOf } from '../api.ts';
import { basename, TURN_BOUNDARY_HINT } from '../util.ts';
import { useModal } from '../useModal.ts';
import { ORCH_COMMANDS, GLOSSARY } from '../helpText.ts';
import type { SessionEntry, RepoEntry } from '../../../contracts/index.ts';

// The turn-boundary definition, single-sourced (helpText.js) — the footer's
// title= is a bonus affordance that must never drift from the ? overlay.
const TURN_BOUNDARY_DEF = GLOSSARY.find((g) => g.term === 'turn boundary')?.def ?? '';

// One recipient in POST /mail's `targets` receipt — how each session will get
// the mail. `callsign` may be absent; `route` is the daemon's own word (watcher
// / pane deliver now, everything else waits for the session).
interface DeliveryTarget {
  session_id: string;
  callsign: string | null;
  route: string;
}

// The union of daemon body fields send() reads across /mail and /command. The
// shared wire contract (contracts/wire.ts) does not yet model these richer
// fields, so this local structural shape keeps every field OPTIONAL and each
// defensive read honest until commands.mjs/http.mjs converts.
interface SendResp {
  ok?: boolean;
  reason?: string;
  assigned_to?: { callsign?: string | null };
  unrouted?: boolean;
  text?: string;
  renamed?: boolean;
  previous?: string;
  callsign?: string;
  ticket?: string | null;
  targets?: DeliveryTarget[];
}

interface ComposeProps {
  initialTarget: string;
  sessions: SessionEntry[];
  repos: RepoEntry[];
  onClose: () => void;
  onSent?: (target: string, text: string) => void;
  spawnAvailable: boolean;
  onSpawnFor?: (text: string) => void;
}

// Honest per-target feedback from POST /mail `targets` ({session_id, callsign,
// route}). watcher/pane routes deliver without the human doing anything;
// turn-boundary/offline-queued wait for the session itself.
function deliveryNote(targets: DeliveryTarget[]) {
  if (!targets.length) return '→ sent — no matching recipients';
  const names = (list: DeliveryTarget[]) =>
    list.map((t) => (t.callsign ?? '') || t.session_id).join(', ');
  const now = targets.filter((t) => t.route === 'watcher' || t.route === 'pane');
  const later = targets.filter((t) => t.route !== 'watcher' && t.route !== 'pane');
  if (!later.length) return `→ delivering now to ${names(now)}`;
  if (!now.length) return `→ queued for ${names(later)} — delivers at next turn boundary`;
  return `→ delivering now to ${names(now)} · queued: ${names(later)} (next turn boundary)`;
}

// Compose modal: mail to a session / all / repo:<name>, or free text to the
// orchestrator (POST /command — broadcast / assign / note).
export default function Compose({
  initialTarget,
  sessions,
  repos,
  onClose,
  onSent,
  spawnAvailable,
  onSpawnFor,
}: ComposeProps) {
  const [target, setTarget] = useState(initialTarget || 'all');
  const [text, setText] = useState('');
  const [err, setErr] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null); // brief confirmation after a daemon command
  const [unroutedText, setUnroutedText] = useState<string | null>(null); // v1.2: task text of an unrouted command → spawn CTA
  const [busy, setBusy] = useState(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  // M-A2 — trap Tab + restore focus on close; the textarea owns initial focus.
  useModal(dialogRef, { initialFocus: false });

  useEffect(() => {
    taRef.current?.focus();
  }, []);

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
      .filter((s) => s.col !== 'offline' && s.source !== 'shell')
      .map((s) => ({ id: s.session_id, label: s.callsign || s.session_id })),
  ];

  const send = async () => {
    if (!text.trim() || busy) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    const sent = text.trim();
    try {
      const res = target === 'daemon' ? await sendCommand(sent) : await sendMail(target, sent);
      // v1.1 auto-routing: /command may answer {ok:true, assigned_to:{callsign}}
      // (routed) or {ok:false, unrouted:true} (no candidate, task still logged)
      // — both count as "sent", neither is an error to show in red.
      const body: SendResp = res.json ?? {};
      const callsign = target === 'daemon' ? body.assigned_to?.callsign : null;
      const unrouted = target === 'daemon' && body.unrouted === true;
      if (target === 'daemon' && body.ok === false && !unrouted) {
        // /command always answers HTTP 200, so res.ok can't see a daemon-side
        // rejection — a {ok:false, reason} body (malformed/invalid/ambiguous
        // `ticket`, …) must surface in red, not close the dialog as a success.
        setErr((body.reason ?? '') || 'command failed');
      } else if (res.ok || unrouted) {
        onSent?.(target, sent);
        if (callsign) {
          setNote(`→ routed to ${callsign}`);
          setText('');
        } else if (unrouted) {
          setNote('no available session — task logged');
          setUnroutedText(body.text ?? sent);
          setText('');
        } else if (target === 'daemon' && typeof body.renamed === 'boolean') {
          // `ticket` outcomes: confirm the (possibly new) name inline.
          setNote(
            body.renamed
              ? `→ ${String(body.previous)} is now ${String(body.callsign)}`
              : body.ticket
                ? `→ ${String(body.callsign)} pinned to ${body.ticket}`
                : `→ ${String(body.callsign)} ticket cleared`,
          );
          setText('');
        } else if (target !== 'daemon' && Array.isArray(body.targets)) {
          // fix C: the daemon now says HOW each recipient gets the mail —
          // confirm inline (same affordance as daemon-command notes).
          setNote(deliveryNote(body.targets));
          setText('');
        } else {
          onClose();
        }
      } else {
        setErr(reasonOf(res, `send failed (${res.status})`));
      }
    } catch {
      setErr('daemon unreachable');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fd-composewrap" onClick={onClose}>
      <div
        className="fd-compose"
        role="dialog"
        aria-modal="true"
        aria-label="Compose"
        ref={dialogRef}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <span className="lbl">COMPOSE</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="fd-targets">
          {targets.map((t) => (
            <button
              key={t.id}
              type="button"
              className={`fd-target${target === t.id ? ' on' : ''}`}
              onClick={() => {
                setTarget(t.id);
                setNote(null);
                setErr(null);
                setUnroutedText(null);
              }}
            >
              {t.label}
            </button>
          ))}
        </div>
        <textarea
          ref={taRef}
          rows={4}
          // This placeholder is the ONLY place the orchestrator's grammar is
          // discoverable — every command the daemon parses has to appear here, or
          // nobody finds it. v2.1 adds `name` (custom callsign suffix; `clear`
          // reverts to the automatic name). Its response reuses the ticket
          // confirmation path below verbatim — {renamed, callsign, previous}.
          placeholder={
            target === 'daemon'
              ? 'Instruct the orchestrator…  (broadcast <text> · assign <callsign> <text> · assign auto <text> · assign auto:<repo> <text> · ticket <callsign> <KEY-1> · name <callsign> <suffix|clear> · note)'
              : 'Write to the fleet…'
          }
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            if (note) setNote(null);
            if (unroutedText) setUnroutedText(null);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void send();
          }}
        />
        {/* v2.6 — the grammar, discoverable: one line on the two modes, then a
            chip per command that seeds the textarea. helpText.js is the shared
            source, so these can't drift from the "?" overlay. Only for the
            ORCHESTRATOR target — mail needs no grammar. */}
        {target === 'daemon' && (
          <div className="fd-composehint">
            <span className="hint-line">
              Commands run in the daemon — everything else you type here is logged as a note. Mail
              (any other target) is plain text.
            </span>
            <span className="hint-chips">
              {ORCH_COMMANDS.filter((c) => c.chip).map((c) => (
                <button
                  key={c.syntax}
                  type="button"
                  className="fd-cmdchip"
                  title={c.does}
                  // Seed the command PREFIX in front of whatever is typed — a
                  // draft must never be thrown away by a chip click.
                  onClick={() => {
                    setText((cur) =>
                      cur.startsWith(c.chip ?? '') ? cur : (c.chip ?? '') + cur.trimStart(),
                    );
                    setNote(null);
                    setErr(null);
                    setUnroutedText(null);
                    taRef.current?.focus();
                  }}
                >
                  {(c.chip ?? '').trim()}
                </button>
              ))}
            </span>
          </div>
        )}
        <div className="foot">
          {note ? (
            <span className="note" style={{ color: 'var(--ok)' }}>
              {note}
            </span>
          ) : (
            <span
              className="note"
              title={
                target === 'daemon'
                  ? 'commands run in the daemon itself, the moment you send'
                  : TURN_BOUNDARY_DEF
              }
            >
              {target === 'daemon'
                ? 'runs in the daemon immediately'
                : `delivers at the agent’s ${TURN_BOUNDARY_HINT}`}
            </span>
          )}
          {unroutedText != null && spawnAvailable && (
            <button
              type="button"
              className="fd-ctabtn"
              onClick={() => {
                onSpawnFor?.(unroutedText);
              }}
            >
              spawn a session for this
            </button>
          )}
          {err && (
            <span className="note" style={{ color: 'var(--hazard)' }}>
              {err}
            </span>
          )}
          <span className="fd-spacer" />
          <button
            type="button"
            className="send"
            onClick={() => {
              void send();
            }}
            disabled={!text.trim() || busy}
          >
            Send ⏎
          </button>
        </div>
      </div>
    </div>
  );
}
